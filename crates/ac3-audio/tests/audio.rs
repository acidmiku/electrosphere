use ac3_audio::{Bank, Error, MAX_BLOCKS, decode_adpcm, wav_pcm16_mono};

fn block(header: u8, flags: u8, bytes: u8) -> [u8; 16] {
    let mut block = [bytes; 16];
    block[0] = header;
    block[1] = flags;
    block
}

fn bank_fixture() -> Vec<u8> {
    // Two descriptors deliberately ordered after/before one another in RAM.
    let mut bytes = vec![0_u8; 52 + 48];
    bytes[..4].copy_from_slice(&[40, 2, 13, 0]);
    bytes[4..8].copy_from_slice(&0x1000_u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&48_u32.to_le_bytes());
    for (i, address) in [0x1020_u32, 0x1000].into_iter().enumerate() {
        let d = &mut bytes[12 + i * 20..32 + i * 20];
        d[0] = 255;
        d[1..4].copy_from_slice(&[1, 2, 3]);
        d[4..6].copy_from_slice(&(-128_i16).to_le_bytes());
        d[6..8].copy_from_slice(&256_i16.to_le_bytes());
        d[8..10].copy_from_slice(&0x3c00_u16.to_le_bytes());
        d[10..12].copy_from_slice(&0x4800_u16.to_le_bytes());
        d[12..14].copy_from_slice(&0x80ff_u16.to_le_bytes());
        d[14..16].copy_from_slice(&0x5fc9_u16.to_le_bytes());
        d[16..20].copy_from_slice(&address.to_le_bytes());
    }
    bytes[52..68].copy_from_slice(&block(12, 4, 0x21));
    bytes[68..84].copy_from_slice(&block(12, 1, 0x43));
    bytes[84..100].copy_from_slice(&block(12, 1, 0x65));
    bytes
}

#[test]
fn signed_nibbles_are_low_first_and_end_block_is_included() {
    let decoded = decode_adpcm(&block(12, 1, 0x8f), 1).unwrap();
    assert_eq!(decoded.samples.len(), 28);
    assert_eq!(&decoded.samples[..4], &[-1, -8, -1, -8]);
    assert_eq!(decoded.blocks_consumed(), 1);
    assert_eq!(decoded.loop_start, None);
    assert!(!decoded.repeat);
}

#[test]
fn filters_carry_clamped_history_and_shift_each_product() {
    // First block establishes two unequal history values. The second has zero
    // nibbles, exposing independently calculated predictor-only sample values.
    let mut first = block(12, 0, 0);
    first[15] = 0x27;
    for (filter, expected) in [
        (0, [0, 0, 0]),
        (1, [1, 0, 0]),
        (2, [-3, -8, -13]),
        (3, [-4, -9, -11]),
        (4, [-4, -10, -17]),
    ] {
        let mut bytes = first.to_vec();
        bytes.extend_from_slice(&block(filter << 4 | 12, 1, 0));
        let result = decode_adpcm(&bytes, 2).unwrap();
        assert_eq!(&result.samples[28..31], &expected, "filter {filter}");
    }
    let saturated = decode_adpcm(&block(0x10, 1, 0x77), 1).unwrap();
    assert_eq!(saturated.samples[0], 28672);
    assert!(saturated.samples[1..].iter().all(|s| *s == i16::MAX));
    let negative = decode_adpcm(&block(0x10, 1, 0x88), 1).unwrap();
    assert!(negative.samples.iter().all(|s| *s == i16::MIN));
}

#[test]
fn loop_markers_are_preserved_without_expansion() {
    let mut bytes = block(12, 4, 0x11).to_vec();
    bytes.extend_from_slice(&block(12, 6, 0x22));
    bytes.extend_from_slice(&block(12, 3, 0x33));
    // Invalid trailing bytes belong to another sample and must not be decoded.
    bytes.extend_from_slice(&[255; 16]);
    let decoded = decode_adpcm(&bytes, 3).unwrap();
    assert_eq!(decoded.samples.len(), 84);
    assert_eq!(decoded.block_flags, [4, 6, 3]);
    assert_eq!(decoded.loop_start, Some(28));
    assert_eq!(decoded.end_flags, 3);
    assert!(decoded.repeat);
}

