use ac3_audio::{Bank, Error, MenuClickVoice, OUTPUT_SAMPLE_RATE, render_menu_click};

fn bank_fixture() -> Vec<u8> {
    let mut data = vec![0; 48];
    data[0] = 0x3c;
    data[1] = 1;
    data[2..4].copy_from_slice(&8_u16.to_le_bytes());
    data[4..8].copy_from_slice(&0x1010_u32.to_le_bytes());
    data[8..12].copy_from_slice(&16_u32.to_le_bytes());
    data[12] = 255;
    data[16..18].copy_from_slice(&16384_i16.to_le_bytes());
    data[18..20].copy_from_slice(&(-16384_i16).to_le_bytes());
    data[20..22].copy_from_slice(&0x3c00_u16.to_le_bytes());
    data[22..24].copy_from_slice(&0x4800_u16.to_le_bytes());
    data[24..26].copy_from_slice(&0x80ff_u16.to_le_bytes());
    data[26..28].copy_from_slice(&0x5fc9_u16.to_le_bytes());
    data[28..32].copy_from_slice(&0x1010_u32.to_le_bytes());
    data[33] = 1;
    data[34..].fill(0x11); // 28 constant source samples at +4096.
    data
}

#[test]
fn interpolation_envelope_signed_volume_and_finite_duration() {
    let bytes = bank_fixture();
    let mut voice = MenuClickVoice::from_bank(Bank::parse(&bytes).unwrap(), 0x3c, 128).unwrap();
    assert_eq!(voice.len(), 58); // 28 half-rate samples, three-sample priming, six startup ticks.
    for _ in 0..6 {
        assert_eq!(voice.next(), Some([0, 0]));
    }
    // A nonzero source from its first sample exposes the four attack ticks.
    for expected in [760, 1762, 2007, 2039] {
        assert_eq!(voice.next(), Some([expected, -expected]));
    }
    assert_eq!(voice.len(), 48);
    assert_eq!(voice.by_ref().count(), 48);
    assert_eq!(voice.next(), None);
    assert_eq!(voice.next(), None);
}

#[test]
fn unverified_voice_profiles_fail_and_zero_gain_is_silent() {
    let bytes = bank_fixture();
    let bank = Bank::parse(&bytes).unwrap();
    assert!(render_menu_click(bank, 0x3a, 208).is_err());
    assert!(render_menu_click(bank, 0x3c, 257).is_err());
    assert!(render_menu_click(bank, 0x3c, 256).is_err()); // positive volume above fixed signed range
    assert!(
        render_menu_click(bank, 0x3c, 0)
            .unwrap()
            .frames
            .iter()
            .all(|f| *f == [0, 0])
    );
    for offset in [20, 22, 24, 26, 33] {
        let mut changed = bytes.clone();
        changed[offset] ^= 2;
        assert!(matches!(
            render_menu_click(Bank::parse(&changed).unwrap(), 0x3c, 128),
            Err(Error::UnsupportedVoice(_))
        ));
    }
}

