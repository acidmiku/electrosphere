use ac3_menu::*;

fn context() -> MenuContext {
    MenuContext {
        progress_flags: 1,
        current_pair: [8, 9],
        ..MenuContext::default()
    }
}
fn menu() -> MenuState {
    MenuState {
        state: 3,
        substate: 1,
        choice3: 1,
        ..MenuState::default()
    }
}
fn sounds(effects: &[Effect]) -> Vec<u32> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::PlaySound { id } => Some(*id),
            _ => None,
        })
        .collect()
}

#[test]
fn title_ignores_start_during_fade_then_waits_for_start() {
    let mut frame = initialize_title(context());
    assert_eq!(frame.callback_result, 1);
    for timer in 1..=25 {
        frame = tick_title(frame.state, frame.context, INPUT_START).unwrap();
        assert_eq!((frame.state.phase, frame.state.timer), (0, timer));
        assert_eq!(frame.state.idle_timer, 1000);
        assert_eq!(frame.callback_result, 0);
    }
    frame = tick_title(frame.state, frame.context, INPUT_START).unwrap();
    assert_eq!(frame.state.phase, 1);
    assert_eq!(frame.callback_result, 0);
    assert!(frame.effects.contains(&Effect::DrawScreenFade { shade: 0 }));
    frame = tick_title(frame.state, frame.context, INPUT_START | INPUT_CONFIRM).unwrap();
    assert_eq!(frame.callback_result, 0x100);
    assert_eq!(frame.state.idle_timer, 1000);
    assert_eq!(sounds(&frame.effects), vec![0x58]);
    assert_eq!(frame.effects.len(), 1);
}

#[test]
fn title_timeout_starts_after_negative_counter_and_start_has_priority() {
    let frame = tick_title(
        TitleState {
            phase: 1,
            timer: 25,
            idle_timer: 0,
        },
        context(),
        0,
    )
    .unwrap();
    assert_eq!(frame.state.idle_timer, -1);
    assert_eq!(frame.callback_result, 0);
    let timeout = tick_title(frame.state, context(), 0).unwrap();
    assert_eq!(timeout.callback_result, 8);
    assert_eq!(timeout.state.idle_timer, -1);
    assert_eq!(
        timeout.effects,
        vec![Effect::PendingOriginalCall {
            address: 0x800cc6f4,
            arguments: vec![]
        }]
    );
    let start = tick_title(frame.state, context(), INPUT_START).unwrap();
    assert_eq!(start.callback_result, 0x100);
    assert_eq!(sounds(&start.effects), vec![0x58]);
    assert!(
        tick_title(
            TitleState {
                phase: 2,
                ..frame.state
            },
            context(),
            0
        )
        .is_err()
    );
}

#[test]
fn title_icon_styles_read_exact_context_ranges_and_default_zero_to_five() {
    let mut bytes = [0; 0xcfd];
    bytes[0xc61] = 7;
    bytes[0xcc0] = 0xff;
    bytes[0xcfc] = 3;
    let effects = title_progress_effects(&bytes).unwrap();
    assert_eq!(effects.len(), 108);
    assert_eq!(effects[0], Effect::SetVisualStyle { slot: 1, style: 7 });
    assert_eq!(
        effects[1],
        Effect::SetVisualStyle {
            slot: 0x25,
            style: 5
        }
    );
    assert_eq!(
        effects[106],
        Effect::SetVisualStyle {
            slot: 0x48,
            style: 0xff
        }
    );
    assert_eq!(
        effects[107],
        Effect::SetVisualStyle {
            slot: 0x6c,
            style: 3
        }
    );
    assert!(title_progress_effects(&bytes[..0xcfc]).is_err());
}

#[test]
fn pulse_color_uses_original_period_and_truncation() {
    let a = [248, 152, 24];
    let b = [248, 248, 240];
    assert_eq!(pulse_color(0, a, b), b);
    assert_eq!(pulse_color(25, a, b), a);
    assert_eq!(pulse_color(50, a, b), b);
    assert_eq!(pulse_color(12, a, b), [248, 201, 136]);
    let _ = pulse_color(i32::MIN, a, b);
}

#[test]
fn main_menu_init_retains_signed_choice_and_progress_controls_third_item() {
    let frame = initialize_main_menu(menu(), context());
    assert_eq!(
        frame.state,
        MainMenuState {
            selection: 1,
            choice_count: 2
        }
    );
    assert_eq!(frame.callback_result, 1);
    assert!(
        frame
            .effects
            .contains(&Effect::SetTextFont { entry: 14, font: 4 })
    );
    assert_eq!(
        frame.effects.last(),
        Some(&Effect::StartStream { resource: 0x81c })
    );
    let frame = initialize_main_menu(
        MenuState {
            choice3: -1,
            ..menu()
        },
        MenuContext {
            progress_flags: 3,
            ..context()
        },
    );
    assert_eq!(
        frame.state,
        MainMenuState {
            selection: -1,
            choice_count: 3
        }
    );
    assert!(main_menu_styles(-1, 3).is_empty());
}

