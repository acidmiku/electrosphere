//! `ov2_aircraft` against the original game on ov2air's call traces (see
//! `tests/ov2air_trace/mod.rs` for the comparison), plus unit tests for
//! paths no capture reaches.
mod ov2air_trace;

use ac3_flight::SmallData;
use ac3_flight::{
    Machine,
    ov2_aircraft::{self as air, Calls},
};
use ov2air_trace::{Stats, Trace, check_capture};
use std::collections::BTreeMap;

/// Every routine of `ov2_aircraft` with whether it returns a value.
const ROUTINES: [(u32, bool); 31] = [
    (0x800c_e978, true),
    (0x800d_17d0, true),
    (0x800d_77e4, false),
    (0x800d_7950, false),
    (0x800d_7c54, false),
    (0x800d_7dc8, false),
    (0x800d_804c, false),
    (0x800d_808c, false),
    (0x800d_80b4, false),
    (0x800d_83fc, true),
    (0x800d_85b8, true),
    (0x800d_87f8, true),
    (0x800d_8abc, true),
    (0x800d_8cb4, true),
    (0x800d_8cfc, false),
    (0x800d_8d34, false),
    (0x800d_8e9c, false),
    (0x800d_9038, true),
    (0x800d_92b8, true),
    (0x800d_9438, true),
    (0x800d_9770, true),
    (0x800d_98a0, true),
    (0x800d_98c4, true),
    (0x800d_9c78, true),
    (0x800d_9ddc, true),
    (0x800d_a2cc, true),
    (0x800d_a440, true),
    (0x800d_a5f4, true),
    (0x800d_a7ac, true),
    (0x800d_a80c, true),
    (0x800e_7b50, false),
];

#[test]
fn routine_table_matches_module() {
    let listed: Vec<u32> = ROUTINES.iter().map(|r| r.0).collect();
    assert_eq!(listed, air::ROUTINES);
}

/// The traces (`.ov2air/captures.py`; 004 and 006 are natural, the others
/// documented perturbations written once at the aircraft-selection entry).
const CAPTURES: [(&str, &str); 8] = [
    (
        "ov2air-004",
        "natural first mission from mission-004/final.sav (inputs of aircraft-vectors-001):          aircraft init, 398 aircraft/weapon updates, weapon cancel, launch to 0x80",
    ),
    (
        "ov2air-005",
        "all aircraft available: aircraft and weapon navigation, slides restarted while          running, weapon cancel, launch skip",
    ),
    (
        "ov2air-006",
        "natural: aircraft selection cancelled back to the briefing (0x200)",
    ),
    (
        "ov2air-007",
        "mission 0x10 (category 0x10: only aircraft 0x12)",
    ),
    (
        "ov2air-008",
        "mission 0x14 (category 0x13: only aircraft 0x11), option 0x10, both weapon unlocks",
    ),
    (
        "ov2air-009",
        "mission 0x13 (category 0x12: forced weapons), option 0x10 with saved availability          3/5/8, context aircraft unavailable",
    ),
    (
        "ov2air-010",
        "mission 0x21 (category 0x20), option 0x10, both unlocks set (only +0xA04 applies)",
    ),
    (
        "ov2air-011",
        "all aircraft available, context aircraft 0x15: weapon navigation in both rows,          slides reversed and locked while running",
    ),
];

/// Every routine reproduces the original on every traced occurrence, and
/// every routine is covered non-vacuously (state changed, or `v0` checked).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2air-004..011"]
fn traced_captures_reproduce_the_original() {
    let mut total: BTreeMap<u32, Stats> = BTreeMap::new();
    for (capture, description) in CAPTURES {
        eprintln!("== {capture}: {description}");
        for (routine, s) in check_capture(&Trace::load(capture), &ROUTINES) {
            let t = total.entry(routine).or_default();
            t.passed += s.passed;
            t.effective += s.effective;
            t.changed += s.changed;
            t.spliced += s.spliced;
            t.interrupts += s.interrupts;
        }
    }
    eprintln!("== all captures");
    for (routine, returns) in ROUTINES {
        let s = total.get(&routine).copied().unwrap_or_default();
        eprintln!(
            "{routine:08x}: {} exact ({} changing state), {} bytes changed, {} calls spliced, {} interrupts",
            s.passed, s.effective, s.changed, s.spliced, s.interrupts
        );
        assert!(s.passed > 0, "{routine:08x} not reached");
        assert!(
            s.effective > 0 || returns,
            "{routine:08x}: no occurrence changes state"
        );
    }
}

// ---- paths the captures do not reach ------------------------------------------------

/// Records every outside call and returns 0.
#[derive(Default)]
struct Recorder {
    calls: Vec<(u32, [u32; 4])>,
}

