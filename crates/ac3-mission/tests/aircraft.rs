use ac3_mission::*;

fn tables() -> AircraftTables {
    AircraftTables {
        aircraft: std::array::from_fn(|index| {
            let mut r = AircraftRecord::default();
            r.0[..2].copy_from_slice(&(index as i16).to_le_bytes());
            r
        }),
        loadouts: std::array::from_fn(|index| {
            let mut r = [0; 16];
            r[..4].copy_from_slice(&(index as u32).to_le_bytes());
            r[4] = 3;
            r[5] = 2;
            r[8..11].copy_from_slice(&[0, 1, 2]);
            r[12..14].copy_from_slice(&[0, 1]);
            r
        }),
        primary_weapons: std::array::from_fn(|index| {
            let mut r = WeaponRecord::default();
            r.0[..4].copy_from_slice(&(index as u32).to_le_bytes());
            r
        }),
        secondary_weapons: std::array::from_fn(|index| {
            let mut r = WeaponRecord::default();
            r.0[..4].copy_from_slice(&(index as u32).to_le_bytes());
            r
        }),
        missions: [[0; 12]; 42],
    }
}
fn sounds(effects: &[Effect]) -> Vec<u32> {
    effects
        .iter()
        .filter_map(|effect| {
            if let Effect::PlaySound { id } = effect {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn aircraft_block_roundtrip_preserves_all_bytes() {
    let original: [u8; AircraftSelectionState::BYTE_LEN] =
        std::array::from_fn(|index| (index.wrapping_mul(167) + 91) as u8);
    assert_eq!(
        AircraftSelectionState::from_bytes(&original)
            .unwrap()
            .to_bytes(),
        original
    );
}

#[test]
fn launch_confirm_changes_local_comparison_and_exit_preserves_timer() {
    let context = MissionContext::default();
    let initialized = initialize_launch(context);
    assert_eq!(initialized.context.options, 4);
    assert_eq!(sounds(&initialized.effects), [0x63]);
    let tick = tick_launch(LaunchState { timer: 50 }, context, 0);
    assert_eq!((tick.state.timer, tick.callback_result), (50, 0x80));
    let tick = tick_launch(LaunchState { timer: 50 }, context, 0x10);
    assert_eq!((tick.state.timer, tick.callback_result), (51, 0));
    assert_eq!(tick.effects.len(), 4);
    let tick = tick_launch(LaunchState { timer: i32::MAX }, context, 0);
    assert_eq!(tick.state.timer, i32::MIN);
    assert_eq!(
        tick_launch(LaunchState { timer: 75 }, context, 0).callback_result,
        0
    );
}

#[test]
fn aircraft_confirm_wins_cancel_but_weapon_cancel_wins_confirm() {
    let context = AircraftContext::default();
    let tables = tables();
    let aircraft = AircraftSelectionState {
        substate: 2,
        flags: 0x1000000,
        ..AircraftSelectionState::default()
    };
    let a = tick_aircraft_leaf(aircraft, context, &tables, 0x3c).unwrap();
    assert_eq!(a.state.flags, 0x1000010);
    assert_eq!(sounds(&a.effects), [0x3c, 0x5b]);
    let w = tick_aircraft_leaf(
        AircraftSelectionState {
            phase: 1,
            ..aircraft
        },
        context,
        &tables,
        0x3c,
    )
    .unwrap();
    assert_eq!(w.state.flags, aircraft.flags);
    assert_eq!(sounds(&w.effects), [0x3b, 0x5b]);
}

#[test]
fn aircraft_navigation_clamps_and_interpolates_metadata_before_stats() {
    let mut t = tables();
    t.aircraft[1].0[2..4].copy_from_slice(&10_i16.to_le_bytes());
    t.aircraft[1].0[32] = 7;
    let state = AircraftSelectionState {
        substate: 2,
        availability: [1; 24],
        ..AircraftSelectionState::default()
    };
    let context = AircraftContext::default();
    let left = tick_aircraft(state, context, &t, 4).unwrap();
    assert_eq!(left.state.selection, 0);
    assert!(sounds(&left.effects).is_empty());
    let right = tick_aircraft(state, context, &t, 12).unwrap();
    assert_eq!(
        (
            right.state.selection,
            right.state.current.id(),
            right.state.interpolation_timer
        ),
        (1, 1, 1)
    );
    assert_eq!(right.state.current.0[2..4], [0, 0]);
    assert_eq!(right.state.current.0[32], 7);
    assert_eq!((right.state.load_timer, right.state.flash_timer), (1, 1));
}

#[test]
fn weapon_navigation_retains_second_sound_and_reverses_running_slide() {
    let state = AircraftSelectionState {
        phase: 1,
        substate: 2,
        weapon_availability: [[1, 1, 1, 0, 0, 0, 0, 0]; 2],
        ..AircraftSelectionState::default()
    };
    let context = AircraftContext::default();
    let t = tables();
    let right = tick_aircraft(state, context, &t, 8).unwrap();
    assert_eq!(right.state.weapon_choice, [1, 0]);
    assert_eq!(sounds(&right.effects), [0x3d, 0x3d]);
    assert_eq!(right.state.weapon_slide_timer[0], 1);
    let left = tick_aircraft(right.state, context, &t, 4).unwrap();
    assert_eq!(left.state.weapon_choice, [0, 0]);
    assert_eq!(left.state.weapon_slide_timer[0], 10);
    assert_eq!(left.state.weapon_flags[0] & 6, 2);
}

#[test]
fn final_weapon_commit_preserves_unmatched_codes_and_special_pairs() {
    let state = AircraftSelectionState {
        phase: 1,
        substate: 4,
        weapon_choice: [6, 2],
        ..AircraftSelectionState::default()
    };
    let mut context = AircraftContext {
        weapon_codes: [99; 3],
        ..AircraftContext::default()
    };
    let tick = tick_aircraft(state, context, &tables(), 0).unwrap();
    assert_eq!(tick.context.weapon_codes, [0, 1, 3]);
    assert_eq!(tick.callback_result, 0x100);
    assert_eq!(tick.effects, [Effect::StopStream]);
    context.weapon_codes = [11, 12, 13];
    assert_eq!(
        tick_aircraft(
            AircraftSelectionState {
                weapon_choice: [-1, 8],
                ..state
            },
            context,
            &tables(),
            0
        )
        .unwrap()
        .context
        .weapon_codes,
        [11, 12, 13]
    );
}

#[test]
fn mission_restriction_and_sparse_initialization_match_original_stores() {
    let mut t = tables();
    t.missions[0][2] = 0x10;
    let mut state = AircraftSelectionState {
        phase: 0,
        substate: 0,
        flags: 0x40000,
        flash_timer: 100,
        weapon_choice: [5, 6],
        ..AircraftSelectionState::default()
    };
    let u = tick_aircraft_leaf(state, AircraftContext::default(), &t, 0).unwrap();
    assert_eq!(u.context.aircraft_id, 18);
    assert_eq!(u.state.availability.iter().sum::<u8>(), 1);
    assert_eq!(u.state.availability[18], 1);
    state.phase = 1;
    state.substate = 4;
    state.timer = 42;
    let init = initialize_aircraft(state, AircraftContext::default());
    assert_eq!(init.state.timer, 42);
    assert_eq!(init.state.flash_timer, 100);
    assert_eq!(init.state.weapon_choice, [5, 6]);
    assert_eq!(
        (init.state.phase, init.state.substate, init.state.flags),
        (0, 0, 0x40000)
    );
}

#[test]
fn interpolation_truncates_signed_overflow_and_preserves_unused_record_bytes() {
    let mut s = AircraftSelectionState {
        flags: 1,
        interpolation_timer: 25,
        ..AircraftSelectionState::default()
    };
    s.previous.0[12..16].copy_from_slice(&i32::MIN.to_le_bytes());
    s.next.0[12..16].copy_from_slice(&i32::MAX.to_le_bytes());
    s.current.0[47] = 83;
    interpolate_aircraft(&mut s);
    assert_eq!(
        i32::from_le_bytes(s.current.0[12..16].try_into().unwrap()),
        i32::MIN
    );
    assert_eq!(s.current.0[47], 83);
}
