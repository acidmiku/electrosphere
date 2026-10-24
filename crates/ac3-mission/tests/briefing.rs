use ac3_mission::*;

fn script(commands: &[(i16, u8, u8, [i16; 6])]) -> BriefingScript {
    let mut bytes = vec![];
    for (time, opcode, slot, args) in commands {
        bytes.extend(time.to_le_bytes());
        bytes.extend([*opcode, *slot]);
        for value in args {
            bytes.extend(value.to_le_bytes());
        }
    }
    BriefingScript::parse(&bytes).unwrap()
}
fn idle_script() -> BriefingScript {
    script(&[(30000, 0, 0, [0; 6])])
}
fn tick(state: FullBriefingState, pressed: u32) -> BriefingUpdate {
    prepare_briefing_tick(
        state,
        &idle_script(),
        BriefingFrameInput {
            pressed,
            ..Default::default()
        },
        &MissingBriefingMath,
    )
    .unwrap()
}
fn sounds(update: &BriefingUpdate) -> Vec<u32> {
    update
        .effects
        .iter()
        .filter_map(|e| match e {
            BriefingEffect::Common(Effect::PlaySound { id }) => Some(*id),
            _ => None,
        })
        .collect()
}

#[test]
fn full_bridge_preserves_padding_and_initialization_does_not_reset_view_or_finish_timer() {
    let mut bytes = [0xa5; 0x554];
    bytes[0xc..0x10].copy_from_slice(&0x80100000u32.to_le_bytes());
    let state = FullBriefingState::from_bytes(&bytes, 0x80100000).unwrap();
    let before = bytes;
    state.write_into(&mut bytes, 0x80100000).unwrap();
    assert_eq!(bytes, before);
    let init = initialize_briefing(state, &idle_script(), 4, &MissingBriefingMath).unwrap();
    assert_eq!(init.state.script.finish_timer, state.script.finish_timer);
    assert_eq!(init.state.zoom, state.zoom);
    assert_eq!(init.state.visual_address, state.visual_address);
    assert_eq!(init.state.panel_offsets, [625, -625]);
    assert_eq!(
        (
            init.state.text_timer,
            init.state.text_index,
            init.state.text_count
        ),
        (0, 0, 4)
    );
    assert_eq!(init.callback_result, 1);
    assert!(!init.render_pending);
    assert_eq!(
        init.state.script.objects[0].halfword(4),
        Some(0xa5a5u16 as i16)
    );
}

#[test]
fn text_uses_old_timer_and_separate_three_line_gap() {
    let mut state = FullBriefingState {
        control: BriefingState {
            phase: 1,
            substate: 0,
        },
        text_count: 4,
        text_timer: 94,
        ..Default::default()
    };
    state = tick(state, 0).state;
    assert_eq!((state.text_index, state.text_timer), (0, 95));
    state = tick(state, 0).state;
    assert_eq!((state.text_index, state.text_timer), (1, 0));
    state = tick(state, 0x10).state;
    assert_eq!((state.text_index, state.text_timer), (2, 0));
    state = tick(state, 0x10).state;
    assert_eq!(state.script.flags & 8, 8);
    assert_eq!(state.text_index, 2);
    state.text_timer = 14;
    state = tick(state, 0).state;
    assert_eq!((state.text_index, state.text_timer), (2, 15));
    state = tick(state, 0).state;
    assert_eq!((state.text_index, state.text_timer), (3, 0));
    assert_eq!(state.script.flags & 8, 0);
    state = tick(state, 0x10).state;
    assert_eq!(state.script.flags & 0x10, 0x10);
}

#[test]
fn cancel_runs_due_commands_first_and_wins_start_confirm() {
    let state = FullBriefingState {
        control: BriefingState {
            phase: 1,
            substate: 0,
        },
        ..Default::default()
    };
    let script = script(&[
        (0, 3, 0, [1, 0, 0, 0, 0, 0]),
        (100, 4, 0, [0; 6]),
        (101, 0, 0, [0; 6]),
    ]);
    let update = prepare_briefing_tick(
        state,
        &script,
        BriefingFrameInput {
            pressed: 0x70,
            ..Default::default()
        },
        &MissingBriefingMath,
    )
    .unwrap();
    assert_eq!(
        update.state.control,
        BriefingState {
            phase: 3,
            substate: 0
        }
    );
    assert_eq!(update.state.script.flags & 6, 4);
    assert_eq!(update.state.script.label_count, 1);
    assert_eq!(update.state.script.cursor, 1);
    assert_eq!(sounds(&update), [100]);
    assert!(
        !update
            .effects
            .iter()
            .any(|e| matches!(e, BriefingEffect::DrawLabelConnectors { .. }))
    );
}

