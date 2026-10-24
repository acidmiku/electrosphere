//! World and entity updates (`src/entities.rs`) on original entry snapshots
//! must reproduce the original return snapshots (`snapshot::check_call`: all
//! RAM outside the stack, scratchpad, GTE registers, and v0 where returned).
//!
//! Captures (`analysis/flight-runtime/entities-001..006`, checkpoint
//! `menu-runtime/mission-006/final.sav`): every entry snapshot of the tested
//! routine in every capture runs. 001: the first three flight callbacks
//! (frames 307-311). 002-006: later frames with homing missiles fired
//! (CIRCLE) from frame 400 and enemies present from about frame 900: 002
//! all routines at frames 411-1301, 003 lock-on and movement helpers at
//! 901-1501, 004 the 80045DA8 checks at 901-1301, 005 all routines at
//! 2201-3401, 006 steering, throttle and movement states at 1101-3401.
//!
//! A captured path that reaches a routine this module does not translate
//! raises `Fault::Untranslated` before the comparison. Such pairs are
//! counted as blocked, separately for shared/SDK stubs (`shared.rs`,
//! `sdk.rs`) and for routines outside this module, and printed; the test
//! validates them fully once those translations land. An `Untranslated`
//! fault naming a routine of this module always fails.
use ac3_flight::{Machine, dispatch, entities, shared, snapshot};
use std::path::{Path, PathBuf};

const CAPTURES: &[&str] = &[
    "entities-001",
    "entities-002",
    "entities-003",
    "entities-004",
    "entities-005",
    "entities-006",
];

/// Every routine `entities::dispatch` claims. The captures also validate
/// routines this module calls in `shared.rs` (lock links 8004577C/80045874,
/// the event ring 8007C8AC/8007C7E8/8007C778/8007C5C0/8007C6DC) and the
/// area check 80046AA8 of `hud.rs` (through `dispatch::call`).
const OWN: &[u32] = &[
    0x8003_8728,
    0x8003_7dfc,
    0x8003_7da0,
    0x8004_59c0,
    0x8004_5c10,
    0x8004_a2c4,
    0x8004_a6e4,
    0x8004_5da8,
    0x8004_61b8,
    0x8004_63d8,
    0x8004_7598,
    0x8002_f354,
    0x8002_f590,
    0x8003_809c,
    0x8006_ade4,
    0x8006_9940,
    0x8006_9d34,
    0x8006_9750,
    0x8006_894c,
    0x8006_93e8,
    0x8006_87f8,
    0x8006_87f0,
    0x8006_9464,
    0x8006_6d04,
    0x8007_c44c,
    0x8007_b36c,
    0x8007_a8b4,
    0x8007_c164,
    0x8007_b834,
    0x8008_3a64,
    0x8008_1838,
    0x8008_3914,
    0x8008_8600,
    0x8008_36ac,
    0x8008_8768,
    0x8008_e8d8,
    0x8008_fde4,
    0x8009_1084,
    0x8003_9d08,
    0x8003_a2c4,
    0x8003_a614,
    0x8003_9f10,
    0x8003_a064,
    0x8007_da7c,
    0x8007_d0a4,
    0x8007_cf40,
    0x8007_d05c,
    0x8007_cb50,
    0x8007_c948,
    0x8007_d214,
    0x8007_cc34,
    0x8007_d248,
    0x8007_d284,
    0x8007_d2b4,
    0x8007_d2ec,
    0x8003_9fb4,
    0x8003_ab44,
    0x8003_a820,
    0x8003_a450,
    0x8003_a4f0,
    0x8004_f070,
    0x8004_efbc,
    0x8004_d794,
    0x8004_decc,
    0x8004_6828,
    0x8004_6968,
    0x8004_6bd4,
    0x8004_da2c,
    0x8004_e504,
    0x8004_3af4,
    0x8004_3bc4,
    0x8004_3d3c,
    0x8004_3e94,
    0x8004_3f24,
    0x8006_d614,
    0x8004_40e0,
];

