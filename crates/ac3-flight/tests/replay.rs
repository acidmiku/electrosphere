//! Flight replay (state 80033DA8) and the other unowned routines of
//! `replay.rs` on original snapshots (see `snapshot::check_call`).
//!
//! Captures (`analysis/flight-runtime`): `replay-002` (dive and crash, then
//! the replay from frame 1535; START at 1701 ends it at frame 1705) and
//! `replay-005` (the same with weapons fire, so the replay frames 1701/1703
//! carry type-2 particle records); `sequence-001` is an oracle run (one RAM
//! write: story progress +0x29 = 1 / +0x24 = 8 at the event choice after the
//! continue menu) that plays the in-engine sequence of mode 1 from frame
//! 1905 (phase 0x10 at 1907, records at 1937..1941, end at 2029);
//! `course-001` / `course-002` are oracle runs (progress +0x29 = 3, +0x25 =
//! 0, event index 800BAEB4 = 4 at the same choice) of course 0 (flags 0x207):
//! phase 0x400 from 1905, 0x800 at 2029, START at 2200, course end (phase
//! 0x2000, result 0) from 2267, next event at 2569 and a restart at 2815.
use ac3_flight::SmallData;
use ac3_flight::{GP, Machine, replay, session, snapshot};
use std::path::PathBuf;

fn index(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join("snapshots.jsonl")
}

fn check(
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &snapshot::Snapshot) -> Option<u32>,
) -> usize {
    snapshot::check_call(&index("replay-002"), callee, occurrence, native).unwrap()
}

fn check_fire(
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &snapshot::Snapshot) -> Option<u32>,
) -> usize {
    snapshot::check_call(&index("replay-005"), callee, occurrence, native).unwrap()
}

fn check_sequence(
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &snapshot::Snapshot) -> Option<u32>,
) -> usize {
    snapshot::check_call(&index("sequence-001"), callee, occurrence, native).unwrap()
}

fn check_course(
    capture: &str,
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &snapshot::Snapshot) -> Option<u32>,
) -> usize {
    snapshot::check_call(&index(capture), callee, occurrence, native).unwrap()
}

fn void(native: fn(&mut Machine)) -> impl FnOnce(&mut Machine, &snapshot::Snapshot) -> Option<u32> {
    move |m, _| {
        native(m);
        None
    }
}

fn invert(m: &mut Machine, addresses: impl IntoIterator<Item = u32>) {
    for address in addresses {
        let value = m.lbu(address);
        m.sb(address, !value);
    }
}

/// The camera's +0x15C halfword (target mode), always stored by 800780C0.
fn camera_mode(m: &mut Machine) -> [u32; 2] {
    let gameplay = m.lw(GP + 0x2d0);
    let camera = m.lw(gameplay + 0x28);
    [camera + 0x15c, camera + 0x15d]
}

