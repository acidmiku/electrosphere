//! Entity AI, projectile behaviours, trails and effect spawners
//! (`src/ai.rs`) on original entry snapshots must reproduce the original
//! return snapshots (`snapshot::check_call`: all RAM outside the stack,
//! scratchpad, GTE registers, and v0 where returned).
//!
//! Captures (`analysis/flight-runtime/entities-007..`, checkpoint
//! `menu-runtime/mission-006/final.sav`, CIRCLE every 45 frames from frame
//! 400 so missiles fly and enemies engage): every entry snapshot of the
//! tested routine in every capture runs. See `CAPTURES` for the frames.
//!
//! A captured path that reaches a routine this module does not translate
//! raises `Fault::Untranslated` before the comparison. Such pairs are
//! counted as blocked, separately for shared/SDK routines and for routines
//! outside this module (for example the effect commands of the player
//! agent's `effects.rs`), and printed; the test validates them fully once
//! those translations land. An `Untranslated` fault naming a routine of
//! this module always fails.
mod support;
use support::oracle::{self, Rng};

use ac3_flight::SmallData;
use ac3_flight::{Machine, ai, snapshot};
use std::path::PathBuf;

/// The ai captures (see `analysis/flight-runtime/<name>/capture-addon.lua`
/// for the frames and callees of each).
const CAPTURES: &[&str] = &["entities-007", "entities-008"];

/// Every routine `ai::dispatch` claims.
const OWN: &[u32] = &[
    0x8004_d860,
    0x8004_d978,
    0x8004_dac0,
    0x8004_df64,
    0x8004_e580,
    0x8004_e5e0,
    0x8004_e6d8,
    0x8004_e720,
    0x8004_7d78,
    0x8004_e784,
    0x8004_e970,
    0x8004_ead4,
    0x8004_eb1c,
    0x8004_eb6c,
    0x8004_ecbc,
    0x8004_ef14,
    0x8004_34ec,
    0x8004_3554,
    0x8004_3640,
    0x8004_3818,
    0x8004_392c,
    0x8004_7ed4,
    0x8004_7f34,
    0x8004_82e4,
    0x8004_83c0,
    0x8004_8514,
    0x8004_8134,
    0x8004_86f0,
    0x8004_878c,
    0x8004_8800,
    0x8004_8ba0,
    0x8004_8c5c,
    0x8004_8d3c,
    0x8004_8e34,
    0x8004_8f40,
    0x8004_90ac,
    0x8004_9300,
    0x8004_9554,
    0x8004_965c,
    0x8004_9870,
    0x8004_99cc,
    0x8004_9bc4,
    0x8004_9c84,
    0x8004_9d94,
    0x8004_9f9c,
    0x8004_88e0,
    0x8004_8988,
    0x8004_a144,
    0x8004_f100,
    0x8004_f2b0,
    0x8004_f4a0,
    0x8004_f594,
    0x8004_f988,
    0x8004_f7c8,
    0x8004_f9f0,
    0x8004_fce4,
    0x8004_fdb0,
    0x8004_5ce4,
    0x8003_7fc4,
    0x8004_a5c8,
    0x8004_a658,
    0x8004_a334,
    0x8004_6e0c,
    0x8004_6ebc,
    0x8004_7124,
    0x8006_786c,
    0x8006_7988,
    0x8006_8548,
    0x8006_8778,
    0x8006_859c,
    0x8003_1784,
    0x8006_7af4,
    0x8006_7cfc,
    0x8006_82cc,
    0x8006_7f04,
    0x8006_8130,
    0x8006_7658,
    0x8006_84bc,
    0x8006_85b8,
    0x8006_8640,
    0x8006_86f0,
    0x8006_6c84,
    0x8006_91a8,
    0x8006_932c,
    0x8007_e7e0,
    0x8007_ea98,
    0x8007_eaf8,
    0x8007_ee70,
    0x8007_d830,
    0x8008_86d8,
    0x8007_f3a8,
    0x8007_f104,
    0x8007_f8c8,
    0x8007_f928,
    0x8007_fc50,
    0x8008_1738,
    0x8008_19a8,
    0x8008_1a0c,
    0x8008_1a94,
    0x8008_1b88,
    0x8008_15d4,
    0x8008_16f0,
    0x8008_3814,
    0x8009_2350,
    0x8007_a60c,
    0x8007_a7c0,
    0x8007_b5f0,
    0x8007_b77c,
];