/// Shared and SDK routines that are still stubs in `shared.rs`/`sdk.rs`.
const SHARED: &[u32] = &[
    0x8002_daa0,
    0x8003_0534,
    0x8003_06ac,
    0x8003_0824,
    0x8003_0998,
    0x8003_0b10,
    0x8003_0c74,
    0x8003_a08c,
    0x8003_a0f8,
    0x8008_2740,
    0x8008_27b4,
    0x800a_4d78,
    0x800a_4fe8,
    0x800a_5128,
    0x800a_5238,
    0x800a_47c8,
    0x800a_4928,
    0x800a_6088,
    0x800a_5be8,
    0x8009_ce80,
    0x800a_c108,
    0x800a_c148,
    0x800a_c178,
];

fn index(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join("snapshots.jsonl")
}

#[derive(Debug, Default)]
struct Outcome {
    /// Snapshot pairs reproduced exactly.
    exact: usize,
    /// Pairs stopped by a shared/SDK stub.
    shared: usize,
    /// Pairs stopped by a routine outside this module.
    external: usize,
    /// Original bytes changed over the exact pairs.
    changed: usize,
}

/// The routine named by an `Untranslated` fault message.
fn untranslated(error: &str) -> Option<u32> {
    let (_, tail) = error.split_once("untranslated original routine 0x")?;
    u32::from_str_radix(tail.get(..8)?, 16).ok()
}

/// Run `run` on every entry snapshot of `callee` in every capture.
fn check(callee: u32, run: impl Fn(&mut Machine, &snapshot::Snapshot) -> Option<u32>) -> Outcome {
    check_with(callee, |_, m, e| run(m, e))
}

/// [`check`] with the capture's index path passed to `run`.
fn check_with(
    callee: u32,
    run: impl Fn(&Path, &mut Machine, &snapshot::Snapshot) -> Option<u32>,
) -> Outcome {
    let mut outcome = Outcome::default();
    for capture in CAPTURES {
        let path = index(capture);
        let Ok(entries) = snapshot::read_index(&path) else {
            continue;
        };
        let count = entries
            .iter()
            .filter(|s| s.callee == callee && s.stage == "entry")
            .count();
        for occurrence in 0..count {
            match snapshot::check_call(&path, callee, occurrence, |m, e| run(&path, m, e)) {
                Ok(changed) => {
                    outcome.exact += 1;
                    outcome.changed += changed;
                }
                Err(error) => match untranslated(&error) {
                    Some(address) if SHARED.contains(&address) => {
                        eprintln!("{capture} #{occurrence}: blocked (shared): {error}");
                        outcome.shared += 1;
                    }
                    Some(address) if !OWN.contains(&address) => {
                        eprintln!("{capture} #{occurrence}: blocked (external): {error}");
                        outcome.external += 1;
                    }
                    _ => panic!("{capture} #{occurrence}: {error}"),
                },
            }
        }
    }
    eprintln!("{callee:08x}: {outcome:?}");
    outcome
}

/// [`check`] for routines the captures must reach.
fn check_reached(
    callee: u32,
    run: impl Fn(&mut Machine, &snapshot::Snapshot) -> Option<u32>,
) -> Outcome {
    let outcome = check(callee, run);
    assert!(
        outcome.exact + outcome.shared + outcome.external > 0,
        "no snapshots of {callee:08x}"
    );
    outcome
}

fn args(e: &snapshot::Snapshot) -> [u32; 4] {
    [e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)]
}

/// Routines outside the module are not claimed. (Claims of `OWN` are
/// checked whenever a captured path reaches them: an `Untranslated` fault
/// naming one fails the test.)
#[test]
fn dispatch_leaves_foreign_routines() {
    let mut m = Machine::default();
    assert!(entities::dispatch(&mut m, 0x8004_7a0c, [0; 4]).is_none());
    assert!(entities::dispatch(&mut m, 0x8002_daa0, [0; 4]).is_none());
    assert!(m.take_fault().is_ok());
}