impl Calls for Recorder {
    fn call(&mut self, _: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
        self.calls.push((target, args));
        0
    }
}

const CONTEXT: u32 = 0x800b_d158;
const SELECTION: u32 = 0x800f_9598;

/// Module 2 loaded, the context and the selection block at their natural
/// addresses.
fn machine() -> Machine {
    let mut m = Machine::default();
    m.set_gw(0x2dc, CONTEXT);
    m.sw(CONTEXT + 0x38, 2);
    m.sw(air::BLOCK, SELECTION);
    m
}

#[test]
fn mission_category_falls_back_to_record_41() {
    let mut m = machine();
    for i in 0..42 {
        m.sb(air::MISSIONS + i * 12, i as u8);
        m.sb(air::MISSIONS + i * 12 + 2, 0x80 + i as u8);
    }
    assert_eq!(air::mission_category_800d17d0(&mut m, 40), 0x80 + 40);
    // Record 41 is never compared: its category is only the fallback.
    assert_eq!(air::mission_category_800d17d0(&mut m, 41), 0x80 + 41);
    assert_eq!(
        air::mission_category_800d17d0(&mut m, 0xffff_ffff),
        0x80 + 41
    );
    m.sb(air::MISSIONS + 5 * 12, 3);
    assert_eq!(
        air::mission_category_800d17d0(&mut m, 3),
        0x80 + 3,
        "first match"
    );
}

#[test]
fn weapon_codes_follow_the_jump_tables() {
    let primary = [1, 2, 3, 4, 5, 6, 0];
    let secondary = [
        (1, 1),
        (2, 2),
        (1, 3),
        (4, 4),
        (10, 10),
        (7, 7),
        (1, 5),
        (6, 6),
    ];
    for first in -1i16..9 {
        for second in -1i16..10 {
            let mut m = machine();
            m.sb(CONTEXT + 0x3ad, 0xee);
            m.sb(CONTEXT + 0x3ae, 0xee);
            m.sb(CONTEXT + 0x3af, 0xee);
            m.sh(SELECTION + 0xec, first as u16);
            m.sh(SELECTION + 0xee, second as u16);
            assert_eq!(air::commit_weapons_800da440(&mut m), 4);
            let code = usize::try_from(first)
                .ok()
                .and_then(|i| primary.get(i).copied())
                .unwrap_or(0xee);
            assert_eq!(m.lbu(CONTEXT + 0x3ad), code, "{first}");
            let (low, high) = usize::try_from(second)
                .ok()
                .and_then(|i| secondary.get(i).copied())
                .unwrap_or((0xee, 0xee));
            assert_eq!(m.lbu(CONTEXT + 0x3ae), low, "{second}");
            assert_eq!(m.lbu(CONTEXT + 0x3af), high, "{second}");
        }
    }
}

#[test]
fn weapon_phase_without_loadout_faults() {
    let mut m = machine();
    // Category 0 and aircraft 0x63 without a loadout record: the original
    // would read through an uninitialized register.
    m.sh(CONTEXT + 0x392, 0x63);
    let mut calls = Recorder::default();
    assert_eq!(air::weapon_phase_init_800d98c4(&mut m, &mut calls), 0);
    assert_eq!(
        m.take_fault(),
        Err(ac3_flight::Fault::Untranslated {
            address: 0x800d_9a2c
        })
    );
}

#[test]
fn availability_special_categories_and_fallback() {
    for (category, only) in [(0xc, 0x11), (0x13, 0x11), (0x10, 0x12)] {
        let mut m = machine();
        m.sb(air::MISSIONS, 7);
        m.sb(air::MISSIONS + 2, category);
        m.sh(CONTEXT + 0x390, 7);
        for i in 0..24 {
            m.sb(CONTEXT + 0x9b8 + i, 1);
            m.sb(SELECTION + 0xbc + i, 9);
        }
        air::rebuild_availability_800d77e4(&mut m);
        for i in 0..24 {
            assert_eq!(m.lbu(SELECTION + 0xbc + i), u8::from(i == only), "{i}");
        }
        assert_eq!(u32::from(m.lhu(CONTEXT + 0x392)), only);
    }
    // Ordinary category: 17 and 18 excluded, only bytes equal to 1 copied
    // (saved table under option 0x10); an unavailable aircraft becomes the
    // first available one.
    let mut m = machine();
    m.sb(air::MISSIONS + 2, 1);
    m.sw(CONTEXT + 0x970, 0x10);
    for (id, value) in [(2, 2), (5, 1), (9, 1), (17, 1), (18, 1)] {
        m.sb(CONTEXT + 0x978 + id, value);
    }
    m.sb(CONTEXT + 0x9b8 + 3, 1);
    m.sh(CONTEXT + 0x392, 2);
    air::rebuild_availability_800d77e4(&mut m);
    let available: Vec<u32> = (0..24)
        .filter(|&i| m.lbu(SELECTION + 0xbc + i) != 0)
        .collect();
    assert_eq!(available, [5, 9]);
    assert_eq!(m.lhu(CONTEXT + 0x392), 5);
    // Nothing available: the aircraft stays.
    let mut m = machine();
    m.sb(air::MISSIONS + 2, 1);
    m.sh(CONTEXT + 0x392, 4);
    air::rebuild_availability_800d77e4(&mut m);
    assert_eq!(m.lhu(CONTEXT + 0x392), 4);
}

