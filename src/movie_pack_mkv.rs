//! One file per movie in a movie pack: Motion JPEG in Matroska (`.mkv`), as
//! `ffmpeg -i ... -c:v mjpeg -q:v 2 movie.mkv` writes it. Every frame is a
//! complete JPEG, so the pack decodes any frame on its own, in parallel,
//! with the same JPEG decoder as image packs ([`crate::movie_pack`]).
//!
//! [`mjpeg_frames`] scans the file's EBML structure once (element headers
//! only; frame data is skipped) for the video track's blocks and returns
//! where each frame's JPEG lies, in file order (Matroska stores Motion JPEG
//! in presentation order: no reordering). Laced blocks and other codecs are
//! refused.
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

const EBML: u32 = 0x1a45_dfa3;
const SEGMENT: u32 = 0x1853_8067;
const CLUSTER: u32 = 0x1f43_b675;
const TRACKS: u32 = 0x1654_ae6b;
const TRACK_ENTRY: u32 = 0xae;
const TRACK_NUMBER: u32 = 0xd7;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const BLOCK_GROUP: u32 = 0xa0;
const BLOCK: u32 = 0xa1;
const SIMPLE_BLOCK: u32 = 0xa3;

/// Elements whose children are read in line (the rest is skipped whole).
const ENTERED: [u32; 5] = [SEGMENT, CLUSTER, TRACKS, TRACK_ENTRY, BLOCK_GROUP];

/// Where a frame's JPEG lies in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSpan {
    pub offset: u64,
    pub length: u32,
}

/// A variable-length EBML integer: (value with or without the length
/// marker, its byte count); `None` at the end of the file.
fn vint<R: Read>(reader: &mut R, keep_marker: bool) -> Result<Option<(u64, u64)>, String> {
    let mut first = [0u8; 1];
    match reader.read(&mut first) {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(error) => return Err(error.to_string()),
    }
    let length = first[0].leading_zeros() + 1;
    if length > 8 {
        return Err("bad EBML number".to_owned());
    }
    let mut value = u64::from(first[0]);
    if !keep_marker {
        value &= (1u64 << (8 - length)) - 1;
    }
    let mut all_ones = value == (1u64 << (8 - length)) - 1;
    for _ in 1..length {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).map_err(|e| e.to_string())?;
        all_ones &= byte[0] == 0xff;
        value = (value << 8) | u64::from(byte[0]);
    }
    // An unknown size (all value bits set) reads as u64::MAX.
    if !keep_marker && all_ones {
        value = u64::MAX;
    }
    Ok(Some((value, u64::from(length))))
}

fn read_uint<R: Read>(reader: &mut R, size: u64) -> Result<u64, String> {
    if size > 8 {
        return Err("integer too long".to_owned());
    }
    let mut value = 0u64;
    for _ in 0..size {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).map_err(|e| e.to_string())?;
        value = (value << 8) | u64::from(byte[0]);
    }
    Ok(value)
}