#[test]
fn fast_forward_preserves_global_clock_and_does_not_set_normal_completion_bit() {
    let state = FullBriefingState {
        control: BriefingState {
            phase: 1,
            substate: 0,
        },
        ..Default::default()
    };
    let script = script(&[(10, 3, 0, [1, 0, 0, 0, 0, 0]), (11, 0, 0, [0; 6])]);
    let update = prepare_briefing_tick(
        state,
        &script,
        BriefingFrameInput {
            pressed: 0x40,
            ..Default::default()
        },
        &MissingBriefingMath,
    )
    .unwrap();
    assert_eq!(
        update.state.control,
        BriefingState {
            phase: 1,
            substate: 1
        }
    );
    assert_eq!(update.state.script.clock, 0);
    assert_eq!(update.state.script.flags & 0x22, 2);
    assert_eq!(update.state.script.cursor, 1);
    assert_eq!(update.state.script.labels[0].timer, 1);
    assert_eq!(sounds(&update), [60]);
}

#[test]
fn label_fade_priority_old_timer_parameter_and_signed_offset_are_preserved() {
    let mut state = BriefingScriptState::default();
    state.labels[0] = BriefingLabel {
        flags: 0x77,
        timer: 1,
        x: -5,
        y: 7,
        ..Default::default()
    };
    let update = tick_briefing_labels(state, [2, -3]);
    assert_eq!(update.state.labels[0].timer, 0);
    assert_eq!(update.state.labels[0].flags, 0x77);
    assert_eq!(
        update.effects,
        [
            Effect::SetVisualParameter { slot: 1, value: 1 },
            Effect::SetVisualOffset {
                slot: 4,
                x: -3,
                y: 4
            }
        ]
    );
    let update = tick_briefing_labels(update.state, [2, -3]);
    assert_eq!(update.state.labels[0].flags, 0x55);
    assert_eq!(update.effects.len(), 1);
    state.labels[0] = BriefingLabel {
        flags: 0x13,
        timer: 25,
        y: 557,
        ..Default::default()
    };
    let update = tick_briefing_labels(state, [0; 2]);
    assert_eq!(update.state.labels[0].flags, 1);
    assert_eq!(update.state.labels[0].y, 557);
    assert_eq!(
        update.effects[0],
        Effect::SetVisualParameter { slot: 1, value: 25 }
    );
}

#[test]
fn navigation_wraps_right_first_and_reverses_an_existing_fade() {
    let mut state = BriefingScriptState {
        label_count: 2,
        selected_label: 1,
        ..Default::default()
    };
    state.labels[0] = BriefingLabel {
        flags: 0x21,
        timer: 9,
        ..Default::default()
    };
    state.labels[1] = BriefingLabel {
        flags: 7,
        timer: 5,
        ..Default::default()
    };
    let update = navigate_briefing_label(state, 12).unwrap();
    assert_eq!(update.state.selected_label, 0);
    assert_eq!(
        (update.state.labels[0].flags, update.state.labels[0].timer),
        (5, 9)
    );
    assert_eq!(
        (update.state.labels[1].flags, update.state.labels[1].timer),
        (0x23, 5)
    );
    assert_eq!(update.effects, [Effect::PlaySound { id: 0x65 }]);
}

#[test]
fn rendering_requires_exact_active_object_set_and_preserves_control() {
    let mut state = FullBriefingState::default();
    state.script.flags = 1;
    state.script.objects[1].0[..4].copy_from_slice(&1u32.to_le_bytes());
    let update = BriefingUpdate {
        state,
        callback_result: 0,
        effects: vec![],
        render_pending: true,
    };
    assert_eq!(
        finish_briefing_render(update.clone(), &[]),
        Err(BriefingScriptError::MissingDrawOutcome(1))
    );
    let outcome = BriefingObjectDrawOutcome {
        index: 1,
        flags: 3,
        appearance_remaining: 9,
        screen: [-1, -1],
    };
    let done = finish_briefing_render(update, &[outcome]).unwrap();
    assert!(!done.render_pending);
    assert_eq!(done.state.control, state.control);
    assert_eq!(done.state.script.objects[1].flags(), 3);
    assert_eq!(
        finish_briefing_render(done, &[outcome]),
        Err(BriefingScriptError::UnexpectedDrawOutcome(1))
    );
}

#[test]
fn both_exit_leaf_tables_use_same_timer_and_route_cancel_after_zero() {
    for phase in [2, 3] {
        let substate = if phase == 2 { 2 } else { 1 };
        let mut state = FullBriefingState {
            control: BriefingState { phase, substate },
            panel_timer: 1,
            ..Default::default()
        };
        let update = tick(state, 0);
        assert_eq!(update.callback_result, 0);
        assert_eq!(update.state.panel_offsets, [576, -576]);
        assert_eq!(update.state.panel_timer, 0);
        state = update.state;
        assert_eq!(tick(state, 0).callback_result, 0x100);
        state.script.flags |= 4;
        assert_eq!(tick(state, 0).callback_result, 0x200);
    }
}