#[test]
fn launch_skip_compares_as_75_without_storing_it() {
    let timer = 0x800f_96d8;
    let mut m = machine();
    m.sw(air::LAUNCH_TIMER, timer);
    m.sw(timer, 10);
    m.sw(CONTEXT + 0x12c, 0x10);
    let mut calls = Recorder::default();
    assert_eq!(air::tick_launch_800da80c(&mut m, &mut calls), 0);
    assert_eq!(m.lw(timer), 11);
    assert_eq!(
        calls.calls,
        [
            (air::OFFSET, [1, 0, 0x271, 0]),
            (air::OFFSET, [2, 0, (-0x271i32) as u32, 0]),
            (air::STYLE, [1, 0, 0, 0]),
            (air::SCENE_COLOUR, [0, 0, 0, 0]),
        ]
    );
    // 50..74 return 0x80 without counting; 75 and above count on.
    for (t, result, after) in [
        (50, 0x80, 50),
        (74, 0x80, 74),
        (75, 0, 76),
        (0xffff_ffff, 0, 0),
    ] {
        let mut m = machine();
        m.sw(air::LAUNCH_TIMER, timer);
        m.sw(timer, t);
        let mut calls = Recorder::default();
        assert_eq!(air::tick_launch_800da80c(&mut m, &mut calls), result, "{t}");
        assert_eq!(m.lw(timer), after, "{t}");
    }
}

#[test]
fn launch_fade_colour_uses_signed_reciprocal_division() {
    // Timer -3 (< 25): left 28 -> 28*143/25 = 160, 28*135/25 = 151,
    // 28*83/25 = 92; panels at +-9.
    let timer = 0x800f_96d8;
    let mut m = machine();
    m.sw(air::LAUNCH_TIMER, timer);
    m.sw(timer, -3i32 as u32);
    let mut calls = Recorder::default();
    air::tick_launch_800da80c(&mut m, &mut calls);
    assert_eq!(calls.calls[0], (air::OFFSET, [1, 0, 9, 0]));
    assert_eq!(calls.calls[1], (air::OFFSET, [2, 0, -9i32 as u32, 0]));
    assert_eq!(calls.calls[2], (air::SCENE_COLOUR, [160, 151, 92, 0]));
}

#[test]
fn resource_load_without_compression_or_wait() {
    let mut m = machine();
    let frame = m.push_frame(0x30);
    m.sw(frame + 0x10, 0);
    let table = 0x8012_0000;
    m.sw(table + 8, 0x8013_0000);
    m.sw(0x8013_0000, 0x10);
    m.sw(air::WORK, 0x8014_0000);
    m.sw(0x8014_0008, 0x8015_0000);
    m.sw(CONTEXT + 0x18, 0x8010_0000);
    m.sw(CONTEXT + 0x1c, 0x8016_1000);
    let mut calls = Recorder::default();
    air::load_resource_800ce978(&mut m, &mut calls, table, 2, 0x3820, 0);
    assert_eq!(calls.calls, [(air::UPLOAD_TIM, [0x8013_0000, 0, 0, 0])]);
    assert_eq!(
        m.lw(CONTEXT + 0x1c),
        0x8015_0000,
        "arena back to the stale work buffer"
    );
    assert_eq!(m.sp, frame);
    // "Ulz" with a clear rectangle and the wait.
    m.sw(frame + 0x10, 1);
    m.sw(0x8013_0000, 0x1a7a_6c55);
    m.sw(CONTEXT + 0x1c, 0x8016_1000);
    let mut calls = Recorder::default();
    air::load_resource_800ce978(&mut m, &mut calls, table, 2, 0x828, 0x800f_964c);
    assert_eq!(
        calls.calls,
        [
            (air::CLEAR_IMAGE, [0x800f_964c, 0, 0, 0]),
            (air::DECOMPRESS, [0x8013_0000, 0x8016_1000, 0, 0]),
            (air::UPLOAD_TIM, [0x8016_1000, 0, 0, 0]),
            (air::DRAW_SYNC, [0, 0, 0, 0]),
        ]
    );
    assert_eq!(m.lw(0x8014_0008), 0x8016_1000);
    assert_eq!(m.lw(CONTEXT + 0x1c), 0x8016_1000, "allocation released");
}