#[test]
fn malformed_streams_fail_before_allocating_output() {
    assert!(matches!(
        decode_adpcm(&[0; 15], 1),
        Err(Error::Truncated(_))
    ));
    assert_eq!(decode_adpcm(&[0; 16], 2), Err(Error::MissingEnd));
    assert!(matches!(
        decode_adpcm(&[0; 32], 1),
        Err(Error::LimitExceeded(_))
    ));
    assert!(matches!(decode_adpcm(&[], 0), Err(Error::Invalid(_))));
    assert!(matches!(
        decode_adpcm(&[], MAX_BLOCKS + 1),
        Err(Error::Invalid(_))
    ));
    for header in [13, 14, 15, 0x50, 0xff] {
        assert!(matches!(
            decode_adpcm(&block(header, 1, 0), 1),
            Err(Error::UnsupportedAdpcmHeader { block: 0, .. })
        ));
    }
    assert!(matches!(
        decode_adpcm(&block(0, 9, 0), 1),
        Err(Error::UnsupportedAdpcmFlags { block: 0, .. })
    ));
}

#[test]
fn bank_descriptors_and_unsorted_sample_extents() {
    let bytes = bank_fixture();
    let bank = Bank::parse(&bytes).unwrap();
    assert_eq!((bank.first_sound_id(), bank.sound_count()), (40, 2));
    assert_eq!(bank.spu_address(), 0x1000);
    assert_eq!(bank.bytes_consumed(), bytes.len());
    let sound = bank.sound(40).unwrap();
    assert_eq!(sound.attributes, [1, 2, 3]);
    assert_eq!((sound.volume_left, sound.volume_right), (-128, 256));
    assert_eq!((sound.note, sound.sample_note), (0x3c00, 0x4800));
    assert_eq!((sound.adsr1, sound.adsr2), (0x80ff, 0x5fc9));
    assert_eq!(bank.encoded_sound(40).unwrap().len(), 16);
    assert_eq!(bank.encoded_sound(41).unwrap().len(), 32);
    assert_eq!(bank.decode_sound(41, 2).unwrap().pcm.samples.len(), 56);
    assert_eq!(bank.sound(39), Err(Error::UnknownSound(39)));
    assert_eq!(bank.sound(42), Err(Error::UnknownSound(42)));
}

#[test]
fn banks_reject_invalid_extents_addresses_and_ids() {
    let bytes = bank_fixture();
    for length in 0..bytes.len() {
        assert!(Bank::parse(&bytes[..length]).is_err());
    }
    for (offset, replacement) in [
        (0, vec![255]),
        (2, vec![0xff, 0xff]),
        (2, vec![3, 0]),
        (4, u32::MAX.to_le_bytes().to_vec()),
        (8, u32::MAX.to_le_bytes().to_vec()),
        (12, vec![24]),
        (28, 0xfff0_u32.to_le_bytes().to_vec()),
        (28, 0xffff_fff0_u32.to_le_bytes().to_vec()),
        (28, 0x1008_u32.to_le_bytes().to_vec()),
    ] {
        let mut invalid = bytes.clone();
        invalid[offset..offset + replacement.len()].copy_from_slice(&replacement);
        assert!(Bank::parse(&invalid).is_err(), "offset {offset}");
    }
    let mut shared = bytes.clone();
    shared[28..32].copy_from_slice(&0x1000_u32.to_le_bytes());
    let bank = Bank::parse(&shared).unwrap();
    assert_eq!(bank.encoded_sound(40), bank.encoded_sound(41));
    let mut padded = bytes.clone();
    padded.extend_from_slice(&[255; 8]);
    assert_eq!(Bank::parse(&padded).unwrap().bytes_consumed(), bytes.len());
}

#[test]
fn wav_is_explicit_mono_pcm_with_signed_little_endian_data() {
    let wav = wav_pcm16_mono(&[i16::MIN, -1, 0, i16::MAX], 22050).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 44);
    assert_eq!(&wav[20..24], &[1, 0, 1, 0]);
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 22050);
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(&wav[44..], &[0, 128, 255, 255, 0, 0, 255, 127]);
    assert!(wav_pcm16_mono(&[], 0).is_err());
    assert!(wav_pcm16_mono(&[], 192001).is_err());
}

