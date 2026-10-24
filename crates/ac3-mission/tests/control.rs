use ac3_mission::*;

#[test]
fn dispatcher_uses_exact_result_values_and_halfword_wrapping() {
    let state = MissionState {
        substate: i16::MAX,
        ..MissionState::default()
    };
    let context = MissionContext::default();
    assert_eq!(
        apply_callback_result(state, context, 1)
            .unwrap()
            .state
            .substate,
        i16::MIN
    );
    let update = apply_callback_result(state, context, 0x101).unwrap();
    assert_eq!(update.state, state);
    assert_eq!(update.callback_result, 0);
    assert_eq!(
        select_callback(MissionState::default()).unwrap(),
        0x800d483c
    );
    assert!(select_callback(MissionState { state: -1, ..state }).is_err());
}

#[test]
fn hub_routing_saves_origin_and_briefing_back_respects_bypass() {
    let mission = MissionState {
        choice: 0,
        substate: 1,
        previous: 7,
        ..MissionState::default()
    };
    let context = MissionContext {
        options: 0x80,
        ..MissionContext::default()
    };
    let next = apply_callback_result(mission, context, 0x100).unwrap();
    assert_eq!(
        (next.state.state, next.state.substate, next.state.previous),
        (2, 0, 0)
    );
    assert_eq!(
        apply_callback_result(next.state, context, 0x200)
            .unwrap()
            .state
            .state,
        0
    );
    assert_eq!(
        apply_callback_result(next.state, MissionContext::default(), 0x200)
            .unwrap()
            .state
            .state,
        1
    );
}

#[test]
fn initialization_prioritizes_resume_and_restores_saved_pair_for_load_return() {
    let context = MissionContext {
        resume_byte: 1,
        progress_flags: 0x38,
        options: 8,
        current_pair: [1, 1],
        saved_pair: [32, 2],
        ..MissionContext::default()
    };
    let update = initialize_mission(context);
    assert_eq!(update.state.state, 13);
    assert_eq!(update.context.progress_flags, 0x38);
    assert_eq!(update.context.resume_byte, 0);
    assert_eq!(update.context.options & 0x12, 0x10);
    let update = initialize_mission(MissionContext {
        resume_byte: 0,
        progress_flags: 0x28,
        ..context
    });
    assert_eq!(
        (
            update.state.state,
            update.state.choice,
            update.state.previous
        ),
        (15, 3, 4)
    );
    assert_eq!(update.context.current_pair, [32, 2]);
    assert_eq!(update.context.progress_flags, 8);
    assert_ne!(update.context.options & 0x80, 0);
}

#[test]
fn state_twelve_refreshes_mission_flags_before_routing() {
    let state = MissionState {
        state: 12,
        substate: 1,
        ..MissionState::default()
    };
    let context = MissionContext {
        current_pair: [32, 0],
        ..MissionContext::default()
    };
    let update = apply_callback_result(state, context, 0x100).unwrap();
    assert_eq!(update.callback_result, 0x60000002);
    assert_eq!(update.context.progress_flags, 8);
    assert_eq!(update.state.state, 12);
    let update = apply_callback_result(
        state,
        MissionContext {
            options: 0x18,
            ..context
        },
        0x100,
    )
    .unwrap();
    assert_eq!(update.callback_result, 0);
    assert_eq!(update.state.state, 9); // bit 8 precedes bit 0x10 and refreshed bit 0x80
}

#[test]
fn hub_navigation_wraps_and_confirm_has_priority() {
    let state = HubState {
        phase: 2,
        ..HubState::default()
    };
    let mission = MissionState::default();
    let context = MissionContext::default();
    assert_eq!(
        tick_hub(state, mission, context, 4)
            .unwrap()
            .state
            .selection,
        3
    );
    assert_eq!(
        tick_hub(
            HubState {
                selection: 3,
                ..state
            },
            mission,
            context,
            8
        )
        .unwrap()
        .state
        .selection,
        0
    );
    assert_eq!(
        tick_hub(state, mission, context, 12)
            .unwrap()
            .state
            .selection,
        1
    );
    let update = tick_hub(state, mission, context, 0x1c).unwrap();
    assert_eq!(
        (
            update.state.phase,
            update.state.selection,
            update.mission.choice
        ),
        (3, 0, 0)
    );
    let sounds: Vec<_> = update
        .effects
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::PlaySound { id } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(sounds, [0x3c, 0x5b]);
}

#[test]
fn hub_exit_uses_old_timer_for_map_offset_and_finishes_on_twenty_sixth_call() {
    let mut state = HubState {
        phase: 3,
        flags: 2,
        ..HubState::default()
    };
    for frame in 0..26_i32 {
        let update =
            tick_hub(state, MissionState::default(), MissionContext::default(), 0).unwrap();
        assert_eq!(update.callback_result, if frame == 25 { 0x100 } else { 0 });
        if frame < 25 {
            assert_eq!(update.state.exit_offset, -(frame * frame));
        }
        state = update.state;
    }
    assert_eq!((state.timer, state.exit_offset, state.flags), (25, -576, 0));
}

#[test]
fn hub_intro_and_reveal_can_be_skipped_without_confirming_the_choice() {
    let mission = MissionState::default();
    let context = MissionContext {
        options: 0x43,
        ..MissionContext::default()
    };
    let init = initialize_hub(mission, context);
    let intro = tick_hub(init.state, mission, context, 0x10).unwrap();
    assert_eq!(
        (intro.state.phase, intro.state.timer, intro.state.flags),
        (1, 0, 4)
    );
    assert_eq!(intro.context.options, 3);
    let reveal = tick_hub(intro.state, mission, intro.context, 0).unwrap();
    assert_eq!(
        (
            reveal.state.phase,
            reveal.state.reveal_timer,
            reveal.state.flags
        ),
        (2, 120, 0)
    );
    assert_eq!(reveal.callback_result, 0);
}

#[test]
fn wrappers_have_distinct_priority_and_wrapping() {
    let briefing = apply_briefing_result(
        BriefingState {
            phase: 0,
            substate: i16::MAX,
        },
        3,
        0,
    );
    assert_eq!(
        briefing.state,
        BriefingState {
            phase: 0,
            substate: i16::MIN
        }
    );
    let aircraft = apply_aircraft_result(
        AircraftState {
            phase: 0,
            substate: i32::MAX,
        },
        3,
    );
    assert_eq!(
        aircraft.state,
        AircraftState {
            phase: 0,
            substate: i32::MIN
        }
    );
    assert_eq!(
        apply_briefing_result(BriefingState::default(), 12, 0).callback_result,
        0x100
    );
    let aircraft = apply_aircraft_result(AircraftState::default(), 12);
    assert_eq!((aircraft.state.phase, aircraft.callback_result), (-1, 0));
    assert!(select_aircraft_callback(aircraft.state).is_err());
    assert_eq!(
        apply_briefing_result(BriefingState::default(), 16, 0)
            .state
            .phase,
        3
    );
}

#[test]
fn sparse_writes_are_atomic_on_short_buffers_and_preserve_map_memory() {
    let state = HubState {
        phase: 2,
        flags: 7,
        ..HubState::default()
    };
    let mut short = [0x5a; 32];
    assert!(state.write_into(&mut short).is_err());
    assert_eq!(short, [0x5a; 32]);
    let mut memory = [0x5a; HubState::BYTE_LEN];
    state.write_into(&mut memory).unwrap();
    assert!(memory[0x18..0x11c].iter().all(|byte| *byte == 0x5a));
    assert_eq!(HubState::from_bytes(&memory).unwrap(), state);
}