// ---- 80038728 tree -----------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn mark_entities_37dfc() {
    check_reached(0x8003_7dfc, |m, _| {
        entities::mark_entities_37dfc(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn retire_entities_459c0() {
    check_reached(0x8004_59c0, |m, _| {
        entities::retire_entities_459c0(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_groups_4a2c4() {
    check_reached(0x8004_a2c4, |m, _| {
        entities::update_groups_4a2c4(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn mark_formations_4a6e4() {
    check_reached(0x8004_a6e4, |m, _| {
        entities::mark_formations_4a6e4(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn reset_idle_45c10() {
    check_reached(0x8004_5c10, |m, _| {
        entities::reset_idle_45c10(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn select_behaviour_45da8() {
    check_reached(0x8004_5da8, |m, _| {
        entities::select_behaviour_45da8(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn run_movement_461b8() {
    check_reached(0x8004_61b8, |m, _| {
        entities::run_movement_461b8(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn run_actions_463d8() {
    check_reached(0x8004_63d8, |m, _| {
        entities::run_actions_463d8(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn assign_tracks_2f354() {
    check_reached(0x8002_f354, |m, e| {
        entities::assign_tracks_2f354(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_track_sounds_2f590() {
    check_reached(0x8002_f590, |m, e| {
        entities::update_track_sounds_2f590(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn nearest_targets_47598() {
    check_reached(0x8004_7598, |m, _| {
        entities::nearest_targets_47598(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn run_entity_logic_37da0() {
    check_reached(0x8003_7da0, |m, _| {
        entities::run_entity_logic_37da0(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn integrate_entities_3809c() {
    check_reached(0x8003_809c, |m, _| {
        entities::integrate_entities_3809c(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_entities_38728() {
    check_reached(0x8003_8728, |m, _| {
        entities::update_entities_38728(m);
        None
    });
}

// ---- 8006ADE4 tree -----------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn spawn_particles_69940() {
    check_reached(0x8006_9940, |m, _| {
        entities::spawn_particles_69940(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_particles_69d34() {
    let outcome = check_reached(0x8006_9d34, |m, _| {
        entities::update_particles_69d34(m);
        None
    });
    assert!(outcome.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn projectile_collisions_6894c() {
    check_reached(0x8006_894c, |m, _| {
        entities::projectile_collisions_6894c(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn move_projectiles_693e8() {
    check_reached(0x8006_93e8, |m, _| {
        entities::move_projectiles_693e8(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn run_projectiles_687f8() {
    check_reached(0x8006_87f8, |m, _| {
        entities::run_projectiles_687f8(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn age_projectiles_69464() {
    check_reached(0x8006_9464, |m, _| {
        entities::age_projectiles_69464(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_projectiles_69750() {
    check_reached(0x8006_9750, |m, _| {
        entities::update_projectiles_69750(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn apply_hit_timers_66d04() {
    check_reached(0x8006_6d04, |m, _| {
        entities::apply_hit_timers_66d04(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_shots_6ade4() {
    let outcome = check_reached(0x8006_ade4, |m, _| {
        entities::update_shots_6ade4(m);
        None
    });
    assert!(outcome.shared + outcome.external > 0 || outcome.changed > 0);
}

// ---- 8007C44C tree -----------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn missile_trails_7b36c() {
    check_reached(0x8007_b36c, |m, _| {
        entities::missile_trails_7b36c(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn advance_trail_queues_7a8b4() {
    check_reached(0x8007_a8b4, |m, _| {
        entities::advance_trail_queues_7a8b4(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn entity_trails_7c164() {
    check_reached(0x8007_c164, |m, _| {
        entities::entity_trails_7c164(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn advance_entity_trail_queues_7b834() {
    check_reached(0x8007_b834, |m, _| {
        entities::advance_entity_trail_queues_7b834(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_trails_7c44c() {
    check_reached(0x8007_c44c, |m, _| {
        entities::update_trails_7c44c(m);
        None
    });
}

// ---- 80083A64 tree -----------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn proximity_effect_81838() {
    check_reached(0x8008_1838, |m, e| {
        let [a0, a1, a2, _] = args(e);
        entities::proximity_effect_81838(m, a0, a1, a2);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn approach_effect_83914() {
    check_reached(0x8008_3914, |m, e| {
        entities::approach_effect_83914(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn run_timers_88600() {
    check_reached(0x8008_8600, |m, e| {
        entities::run_timers_88600(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn move_sprites_836ac() {
    check_reached(0x8008_36ac, |m, e| {
        entities::move_sprites_836ac(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn age_timers_88768() {
    check_reached(0x8008_8768, |m, e| {
        entities::age_timers_88768(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_shake_8e8d8() {
    check_reached(0x8008_e8d8, |m, e| {
        entities::update_shake_8e8d8(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn toggle_bits_8fde4() {
    check_reached(0x8008_fde4, |m, e| {
        entities::toggle_bits_8fde4(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_explosion_91084() {
    check_reached(0x8009_1084, |m, e| {
        entities::update_explosion_91084(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_effects_83a64() {
    check_reached(0x8008_3a64, |m, _| {
        entities::update_effects_83a64(m);
        None
    });
}

// ---- 80039D08 tree -----------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn release_lock_45874() {
    let outcome = check_reached(0x8004_5874, |m, e| Some(shared::release_lock(m, e.gpr(4))));
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn set_lock_4577c() {
    let outcome = check_reached(0x8004_577c, |m, e| {
        Some(shared::set_lock(m, e.gpr(4), e.gpr(5)))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_player_lock_3a2c4() {
    check_reached(0x8003_a2c4, |m, _| {
        entities::update_player_lock_3a2c4(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_warning_3a614() {
    check_reached(0x8003_a614, |m, _| {
        entities::update_warning_3a614(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn lock_report_39f10() {
    let outcome = check_reached(0x8003_9f10, |m, _| Some(entities::lock_report_39f10(m)));
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn secondary_target_3a064() {
    let outcome = check_reached(0x8003_a064, |m, _| {
        Some(entities::secondary_target_3a064(m))
    });
    assert!(outcome.exact > 0);
}

/// 80039D08 also writes the caller's stack buffer (+0x10/+0x14), which the
/// RAM comparison excludes: compare those words with the return snapshot.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn update_lock_on_39d08() {
    check_with(0x8003_9d08, |path, m, e| {
        let buffer = e.gpr(4);
        let value = entities::update_lock_on_39d08(m, buffer);
        let outputs = [m.lw(buffer + 0x10), m.lw(buffer + 0x14)];
        let entries = snapshot::read_index(path).expect("index");
        let exit = entries
            .iter()
            .find(|s| {
                s.callee == 0x8003_9d08
                    && s.stage == "return"
                    && s.sequence > e.info.sequence
                    && s.ra == e.info.ra
            })
            .expect("return snapshot");
        let exit = snapshot::Snapshot::load(path.parent().unwrap(), exit).expect("load");
        let expected = [buffer + 0x10, buffer + 0x14].map(|a| {
            u32::from_le_bytes(
                exit.ram[(a & 0x1f_ffff) as usize..][..4]
                    .try_into()
                    .unwrap(),
            )
        });
        assert_eq!(outputs, expected, "stack buffer outputs");
        Some(value)
    });
}

// ---- 8007DA7C tree -----------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn compare_entity_7cf40() {
    let outcome = check_reached(0x8007_cf40, |m, e| {
        Some(entities::compare_entity_7cf40(m, e.gpr(4), e.gpr(5)))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn store_entity_7d05c() {
    check_reached(0x8007_d05c, |m, e| {
        entities::store_entity_7d05c(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn reserve_7c8ac() {
    let outcome = check_reached(0x8007_c8ac, |m, e| {
        Some(shared::reserve_events(m, e.gpr(4)))
    });
    assert!(outcome.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn emit_position_7cb50() {
    let outcome = check_reached(0x8007_cb50, |m, e| {
        let [a0, a1, a2, _] = args(e);
        entities::emit_position_7cb50(m, a0, a1, a2);
        None
    });
    assert!(outcome.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn emit_event_7c948() {
    let outcome = check_reached(0x8007_c948, |m, e| {
        let [a0, a1, a2, a3] = args(e);
        entities::emit_event_7c948(m, a0, a1, a2, a3);
        None
    });
    assert!(outcome.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn record_changes_7d0a4() {
    let outcome = check_reached(0x8007_d0a4, |m, _| {
        entities::record_changes_7d0a4(m);
        None
    });
    assert!(outcome.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn record_frame_7da7c() {
    let outcome = check_reached(0x8007_da7c, |m, _| {
        entities::record_frame_7da7c(m);
        None
    });
    assert!(outcome.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn set_event_focus_7d214() {
    check(0x8007_d214, |m, e| {
        entities::set_event_focus_7d214(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn drop_events_7c7e8() {
    check(0x8007_c7e8, |m, e| {
        shared::drop_events(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn apply_event_7c778() {
    check(0x8007_c778, |m, e| Some(shared::apply_event(m, e.gpr(4))));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn apply_word_event_7c5c0() {
    check(0x8007_c5c0, |m, e| {
        shared::apply_word_event(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn apply_position_event_7c6dc() {
    check(0x8007_c6dc, |m, e| {
        shared::apply_position_event(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn write_event_7cc34() {
    check_reached(0x8007_cc34, |m, e| {
        let [a0, a1, a2, a3] = args(e);
        let extra = m.lw(e.gpr(29) + 0x10);
        entities::write_event_7cc34(m, a0, a1, a2, a3, extra);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn write_particle_event_7d248() {
    check(0x8007_d248, |m, e| {
        let [a0, a1, a2, a3] = args(e);
        entities::write_particle_event_7d248(m, a0, a1, a2, a3);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn write_expiry_event_7d284() {
    check(0x8007_d284, |m, e| {
        entities::write_expiry_event_7d284(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn write_projectile_event_7d2b4() {
    check_reached(0x8007_d2b4, |m, e| {
        let [a0, a1, a2, _] = args(e);
        entities::write_projectile_event_7d2b4(m, a0, a1, a2);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn write_projectile_end_7d2ec() {
    check(0x8007_d2ec, |m, e| {
        entities::write_projectile_end_7d2ec(m, e.gpr(4));
        None
    });
}

// ---- lock-on selection and movement states (entities-003) -----------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn lock_priority_3ab44() {
    check(0x8003_ab44, |m, e| {
        Some(entities::lock_priority_3ab44(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn choose_lock_3a820() {
    let outcome = check_reached(0x8003_a820, |m, e| {
        let [a0, a1, a2, _] = args(e);
        Some(entities::choose_lock_3a820(m, a0, a1, a2))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn switch_lock_3a450() {
    check_reached(0x8003_a450, |m, _| Some(entities::switch_lock_3a450(m)));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn auto_lock_3a4f0() {
    check_reached(0x8003_a4f0, |m, _| Some(entities::auto_lock_3a4f0(m)));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn nearest_marked_39fb4() {
    check(0x8003_9fb4, |m, _| Some(entities::nearest_marked_39fb4(m)));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn clamp_height_4f070() {
    check_reached(0x8004_f070, |m, e| {
        entities::clamp_height_4f070(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn clamp_destination_4efbc() {
    check_reached(0x8004_efbc, |m, e| {
        entities::clamp_destination_4efbc(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn pursue_4d794() {
    let outcome = check_reached(0x8004_d794, |m, e| {
        entities::pursue_4d794(m, e.gpr(4));
        None
    });
    assert!(outcome.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn patrol_4decc() {
    let outcome = check_reached(0x8004_decc, |m, e| {
        entities::patrol_4decc(m, e.gpr(4));
        None
    });
    assert!(outcome.exact > 0);
}

// ---- behaviour checks of 80045DA8 (entities-004) ---------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn ceiling_check_46828() {
    let outcome = check_reached(0x8004_6828, |m, e| {
        Some(entities::ceiling_check_46828(m, e.gpr(4)))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn floor_check_46968() {
    let outcome = check_reached(0x8004_6968, |m, e| {
        Some(entities::floor_check_46968(m, e.gpr(4)))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn area_check_46aa8() {
    let outcome = check_reached(0x8004_6aa8, |m, e| {
        Some(dispatch::call(m, 0x8004_6aa8, [e.gpr(4), 0, 0, 0]))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn goal_flags_46bd4() {
    let outcome = check_reached(0x8004_6bd4, |m, e| {
        Some(entities::goal_flags_46bd4(m, e.gpr(4)))
    });
    assert!(outcome.exact > 0);
}

// ---- steering, throttle and further movement states (entities-006) ---------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn escort_4da2c() {
    let outcome = check_reached(0x8004_da2c, |m, e| {
        entities::escort_4da2c(m, e.gpr(4));
        None
    });
    assert!(outcome.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn hold_4e504() {
    check_reached(0x8004_e504, |m, e| {
        entities::hold_4e504(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn normalize_angles_43af4() {
    check_reached(0x8004_3af4, |m, e| {
        entities::normalize_angles_43af4(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn roll_rate_43bc4() {
    let outcome = check_reached(0x8004_3bc4, |m, e| {
        Some(entities::roll_rate_43bc4(m, e.gpr(4), e.gpr(5)))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn pitch_rate_43d3c() {
    let outcome = check_reached(0x8004_3d3c, |m, e| {
        Some(entities::pitch_rate_43d3c(m, e.gpr(4), e.gpr(5)))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn yaw_rate_43e94() {
    let outcome = check_reached(0x8004_3e94, |m, e| {
        Some(entities::yaw_rate_43e94(m, e.gpr(4), e.gpr(5)))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn steer_43f24() {
    let outcome = check_reached(0x8004_3f24, |m, e| {
        entities::steer_43f24(m, e.gpr(4));
        None
    });
    assert!(outcome.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn speed_limits_6d614() {
    let outcome = check_reached(0x8006_d614, |m, e| {
        let [a0, a1, a2, a3] = args(e);
        Some(entities::speed_limits_6d614(m, a0, a1, a2, a3))
    });
    assert!(outcome.exact > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-001..006"]
fn throttle_440e0() {
    let outcome = check_reached(0x8004_40e0, |m, e| {
        Some(entities::throttle_440e0(m, e.gpr(4)))
    });
    assert!(outcome.changed > 0);
}

// ---- unit tests for paths the captures do not reach -------------------------------------

/// A machine with a minimal lock-link table.
fn links() -> Machine {
    let mut m = Machine::default();
    for slot in 0..32u32 {
        m.sw(0x800c_4b48 + slot * 4, 0xffff_ffff);
    }
    m
}

#[test]
fn lock_links_track_and_release() {
    let mut m = links();
    m.sw(ac3_flight::GP + 0x2dc, 0x8010_0000);
    // Slot 3 locks onto 5, slot 4 too; a third tracker is refused.
    assert_eq!(shared::set_lock(&mut m, 3, 5), 5);
    assert_eq!(shared::set_lock(&mut m, 4, 5), 5);
    assert_eq!(shared::set_lock(&mut m, 6, 5), 0xff);
    assert_eq!(m.lbu(0x800c_4b48 + 5 * 4 + 2), 3);
    assert_eq!(m.lbu(0x800c_4b48 + 5 * 4 + 3), 4);
    assert_eq!(m.lbu(0x800c_4b48 + 3 * 4 + 1), 5);
    // The player takes over the +3 tracker of a full target.
    assert_eq!(shared::set_lock(&mut m, 0, 5), 5);
    assert_eq!(m.lbu(0x800c_4b48 + 5 * 4 + 3), 0);
    assert_eq!(m.lbu(0x800c_4b48 + 4 * 4 + 1), 0xff, "slot 4 released");
    // Releasing slot 3 clears the target's +2 tracker.
    assert_eq!(shared::release_lock(&mut m, 3), 0xff);
    assert_eq!(m.lbu(0x800c_4b48 + 5 * 4 + 2), 0xff);
    // Second tracker on the player needs context +0x398.
    assert_eq!(shared::set_lock(&mut m, 7, 0), 0);
    assert_eq!(shared::set_lock(&mut m, 8, 0), 0xff);
    m.sw(0x8010_0398, 1);
    assert_eq!(shared::set_lock(&mut m, 8, 0), 0);
    assert!(m.take_fault().is_ok());
}

#[test]
fn bit_toggles_and_sprite_motion() {
    let mut m = Machine::default();
    m.sh(0x8010_0000, 0b1011);
    m.sh(0x8010_0002, 0b0010);
    entities::toggle_bits_8fde4(&mut m, 0x8010_0000);
    assert_eq!((m.lhu(0x8010_0000), m.lhu(0x8010_0002)), (0b0010, 0));
    // One sprite group, record 0: count 0 of 5, velocity (2, -3, 4) / 256,
    // drag shift 1, rise mode 0x10.
    let masks = 0x8011_0000;
    let record = masks + 4;
    m.sw(masks, 1);
    m.sh(record + 2, 5);
    m.sh(record + 0x10, 2);
    m.sh(record + 0x12, 0xfffd);
    m.sh(record + 0x14, 4);
    m.sh(record + 0x16, 0x11);
    entities::move_sprites_836ac(&mut m, masks, 1);
    assert_eq!(m.lw(record + 4), 0x200);
    assert_eq!(m.lw(record + 8), 0xffff_fd00);
    assert_eq!(m.lw(record + 0xc), 0x400);
    assert_eq!(m.lhu(record + 0x10), 1);
    // dy = -0x300 + 0x180 + 0x1A00 = 0x1880 → 0x18.
    assert_eq!(m.lhu(record + 0x12), 0x18);
    m.sh(record, 4);
    entities::move_sprites_836ac(&mut m, masks, 1);
    assert_eq!(m.lw(masks), 0, "limit reached clears the mask bit");
    assert!(m.take_fault().is_ok());
}

#[test]
fn event_words_pack_like_the_original() {
    let mut m = Machine::default();
    let events = 0x8012_0000;
    m.sw(ac3_flight::GP + 0x54c, events);
    m.sw(events, 1);
    m.sw(events + 4, 0x8012_1000);
    m.sw(events + 8, 0x8012_2000);
    m.sw(events + 0xc, 0x8012_1000);
    m.sw(events + 0x10, 0x8012_0f00);
    m.sw(0x8012_1000, 0xffff_ffff);
    entities::emit_event_7c948(&mut m, 3, 0x25, 0x1234, 0);
    // header type 1, kind 3 in bits 7..9, slot in 10..15, kind word at 16.
    let expected = ((((0xffff_fff0u32 | 1) & 0xffff_ff8f & 0xffff_fc7f) | (3 << 7)) & 0xffff_03ff
        | (0x25 << 10))
        & 0xff81
        | (0x1234 << 16);
    assert_eq!(m.lw(0x8012_1000), expected);
    assert_eq!(m.lw(events + 0xc), 0x8012_1004);
    // Wrap: a reservation past the end restarts at the base.
    m.sw(events + 0xc, 0x8012_1ffc);
    m.sw(events + 0x10, 0x8012_1800);
    assert_eq!(shared::reserve_events(&mut m, 4), 0x8012_1ffc);
    assert_eq!(m.lw(events + 0xc), 0x8012_1000);
    assert!(m.take_fault().is_ok());
}

/// A recorder with the ring at 0x8013_0000..0x8013_0100 (write at the
/// base, read pointer well ahead so nothing is dropped) and replay copies
/// at +0x18.
fn recorder() -> (Machine, u32) {
    let mut m = Machine::default();
    let events = 0x8012_0000;
    m.sw(ac3_flight::GP + 0x54c, events);
    m.sw(events + 4, 0x8013_0000);
    m.sw(events + 8, 0x8013_0100);
    m.sw(events + 0xc, 0x8013_0000);
    m.sw(events + 0x10, 0x8013_0080);
    (m, events)
}

#[test]
fn focus_event_and_replay() {
    let (mut m, events) = recorder();
    m.sw(events, 3);
    entities::set_event_focus_7d214(&mut m, 0x25);
    assert_eq!(m.lw(events + 0x184c), 0x25);
    // kind 4: header 1, class 0, kind 4 << 7, slot << 10, half +2 cleared.
    let word = m.lw(0x8013_0000);
    assert_eq!(word & 0xffff, 1 | (4 << 7) | (0x25 << 10));
    // Replaying it sets the replay focus (+0x1840) and clears +0x1830..
    m.sw(events + 0x1830, 7);
    assert_eq!(shared::apply_event(&mut m, 0x8013_0000), 1);
    assert_eq!(m.lw(events + 0x1840), 0x25);
    assert_eq!(m.lw(events + 0x1830), 0);
    assert!(m.take_fault().is_ok());
}

#[test]
fn dropping_events_replays_them_in_order() {
    let (mut m, events) = recorder();
    m.sw(events, 2);
    let copy = events + 0x18 + 5 * 0x20;
    // Frame marker, presence of slot 5 (bit 16 set), position of slot 5.
    entities::emit_event_7c948(&mut m, 0, 0, 0, 0);
    entities::emit_event_7c948(&mut m, 1, 5, 1, 0);
    let position = 0x8014_0000;
    m.sw(position, 0x0012_3456);
    m.sw(position + 4, 0xfff0_1234);
    m.sw(position + 8, 0x0000_5fff);
    m.sh(position + 0x10, 0x0404);
    m.sh(position + 0x12, 0xfffc);
    m.sh(position + 0x14, 8);
    entities::emit_position_7cb50(&mut m, 0, 5, position);
    assert_eq!(m.lw(events + 0xc), 0x8013_0018);
    m.sw(events + 0x10, 0x8013_0000);
    shared::drop_events(&mut m, 6);
    assert_eq!(m.lw(events + 0x10), 0x8013_0018, "read pointer");
    assert_eq!(m.lw(events), 1, "one frame marker dropped");
    assert_eq!(m.lw(events + 0x14), 1, "frame marker replayed");
    assert_eq!(m.lw(copy) & 0x1_4000, 0x1_4000, "presence replayed");
    assert_eq!(m.lw(copy + 8), 0x0012_3000);
    assert_eq!(m.lw(copy + 0xc), 0xfff0_1000);
    assert_eq!(m.lw(copy + 0x10), 0x0000_5000);
    assert_eq!(m.lhu(copy + 0x18), 0x0404);
    assert_eq!(m.lhu(copy + 0x1a), 0xfffc);
    assert_eq!(m.lhu(copy + 0x1c), 8);
    assert!(m.take_fault().is_ok());
}

#[test]
fn object_events_pack_like_the_original() {
    let (mut m, _) = recorder();
    entities::write_expiry_event_7d284(&mut m, 0x13);
    entities::write_projectile_end_7d2ec(&mut m, 0x7f);
    assert_eq!(m.lw(0x8013_0000), 1 | 0x20 | 0x80 | (0x13 << 9));
    assert_eq!(m.lw(0x8013_0004), 1 | 0x1a0 | (0x7f << 9));
    assert!(m.take_fault().is_ok());
}

#[test]
fn lock_choice_steps_through_priorities() {
    let mut m = Machine::default();
    let table = 0x8015_0000;
    m.sw(ac3_flight::GP + 0x2d8, table);
    for slot in 0..64u32 {
        let entity = 0x8016_0000 + slot * 0x100;
        m.sw(table + slot * 4, entity);
    }
    // Lockable slots 3, 6 and 9 with object distances 0x7000, 0x6000, 0x7800.
    for (slot, distance) in [(3u32, 0x7000u32), (6, 0x6000), (9, 0x7800)] {
        let entity = 0x8016_0000 + slot * 0x100;
        m.sw(entity, 0x4000);
        m.sw(0x800c_4d98 + slot * 4, 0x8001);
        let object = 0x8017_0000 + slot * 0x40;
        m.sw(0x800c_4c98 + slot * 4, object);
        m.sw(object, 1);
        m.sw(object + 4, distance);
    }
    let score = |slot: u32, distance: u32| slot + ((0x8000 - distance) << 6);
    assert_eq!(entities::lock_priority_3ab44(&mut m, 6), score(6, 0x6000));
    // Scores: 9 lowest, then 3, then 6 highest.
    assert_eq!(entities::choose_lock_3a820(&mut m, 0xff, 0, 0), 6);
    assert_eq!(entities::choose_lock_3a820(&mut m, 3, 1, 0), 6);
    assert_eq!(entities::choose_lock_3a820(&mut m, 3, 0, 0), 9);
    assert_eq!(entities::choose_lock_3a820(&mut m, 6, 1, 0), 9, "wraps");
    assert_eq!(entities::choose_lock_3a820(&mut m, 9, 0, 0), 6, "wraps");
    // A priority object (flag 2) wins over the current ordinary target.
    m.sw(0x8017_0000 + 9 * 0x40, 3);
    assert_eq!(entities::choose_lock_3a820(&mut m, 3, 0, 0), 9);
    assert!(m.take_fault().is_ok());
}
