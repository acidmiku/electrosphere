//! The resident CD audio volume settings 8002B900..8002B988 in `mainloop.rs`
//! (no functions.tsv records; overlay 1's sound options call 8002B900 and
//! 8002B934). No capture records a call, so each routine is compared with
//! the test oracle (`support::oracle`), which interprets the original
//! instructions from a captured machine (`mainloop-sprintf-001`'s first
//! sprintf entry: resident code, `[gp+0x48]` set). 8002B934's call of
//! 800295B8 leaves the interpreted range and runs `boot::set_cd_volume` on
//! both sides; its CdMix host call and the CdlATV it builds in the dead
//! stack are compared as well.
mod support;

use ac3_flight::{Machine, mainloop};
use support::oracle;

const QUEUE_POINTER: u32 = ac3_flight::GP + 0x48;

fn base() -> Machine {
    let entry = oracle::entries("mainloop-sprintf-001", 0x800a_cd28)
        .into_iter()
        .next()
        .unwrap();
    entry.machine()
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-sprintf-001"]
fn oracle_cd_audio_volume_settings() {
    let base = base();
    let queue = base.clone().lw(QUEUE_POINTER);
    assert!(queue & 0xff00_0000 == 0x8000_0000, "queue {queue:08x}");
    let mut changed = 0;
    for stereo in [0, 1, 0x1234_5678] {
        changed += oracle::diff(
            "8002B900",
            &base,
            (0x8002_b900, 0x8002_b90c),
            [stereo, 0, 0, 0],
            |m| Some(mainloop::set_cd_stereo(m, stereo)),
        );
    }
    for volume in [0, 0x7f, 0xff, 0x100, -1i32 as u32, 0x7fff_ffff, 0x8000_0000] {
        changed += oracle::diff(
            "8002B90C",
            &base,
            (0x8002_b90c, 0x8002_b934),
            [volume, 0, 0, 0],
            |m| Some(mainloop::set_cd_audio_volume(m, volume)),
        );
    }
    for (offset, start) in [(0x160, 0x8002_b960), (0x164, 0x8002_b974)] {
        let mut m = base.clone();
        m.sw(queue + offset, 0x55aa_1234);
        oracle::diff("8002B960/74", &m, (start, start + 0x14), [0; 4], |m| {
            Some(mainloop::cd_audio_setting(m, offset))
        });
    }
    for (volume, stereo) in [
        (0x80, 0),
        (0x80, 1),
        (0xff, 1),
        (0x96, 0),
        (0x97, 1),
        (0, 0),
    ] {
        let mut m = base.clone();
        m.sw(queue + 0x164, volume);
        m.sw(queue + 0x160, stereo);
        oracle::diff("8002B934", &m, (0x8002_b934, 0x8002_b960), [0; 4], |m| {
            Some(mainloop::apply_cd_audio_volume(m))
        });
        // The CdMix call and its CdlATV (in the stack, which `diff` skips).
        let (mut original, mut native) = (m.clone(), m.clone());
        oracle::run(&mut original, 0x8002_b934, 0x8002_b960, [0; 4]);
        mainloop::apply_cd_audio_volume(&mut native);
        let calls = |m: &mut Machine| {
            let host_calls = m.host_calls.clone();
            host_calls
                .into_iter()
                .map(|c| {
                    let atv = m.read_bytes(c.args[0], 4);
                    (c.function, c.args, atv)
                })
                .collect::<Vec<_>>()
        };
        let (expected, actual) = (calls(&mut original), calls(&mut native));
        assert!(!expected.is_empty(), "8002B934: no CdMix");
        assert_eq!(actual, expected, "8002B934 ({volume:#x}, {stereo}): CdMix");
    }
    println!("CD audio volume settings: {changed} bytes changed");
    // Setter cases change one to four bytes each where the value differs.
    assert!(changed >= 8, "{changed}");
}