/// The Motion JPEG frames of the Matroska file at `path`, in order.
pub fn mjpeg_frames(path: &Path) -> Result<Vec<FrameSpan>, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let mut reader = BufReader::with_capacity(1 << 16, file);
    let mut at = 0u64;
    let mut frames = Vec::new();
    // The video track: its number, once its entry is read.
    let mut video: Option<u64> = None;
    let (mut number, mut kind, mut codec) = (None, None, None::<String>);
    let mut first = true;
    while at < length {
        let Some((id, id_bytes)) = vint(&mut reader, true)? else {
            break;
        };
        let Some((size, size_bytes)) = vint(&mut reader, false)? else {
            break;
        };
        at += id_bytes + size_bytes;
        let id = id as u32;
        if first {
            if id != EBML {
                return Err(format!("{}: not a Matroska file", path.display()));
            }
            first = false;
        }
        if ENTERED.contains(&id) {
            if id == TRACK_ENTRY {
                (number, kind, codec) = (None, None, None);
            }
            continue;
        }
        if size == u64::MAX {
            return Err(format!("{}: an element of unknown size", path.display()));
        }
        match id {
            TRACK_NUMBER => number = Some(read_uint(&mut reader, size)?),
            TRACK_TYPE => kind = Some(read_uint(&mut reader, size)?),
            CODEC_ID => {
                let mut text = vec![0u8; size as usize];
                reader.read_exact(&mut text).map_err(|e| e.to_string())?;
                codec = Some(
                    String::from_utf8_lossy(&text)
                        .trim_end_matches('\0')
                        .to_owned(),
                );
            }
            SIMPLE_BLOCK | BLOCK => {
                let Some((track, track_bytes)) = vint(&mut reader, false)? else {
                    break;
                };
                let mut header = [0u8; 3];
                reader.read_exact(&mut header).map_err(|e| e.to_string())?;
                let used = track_bytes + 3;
                if Some(track) == video {
                    if header[2] & 0x06 != 0 {
                        return Err(format!("{}: laced video blocks", path.display()));
                    }
                    frames.push(FrameSpan {
                        offset: at + used,
                        length: u32::try_from(size - used).map_err(|_| "frame too large")?,
                    });
                }
                reader
                    .seek_relative((size - used) as i64)
                    .map_err(|e| e.to_string())?;
            }
            _ => {
                reader
                    .seek_relative(size as i64)
                    .map_err(|e| e.to_string())?;
            }
        }
        if matches!(id, TRACK_NUMBER | TRACK_TYPE | CODEC_ID)
            && video.is_none()
            && kind == Some(1)
            && let (Some(track), Some(codec)) = (number, &codec)
        {
            if codec != "V_MJPEG" {
                return Err(format!(
                    "{}: the video is {codec}; movie packs read Motion JPEG (V_MJPEG)",
                    path.display()
                ));
            }
            video = Some(track);
        }
        at += size;
    }
    if frames.is_empty() {
        return Err(format!("{}: no Motion JPEG frames", path.display()));
    }
    Ok(frames)
}

/// The bytes of `span` in the file at `path`.
pub fn read_span(path: &Path, span: FrameSpan) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    file.seek(SeekFrom::Start(span.offset))
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![0; span.length as usize];
    file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An EBML element: the ID's bytes, an 8-byte size, the body.
    fn element(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.push(0x01);
        out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn frames_are_found_in_simple_blocks_and_groups() {
        let track = |number: u8, kind: u8, codec: &str| {
            element(
                &[0xae],
                &[
                    element(&[0xd7], &[number]),
                    element(&[0x83], &[kind]),
                    element(&[0x86], codec.as_bytes()),
                ]
                .concat(),
            )
        };
        let tracks = element(
            &[0x16, 0x54, 0xae, 0x6b],
            &[track(2, 2, "A_FLAC"), track(1, 1, "V_MJPEG")].concat(),
        );
        let block = |id: &[u8], track: u8, data: &[u8]| {
            element(id, &[&[0x80 | track, 0, 0, 0x80][..], data].concat())
        };
        let cluster = element(
            &[0x1f, 0x43, 0xb6, 0x75],
            &[
                element(&[0xe7], &[0]),
                block(&[0xa3], 1, b"jpeg-one"),
                block(&[0xa3], 2, b"audio"),
                element(&[0xa0], &block(&[0xa1], 1, b"jpeg-two!")),
            ]
            .concat(),
        );
        let file = [
            element(
                &[0x1a, 0x45, 0xdf, 0xa3],
                &element(&[0x42, 0x82], b"matroska"),
            ),
            element(&[0x18, 0x53, 0x80, 0x67], &[tracks, cluster].concat()),
        ]
        .concat();
        let path = std::env::temp_dir().join(format!("ac3-mkv-{}.mkv", std::process::id()));
        std::fs::write(&path, &file).unwrap();
        let frames = mjpeg_frames(&path).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(read_span(&path, frames[0]).unwrap(), b"jpeg-one");
        assert_eq!(read_span(&path, frames[1]).unwrap(), b"jpeg-two!");
        // Another video codec is refused, as is a file that is not Matroska.
        let mut other = file.clone();
        let at = other.windows(7).position(|w| w == b"V_MJPEG").unwrap();
        other[at..at + 7].copy_from_slice(b"V_AV1\0\0");
        std::fs::write(&path, other).unwrap();
        assert!(mjpeg_frames(&path).unwrap_err().contains("V_AV1"));
        std::fs::write(&path, b"not matroska at all").unwrap();
        assert!(mjpeg_frames(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
