//! Session step 80036E54, the pause path and the alternate updates on
//! original snapshots (see `snapshot::check_call`).
//!
//! Captures (`analysis/flight-runtime`): `session-004` (intro frames 307..
//! 429, pause at 471..531, frame 551), `session-006` (dive and crash, frames
//! around 911 and 1533), `session-007` (replay state 80033DA8),
//! `session-008` (intro, CROSS held at frame 360), `session-009` (result
//! message and fade frames 1013/1473 of the dive run).
//!
//! Routines owned by other modules (script 8004B760/80077D54/8002D724,
//! tail 80035248/80030248, loading 800353D8/800343D0/80082CD0, SDK rand)
//! are reached through `dispatch::call`; tests whose path crosses them need
//! those translations.
use ac3_flight::{GP, Machine, dispatch, session, snapshot};
use std::path::PathBuf;

fn index(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join("snapshots.jsonl")
}

/// Check occurrence `occurrence` of `callee` in `capture`; returns the bytes
/// the original changed.
fn check(
    capture: &str,
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &snapshot::Snapshot) -> Option<u32>,
) -> usize {
    snapshot::check_call(&index(capture), callee, occurrence, native).unwrap()
}

/// 800C4F38 halfword: cleared unconditionally by 80040FCC, which the radio
/// update reaches every frame. Inverting it first makes runs whose original
/// changed nothing prove that the translation stores it.
const INDICATOR_HALF: [u32; 2] = [0x800c_4f38, 0x800c_4f39];

fn invert(m: &mut Machine, addresses: &[u32]) {
    for &address in addresses {
        let value = m.lbu(address);
        m.sb(address, !value);
    }
}

fn void(native: fn(&mut Machine)) -> impl FnOnce(&mut Machine, &snapshot::Snapshot) -> Option<u32> {
    move |m, _| {
        native(m);
        None
    }
}

