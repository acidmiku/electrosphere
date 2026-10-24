use ac3_audio::{BackgroundVoice, Bank, MAX_BACKGROUND_FRAMES};

fn fixture() -> Vec<u8> {
    let mut data = vec![0; 32 + 48];
    data[0] = 0x5d;
    data[1] = 1;
    data[2..4].copy_from_slice(&8_u16.to_le_bytes());
    data[4..8].copy_from_slice(&0x1010_u32.to_le_bytes());
    data[8..12].copy_from_slice(&48_u32.to_le_bytes());
    data[16..18].copy_from_slice(&12000_i16.to_le_bytes());
    data[18..20].copy_from_slice(&(-6000_i16).to_le_bytes());
    data[20..22].copy_from_slice(&0x3c00_u16.to_le_bytes());
    data[22..24].copy_from_slice(&0x5401_u16.to_le_bytes());
    data[24..26].copy_from_slice(&0x80ff_u16.to_le_bytes());
    data[26..28].copy_from_slice(&0x5fc9_u16.to_le_bytes());
    data[28..32].copy_from_slice(&0x1010_u32.to_le_bytes());
    for (index, block) in data[32..].chunks_exact_mut(16).enumerate() {
        block.fill(0x11);
        block[0] = if index == 0 { 0x02 } else { 0x18 };
        block[1] = [0, 6, 3][index];
    }
    data
}

#[test]
fn muted_ticks_preserve_history_and_chunked_rendering_is_continuous() {
    let bytes = fixture();
    let bank = Bank::parse(&bytes).unwrap();
    let mut reference = BackgroundVoice::from_bank(bank, 0x5d, 208).unwrap();
    let expected = reference.render_frames(10000).unwrap().frames;
    let mut segmented = BackgroundVoice::from_bank(bank, 0x5d, 208).unwrap();
    let mut got = segmented.render_frames(333).unwrap().frames;
    got.extend(segmented.render_frames(9667).unwrap().frames);
    assert_eq!(got, expected);
    let mut muted = BackgroundVoice::from_bank(bank, 0x5d, 208).unwrap();
    muted.set_gain(0).unwrap();
    assert!(
        muted
            .render_frames(4096)
            .unwrap()
            .frames
            .iter()
            .all(|f| *f == [0, 0])
    );
    muted.set_gain(128).unwrap();
    assert_eq!(muted.render_frames(5904).unwrap().frames, expected[4096..]);
    assert!(muted.set_gain(129).is_err());
    assert!(muted.render_frames(MAX_BACKGROUND_FRAMES + 1).is_err());
}

#[test]
fn background_rejects_wrong_profile_or_missing_loop_before_playback() {
    let bytes = fixture();
    assert!(BackgroundVoice::from_bank(Bank::parse(&bytes).unwrap(), 0x56, 208).is_err());
    assert!(BackgroundVoice::from_bank(Bank::parse(&bytes).unwrap(), 0x5d, 257).is_err());
    for (offset, replacement) in [(12, 1), (22, 0), (24, 0), (26, 0), (49, 2), (65, 1)] {
        let mut changed = bytes.clone();
        changed[offset] = replacement;
        assert!(BackgroundVoice::from_bank(Bank::parse(&changed).unwrap(), 0x5d, 208).is_err());
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and controlled original background PCM captures"]
fn repeated_background_segments_match_original_mixer() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let bytes = std::fs::read(root.join("extracted/archives/bpb_0072_0009.bin")).unwrap();
    let bank = Bank::parse(&bytes).unwrap();
    for (id, capture, total, start, pitch, volumes) in [
        (0x5d, "mission-5d-001", 1426635, 737992, 0x400, [1098, 1765]),
        (0x5e, "mission-5e-001", 1427958, 738883, 0x723, [2171, 1230]),
        (0x5f, "mission-5f-002", 1427517, 738433, 0xcb6, [678, 1744]),
        (0x60, "mission-60-001", 1427076, 737686, 0x285, [1344, 556]),
        (0x61, "mission-61-001", 1427076, 738001, 0x65a, [311, 771]),
    ] {
        let mut voice = BackgroundVoice::from_bank(bank, id, 208).unwrap();
        let directory = root.join("analysis/audio-runtime").join(capture);
        let captured = std::fs::read(directory.join("original-mix.f32le-stereo.raw")).unwrap();
        assert_eq!(captured.len(), total * 8);
        for frame in start - 64..start + 600000 {
            let expected = if frame < start { [0, 0] } else { voice.tick() };
            for (channel, sample) in expected.into_iter().enumerate() {
                let offset = frame * 8 + channel * 4;
                let original = f32::from_le_bytes(captured[offset..offset + 4].try_into().unwrap());
                assert_eq!(
                    original,
                    f32::from(sample) / 32767.0,
                    "sound {id:x} frame {} channel {channel}",
                    frame as isize - start as isize
                );
            }
        }
        let registers = std::fs::read(directory.join("final.spu-ports.bin")).unwrap();
        let sound = bank.sound(id).unwrap();
        let offset = usize::from(sound.voice_hint) * 16;
        let words = registers[offset..offset + 12]
            .chunks_exact(2)
            .map(|v| u16::from_le_bytes([v[0], v[1]]));
        assert!(words.eq([
            volumes[0],
            volumes[1],
            pitch,
            (sound.spu_address / 8) as u16,
            sound.adsr1,
            sound.adsr2
        ]));
        assert_eq!(&registers[0x198..0x19c], &[0, 0, 0, 0]);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and controlled original gain trace"]
fn gain_worker_matches_original_state_and_volume_submissions() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let csv = std::fs::read_to_string(
        root.join("analysis/audio-runtime/background-gain-001/gain-oracle.csv"),
    )
    .unwrap();
    let mut gain = ac3_audio::VoiceGain::new();
    let mut rows = 0;
    for line in csv.lines().skip(1) {
        let v: Vec<i32> = line.split(',').map(|s| s.parse().unwrap()).collect();
        assert_eq!(v.len(), 11);
        if v[1] >= 0 {
            gain.fade_to(v[1] as u8, v[2] as u16).unwrap();
        }
        assert_eq!(
            [
                i32::from(gain.target()),
                i32::from(gain.current()),
                i32::from(gain.remaining())
            ],
            v[3..6],
            "pre-update frame {}",
            v[0]
        );
        let write = gain.advance(false);
        assert_eq!(
            write.map(i32::from).unwrap_or(-1),
            v[6],
            "write frame {}",
            v[0]
        );
        if let Some(value) = write {
            assert_eq!(
                [1098, 1765].map(|base| (base * i32::from(value)) >> 7),
                v[7..9],
                "volume frame {}",
                v[0]
            );
        }
        assert_eq!(i32::from(gain.current()), v[9]);
        if v[10] >= 0 {
            assert_eq!(i32::from(gain.remaining()), v[10]);
        }
        rows += 1;
    }
    assert_eq!(rows, 438);
}
