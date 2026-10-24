use ac3_audio::{Bank, MAX_BLOCKS, render_menu_click};

#[test]
#[ignore = "requires AC3_DATA_DIR and original menu/mission sound banks"]
fn mission_bank_replaces_ids_and_all_transition_samples_are_finite() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let bytes = std::fs::read(root.join("extracted/archives/bpb_0072_0009.bin")).unwrap();
    let bank = Bank::parse(&bytes).unwrap();
    assert_eq!(
        (
            bank.first_sound_id(),
            bank.sound_count(),
            bank.spu_address()
        ),
        (84, 23, 0x43c90)
    );
    for (id, samples, sample_note, adsr1, adsr2) in [
        (0x56, 3892, 0x4800, 0x80ff, 0x5fc9),
        (0x5a, 17780, 0x4800, 0xafff, 0x5fc9),
        (0x5b, 24556, 0x4639, 0xb1ff, 0xcc49),
        (0x5c, 2268, 0x4a00, 0x91ff, 0x5fc9),
        (0x62, 39424, 0x5400, 0xb5ff, 0xdfc9),
        (0x63, 39424, 0x6000, 0xacff, 0xce89),
        (0x64, 3892, 0x6200, 0xa764, 0x4c49),
        (0x65, 3892, 0x5e00, 0x80a6, 0x4dc9),
        (0x66, 39424, 0x5200, 0x80ff, 0x4e89),
    ] {
        let decoded = bank.decode_sound(id, MAX_BLOCKS).unwrap();
        assert_eq!(decoded.sound.voice_hint, 255);
        assert_eq!(
            (decoded.sound.note, decoded.sound.sample_note),
            (0x3c00, sample_note)
        );
        assert_eq!((decoded.sound.adsr1, decoded.sound.adsr2), (adsr1, adsr2));
        assert_eq!(decoded.pcm.samples.len(), samples);
        assert_eq!(decoded.pcm.end_flags, 1);
        assert!(!decoded.pcm.repeat);
        assert!(!render_menu_click(bank, id, 208).unwrap().frames.is_empty());
    }
    assert_eq!(
        bank.sound(0x62).unwrap().spu_address,
        bank.sound(0x63).unwrap().spu_address
    );
    let mission = render_menu_click(bank, 0x56, 208).unwrap();
    assert_eq!(mission.frames.len(), 7786);
    let menu_bytes = std::fs::read(root.join("extracted/archives/bpb_0069_0004.bin")).unwrap();
    let menu = Bank::parse(&menu_bytes).unwrap();
    assert_ne!(
        menu.sound(0x56).unwrap().spu_address,
        bank.sound(0x56).unwrap().spu_address
    );
    assert_eq!(
        render_menu_click(menu, 0x56, 208).unwrap().frames.len(),
        191914
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and controlled original mission PCM captures"]
fn complete_finite_mission_voices_match_original_pcm() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let bytes = std::fs::read(root.join("extracted/archives/bpb_0072_0009.bin")).unwrap();
    let bank = Bank::parse(&bytes).unwrap();
    for (id, capture, total, start, length, volume, pitch) in [
        (0x56, "mission-56-001", 1145277, 737956, 7786, 5882, 0x800),
        (0x5a, "mission-5a-002", 949914, 737506, 35562, 6617, 0x800),
        (0x5b, "mission-5b-001", 1144395, 737506, 42640, 4411, 0x937),
        (0x5c, "mission-5c-001", 1144395, 737506, 5092, 9338, 0x721),
        (0x62, "mission-62-001", 1144836, 737992, 157694, 5882, 0x400),
        (0x63, "mission-63-001", 1144836, 737947, 315382, 7353, 0x200),
        (0x64, "mission-64-001", 933156, 737829, 34948, 9338, 0x1c8),
        (0x65, "mission-65-001", 933597, 737956, 27717, 9338, 0x23f),
        (0x66, "mission-66-001", 933597, 737956, 140540, 9338, 0x47d),
    ] {
        let pcm = render_menu_click(bank, id, 208).unwrap();
        assert_eq!(pcm.frames.len(), length);
        let directory = root.join("analysis/audio-runtime").join(capture);
        let captured = std::fs::read(directory.join("original-mix.f32le-stereo.raw")).unwrap();
        assert_eq!(captured.len(), total * 8);
        for frame in start - 64..start + length + 64 {
            for channel in 0..2 {
                let offset = frame * 8 + channel * 4;
                let original = f32::from_le_bytes(captured[offset..offset + 4].try_into().unwrap());
                let expected = if (start..start + length).contains(&frame) {
                    pcm.frames[frame - start][channel]
                } else {
                    0
                };
                assert_eq!(
                    original,
                    f32::from(expected) / 32767.0,
                    "sound {id:x} frame {} channel {channel}",
                    frame as isize - start as isize
                );
            }
        }
        let registers = std::fs::read(directory.join("final.spu-ports.bin")).unwrap();
        let sound = bank.sound(id).unwrap();
        let expected = [
            volume,
            volume,
            pitch,
            (sound.spu_address / 8) as u16,
            sound.adsr1,
            sound.adsr2,
        ];
        // Old register values can survive after a voice ends, so matching the
        // submitted profile does not by itself identify its allocation slot.
        assert!(registers[..24 * 16].chunks_exact(16).any(|voice| {
            voice
                .chunks_exact(2)
                .take(6)
                .map(|v| u16::from_le_bytes([v[0], v[1]]))
                .eq(expected)
        }));
        assert_eq!(&registers[0x198..0x19c], &[0, 0, 0, 0]); // no voice reverb
    }
}
