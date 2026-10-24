use ac3_audio::xa::{Coding, RAW_SECTOR_BYTES, StreamDirectory, XaDecoder};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
};

/// Every audio sector of every PAL entry has a coding the decoder plays:
/// stereo 37.8 kHz (menus, briefing, two movies) and stereo 18.9 kHz (the
/// other movies, one audio entry). The first selected sectors of each
/// entry decode in full.
#[test]
#[ignore = "requires AC3_DATA_DIR and the original SPH/raw SPB"]
fn every_pal_audio_coding_decodes() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let header = fs::read(root.join("extracted/disc/ACE.SPH")).unwrap();
    let mut body = File::open(root.join("extracted/disc/ACE.SPB.raw2352")).unwrap();
    let directory = StreamDirectory::parse(&header, 161952).unwrap();
    assert_eq!(directory.disc(), 1);
    let mut codings: BTreeMap<(u8, u8), usize> = BTreeMap::new();
    let mut raw = [0; RAW_SECTOR_BYTES];
    for index in 0..directory.len() as u16 {
        let entry = directory.entry(0x800 | index).unwrap();
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
            *codings.entry((entry.mode, coding.byte())).or_default() += 1;
            if decoded < 4
                && let Some(pcm) = decoder.decode_sector(&raw).unwrap()
            {
                assert_eq!(pcm.frames.len(), coding.output_frames());
                decoded += 1;
            }
        }
        assert!(decoded > 0, "{:#x}", entry.resource);
    }
    eprintln!("PAL selected audio sectors by (mode, coding): {codings:x?}");
    assert_eq!(
        codings.keys().copied().collect::<Vec<_>>(),
        [(2, 0x01), (2, 0x05), (3, 0x01), (3, 0x05)]
    );
}

struct Reference {
    resource: u16,
    capture: &'static str,
    entry: (u32, u32, u8, u8),
    decoded_sectors: usize,
    first_compared_sector: usize,
    original_frames: usize,
    alignment: usize,
}

fn compare(reference: Reference) {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let header = fs::read(root.join("extracted/disc/ACE.SPH")).unwrap();
    let mut body = File::open(root.join("extracted/disc/ACE.SPB.raw2352")).unwrap();
    assert_eq!(body.metadata().unwrap().len(), 380911104);
    let directory = StreamDirectory::parse(&header, 161952).unwrap();
    assert_eq!(directory.len(), 72);
    let entry = directory.entry(reference.resource).unwrap();
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
            assert_eq!(pcm.frames.len(), 2352);
            frames.extend(pcm.frames);
            count += 1;
        }
    }
    assert_eq!(count, reference.decoded_sectors);
    let captured = fs::read(
        root.join("analysis/audio-runtime")
            .join(reference.capture)
            .join("original-mix.f32le-stereo.raw"),
    )
    .unwrap();
    assert_eq!(captured.len(), reference.original_frames * 8);
    // Each declared window excludes startup host-stream underruns and any
    // title overlap. Decode starts at sector 0 to preserve actual predictor/FIR
    // history. Every sample inside each fixed window must match, without
    // tolerance, shifting or omission.
    for (index, native) in frames
        .iter()
        .enumerate()
        .skip(reference.first_compared_sector * 2352)
    {
        for (channel, value) in native.iter().copied().enumerate() {
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
#[ignore = "requires AC3_DATA_DIR, original SPH/raw SPB and controlled XA capture"]
fn briefing_xa_matches_contiguous_original_output_window() {
    compare(Reference {
        resource: 0x827,
        capture: "stream-827-001",
        entry: (132676, 15680, 16, 2),
        decoded_sectors: 160,
        first_compared_sector: 11,
        original_frames: 1251117,
        alignment: 827904,
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR, original SPH/raw SPB and controlled XA capture"]
fn aircraft_xa_matches_contiguous_original_output_window() {
    compare(Reference {
        resource: 0x81d,
        capture: "stream-81d-001",
        entry: (105670, 9960, 24, 2),
        decoded_sectors: 256,
        first_compared_sector: 64,
        original_frames: 1515717,
        alignment: 823935,
    });
}
