//! Bounded file-backed playback of the original briefing and aircraft XA streams.
//! Disc seek/command latency is not emulated: the host starts at the requested
//! sector immediately and consumes decoded samples at its nominal audio clock.
use crate::{Result, sha256};
use ac3_audio::{
    StereoPcm,
    xa::{RAW_SECTOR_BYTES, StreamDirectory, StreamEntry, XaDecoder},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

const RAW_BYTES: u64 = 380_911_104;
const RAW_HASH: &str = "a77dd9a23a70017b7aa14f12aaac6b852e0c5ba62fd31a07310ee12535fde3a8";
const HEADER_HASH: &str = "4e90a1008ea50d542c44a15c9d8a164eb4601ea8f16bf441eb5bf80d1554cb63";

struct Source {
    file: File,
    header: Vec<u8>,
}
struct Cursor {
    entry: StreamEntry,
    next_sector: u32,
    selected_sectors: u32,
}

pub struct StreamAudio {
    directory: PathBuf,
    source: Option<Source>,
    decoder: XaDecoder,
    current: Option<Cursor>,
    buffered: VecDeque<[i16; 2]>,
    starts: u64,
    selected_sectors: u64,
    physical_sectors: u64,
}

impl StreamAudio {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            source: None,
            decoder: XaDecoder::new(1, 16, [150, 0, 150, 0]),
            current: None,
            buffered: VecDeque::new(),
            starts: 0,
            selected_sectors: 0,
            physical_sectors: 0,
        }
    }

    fn open_source(&mut self) -> Result<()> {
        if self.source.is_some() {
            return Ok(());
        }
        let header = fs::read(self.directory.join("ACE.SPH"))?;
        if header.len() != 596 || sha256(&header) != HEADER_HASH {
            return Err("stream header does not match the validated PAL disc".into());
        }
        let mut file = File::open(self.directory.join("ACE.SPB.raw2352"))?;
        if file.metadata()?.len() != RAW_BYTES {
            return Err("raw SPB has the wrong length".into());
        }
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        if format!("{:x}", digest.finalize()) != RAW_HASH {
            return Err("raw SPB does not match the validated PAL disc".into());
        }
        StreamDirectory::parse(&header, RAW_BYTES / RAW_SECTOR_BYTES as u64)?;
        self.source = Some(Source { file, header });
        Ok(())
    }

    pub fn start(&mut self, resource: u16) -> Result<Value> {
        // File filter and volume matrix are presently proven for this stream.
        // Other modes/resources must get an explicit implementation first.
        let channel = match resource {
            0x827 => 16,
            0x81d => 24,
            _ => {
                return Err(format!("stream {resource:#x} host playback is not validated").into());
            }
        };
        self.open_source()?;
        let source = self.source.as_mut().unwrap();
        let entry = StreamDirectory::parse(&source.header, RAW_BYTES / RAW_SECTOR_BYTES as u64)?
            .entry(resource)?;
        if entry.mode != 2 || entry.channel != channel {
            return Err("unexpected validated XA profile".into());
        }
        source.file.seek(SeekFrom::Start(
            u64::from(entry.first_sector) * RAW_SECTOR_BYTES as u64,
        ))?;
        self.decoder.select_stream(1, entry.channel);
        self.decoder.set_volume_matrix([150, 0, 150, 0]);
        self.buffered.clear();
        self.current = Some(Cursor {
            entry,
            next_sector: entry.first_sector,
            selected_sectors: 0,
        });
        self.starts += 1;
        Ok(
            json!({"action":"start","resource":resource,"status":"started","file":1,"channel":entry.channel,
            "first_sector":entry.first_sector,"sector_count":entry.sector_count,"volume_matrix":[150,0,150,0],
            "timing":"Immediate host start; original CD command and seek latency are not reproduced."}),
        )
    }

    pub fn stop(&mut self) -> Value {
        let resource = self.current.take().map(|cursor| cursor.entry.resource);
        self.buffered.clear();
        json!({"action":"stop","resource":resource,"status":"stopped"})
    }

    /// Retains at most one selected sector beyond the requested output block.
    /// The seek cursor never leaves the extent declared by the SPH directory.
    pub fn render_frames(&mut self, frames: usize) -> Result<Option<StereoPcm>> {
        let Some(cursor) = self.current.as_mut() else {
            return Ok(None);
        };
        let source = self.source.as_mut().ok_or("active stream without source")?;
        let end = cursor.entry.first_sector + cursor.entry.sector_count;
        let mut sector = [0_u8; RAW_SECTOR_BYTES];
        while self.buffered.len() < frames && cursor.next_sector < end {
            source.file.read_exact(&mut sector)?;
            cursor.next_sector += 1;
            self.physical_sectors += 1;
            if let Some(pcm) = self.decoder.decode_sector(&sector)? {
                cursor.selected_sectors += 1;
                self.selected_sectors += 1;
                self.buffered.extend(pcm.frames);
            }
        }
        let count = frames.min(self.buffered.len());
        let pcm = StereoPcm {
            frames: self.buffered.drain(..count).collect(),
        };
        if cursor.next_sector == end && self.buffered.is_empty() {
            self.current = None;
        }
        Ok((!pcm.frames.is_empty()).then_some(pcm))
    }

    pub fn status(&self) -> Value {
        json!({"resource":self.current.as_ref().map(|c|c.entry.resource),"active":self.current.is_some(),
            "starts":self.starts,"selected_sectors":self.selected_sectors,"physical_sectors":self.physical_sectors,
            "buffered_frames":self.buffered.len(),"source_verified":self.source.is_some()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_playback_is_lazy_and_unsupported_start_does_not_open_files() {
        let mut stream = StreamAudio::new(PathBuf::from("nonexistent-disc-directory"));
        assert!(stream.render_frames(882).unwrap().is_none());
        assert!(stream.start(0x828).is_err());
        assert_eq!(stream.status()["source_verified"], false);
        assert_eq!(stream.stop()["status"], "stopped");
    }
}
