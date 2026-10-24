//! The Japanese release's XA profiles on its discs (Rev 1; ignored: the
//! extracted files and captures live under `AC3_JP_DIR`, default
//! `~/ac3-jp`).
//!
//! - Both SPH directories parse (type 1 / 2, the other disc's entries
//!   absent), and every audio sector of every present entry has a coding
//!   the decoder plays: stereo 37.8 / 18.9 kHz and mono 18.9 kHz, all 4-bit.
//! - Controlled PCSX-Redux captures (the pinned build, disc 1 booted from
//!   power-on; the first SPH lookup at 8002995C redirected from the attract
//!   movie `0x9E6` to the resource; CdMix [150, 0, 150, 0]; SDL disk output,
//!   f32 = i16 / 32767): every sample of a declared window equals the
//!   native decode, at one fixed alignment, from zero decoder state.
//!   - `jp-xa-8a5-001`: mono 18.9 kHz voice clip `0x8A5` (channel 1, 41
//!     sectors). Sectors 2..17 (150528 frames); the first two hold the
//!     emulator's startup underruns, and the stream stops after sector 17
//!     (the movie player gives it up).
//!   - `jp-xa-83a-001`: stereo 18.9 kHz music `0x83A` (channel 4).
//!     Sectors 0..64 (305760 frames) from the stream's first frame, until
//!     the player stops it.
use ac3_audio::xa::{Coding, RAW_SECTOR_BYTES, StreamDirectory, XaDecoder};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

fn jp_dir() -> PathBuf {
    std::env::var_os("AC3_JP_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("ac3-jp")))
        .expect("AC3_JP_DIR or HOME")
}

fn disc(n: u32) -> (Vec<u8>, File, u64) {
    let directory = jp_dir().join(format!("extracted/disc{n}/disc"));
    let header = fs::read(directory.join("ACE.SPH")).unwrap();
    let body = File::open(directory.join("ACE.SPB.raw2352")).unwrap();
    let sectors = body.metadata().unwrap().len() / RAW_SECTOR_BYTES as u64;
    (header, body, sectors)
}

#[test]
#[ignore = "requires the Japanese discs' extracted SPH/SPB (AC3_JP_DIR)"]
fn japanese_directories_and_every_audio_coding() {
    for (n, present) in [(1, 319), (2, 210)] {
        let (header, mut body, sectors) = disc(n);
        let directory = StreamDirectory::parse(&header, sectors).unwrap();
        assert_eq!((directory.disc(), directory.len()), (n, 487));
        let entries: Vec<_> = (0..487u16)
            .filter_map(|i| directory.entry(0x800 | i).ok())
            .collect();
        assert_eq!(entries.len(), present, "disc {n}");
        let mut codings: BTreeMap<u8, usize> = BTreeMap::new();
        let mut raw = [0; RAW_SECTOR_BYTES];
        for entry in &entries {
            body.seek(SeekFrom::Start(
                u64::from(entry.first_sector) * RAW_SECTOR_BYTES as u64,
            ))
            .unwrap();
            let mut decoder = XaDecoder::new(1, entry.channel, [128, 0, 128, 0]);
            let mut decoded = 0;
            for _ in 0..entry.sector_count {
                body.read_exact(&mut raw).unwrap();
                if raw[18] & 4 == 0 || raw[17] != entry.channel {
                    continue;
                }
                let coding = Coding::parse(raw[19]).unwrap();
                *codings.entry(coding.byte()).or_default() += 1;
                // The first selected sectors of each entry decode fully.
                if decoded < 4
                    && let Some(pcm) = decoder.decode_sector(&raw).unwrap()
                {
                    assert_eq!(pcm.frames.len(), coding.output_frames());
                    decoded += 1;
                }
            }
            assert!(decoded > 0, "disc {n} {:#x}", entry.resource);
        }
        eprintln!("disc {n}: selected audio sectors by coding {codings:x?}");
        assert_eq!(
            codings.keys().copied().collect::<Vec<_>>(),
            [0x01, 0x04, 0x05],
            "stereo 37.8, mono 18.9 and stereo 18.9 kHz"
        );
    }
}

struct Reference {
    resource: u16,
    capture: &'static str,
    entry: (u32, u32, u8, u8),
    coding: u8,
    decoded_sectors: usize,
    window: std::ops::Range<usize>,
    original_frames: usize,
    alignment: usize,
}

fn compare(reference: Reference) {
    let (header, mut body, sectors) = disc(1);
    let entry = StreamDirectory::parse(&header, sectors)
        .unwrap()
        .entry(reference.resource)
        .unwrap();
    assert_eq!(
        (
            entry.first_sector,
            entry.sector_count,
            entry.channel,
            entry.mode
        ),
        reference.entry
    );
    body.seek(SeekFrom::Start(
        u64::from(entry.first_sector) * RAW_SECTOR_BYTES as u64,
    ))
    .unwrap();
    let mut decoder = XaDecoder::new(1, entry.channel, [150, 0, 150, 0]);
    let mut raw = [0; RAW_SECTOR_BYTES];
    let mut frames = Vec::new();
    let mut count = 0;
    for _ in 0..entry.sector_count {
        if count == reference.decoded_sectors {
            break;
        }
        body.read_exact(&mut raw).unwrap();
        if let Some(pcm) = decoder.decode_sector(&raw).unwrap() {
            assert_eq!(raw[19], reference.coding);
            frames.extend(pcm.frames);
            count += 1;
        }
    }
    assert_eq!(count, reference.decoded_sectors);
    let captured = fs::read(
        jp_dir()
            .join("analysis/audio-runtime")
            .join(reference.capture)
            .join("original-mix.f32le-stereo.raw"),
    )
    .unwrap();
    assert_eq!(captured.len(), reference.original_frames * 8);
    assert!(reference.window.end <= frames.len());
    for index in reference.window.clone() {
        for (channel, value) in frames[index].iter().copied().enumerate() {
            let offset = (reference.alignment + index) * 8 + channel * 4;
            let original = f32::from_le_bytes(captured[offset..offset + 4].try_into().unwrap());
            assert_eq!(
                original,
                f32::from(value) / 32767.0,
                "XA resource {:x} frame {index} channel {channel}",
                reference.resource
            );
        }
    }
}

#[test]
#[ignore = "requires the Japanese disc 1 files and capture jp-xa-8a5-001 (AC3_JP_DIR)"]
fn mono_18900_matches_the_original_output_window() {
    compare(Reference {
        resource: 0x8a5,
        capture: "jp-xa-8a5-001",
        entry: (58832, 1312, 1, 2),
        coding: 0x04,
        decoded_sectors: 18,
        window: 2 * 9408..18 * 9408,
        original_frames: 2034333,
        alignment: 1556877,
    });
}

#[test]
#[ignore = "requires the Japanese disc 1 files and capture jp-xa-83a-001 (AC3_JP_DIR)"]
fn stereo_18900_matches_the_original_output_from_the_start() {
    compare(Reference {
        resource: 0x83a,
        capture: "jp-xa-83a-001",
        entry: (141281, 19128, 4, 2),
        coding: 0x05,
        decoded_sectors: 65,
        window: 0..65 * 4704,
        original_frames: 2339064,
        alignment: 1567314,
    });
}