/// Shared and SDK routines (`shared.rs`, `sdk.rs`) the module calls.
const SHARED: &[u32] = &[
    0x8003_0534,
    0x8003_06ac,
    0x8003_0b10,
    0x8008_2740,
    0x8008_27b4,
    0x800a_4d78,
    0x800a_4fe8,
    0x800a_5518,
    0x800a_6088,
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
            match snapshot::check_call(&path, callee, occurrence, &run) {
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

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn formation_4d860() {
    check_reached(0x8004_d860, |m, e| {
        ai::formation_4d860(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn wingman_4d978() {
    check(0x8004_d978, |m, e| {
        ai::wingman_4d978(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn attack_run_4dac0() {
    check_reached(0x8004_dac0, |m, e| {
        ai::attack_run_4dac0(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn evade_4df64() {
    check(0x8004_df64, |m, e| Some(ai::evade_4df64(m, e.gpr(4))));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn state_11_4e580() {
    check(0x8004_e580, |m, e| {
        ai::state_11_4e580(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn destroyed_4e5e0() {
    check(0x8004_e5e0, |m, e| {
        ai::destroyed_4e5e0(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn state_14_4e6d8() {
    check(0x8004_e6d8, |m, e| {
        ai::state_14_4e6d8(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn state_15_4e720() {
    check(0x8004_e720, |m, e| {
        ai::state_15_4e720(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn latest_event_47d78() {
    check(0x8004_7d78, |m, _| Some(ai::latest_event_47d78(m)));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn launch_4e784() {
    check(0x8004_e784, |m, e| {
        ai::launch_4e784(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn escorted_4e970() {
    check(0x8004_e970, |m, e| {
        ai::escorted_4e970(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn state_18_4ead4() {
    check(0x8004_ead4, |m, e| {
        ai::state_18_4ead4(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn hover_4eb1c() {
    check(0x8004_eb1c, |m, e| {
        ai::hover_4eb1c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn timed_flight_4eb6c() {
    check(0x8004_eb6c, |m, e| {
        ai::timed_flight_4eb6c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn escort_duty_4ecbc() {
    check(0x8004_ecbc, |m, e| {
        ai::escort_duty_4ecbc(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn peel_off_4ef14() {
    check(0x8004_ef14, |m, e| {
        ai::peel_off_4ef14(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn level_off_434ec() {
    check_reached(0x8004_34ec, |m, e| Some(ai::level_off_434ec(m, e.gpr(4))));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn relax_attitude_43554() {
    check_reached(0x8004_3554, |m, e| {
        Some(ai::relax_attitude_43554(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn aim_43640() {
    check_reached(0x8004_3640, |m, e| {
        Some(ai::aim_43640(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn climb_to_43818() {
    check_reached(0x8004_3818, |m, e| {
        Some(ai::climb_to_43818(m, e.gpr(4), e.gpr(5)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn turn_to_4392c() {
    check_reached(0x8004_392c, |m, e| {
        Some(ai::turn_to_4392c(m, e.gpr(4), e.gpr(5)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn aim_47ed4() {
    check(0x8004_7ed4, |m, e| Some(ai::aim_47ed4(m, e.gpr(4))));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn approach_47f34() {
    check_reached(0x8004_7f34, |m, e| {
        ai::approach_47f34(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn attack_aim_482e4() {
    check(0x8004_82e4, |m, e| {
        ai::attack_aim_482e4(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn fly_to_483c0() {
    check_reached(0x8004_83c0, |m, e| {
        ai::fly_to_483c0(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn hover_turn_48514() {
    check(0x8004_8514, |m, e| {
        ai::hover_turn_48514(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn dock_48134() {
    check(0x8004_8134, |m, e| {
        ai::dock_48134(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn level_486f0() {
    check_reached(0x8004_86f0, |m, e| {
        ai::level_486f0(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn cruise_4878c() {
    check(0x8004_878c, |m, e| {
        ai::cruise_4878c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn strafe_48800() {
    check(0x8004_8800, |m, e| {
        ai::strafe_48800(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn loop_48ba0() {
    check(0x8004_8ba0, |m, e| {
        ai::loop_48ba0(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn half_loop_48c5c() {
    check(0x8004_8c5c, |m, e| {
        ai::half_loop_48c5c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn knife_edge_48d3c() {
    check(0x8004_8d3c, |m, e| {
        ai::knife_edge_48d3c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn knife_slide_48e34() {
    check(0x8004_8e34, |m, e| {
        ai::knife_slide_48e34(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn attitude_48f40() {
    check(0x8004_8f40, |m, e| {
        ai::attitude_48f40(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn landing_490ac() {
    check(0x8004_90ac, |m, e| {
        ai::landing_490ac(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn catapult_49300() {
    check(0x8004_9300, |m, e| {
        ai::catapult_49300(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn climb_out_49554() {
    check(0x8004_9554, |m, e| {
        ai::climb_out_49554(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn shadow_4965c() {
    check(0x8004_965c, |m, e| {
        ai::shadow_4965c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn height_band_49870() {
    check_reached(0x8004_9870, |m, e| {
        ai::height_band_49870(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn turn_back_499cc() {
    check(0x8004_99cc, |m, e| {
        ai::turn_back_499cc(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn blow_up_49bc4() {
    check(0x8004_9bc4, |m, e| {
        ai::blow_up_49bc4(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn burn_49c84() {
    check(0x8004_9c84, |m, e| {
        ai::burn_49c84(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn spin_fall_49d94() {
    check(0x8004_9d94, |m, e| {
        ai::spin_fall_49d94(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn glide_fall_49f9c() {
    check(0x8004_9f9c, |m, e| {
        ai::glide_fall_49f9c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn halt_488e0() {
    check(0x8004_88e0, |m, e| {
        ai::halt_488e0(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn taxi_48988() {
    check(0x8004_8988, |m, e| {
        ai::taxi_48988(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn take_off_4a144() {
    check(0x8004_a144, |m, e| {
        ai::take_off_4a144(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn select_default_4f100() {
    check_reached(0x8004_f100, |m, e| {
        Some(ai::select_default_4f100(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn select_escort_4f2b0() {
    check_reached(0x8004_f2b0, |m, e| {
        Some(ai::select_escort_4f2b0(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn follow_leader_4f4a0() {
    check_reached(0x8004_f4a0, |m, e| {
        ai::follow_leader_4f4a0(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn select_formation_4f594() {
    check(0x8004_f594, |m, e| {
        Some(ai::select_formation_4f594(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn load_waypoint_4f988() {
    check(0x8004_f988, |m, e| {
        Some(ai::load_waypoint_4f988(m, e.gpr(4), e.gpr(5)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn follow_route_4f7c8() {
    check(0x8004_f7c8, |m, e| {
        Some(ai::follow_route_4f7c8(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn keyframes_4f9f0() {
    check(0x8004_f9f0, |m, e| Some(ai::keyframes_4f9f0(m, e.gpr(4))));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn special_state_4fce4() {
    check(0x8004_fce4, |m, e| {
        Some(ai::special_state_4fce4(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn check_finished_4fdb0() {
    check(0x8004_fdb0, |m, e| {
        Some(ai::check_finished_4fdb0(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn next_spawn_45ce4() {
    check(0x8004_5ce4, |m, e| Some(ai::next_spawn_45ce4(m, e.gpr(4))));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn clear_slot_flags_37fc4() {
    check(0x8003_7fc4, |m, e| {
        ai::clear_slot_flags_37fc4(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn formation_offset_4a5c8() {
    check(0x8004_a5c8, |m, e| {
        Some(ai::formation_offset_4a5c8(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn disband_group_4a658() {
    check(0x8004_a658, |m, e| {
        ai::disband_group_4a658(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn update_group_4a334() {
    check(0x8004_a334, |m, e| {
        ai::update_group_4a334(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn fire_control_46e0c() {
    check_reached(0x8004_6e0c, |m, _| Some(ai::fire_control_46e0c(m)));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn gun_46ebc() {
    check_reached(0x8004_6ebc, |m, e| Some(ai::gun_46ebc(m, e.gpr(4))));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn missiles_47124() {
    check_reached(0x8004_7124, |m, e| Some(ai::missiles_47124(m, e.gpr(4))));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn rocket_6786c() {
    check_reached(0x8006_786c, |m, e| {
        ai::rocket_6786c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn drop_rocket_67988() {
    check(0x8006_7988, |m, e| {
        ai::drop_rocket_67988(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn bomb_68548() {
    check(0x8006_8548, |m, e| {
        ai::bomb_68548(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn accelerate_68778() {
    check(0x8006_8778, |m, e| {
        ai::accelerate_68778(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn arm_6859c() {
    check(0x8006_859c, |m, e| {
        ai::arm_6859c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn aim_31784() {
    check(0x8003_1784, |m, e| {
        Some(ai::aim_31784(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn homing_67af4() {
    check(0x8006_7af4, |m, e| {
        ai::homing_67af4(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn homing_67cfc() {
    check(0x8006_7cfc, |m, e| {
        ai::homing_67cfc(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn homing_682cc() {
    check(0x8006_82cc, |m, e| {
        ai::homing_682cc(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn sea_skimmer_67f04() {
    check(0x8006_7f04, |m, e| {
        ai::sea_skimmer_67f04(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn cluster_68130() {
    check(0x8006_8130, |m, e| {
        ai::cluster_68130(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn submunition_67658() {
    check(0x8006_7658, |m, e| Some(ai::submunition_67658(m, args(e))));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn impact_684bc() {
    check(0x8006_84bc, |m, e| {
        ai::impact_684bc(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn splash_685b8() {
    check(0x8006_85b8, |m, e| {
        ai::splash_685b8(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn burst_on_target_68640() {
    check(0x8006_8640, |m, e| {
        ai::burst_on_target_68640(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn splash_686f0() {
    check(0x8006_86f0, |m, e| {
        ai::splash_686f0(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn queue_damage_66c84() {
    check(0x8006_6c84, |m, e| {
        Some(ai::queue_damage_66c84(
            m,
            e.gpr(4),
            e.gpr(5),
            e.gpr(6),
            e.gpr(7),
        ))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn blast_691a8() {
    check(0x8006_91a8, |m, e| {
        ai::blast_691a8(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn direct_hit_6932c() {
    check(0x8006_932c, |m, e| {
        ai::direct_hit_6932c(m, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn explosion_particles_7e7e0() {
    check(0x8007_e7e0, |m, e| {
        ai::explosion_particles_7e7e0(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn explosion_7ea98() {
    check(0x8007_ea98, |m, e| {
        ai::explosion_7ea98(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn debris_particles_7eaf8() {
    check(0x8007_eaf8, |m, e| {
        ai::debris_particles_7eaf8(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn debris_7ee70() {
    check(0x8007_ee70, |m, e| {
        ai::debris_7ee70(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn vector_command_7d830() {
    check(0x8007_d830, |m, e| {
        Some(ai::vector_command_7d830(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn direction_angles_886d8() {
    check(0x8008_86d8, |m, e| {
        Some(ai::direction_angles_886d8(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn impact_burst_7f3a8() {
    check(0x8007_f3a8, |m, e| {
        ai::impact_burst_7f3a8(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn impact_sparks_7f104() {
    check(0x8007_f104, |m, e| {
        ai::impact_sparks_7f104(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn splash_7f8c8() {
    check(0x8007_f8c8, |m, e| {
        ai::splash_7f8c8(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn explosion_particles_7f928() {
    check(0x8007_f928, |m, e| {
        ai::explosion_particles_7f928(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn explosion_7fc50() {
    check(0x8007_fc50, |m, e| {
        ai::explosion_7fc50(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn dust_81738() {
    check(0x8008_1738, |m, e| {
        ai::dust_81738(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn claim_tracer_819a8() {
    check(0x8008_19a8, |m, _| Some(ai::claim_tracer_819a8(m)));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn tracer_81a0c() {
    check(0x8008_1a0c, |m, e| {
        Some(ai::tracer_81a0c(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn tracer_command_81a94() {
    check(0x8008_1a94, |m, e| {
        ai::tracer_command_81a94(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn gun_fire_81b88() {
    check(0x8008_1b88, |m, e| {
        ai::gun_fire_81b88(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn spark_815d4() {
    check(0x8008_15d4, |m, e| {
        ai::spark_815d4(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn sparks_816f0() {
    check(0x8008_16f0, |m, e| {
        ai::sparks_816f0(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn wake_83814() {
    check(0x8008_3814, |m, e| {
        ai::wake_83814(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn fade_92350() {
    check(0x8009_2350, |m, e| {
        Some(ai::fade_92350(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn push_missile_trail_7a60c() {
    check_reached(0x8007_a60c, |m, e| {
        Some(ai::push_missile_trail_7a60c(m, e.gpr(4), e.gpr(5)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn rewrite_missile_trail_7a7c0() {
    check_reached(0x8007_a7c0, |m, e| {
        Some(ai::rewrite_missile_trail_7a7c0(m, e.gpr(4), e.gpr(5)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn push_entity_trail_7b5f0() {
    check_reached(0x8007_b5f0, |m, e| {
        Some(ai::push_entity_trail_7b5f0(m, e.gpr(4), e.gpr(5)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn rewrite_entity_trail_7b77c() {
    check_reached(0x8007_b77c, |m, e| {
        Some(ai::rewrite_entity_trail_7b77c(m, e.gpr(4), e.gpr(5)))
    });
}

// ---- edge paths ------------------------------------------------------------------

/// Routines outside the module are not claimed.
#[test]
fn dispatch_leaves_foreign_routines() {
    let mut m = Machine::default();
    assert!(ai::dispatch(&mut m, 0x8004_61b8, [0; 4]).is_none());
    assert!(ai::dispatch(&mut m, 0x8007_d720, [0; 4]).is_none());
    assert!(ai::dispatch(&mut m, 0x8002_daa0, [0; 4]).is_none());
    assert!(m.take_fault().is_ok());
}

/// `[gp+0x544]` for the trail tests.
const TRAILS: u32 = 0x8010_0000;

#[test]
fn missile_trail_queue_full_and_unused() {
    let mut m = Machine::default();
    m.set_gw(0x544, TRAILS);
    m.sw(TRAILS, u32::MAX);
    assert_eq!(
        ai::push_missile_trail_7a60c(&mut m, u32::MAX, 0x8011_0000),
        u32::MAX
    );
    m.sw(TRAILS, 0);
    assert_eq!(
        ai::push_missile_trail_7a60c(&mut m, 3, 0x8011_0000),
        u32::MAX
    );
    assert_eq!(
        ai::rewrite_missile_trail_7a7c0(&mut m, u32::MAX - 1, 0x8011_0000),
        u32::MAX
    );
    assert_eq!(
        ai::rewrite_missile_trail_7a7c0(&mut m, 3, 0x8011_0000),
        u32::MAX
    );
}

#[test]
fn missile_trail_claims_first_free_queue() {
    let mut m = Machine::default();
    m.set_gw(0x544, TRAILS);
    m.sw(TRAILS, 0b1011);
    let point = 0x8011_0000;
    for offset in (0..0x3c).step_by(4) {
        m.sw(point + offset, 0x100 + offset);
    }
    let queue = TRAILS + 4 + 2 * 0x908;
    m.sw(queue, 7);
    m.sw(queue + 4, 7);
    assert_eq!(ai::push_missile_trail_7a60c(&mut m, u32::MAX, point), 2);
    assert_eq!(m.lw(TRAILS), 0b1111);
    assert_eq!(m.lw(queue), 1);
    assert_eq!(m.lw(queue + 4), 0);
    assert_eq!(m.lw(queue + 0x88), 0x5a);
    // Words +0xC, +0x1C and +0x2C are not copied.
    assert_eq!(m.lw(queue + 0x108), 0x100);
    assert_eq!(m.lw(queue + 0x108 + 0xc), 0);
    assert_eq!(m.lw(queue + 0x108 + 0x38), 0x138);
    // A queue with 32 pending points keeps its handle and is unchanged.
    m.sw(queue, 0x25);
    m.sw(queue + 4, 5);
    assert_eq!(ai::push_missile_trail_7a60c(&mut m, 2, point), 2);
    assert_eq!(m.lw(queue), 0x25);
}

#[test]
fn entity_trail_rewrites_newest_point() {
    let mut m = Machine::default();
    m.set_gw(0x544, TRAILS);
    m.sw(TRAILS + 0x1_2124, 1 << 4);
    let queue = TRAILS + 0x1_2128 + 4 * 0x508;
    m.sw(queue, 0x23);
    let point = 0x8011_0000;
    for offset in (0..0x1c).step_by(4) {
        m.sw(point + offset, 0x200 + offset);
    }
    assert_eq!(ai::rewrite_entity_trail_7b77c(&mut m, 4, point), 4);
    let newest = queue + 0x108 + 2 * 0x20;
    assert_eq!(m.lw(newest), 0x200);
    assert_eq!(m.lw(newest + 0xc), 0);
    assert_eq!(m.lw(newest + 0x18), 0x218);
}

#[test]
fn tracer_and_damage_queues_report_full() {
    let mut m = Machine::default();
    let effects = 0x8012_0000;
    m.set_gw(0x40c, effects);
    m.sw(effects + 0x3964, 0x8000_0000);
    assert_eq!(ai::claim_tracer_819a8(&mut m), 0);
    m.sw(effects + 0x3964, 0x0003_0007);
    // Bits 0 and 1 are set in both halves: the third tracer.
    assert_eq!(ai::claim_tracer_819a8(&mut m), effects + 0x3968 + 2 * 0x1c);
    assert_eq!(m.lw(effects + 0x3964), 0x0007_0007);
    let shots = 0x8013_0000;
    m.set_gw(0x514, shots);
    m.sw(shots + 0x33e0, u32::MAX);
    assert_eq!(ai::queue_damage_66c84(&mut m, 1, 2, 3, 4), 0);
    m.sw(shots + 0x33e0, 1);
    let entry = shots + 0x33e4 + 12;
    assert_eq!(ai::queue_damage_66c84(&mut m, 1, 2, 3, 4), entry);
    assert_eq!(m.lw(shots + 0x33e0), 3);
    assert_eq!((m.lhu(entry), m.lhu(entry + 2)), (4, 3));
    assert_eq!((m.lw(entry + 4), m.lw(entry + 8)), (2, 1));
}

#[test]
fn fade_without_level_adds_nothing() {
    let mut m = Machine::default();
    let (ot, prim) = (0x8014_0000, 0x8014_1000);
    m.sw(ot, 0x00ff_ffff);
    assert_eq!(ai::fade_92350(&mut m, ot, prim, 0x8014_2000, 0), prim);
    assert_eq!(m.lw(ot), 0x00ff_ffff);
    assert_eq!(m.lw(prim), 0);
}

#[test]
fn finished_slots_retire() {
    let mut m = Machine::default();
    let records = 0x8015_0000;
    m.set_gw(0x33c, records);
    let record = records + 5 * 0xb8;
    m.sb(record + 0x3d, 0xff);
    assert_eq!(ai::check_finished_4fdb0(&mut m, 5), 0);
    assert_eq!(m.lbu(record + 0x1c), 0xff);
    m.sb(record + 0x3d, 3);
    m.sb(record + 0x1c, 1);
    assert_eq!(ai::check_finished_4fdb0(&mut m, 5), 1);
    assert_eq!(m.lbu(record + 0x1c), 1);
}

// ==== phase 3: original-code oracle cases ===================================================
//
// No capture reaches the script sound 8002F044, the phase entry 8004C644,
// 8004D204 or the blast spawners 800859F8..800864A8, so each is validated
// with the test-only oracle (`support::oracle`): the original instructions
// run from flight snapshots (entities-007/008, live particle pools) with
// randomised inputs and must match the translation on all of RAM outside
// the dead stack, the scratchpad, the GTE, `sp`, the first fault and `v0`
// where returned. Callees run natively in both runs.

/// The flight bases: entries of 8004F100 in the ai captures.
fn flight_bases() -> Vec<Machine> {
    let mut bases: Vec<Machine> = Vec::new();
    for capture in CAPTURES {
        for snapshot in oracle::entries(capture, 0x8004_f100) {
            bases.push(snapshot.machine());
        }
    }
    assert!(!bases.is_empty(), "no ai flight snapshots");
    bases
}

/// A uniformly random word.
fn word(rng: &mut Rng) -> u32 {
    (rng.next() << 8) ^ rng.next()
}

/// A random value in `-range..range`.
fn signed(rng: &mut Rng, range: u32) -> u32 {
    (word(rng) % (2 * range)).wrapping_sub(range)
}

/// Run `cases` oracle cases per base for `range` (translation through
/// `dispatch::call`); `setup` randomises the base and returns the arguments.
fn oracle_cases(
    name: &str,
    bases: &[Machine],
    range: (u32, u32),
    cases: usize,
    value: bool,
    mut setup: impl FnMut(&mut Machine, &mut Rng) -> [u32; 4],
) {
    let mut rng = Rng(range.0 ^ cases as u32);
    let (mut total, mut changed) = (0, 0);
    for (b, base) in bases.iter().enumerate() {
        for case in 0..cases {
            let mut m = base.clone();
            let args = setup(&mut m, &mut rng);
            changed += oracle::diff(
                &format!("{name} base {b} case {case}"),
                &m,
                range,
                args,
                |m| {
                    let v0 = ac3_flight::dispatch::call(m, range.0, args);
                    value.then_some(v0)
                },
            );
            total += 1;
        }
    }
    println!("{name}: {total} oracle cases exact, original changed {changed} bytes");
    assert!(total > 0 && changed > 0, "{name}: vacuous");
}

/// Entity pointer `index` of `[gp+0x2D8]`.
fn entity_pointer(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(0x2d8);
    m.lw(table + index * 4)
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn script_sound_oracle_8002f044() {
    oracle_cases(
        "script sound 8002F044",
        &flight_bases(),
        (0x8002_f044, 0x8002_f0e4),
        20,
        true,
        |m, rng| {
            let source = entity_pointer(m, rng.next() % 16);
            [rng.next() % 0x30, source + 8, rng.next() & 0x1ff, 0]
        },
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn phase_entry_oracle_8004c644_8004d204() {
    let bases = flight_bases();
    oracle_cases(
        "phase entities 8004C644",
        &bases,
        (0x8004_c644, 0x8004_ca68),
        40,
        false,
        |m, rng| {
            let phase = rng.next() % 8;
            m.set_gw(0x324, phase);
            let entry = 0x800b_0100 + phase * 4;
            m.sb(entry, (rng.next() % 9) as u8);
            for index in 0..2 {
                let slot = if rng.next().is_multiple_of(4) {
                    0
                } else {
                    1 + rng.next() % 31
                };
                m.sb(entry + 2 + index, slot as u8);
            }
            m.sb(
                0x800b_aeb4,
                if rng.next().is_multiple_of(2) {
                    7
                } else {
                    (rng.next() % 40) as u8
                },
            );
            let context = m.gw(0x2dc);
            m.sw(context + 0x398, rng.next() % 4);
            for slot in 2..32 {
                let target = entity_pointer(m, slot);
                let flags = (m.lw(target) & !0x4440) | (word(rng) & 0x4440);
                m.sw(target, flags);
                let shared = m.lw(0x800c_4d98 + slot * 4) ^ (word(rng) & 0x40_0000);
                m.sw(0x800c_4d98 + slot * 4, shared);
            }
            [0; 4]
        },
    );
    oracle_cases(
        "phase ground 8004D204",
        &bases,
        (0x8004_d204, 0x8004_d2f8),
        30,
        true,
        |m, rng| {
            let phase = rng.next() % 8;
            m.set_gw(0x324, phase);
            let entry = 0x800b_0100 + phase * 4;
            m.sb(entry, [4u8, 5, 6, 7][rng.next() as usize % 4]);
            let groups = m.gw(0x378);
            let definitions = 0x801c_0000;
            for slot in 0..32 {
                let target = entity_pointer(m, slot + 32);
                let flags = (m.lw(target) & !0x4000) | (word(rng) & 0x4000);
                m.sw(target, flags);
                m.sh(target + 0x4e, signed(rng, 4) as u16);
                let definition = definitions + slot * 0x20;
                m.sb(
                    definition + 0x1d,
                    [0x10u8, 0x11, 0x12, 0][rng.next() as usize % 4],
                );
                m.sw(groups + slot * 0x28 + 8, definition);
            }
            [0; 4]
        },
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn blast_spawners_oracle_800859f8_to_800864a8() {
    let bases = flight_bases();
    let position = |m: &mut Machine, rng: &mut Rng| {
        let player = entity_pointer(m, 0);
        let mut args = [0u32; 4];
        for (axis, arg) in args.iter_mut().take(3).enumerate() {
            *arg = m
                .lw(player + 8 + axis as u32 * 4)
                .wrapping_add(signed(rng, 0x100_0000));
        }
        args
    };
    oracle_cases(
        "shock wave 800859F8",
        &bases,
        (0x8008_59f8, 0x8008_5aac),
        8,
        false,
        |m, rng| {
            let mut args = position(m, rng);
            args[3] = word(rng);
            args
        },
    );
    for (name, start, end) in [
        ("flash and sparks 80085AAC", 0x8008_5aacu32, 0x8008_5c70u32),
        ("debris 80085C70", 0x8008_5c70, 0x8008_5db0),
        ("medium blast 80085DB0", 0x8008_5db0, 0x8008_5e14),
        ("debris 80085EBC", 0x8008_5ebc, 0x8008_5fec),
        ("flash 80085FEC", 0x8008_5fec, 0x8008_608c),
        ("small blast 8008608C", 0x8008_608c, 0x8008_6144),
        ("flashes 80086144", 0x8008_6144, 0x8008_6360),
        ("debris 80086360", 0x8008_6360, 0x8008_64a8),
        ("large blast 800864A8", 0x8008_64a8, 0x8008_650c),
    ] {
        oracle_cases(name, &bases, (start, end), 4, false, position);
    }
}

// ==== phase 4: oracle cases for the effect handler tables and timer events ==================

/// Free RAM for crafted points and event records.
const CRAFT: u32 = 0x801c_0000;

/// A random point near the player at `at` (three words); returns them.
fn craft_point(m: &mut Machine, at: u32, rng: &mut Rng) -> [u32; 3] {
    let player = entity_pointer(m, 0);
    let mut point = [0u32; 3];
    for (axis, value) in point.iter_mut().enumerate() {
        *value = m
            .lw(player + 8 + axis as u32 * 4)
            .wrapping_add(signed(rng, 0x100_0000));
        m.sw(at + axis as u32 * 4, *value);
    }
    point
}

/// Set or clear the live-flight bit (gameplay word 0 bit 1).
fn set_live(m: &mut Machine, rng: &mut Rng) {
    let gameplay = m.gw(0x2d0);
    let flags = m.lw(gameplay) & !2;
    m.sw(gameplay, flags | (rng.next() & 2));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn spark_and_trail_handlers_oracle_8007fd24_to_800823f8() {
    let bases = flight_bases();
    for (name, start, end) in [
        ("spark 8007FD24", 0x8007_fd24u32, 0x8007_ff44u32),
        ("spark 8007FF44", 0x8007_ff44, 0x8008_0118),
        ("spark 80080118", 0x8008_0118, 0x8008_034c),
        ("spark 8008034C", 0x8008_034c, 0x8008_0580),
    ] {
        oracle_cases(name, &bases, (start, end), 10, false, |m, rng| {
            craft_point(m, CRAFT, rng);
            for axis in 0..3u32 {
                m.sw(CRAFT + 0x10 + axis * 4, signed(rng, 0x4_0000));
            }
            [CRAFT, CRAFT + 0x10, 0, 0]
        });
    }
    for (name, start, end) in [
        ("trail 80081F0C", 0x8008_1f0cu32, 0x8008_211cu32),
        ("trail 8008211C", 0x8008_211c, 0x8008_21fc),
        ("trail 800821FC", 0x8008_21fc, 0x8008_23f8),
        ("trail 800823F8", 0x8008_23f8, 0x8008_25f4),
    ] {
        oracle_cases(name, &bases, (start, end), 10, false, |m, rng| {
            craft_point(m, CRAFT, rng);
            [CRAFT, rng.next() % 2, 0, 0]
        });
    }
    for (name, start, end) in [
        ("water ring 8007F44C", 0x8007_f44cu32, 0x8007_f5c0u32),
        ("water splash 8007F5C0", 0x8007_f5c0, 0x8007_f86c),
    ] {
        oracle_cases(name, &bases, (start, end), 6, false, |m, rng| {
            let [x, y, z] = craft_point(m, CRAFT, rng);
            [x, y, z, 0]
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn destruction_effects_oracle_80085e14_to_800883b4() {
    let bases = flight_bases();
    for (name, start, end) in [
        ("blast 80085E14", 0x8008_5e14u32, 0x8008_5ebcu32),
        ("fireball 8008650C", 0x8008_650c, 0x8008_6770),
        ("smoke ring 80086770", 0x8008_6770, 0x8008_69bc),
        ("blast 80086A4C", 0x8008_6a4c, 0x8008_6ae4),
        ("debris burst 80086AE4", 0x8008_6ae4, 0x8008_6ce4),
        ("blast 80086E6C", 0x8008_6e6c, 0x8008_6fd8),
        ("blast 800871F8", 0x8008_71f8, 0x8008_7314),
        ("flames 800873D8", 0x8008_73d8, 0x8008_75f0),
        ("debris 800875F0", 0x8008_75f0, 0x8008_7738),
        ("blast 80087738", 0x8008_7738, 0x8008_784c),
        ("blast 8008784C", 0x8008_784c, 0x8008_7968),
        ("blast 80087968", 0x8008_7968, 0x8008_7a7c),
        ("blast 80087B8C", 0x8008_7b8c, 0x8008_7c08),
        ("blast 80087D18", 0x8008_7d18, 0x8008_7d94),
        ("blast 8008801C", 0x8008_801c, 0x8008_8110),
        ("blast 80088110", 0x8008_8110, 0x8008_8204),
        ("blast 80088204", 0x8008_8204, 0x8008_8294),
        ("blast 80088294", 0x8008_8294, 0x8008_8324),
        ("blast 80088324", 0x8008_8324, 0x8008_83b4),
        ("blast 800883B4", 0x8008_83b4, 0x8008_8444),
    ] {
        oracle_cases(name, &bases, (start, end), 4, false, |m, rng| {
            set_live(m, rng);
            let [x, y, z] = craft_point(m, CRAFT, rng);
            [x, y, z, 0]
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn timer_events_oracle_800869bc_to_80088568() {
    let bases = flight_bases();
    // (name, range, counts that matter, limit)
    type Event = (&'static str, u32, u32, &'static [u32], u32);
    let events: [Event; 8] = [
        (
            "flash event 800869BC",
            0x8008_69bc,
            0x8008_6a4c,
            &[0, 1, 12, 23],
            0x18,
        ),
        (
            "burning event 80086CE4",
            0x8008_6ce4,
            0x8008_6e6c,
            &[0, 4, 5, 36, 0x5a],
            0x5b,
        ),
        (
            "crash event 80086FD8",
            0x8008_6fd8,
            0x8008_71f8,
            &[3, 4, 0x4f, 0x50, 0x55, 0x5f],
            0x61,
        ),
        (
            "collapse event 80087314",
            0x8008_7314,
            0x8008_73d8,
            &[0x78, 0x10],
            0x79,
        ),
        (
            "fire event 80087A7C",
            0x8008_7a7c,
            0x8008_7b8c,
            &[0, 4, 0x18, 0x52, 0x53, 0x5b],
            0x5d,
        ),
        (
            "fire event 80087C08",
            0x8008_7c08,
            0x8008_7d18,
            &[0, 8, 0x1c, 0x52, 0x55, 0x5c],
            0x5d,
        ),
        (
            "smoke trail 800884D0",
            0x8008_84d0,
            0x8008_8568,
            &[0, 8, 0x10, 3],
            0x384,
        ),
        (
            "fire trail 80088568",
            0x8008_8568,
            0x8008_8600,
            &[0, 8, 0x18, 5],
            0x384,
        ),
    ];
    for (name, start, end, counts, limit) in events {
        oracle_cases(name, &bases, (start, end), 16, false, |m, rng| {
            set_live(m, rng);
            let record = CRAFT + 0x100;
            m.sh(record + 2, limit as u16);
            craft_point(m, record + 4, rng);
            let count = if rng.next().is_multiple_of(2) {
                counts[rng.next() as usize % counts.len()]
            } else {
                rng.next() % (limit + 1)
            };
            m.sh(record, count as u16);
            [count, record, 0, 0]
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the ai captures"]
fn text_format_helpers_oracle_80027784_8002779c_800279e0() {
    let bases = flight_bases();
    oracle_cases(
        "put char 80027784",
        &bases,
        (0x8002_7784, 0x8002_779c),
        6,
        true,
        |m, rng| {
            m.sw(CRAFT, CRAFT + 0x100 + rng.next() % 0x40);
            [rng.next() & 0xff, CRAFT, 0, 0]
        },
    );
    oracle_cases(
        "number 8002779C",
        &bases,
        (0x8002_779c, 0x8002_79e0),
        40,
        true,
        |m, rng| {
            let base = [2u32, 8, 16, 10, 3, 7][rng.next() as usize % 6];
            let sp = m.sp;
            m.sw(sp + 0x10, base);
            m.sw(sp + 0x14, CRAFT);
            let digits = if rng.next().is_multiple_of(2) {
                0x8001_0120
            } else {
                0x8001_0134
            };
            m.sw(sp + 0x18, digits);
            // 64-bit values in every base (others than 2/8/16 divide
            // through 8009CF18, `sdk::udivdi3`).
            let wide = rng.next().is_multiple_of(2);
            let flags = (word(rng) & !4) | if wide { 4 } else { 0 };
            let nonzero = u32::from(!rng.next().is_multiple_of(8));
            [word(rng), word(rng), flags, nonzero]
        },
    );
    oracle_cases(
        "field 800279E0",
        &bases,
        (0x8002_79e0, 0x8002_7bd4),
        40,
        true,
        |m, rng| {
            let text = CRAFT + 0x300;
            for offset in 0..16 {
                m.sb(text + offset, (0x41 + rng.next() % 26) as u8);
            }
            m.sw(CRAFT, CRAFT + 0x200);
            let sp = m.sp;
            m.sw(sp + 0x10, rng.next() % 20);
            m.sw(sp + 0x14, rng.next() % 12);
            m.sw(sp + 0x18, rng.next() % 10);
            let flags = word(rng) & 0x7e0;
            let sign = [0u32, 0x2d, 0x2b, 0x20][rng.next() as usize % 4];
            [CRAFT, text, flags, sign]
        },
    );
}