#[test]
fn bounded_mutation_smoke_test_never_panics() {
    let original = bank_fixture();
    for index in 0..original.len() {
        for value in [0, 1, 0x7f, 0x80, 0xff] {
            let mut bytes = original.clone();
            bytes[index] = value;
            if let Ok(bank) = Bank::parse(&bytes) {
                for id in bank.first_sound_id()..bank.first_sound_id() + bank.sound_count() {
                    let _ = bank.decode_sound(id, 4);
                }
            }
        }
    }
}

#[test]
#[ignore = "requires original local assets via AC3_DATA_DIR"]
fn local_menu_banks_and_click_samples() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let mut total_blocks = 0;
    for (name, first, count, expected_blocks) in [
        ("bpb_0000_0004.bin", 0, 76, 48627),
        ("bpb_0069_0004.bin", 84, 6, 11546),
    ] {
        let bytes = std::fs::read(root.join("extracted/archives").join(name)).unwrap();
        let bank = Bank::parse(&bytes).unwrap();
        assert_eq!((bank.first_sound_id(), bank.sound_count()), (first, count));
        let mut blocks = 0;
        for id in first..first + count {
            let decoded = bank.decode_sound(id, MAX_BLOCKS).unwrap();
            blocks += decoded.pcm.blocks_consumed();
        }
        assert_eq!(blocks, expected_blocks);
        total_blocks += blocks;
        if first == 0 {
            for (id, address, expected_samples, volume) in
                [(0x3c, 0x283b0, 1064, 9956), (0x3d, 0x28620, 2380, 4163)]
            {
                let decoded = bank.decode_sound(id, MAX_BLOCKS).unwrap();
                assert_eq!(decoded.sound.spu_address, address);
                assert_eq!(decoded.sound.volume_left, volume);
                assert_eq!(
                    (decoded.sound.note, decoded.sound.sample_note),
                    (0x3c00, 0x4800)
                );
                assert_eq!((decoded.sound.adsr1, decoded.sound.adsr2), (0x80ff, 0x5fc9));
                assert_eq!(decoded.pcm.samples.len(), expected_samples);
                assert_eq!(decoded.pcm.loop_start, Some(28));
                assert!(!decoded.pcm.repeat);
            }
        }
    }
    assert_eq!(total_blocks, 60173);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and original mission-002 SPU captures"]
fn local_bank_uploads_and_click_registers_match_running_game() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let capture = root.join("analysis/menu-runtime/mission-002");
    let spu_ram = std::fs::read(capture.join("checkpoint.spu-ram.bin")).unwrap();
    assert_eq!(spu_ram.len(), ac3_audio::SPU_RAM_BYTES);
    for name in ["bpb_0000_0004.bin", "bpb_0069_0004.bin"] {
        let bytes = std::fs::read(root.join("extracted/archives").join(name)).unwrap();
        let bank = Bank::parse(&bytes).unwrap();
        let start = bank.spu_address() as usize;
        assert_eq!(
            bank.sample_bytes(),
            &spu_ram[start..start + bank.sample_bytes().len()]
        );
    }
    let bytes = std::fs::read(root.join("extracted/archives/bpb_0000_0004.bin")).unwrap();
    let bank = Bank::parse(&bytes).unwrap();
    for (id, name) in [
        (0x3c, "sound-3c-frame-00024.spu-ports.bin"),
        (0x3d, "sound-3d-frame-00004.spu-ports.bin"),
    ] {
        let ports = std::fs::read(capture.join(name)).unwrap();
        assert_eq!(ports.len(), 512);
        let sound = bank.sound(id).unwrap();
        let register = |offset: usize| {
            u16::from_le_bytes([ports[12 * 16 + offset], ports[12 * 16 + offset + 1]])
        };
        assert_eq!(
            register(0),
            ((i32::from(sound.volume_left) * 208) >> 8) as u16
        );
        assert_eq!(
            register(2),
            ((i32::from(sound.volume_right) * 208) >> 8) as u16
        );
        assert_eq!(register(4), 0x800);
        assert_eq!(u32::from(register(6)) * 8, sound.spu_address);
        assert_eq!(register(8), sound.adsr1);
        assert_eq!(register(10), sound.adsr2);
    }
}