#[test]
fn main_menu_cancel_beats_confirm_and_navigation_uses_repeat_bits() {
    let state = MainMenuState {
        selection: 0,
        choice_count: 2,
    };
    let cancel = tick_main_menu(
        state,
        menu(),
        context(),
        INPUT_CANCEL | INPUT_CONFIRM,
        INPUT_NEXT,
    );
    assert_eq!(cancel.callback_result, 0x200);
    assert_eq!(cancel.menu.choice3, 1);
    assert_eq!(
        cancel.effects,
        vec![Effect::PlaySound { id: 0x3b }, Effect::StopStream]
    );
    let confirm = tick_main_menu(state, menu(), context(), INPUT_CONFIRM, INPUT_NEXT);
    assert_eq!(confirm.callback_result, 0x100);
    assert_eq!(confirm.menu.choice3, 0);
    let pressed_only = tick_main_menu(state, menu(), context(), INPUT_NEXT, 0);
    assert_eq!(pressed_only.state, state);
    let repeat = tick_main_menu(state, menu(), context(), 0, INPUT_NEXT);
    assert_eq!(repeat.state.selection, 1);
    assert_eq!(sounds(&repeat.effects), vec![0x3d]);
}

#[test]
fn main_menu_clamps_and_up_has_priority_even_at_boundary() {
    let first = MainMenuState {
        selection: 0,
        choice_count: 2,
    };
    let same = tick_main_menu(first, menu(), context(), 0, INPUT_PREVIOUS | INPUT_NEXT);
    assert_eq!(same.state, first);
    assert!(sounds(&same.effects).is_empty());
    let last = MainMenuState {
        selection: 1,
        choice_count: 2,
    };
    let same = tick_main_menu(last, menu(), context(), 0, INPUT_NEXT);
    assert_eq!(same.state, last);
    assert!(sounds(&same.effects).is_empty());
    let up = tick_main_menu(last, menu(), context(), 0, INPUT_PREVIOUS | INPUT_NEXT);
    assert_eq!(up.state.selection, 0);
}

#[test]
fn difficulty_init_preserves_state_and_initial_phase_takes_a_separate_call() {
    let retained = DifficultyState {
        phase: 1,
        selection: 2,
    };
    let init = initialize_difficulty(retained, context());
    assert_eq!(init.state, retained);
    assert_eq!(init.callback_result, 1);
    assert_eq!(
        init.effects.last(),
        Some(&Effect::SetContextByte {
            offset: 0x961,
            value: 6
        })
    );
    let tick = tick_difficulty(DifficultyState::default(), context(), INPUT_CONFIRM).unwrap();
    assert_eq!(
        tick.state,
        DifficultyState {
            phase: 1,
            selection: 0
        }
    );
    assert_eq!(tick.callback_result, 0);
    assert!(tick.effects.is_empty());
}

#[test]
fn difficulty_confirm_beats_cancel_and_next_beats_previous_without_wrap() {
    let state = DifficultyState {
        phase: 1,
        selection: 1,
    };
    let confirm =
        tick_difficulty(state, context(), INPUT_CONFIRM | INPUT_CANCEL | INPUT_NEXT).unwrap();
    assert_eq!(
        confirm.state,
        DifficultyState {
            phase: 2,
            selection: 1
        }
    );
    assert_eq!(confirm.callback_result, 0);
    let cancel = tick_difficulty(state, context(), INPUT_CANCEL).unwrap();
    assert_eq!(cancel.callback_result, 0x200);
    assert_eq!(cancel.state, state);
    let down = tick_difficulty(state, context(), INPUT_PREVIOUS | INPUT_NEXT).unwrap();
    assert_eq!(down.state.selection, 2);
    let same = tick_difficulty(down.state, context(), INPUT_NEXT | INPUT_PREVIOUS).unwrap();
    assert_eq!(same.state.selection, 2);
    assert!(sounds(&same.effects).is_empty());
    let done = tick_difficulty(confirm.state, context(), 0).unwrap();
    assert_eq!(done.callback_result, 0x100);
    assert_eq!(done.context.current_pair, [1, 9]);
    assert_eq!(
        done.effects,
        vec![
            Effect::SetContextWord {
                offset: 0x398,
                value: 1
            },
            Effect::ResetNewGameProgress
        ]
    );
}

#[test]
fn new_game_progress_reset_preserves_unrelated_data_and_requires_full_extent() {
    let mut bytes = [0xa5; 0xf74];
    reset_new_game_progress(&mut bytes).unwrap();
    assert_eq!(&bytes[0xbe8..0xbec], &[0xc8, 0xaf, 0xc8, 0xaf]);
    assert!(bytes[0xd50..0xf70].iter().all(|b| *b == 0));
    assert_eq!(bytes[0x9b9], 1);
    assert_eq!(bytes[0x9b8], 0);
    assert_eq!(bytes[0x9cf], 0);
    for index in [0x390, 0x398, 0x9b7, 0x9d0, 0xbe7, 0xc60, 0xd4f, 0xf70] {
        assert_eq!(bytes[index], 0xa5);
    }
    let mut short = [0xa5; 0xf6f];
    assert!(reset_new_game_progress(&mut short).is_err());
    assert!(short.iter().all(|b| *b == 0xa5));
}

#[test]
fn new_state_memory_views_roundtrip_and_preserve_trailing_bytes() {
    let mut bytes = [0xa5; 16];
    let title = TitleState {
        phase: 1,
        timer: -1,
        idle_timer: i32::MIN,
    };
    title.write_into(&mut bytes).unwrap();
    assert_eq!(TitleState::from_bytes(&bytes).unwrap(), title);
    assert_eq!(&bytes[12..], &[0xa5; 4]);
    let main = MainMenuState {
        selection: -2,
        choice_count: 3,
    };
    main.write_into(&mut bytes).unwrap();
    assert_eq!(MainMenuState::from_bytes(&bytes).unwrap(), main);
    let difficulty = DifficultyState {
        phase: 2,
        selection: i32::MAX,
    };
    difficulty.write_into(&mut bytes).unwrap();
    assert_eq!(DifficultyState::from_bytes(&bytes).unwrap(), difficulty);
}