#[test]
fn cancel_and_title_have_distinct_pitch_and_sustain_rates() {
    for (id, note, adsr2, frames, expected) in [
        (0x3b, 0x4800_u16, 0x4c09_u16, 11202, 1020),
        (0x58, 0x5700, 0x4e09, 26638, 1784),
    ] {
        let mut bytes = bank_fixture();
        bytes[0] = id;
        bytes[22..24].copy_from_slice(&note.to_le_bytes());
        bytes[26..28].copy_from_slice(&adsr2.to_le_bytes());
        bytes[8..12].copy_from_slice(&3200_u32.to_le_bytes());
        bytes.resize(32 + 3200, 0);
        for block in bytes[32..].chunks_exact_mut(16) {
            block.fill(0x11);
            block[0] = 0;
            block[1] = 0;
        }
        bytes[32 + 3184 + 1] = 1;
        let pcm = render_menu_click(Bank::parse(&bytes).unwrap(), u16::from(id), 128).unwrap();
        assert_eq!(pcm.frames.len(), frames);
        // After 4096 ticks both pitches reach the same interpolation phase;
        // the different sustain intervals produce the independently calculated levels.
        assert_eq!(pcm.frames[6 + 4096], [expected, -expected]);
        if id == 0x3b {
            assert!(pcm.frames[6 + 8196..].iter().all(|f| *f == [0, 0]));
        }
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and original cancel/title audio captures"]
fn complete_cancel_and_isolated_title_match_original_pcm() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    for (id, bank_name, capture_name, total, start, length) in [
        (
            0x3b,
            "bpb_0000_0004.bin",
            "cancel-001",
            807471,
            737955,
            19098,
        ),
        (
            0x58,
            "bpb_0069_0004.bin",
            "title-isolated-001",
            985635,
            808066,
            92573,
        ),
    ] {
        let bank_bytes = std::fs::read(root.join("extracted/archives").join(bank_name)).unwrap();
        let pcm = render_menu_click(Bank::parse(&bank_bytes).unwrap(), id, 208).unwrap();
        assert_eq!(pcm.frames.len(), length);
        let directory = root.join("analysis/audio-runtime").join(capture_name);
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
        if id == 0x58 {
            let registers = std::fs::read(directory.join("final.spu-ports.bin")).unwrap();
            let voice: Vec<_> = registers[64..80]
                .chunks_exact(2)
                .map(|x| u16::from_le_bytes([x[0], x[1]]))
                .collect();
            assert_eq!(
                &voice[..6],
                &[0x2278, 0x2278, 0x35d, 0xd148, 0x80ff, 0x4e09]
            );
            assert_eq!(&registers[0x198..0x19c], &[0, 0, 0, 0]); // no voice reverb
            // Before XA music begins, the ordinary mixed capture agrees too.
            let mixed = std::fs::read(
                root.join("analysis/audio-runtime/title-002/original-mix.f32le-stereo.raw"),
            )
            .unwrap();
            for (index, frame) in pcm.frames.iter().take(14371).enumerate() {
                for (channel, sample) in frame.iter().enumerate() {
                    let offset = (808552 + index) * 8 + channel * 4;
                    let original =
                        f32::from_le_bytes(mixed[offset..offset + 4].try_into().unwrap());
                    assert_eq!(original, f32::from(*sample) / 32767.0);
                }
            }
        }
    }
}

#[test]
fn stereo_wav_preserves_channels_and_output_clock() {
    let bytes = bank_fixture();
    let pcm = render_menu_click(Bank::parse(&bytes).unwrap(), 0x3c, 128).unwrap();
    let wave = pcm.to_wav().unwrap();
    assert_eq!(&wave[..4], b"RIFF");
    assert_eq!(&wave[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes(wave[22..24].try_into().unwrap()), 2);
    assert_eq!(
        u32::from_le_bytes(wave[24..28].try_into().unwrap()),
        OUTPUT_SAMPLE_RATE
    );
    assert_eq!(wave.len(), 44 + 58 * 4);
    assert_eq!(&wave[68..72], &[0xf8, 0x02, 0x08, 0xfd]); // +760 / -760 at tick six
}

#[test]
#[ignore = "requires AC3_DATA_DIR assets and original audio-runtime/menu-clicks-002 PCM"]
fn complete_click_waveforms_match_original_mixer_capture() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let bytes = std::fs::read(root.join("extracted/archives/bpb_0000_0004.bin")).unwrap();
    let bank = Bank::parse(&bytes).unwrap();
    let captured = std::fs::read(
        root.join("analysis/audio-runtime/menu-clicks-002/original-mix.f32le-stereo.raw"),
    )
    .unwrap();
    assert_eq!(captured.len(), 842310 * 8);
    for (id, start, length) in [(0x3c, 790914, 2130), (0x3d, 738000, 4762)] {
        let generated = render_menu_click(bank, id, 208).unwrap();
        assert_eq!(generated.frames.len(), length);
        // Compare the complete rendered voice and quiet guards, not just a peak
        // or rescaled excerpt. Start positions refer to the immutable capture.
        for frame in start - 64..start + length + 64 {
            for channel in 0..2 {
                let offset = frame * 8 + channel * 4;
                let original = f32::from_le_bytes(captured[offset..offset + 4].try_into().unwrap());
                let expected = if (start..start + length).contains(&frame) {
                    generated.frames[frame - start][channel]
                } else {
                    0
                };
                // Redux's SDL sink divides its i16 mix by 32767, exactly.
                assert_eq!(
                    original,
                    f32::from(expected) / 32767.0,
                    "sound {id:x}, frame {}, channel {channel}",
                    frame as isize - start as isize
                );
            }
        }
    }
}