#[test]
fn background_fade_phases() {
    type Fade = Option<(Option<u32>, u32, u32)>;
    let cases: [(u32, Fade); 6] = [
        (1, Some((Some(0x62), 100, 60))),
        (2, Some((None, 100, 10))),
        (3, Some((Some(0x63), 0, 60))),
        (0, None),
        (4, None),
        (0xffff_ffff, None),
    ];
    for (phase, expected) in cases {
        let mut m = machine();
        m.sw(air::BACKGROUND, 0x8012_0000);
        m.sw(0x8012_0020, 9);
        for (i, channel) in (0x110..=0x120u32).step_by(4).enumerate() {
            m.sw(0x8012_0000 + channel, 0x10 + i as u32);
        }
        let mut calls = Recorder::default();
        air::background_fade_800e7b50(&mut m, &mut calls, phase);
        assert_eq!(m.lw(0x8012_0014), phase);
        assert_eq!(m.lw(0x8012_0020), 0);
        let mut want = Vec::new();
        if let Some((sound, volume, rate)) = expected {
            if let Some(id) = sound {
                want.push((air::SOUND, [id, 0, 0, 0]));
            }
            for i in 0..5 {
                want.push((air::CHANNEL_VOLUME, [0x10 + i, volume, rate, 0]));
            }
        }
        assert_eq!(calls.calls, want, "{phase}");
    }
}

#[test]
fn aircraft_interpolation_wraps_and_truncates() {
    let mut m = machine();
    let b = SELECTION;
    m.sw(b + 8, 1);
    m.sw(b + 0xb0, 25);
    m.sh(b + 0x80, 9);
    // Halfword +2: previous 0x7FFF, next -0x8000 (difference -0xFFFF):
    // -0xFFFF*25/50 = -0x7FFF (toward zero); 0x7FFF - 0x7FFF = 0.
    m.sh(b + 0x22, 0x7fff);
    m.sh(b + 0x82, 0x8000);
    // Word +0xC: the product wraps before the division.
    m.sw(b + 0x2c, 0);
    m.sw(b + 0x8c, 0x7fff_ffff);
    m.sb(b + 0xa0, 0x55);
    air::interpolate_aircraft_800d7950(&mut m);
    assert_eq!(m.lhu(b + 0x50), 9);
    assert_eq!(m.lhu(b + 0x52), 0);
    assert_eq!(
        m.lw(b + 0x5c),
        ((0x7fff_ffffu32.wrapping_mul(25) as i32) / 50) as u32
    );
    assert_eq!(m.lbu(b + 0x70), 0x55);
    assert_eq!(m.lw(b + 0xb0), 26);
    // At 50 the record reaches the next one, then the timer resets and the
    // flag clears.
    m.sw(b + 0xb0, 50);
    air::interpolate_aircraft_800d7950(&mut m);
    assert_eq!(m.lw(b + 0xb0), 0);
    assert_eq!(m.lw(b + 8), 0);
    assert_eq!(m.lhu(b + 0x52), 0x8000);
}

#[test]
fn weapon_input_stops_at_a_running_slide() {
    let b = SELECTION;
    let setup = |flags: u32| {
        let mut m = machine();
        m.sh(b + 0xec, 1);
        for i in 0..8 {
            m.sb(b + 0xdc + i, 1);
        }
        m.sw(b + 0x128, flags);
        m.sw(b + 0x130, 3);
        m.sw(CONTEXT + 0x12c, 8);
        m
    };
    // Already sliding right: one sound, nothing changes.
    let mut m = setup(4);
    let mut calls = Recorder::default();
    assert_eq!(air::weapon_input_800d9ddc(&mut m, &mut calls), 0);
    assert_eq!(calls.calls[0], (air::SOUND, [0x3d, 0, 0, 0]));
    assert_eq!(calls.calls.iter().filter(|c| c.0 == air::SOUND).count(), 1);
    assert_eq!(m.lhu(b + 0xec), 1);
    assert_eq!(m.lw(b + 0x130), 3);
    // Reversing a left slide: timer 10 - t; the scan goes on to the next
    // available weapon (second sound), where the new lock stops it.
    let mut m = setup(2);
    let mut calls = Recorder::default();
    air::weapon_input_800d9ddc(&mut m, &mut calls);
    assert_eq!(calls.calls.iter().filter(|c| c.0 == air::SOUND).count(), 2);
    assert_eq!(m.lhu(b + 0xec), 2);
    assert_eq!(m.lw(b + 0x130), 7);
    assert_eq!(m.lw(b + 0x128) & 7, 5);
}