// ---- leaves ---------------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/replay-002"]
fn camera_set_eye_80078048() {
    for occurrence in 0..2 {
        let changed = check(0x80078048, occurrence, |m, e| {
            replay::camera_set_eye(m, e.gpr(4), e.gpr(5));
            None
        });
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/replay-002"]
fn camera_set_target_800780c0() {
    for occurrence in 0..2 {
        check(0x800780c0, occurrence, |m, e| {
            let mode = camera_mode(m);
            invert(m, mode);
            replay::camera_set_target(m, e.gpr(4), e.gpr(5), e.gpr(6));
            None
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/replay-002"]
fn record_skip_8007d538() {
    for occurrence in 0..2 {
        check(0x8007d538, occurrence, |m, e| {
            let next = replay::record_skip(m, e.gpr(4), e.gpr(5), e.gpr(6));
            assert!(next > e.gpr(4));
            Some(next)
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, replay-002, and the entities 8007C5C0/8007C6DC translations"]
fn record_play_8007db58() {
    for occurrence in 0..3 {
        let changed = check(0x8007db58, occurrence, |m, e| {
            Some(replay::record_play(m, e.gpr(4)))
        });
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/replay-002"]
fn particles_update_8006b1e8() {
    for occurrence in 0..2 {
        check(0x8006b1e8, occurrence, |m, e| {
            // No particle is active here: the visible effect is IR0 = 0xFA0.
            assert_ne!(e.info.registers.cp2d[8], 0xfa0);
            replay::particles_update(m);
            None
        });
    }
    check(0x8006b340, 0, void(session::pad_8006b340));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/replay-005"]
fn emitter_set_8006aee8() {
    for occurrence in 0..2 {
        let changed = check_fire(0x8006aee8, occurrence, |m, e| {
            replay::emitter_set(m, e.gpr(4), e.gpr(5), e.gpr(6));
            None
        });
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/replay-005"]
fn record_particles_8007d31c() {
    for occurrence in 0..2 {
        let changed = check_fire(0x8007d31c, occurrence, |m, e| {
            let sp = m.sp;
            replay::record_particles(m, e.gpr(4));
            assert_eq!(m.sp, sp);
            None
        });
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, replay-005, and the entities 8007C5C0/8007C6DC translations"]
fn record_play_with_particles_8007db58() {
    // Occurrences 0 and 3 are the type-2 records of frames 1701 and 1703.
    for occurrence in 0..6 {
        check_fire(0x8007db58, occurrence, |m, e| {
            Some(replay::record_play(m, e.gpr(4)))
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, replay-005, and the entities translations"]
fn replay_frame_with_particles_8007df54() {
    for occurrence in 0..2 {
        let changed = check_fire(0x8007df54, occurrence, |m, _| Some(replay::replay_frame(m)));
        assert!(changed > 20);
    }
}

// ---- replay control -------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR, replay-002, and the script/tail translations"]
fn camera_replay_view_80078010() {
    assert!(check(0x80078010, 0, void(replay::camera_replay_view)) > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, replay-002, and the shared/init_objects translations"]
fn replay_rewind_8007dc1c() {
    assert!(check(0x8007dc1c, 0, void(replay::replay_rewind)) > 2000);
    assert!(check(0x8007dc1c, 1, void(replay::replay_rewind)) > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/replay-002"]
fn replay_skip_frames_8007d5d8() {
    for occurrence in 0..2 {
        check(0x8007d5d8, occurrence, |m, e| {
            let sp = m.sp;
            replay::replay_skip_frames(m, e.gpr(4));
            assert_eq!(m.sp, sp);
            None
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, replay-002, and the entities translations"]
fn replay_frame_8007df54() {
    for occurrence in 0..2 {
        let changed = check(0x8007df54, occurrence, |m, _| Some(replay::replay_frame(m)));
        assert!(changed > 20);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, replay-002, and the script/tail/shared/init_objects translations"]
fn replay_start_8007ddcc() {
    assert!(check(0x8007ddcc, 0, void(replay::replay_start)) > 4000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, replay-002, and the script/shared/init_objects translations"]
fn replay_finish_8007e1a8() {
    assert!(check(0x8007e1a8, 0, void(replay::replay_finish)) > 50);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, replay-002, and the script/tail/shared/entities translations"]
fn replay_control_8007e284() {
    for occurrence in 0..5 {
        let changed = check(0x8007e284, occurrence, void(session::update_8007e284));
        assert!(changed > 0);
    }
}

// ---- in-engine sequence (sequence-001) ------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/sequence-001"]
fn sequence_leaves() {
    assert!(
        check_sequence(0x8002c500, 0, |m, e| {
            let callback = m.lw(m.sp + 0x10);
            assert_eq!(callback, 0x8003_9128);
            Some(replay::stream_queue(
                m,
                e.gpr(4),
                e.gpr(5),
                e.gpr(6),
                e.gpr(7),
                callback,
            ))
        }) > 0
    );
    assert!(
        check_sequence(0x8002c81c, 0, |m, e| {
            Some(replay::music_select(m, e.gpr(4), e.gpr(5), e.gpr(6)))
        }) > 0
    );
    for occurrence in 0..2 {
        check_sequence(0x8002b87c, occurrence, |m, _| {
            Some(replay::stream_pending(m))
        });
    }
    check_sequence(0x80038b50, 0, |m, _| {
        let block = m.gw(0x514);
        invert(m, block + 0x1548..block + 0x154c);
        Some(replay::emitters_clear(m))
    });
    assert!(check_sequence(0x80038b60, 0, void(replay::entities_clear_hidden)) > 0);
    for occurrence in 0..2 {
        check_sequence(0x80091248, occurrence, |m, _| {
            let base = m.gw(0x40c);
            for offset in [0x33c0, 0x3964, 0x3b28] {
                invert(m, base + offset..base + offset + 4);
            }
            Some(replay::effect_pools_release(m))
        });
    }
    for occurrence in 0..2 {
        // Letterbox level <= 0 here: the packet pointer comes back unchanged.
        check_sequence(0x80092164, occurrence, |m, e| {
            let next = replay::letterbox(m, e.gpr(4), e.gpr(5), e.gpr(6));
            assert_eq!(next, e.gpr(5));
            Some(next)
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, sequence-001, and the init/init_tail translations"]
fn camera_follow_player_8007827c() {
    // v0 is 80035248's (init_tail), which returns the gameplay word there.
    let changed = check_sequence(0x8007827c, 0, |m, _| {
        replay::camera_follow_player(m);
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, sequence-001, and the init_tail translations"]
fn sound_resets_800302b0_8003050c() {
    assert!(check_sequence(0x800302b0, 0, |m, _| Some(replay::sound_listener_reset(m))) > 0);
    for occurrence in 0..2 {
        assert!(
            check_sequence(0x8003050c, occurrence, |m, _| Some(replay::sound_stop_all(
                m
            ))) > 0
        );
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/sequence-001"]
fn stream_next_8002b5c4() {
    for occurrence in 0..2 {
        let changed = check_sequence(0x8002b5c4, occurrence, |m, e| {
            Some(replay::stream_next(m, e.gpr(4), e.gpr(5)))
        });
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/sequence-001"]
fn stream_release_8002b7b8() {
    assert!(
        check_sequence(0x8002b7b8, 0, |m, e| Some(replay::stream_release(
            m,
            e.gpr(4)
        ))) > 0
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR, sequence-001, and the init translations"]
fn camera_lens_800760f8_80076140() {
    for occurrence in 0..2 {
        let changed = check_sequence(0x800760f8, occurrence, |m, e| {
            Some(replay::camera_set_projection(m, e.gpr(4), e.gpr(5)))
        });
        assert!(changed > 0 || occurrence == 1);
    }
    for occurrence in 0..2 {
        check_sequence(0x80076140, occurrence, |m, e| {
            let depth = m.lw(m.sp + 0x10);
            let v0 = replay::camera_set_range(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7), depth);
            assert_eq!(v0, 1);
            Some(v0)
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, sequence-001, and the init/script translations"]
fn sequence_commands_800390c0() {
    for occurrence in 0..3 {
        let changed = check_sequence(0x800390c0, occurrence, |m, e| {
            Some(replay::sequence_commands(m, e.gpr(4)))
        });
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, sequence-001, and the entities 8002F354 subtree (8002F2F0)"]
fn sequence_sound_800302e4() {
    for occurrence in 0..3 {
        let changed = check_sequence(0x800302e4, occurrence, |m, e| {
            Some(replay::sequence_sound(m, e.gpr(4)))
        });
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, sequence-001, and the init/init_tail/init_objects translations"]
fn sequence_start_80039138() {
    let changed = check_sequence(0x80039138, 0, |m, _| Some(replay::sequence_start(m)));
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, sequence-001, and the init/init_tail/init_objects/script translations"]
fn sequence_end_8003926c_80038a24() {
    // v0 is 80037B24's (script), the requested transition word there.
    let changed = check_sequence(0x80038a24, 0, |m, e| {
        replay::sequence_restore(m, e.gpr(4));
        None
    });
    assert!(changed > 0);
    let changed = check_sequence(0x8003926c, 0, |m, _| {
        replay::sequence_end(m);
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, sequence-001, and a translation of 80092350 (entities tree)"]
fn sequence_fade_80039598_fading() {
    for occurrence in 0..2 {
        let changed = check_sequence(0x800923e8, occurrence, |m, e| {
            Some(replay::screen_fade(
                m,
                e.gpr(4),
                e.gpr(5),
                e.gpr(6),
                e.gpr(7),
            ))
        });
        assert!(changed > 0);
        assert!(check_sequence(0x80039598, occurrence, void(replay::sequence_fade)) > 0);
        assert!(check_sequence(0x8003931c, occurrence, void(session::update_8003931c)) > 0);
    }
    for occurrence in 2..4 {
        assert!(check_sequence(0x8003931c, occurrence, void(session::update_8003931c)) > 0);
    }
    assert!(check_sequence(0x80039598, 2, void(replay::sequence_fade)) > 0);
}

// ---- courses (course-001 / course-002) --------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR, course-001, and the player camera modes (800754A8 ...)"]
fn camera_view_apply_80075894() {
    let changed = check_course("course-001", 0x80075894, 0, void(replay::camera_view_apply));
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, course-001, and the player camera modes (800754A8 ...)"]
fn camera_mode_restart_800723ac() {
    let changed = check_course(
        "course-001",
        0x800723ac,
        0,
        void(replay::camera_mode_restart),
    );
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/course-001"]
fn course_waiting_frames() {
    // Course intro before frame 0x3D and course end before 0x96: no stores
    // (the radio and pad reads only); the gameplay frame counter decides.
    for occurrence in 0..3 {
        check_course("course-001", 0x8005a268, occurrence, |m, _| {
            let gameplay = m.gw(0x2d0);
            assert!((m.lw(gameplay + 0xc) as i32) < 0x3d);
            replay::course_start(m);
            None
        });
    }
    for occurrence in 0..4 {
        check_course("course-001", 0x8005a410, occurrence, |m, _| {
            let gameplay = m.gw(0x2d0);
            assert_eq!(m.lw(gameplay + 8), 0);
            assert!((m.lw(gameplay + 0xc) as i32) < 0x96);
            replay::course_end(m);
            None
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, course-002, and the init_tail/script translations"]
fn course_end_8005a410() {
    // Frame 2569: result 0 after 0x96 frames (targets, music, next event);
    // frame 2815: result 1 (restart, transition 0x1000).
    for occurrence in 1..3 {
        let changed = check_course(
            "course-002",
            0x8005a410,
            occurrence,
            void(replay::course_end),
        );
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, course-002, and the init_tail/script translations"]
fn course_update_8005a874() {
    // 2267: the course exit request; 2569 / 2815: through 8005A410.
    for occurrence in [2, 4, 6] {
        let changed = check_course(
            "course-002",
            0x8005a874,
            occurrence,
            void(session::update_8005a874),
        );
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR, course-002, and the player camera modes (800754A8 ...) and hud 80041DEC"]
fn course_start_8005a268_and_shortcuts() {
    let changed = check_course("course-002", 0x8005a268, 0, void(replay::course_start));
    assert!(changed > 0);
    for occurrence in [0, 1] {
        let changed = check_course(
            "course-002",
            0x8005a874,
            occurrence,
            void(session::update_8005a874),
        );
        assert!(changed > 0);
    }
}

#[test]
fn sound_channel_volume_scales_to_128() {
    let mut m = Machine::default();
    assert_eq!(replay::sound_channel_volume(&mut m, 2, 0x1e, 0x96), 0x26);
    let channel = 0x800c_35f8 + 2 * 20;
    assert_eq!(m.lbu(channel + 0x10), 0x26);
    assert_eq!(m.lhu(channel + 0x12), 0x96);
    assert_eq!(replay::sound_channel_volume(&mut m, 0x18, 1, 1), 0xff);
}

// ---- unit checks ----------------------------------------------------------------

#[test]
fn pool_alloc_marks_first_free_object() {
    let mut m = Machine::default();
    let pool = 0x8010_0000;
    m.sw(pool, u32::MAX);
    m.sw(pool + 4, 0b0111);
    // Two mask words, objects of 5 words: word 1 bit 3 is object 32 + 3.
    let object = replay::pool_alloc(&mut m, pool, 2, 5);
    assert_eq!(object, pool + 8 + (32 + 3) * 20);
    assert_eq!(m.lw(pool + 4), 0b1111);
    m.sw(pool + 4, u32::MAX);
    assert_eq!(replay::pool_alloc(&mut m, pool, 2, 5), 0, "full");
    assert_eq!(replay::pool_alloc(&mut m, pool, 0, 5), 0, "no masks");
}

#[test]
fn particles_move_and_expire() {
    let mut m = Machine::default();
    let block = 0x8010_0000;
    m.sw(GP + 0x514, block);
    m.sw(block + 0x33d0, 0b11);
    let first = block + 0x15d0;
    let second = first + 0x3c;
    // Particle 0: flags 3 (no gravity, no drag), life 2, velocity 0x40 each.
    m.sw(first, 3);
    m.sw(first + 4, 2);
    for offset in [0x28, 0x2c, 0x30] {
        m.sw(first + offset, 0x40);
    }
    // Particle 1 expires.
    m.sw(second + 4, 0);
    replay::particles_update(&mut m);
    assert_eq!(m.lw(first + 4), 1);
    assert_eq!(
        [m.lw(first + 8), m.lw(first + 0xc), m.lw(first + 0x10)],
        [0x40; 3]
    );
    assert_eq!(
        [m.lw(first + 0x18), m.lw(first + 0x1c), m.lw(first + 0x20)],
        [0x40; 3]
    );
    assert_eq!(m.lw(second + 4), u32::MAX);
    assert_eq!(m.lw(block + 0x33d0), 0b01, "expired particle cleared");
    assert_eq!(m.gte.data[8], 0xfa0);
}

#[test]
fn emitter_masks() {
    let mut m = Machine::default();
    let block = 0x8010_0000;
    m.sw(GP + 0x514, block);
    let source = 0x8011_0000;
    for i in 0..6 {
        m.sh(source + 2 * i, 0x100 + i as u16);
    }
    m.sw(source, 0x11);
    m.sw(source + 4, 0x22);
    m.sw(source + 8, 0x33);
    replay::emitter_set(&mut m, 33, 0x1234, source);
    assert_eq!(m.lw(block + 0x1548), 1 << 1);
    let emitter = block + 0xc8 + 33 * 0xa4;
    assert_eq!([m.lbu(emitter + 0x34), m.lbu(emitter + 0x35)], [0x34, 0x12]);
    assert_eq!(m.lw(emitter + 0x20), 0x22);
    replay::emitter_stop(&mut m, 1);
    assert_eq!(m.lw(block + 0x1548), 0);
}

#[test]
fn session_release_entities_clears_flagged() {
    let mut m = Machine::default();
    let table = 0x8010_0000;
    m.sw(GP + 0x2d8, table);
    for i in 0..0x40 {
        m.sw(table + 4 * i, 0x8011_0000 + 0x100 * i);
        m.sw(0x8011_0000 + 0x100 * i, 0xffff_ffff);
    }
    // Entity 5: bit 2 (released); entity 1: below the range, untouched.
    m.sw(0x800c_4d98 + 4 * 5, 4 | 1);
    m.sb(0x800c_4e98 + 5, 0x77);
    m.sw(0x800c_4d98 + 4, 4);
    m.sb(0x800c_4e98 + 1, 0x66);
    replay::session_release_entities(&mut m);
    assert_eq!(m.lw(0x800c_4d98 + 4 * 5), 1);
    assert_eq!(m.lbu(0x800c_4e98 + 5), 0);
    assert_eq!(m.lw(0x8011_0500), 0xffff_bfff);
    assert_eq!(m.lw(0x800c_4d98 + 4), 4);
    assert_eq!(m.lbu(0x800c_4e98 + 1), 0x66);
    assert_eq!(m.lw(0x8011_0100), 0xffff_ffff);
}

#[test]
fn tint_rectangle_links_a_tile() {
    let mut m = Machine::default();
    let ot = 0x8010_0008;
    let packets = 0x8011_0000;
    m.sw(ot, 0x00ff_ffff);
    assert_eq!(
        replay::tint_rectangle(&mut m, ot, packets, 0),
        packets,
        "level 0"
    );
    let next = replay::tint_rectangle(&mut m, ot, packets, 0x800);
    assert_eq!(next, packets + 0x10);
    assert_eq!(m.lw(packets), 0x03ff_ffff);
    assert_eq!(m.lw(ot), packets & 0x00ff_ffff);
    assert_eq!(m.lw(packets + 8), 0xff80_ff60);
    assert_eq!(m.lw(packets + 0xc), 0x0100_0140);
    assert_eq!(m.lw(packets + 4) >> 24, 0x62, "semi-transparent tile code");
}

#[test]
fn sequence_sparks_allocates_from_both_pools() {
    let mut m = Machine::default();
    let effects = 0x8010_0000;
    m.sw(GP + 0x40c, effects);
    replay::sequence_sparks(&mut m, 0x1000, 0x2000, 0x3000, 1);
    // The spark pool (3 masks at +0x600) always gets one on odd phases.
    assert_eq!(m.lw(effects + 0x600), 1);
    let spark = effects + 0x600 + 3 * 4;
    assert_eq!(m.lhu(spark), 0xffff);
    assert_eq!(m.lw(spark + 8), 0x2000);
    let life = m.lhu(spark + 2);
    assert!((9..0x1c).contains(&life));
    // The smoke pool (10 masks at +0x1098) gets the smoke puff (and maybe an
    // ember first).
    let used = m.lw(effects + 0x1098);
    assert!(used == 1 || used == 3);
    let smoke = effects + 0x1098 + 10 * 4 + (used.count_ones() - 1) * 7 * 4;
    assert_eq!(m.lhu(smoke), 0x1e);
    assert_eq!(m.lw(smoke + 8), 0x2000u32.wrapping_add(0xfff3_8000));
    assert_eq!(m.lhu(smoke + 0x18), 0x1e04);
}

#[test]
fn sequence_burn_waits_for_frame_11() {
    let mut m = Machine::default();
    let gameplay = 0x8010_0000;
    m.sw(GP + 0x2d0, gameplay);
    m.sw(gameplay + 0x14, 0x8011_0000);
    replay::sequence_burn(&mut m, 0xa);
    replay::sequence_burn(&mut m, 0x2e0);
    assert_eq!(m.lw(gameplay + 0x14), 0x8011_0000);
}