// ---- leaves (session-004) ----------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn pad_buttons_8006b360() {
    check("session-004", 0x8006b360, 0, |m, _| {
        let v0 = session::pad_buttons(m);
        assert_ne!(v0, 0);
        Some(v0)
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn radio_queries_80054dfc_80055038_80054c8c() {
    check("session-004", 0x80054dfc, 0, |m, _| {
        Some(session::radio_busy(m))
    });
    check("session-004", 0x80055038, 0, |m, _| {
        Some(dispatch::call(m, 0x8005_5038, [0; 4]))
    });
    check("session-004", 0x80054c8c, 0, |m, _| {
        Some(session::radio_tick(m))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn indicator_close_80040fcc() {
    check("session-004", 0x80040fcc, 0, |m, e| {
        invert(m, &INDICATOR_HALF);
        session::indicator_close(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn radio_update_80054e10() {
    check("session-004", 0x80054e10, 0, |m, _| {
        invert(m, &INDICATOR_HALF);
        session::radio_update(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn mission_logic_80052aa0() {
    check("session-004", 0x80052aa0, 0, |m, _| {
        invert(m, &INDICATOR_HALF);
        session::mission_logic(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-004, and the loading/SDK rand translations"]
fn intro_fade_80082f18() {
    let changed = check("session-004", 0x80082f18, 0, |m, e| {
        Some(session::intro_fade(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    assert!(changed > 1000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn camera_requests_80074c60_80074c04() {
    assert!(check("session-004", 0x80074c60, 0, void(session::camera_74c60)) > 0);
    assert!(check("session-004", 0x80074c04, 0, void(session::camera_74c04)) > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-004, and the script/tail translations"]
fn camera_follow_80077e68_80077ecc() {
    let changed = check("session-004", 0x80077e68, 0, |m, e| {
        session::camera_follow_player(m, e.gpr(4));
        None
    });
    assert!(changed > 0);
    assert!(
        check(
            "session-004",
            0x80077ecc,
            0,
            void(session::camera_follow_default)
        ) > 0
    );
}

// ---- pause path (session-004) --------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn sound_pause_8002c2cc() {
    for occurrence in 0..2 {
        let changed = check("session-004", 0x8002c2cc, occurrence, |m, e| {
            Some(session::sound_pause(m, e.gpr(4)))
        });
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn pause_input_80034428() {
    for occurrence in 0..2 {
        assert!(
            check(
                "session-004",
                0x80034428,
                occurrence,
                void(session::pause_input)
            ) > 0
        );
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn draw_box_800340d8() {
    for occurrence in 0..2 {
        // a2 is the caller's stack RECT, present in the entry image.
        let changed = check("session-004", 0x800340d8, occurrence, |m, e| {
            Some(session::draw_box(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)))
        });
        assert!(changed > 150);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-004, and the loading translations"]
fn draw_pause_menu_80034640() {
    for occurrence in 0..2 {
        let changed = check("session-004", 0x80034640, occurrence, |m, e| {
            Some(session::draw_pause_menu(m, e.gpr(4), e.gpr(5)))
        });
        assert!(changed > 1300);
    }
}

// ---- session step 80036E54 -----------------------------------------------------

fn session_step(m: &mut Machine, _: &snapshot::Snapshot) -> Option<u32> {
    let sp = m.sp;
    let v0 = session::session_step(m);
    assert_eq!(m.sp, sp, "guest frames balanced");
    Some(v0)
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-004, and the script/tail/loading/SDK translations"]
fn session_step_80036e54_intro_first_frame() {
    assert!(check("session-004", 0x80036e54, 0, session_step) > 1200);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-004, and the loading/SDK rand translations"]
fn session_step_80036e54_intro_fade() {
    assert!(check("session-004", 0x80036e54, 1, session_step) > 1200);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-004"]
fn session_step_80036e54_intro_wait() {
    check("session-004", 0x80036e54, 2, |m, e| {
        invert(m, &INDICATOR_HALF);
        session_step(m, e)
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-004, and the script/tail translations"]
fn session_step_80036e54_intro_end() {
    assert!(check("session-004", 0x80036e54, 3, session_step) > 50);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-004, and the script translation"]
fn session_step_80036e54_after_pause() {
    assert!(check("session-004", 0x80036e54, 4, session_step) > 0);
}

/// Radio words `[gp+0x49C]`, `[gp+0x4AC]`, `[gp+0x4B8]`, `[gp+0x4C4]` and
/// the indicator halfword, all stored unconditionally by 80055044.
fn radio_words() -> Vec<u32> {
    let mut words = Vec::new();
    for offset in [0x49c, 0x4ac, 0x4b8, 0x4c4] {
        words.extend((0..4).map(|i| GP + offset + i));
    }
    words.extend(INDICATOR_HALF);
    words
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-006"]
fn priority_message_80041d4c() {
    let changed = check("session-006", 0x80041d4c, 0, |m, e| {
        dispatch::call(m, 0x8004_1d4c, [e.gpr(4), 0, 0, 0]);
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-006"]
fn radio_reset_80055044() {
    for occurrence in 0..2 {
        check("session-006", 0x80055044, occurrence, |m, _| {
            invert(m, &radio_words());
            Some(session::radio_reset(m))
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-006"]
fn session_step_80036e54_mission_flight() {
    assert!(check("session-006", 0x80036e54, 0, session_step) > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-006, and the script/tail translations"]
fn session_step_80036e54_mission_end() {
    assert!(check("session-006", 0x80036e54, 5, session_step) > 10);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-006"]
fn session_step_80036e54_result_wait() {
    check("session-006", 0x80036e54, 6, |m, e| {
        invert(m, &INDICATOR_HALF);
        session_step(m, e)
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-006, and the script/tail translations"]
fn session_step_80036e54_result_end() {
    assert!(check("session-006", 0x80036e54, 15, session_step) > 30);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-009, and the script translation"]
fn session_step_80036e54_result_message() {
    assert!(check("session-009", 0x80036e54, 4, session_step) > 50);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-009"]
fn session_step_80036e54_result_fade() {
    assert!(check("session-009", 0x80036e54, 7, session_step) > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/session-008"]
fn session_step_80036e54_intro_hold() {
    check("session-008", 0x80036e54, 2, |m, e| {
        invert(m, &INDICATOR_HALF);
        session_step(m, e)
    });
}

// ---- alternate updates (session-007, replay state 80033DA8) -------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR, session-007, and the replay (8007DDCC), 8006B1E8, 8007C44C and 80083A64 translations"]
fn update_32e34_80032e34() {
    assert!(check("session-007", 0x80032e34, 0, void(session::update_32e34)) > 4000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, session-007, and the replay (8007DDCC) translation"]
fn update_8007e284() {
    assert!(check("session-007", 0x8007e284, 0, void(session::update_8007e284)) > 4000);
}

// ---- unit checks ---------------------------------------------------------------

#[test]
fn indicator_bytes() {
    let mut m = Machine::default();
    let byte = GP + 0x450;
    session::indicator_on(&mut m);
    assert_eq!(m.lbu(byte), 1);
    m.sb(byte, 2);
    session::indicator_on(&mut m);
    assert_eq!(session::indicator(&mut m), 2);
    session::indicator_done(&mut m);
    assert_eq!(m.lbu(byte), 3);
    session::indicator_done(&mut m);
    assert_eq!(m.lbu(byte), 3);
}

#[test]
fn radio_sequence_steps_and_ends() {
    let mut m = Machine::default();
    let table = 0x8010_0000;
    let frames = 0x8010_0100;
    m.sw(GP + 0x4a0, table);
    m.sw(GP + 0x4b0, frames);
    // Sequence 1 starts at frame entry 3 and has 2 steps of 1 and 0 waits.
    m.sh(table + 4, 3);
    m.sh(table + 6, 2);
    m.sh(frames + 3 * 4 + 2, 1);
    m.sh(frames + 4 * 4 + 2, 0);
    session::radio_select(&mut m, 1);
    assert_eq!(session::radio_busy(&mut m), 1);
    session::radio_tick(&mut m);
    assert_eq!((m.lw(GP + 0x4b8), m.lw(GP + 0x4c4)), (3, 1));
    session::radio_tick(&mut m);
    assert_eq!((m.lw(GP + 0x4ac), m.lw(GP + 0x4c4)), (1, 0));
    session::radio_tick(&mut m);
    assert_eq!(session::radio_busy(&mut m), 0, "sequence finished");
    assert_eq!(m.lw(GP + 0x4b8), 0);
    assert!(m.take_fault().is_ok());
}

/// 80041D4C (`hud.rs`, reached by original address).
fn priority_message(m: &mut Machine, id: u32) {
    dispatch::call(m, 0x8004_1d4c, [id, 0, 0, 0]);
}

#[test]
fn priority_message_keeps_lower_ids() {
    let mut m = Machine::default();
    m.sw(0x800c_4f48, u32::MAX);
    m.sw(0x800a_fed0 + 5 * 4, 0x0807_0123);
    priority_message(&mut m, 5);
    assert_eq!(m.lw(0x800c_4f48), 5);
    assert_eq!(m.lw(0x800c_4f4c), 0x123);
    assert_eq!((m.lbu(0x800c_4f50), m.lbu(0x800c_4f51)), (1, 7));
    assert_eq!((m.lbu(0x800c_4f54), m.lbu(0x800c_4f55)), (1, 8));
    priority_message(&mut m, 6);
    assert_eq!(m.lw(0x800c_4f48), 5, "a higher id waits");
    priority_message(&mut m, 2);
    assert_eq!(m.lw(0x800c_4f48), 2);
}

#[test]
fn camera_request_modes() {
    let mut m = Machine::default();
    let request = 0x8010_0000;
    m.sw(0x800b_af1c, request);
    m.sw(request, 0x11);
    session::camera_74c80(&mut m, 1);
    assert_eq!([m.lw(request), m.lw(request + 4)], [0x51, 2]);
    session::camera_74c80(&mut m, 0);
    assert_eq!(m.lw(request + 4), 0x40_0000);
    session::camera_74c20(&mut m);
    assert_eq!([m.lw(request), m.lw(request + 4)], [1, 0x0100_0000]);
}
