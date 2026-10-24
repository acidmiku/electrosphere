use ac3_menu::*;

fn active(selection: i32) -> LanguageState {
    LanguageState {
        phase: 1,
        timer: 0,
        selection,
    }
}
fn context() -> MenuContext {
    MenuContext {
        language_id: 3,
        progress_flags: 0x80,
        return_flags: 0x40,
        ..MenuContext::default()
    }
}

#[test]
fn language_permutation_roundtrips_all_five_ids() {
    for (id, row) in [(0, 0), (1, 2), (2, 1), (3, 4), (4, 3)] {
        let init = initialize_language(MenuContext {
            language_id: id,
            ..context()
        });
        assert_eq!(
            init.state,
            LanguageState {
                phase: 0,
                timer: 25,
                selection: row
            }
        );
        assert_eq!(init.callback_result, 1);
        assert_eq!(init.effects.len(), 6);
        let next = tick_language(active(row), context(), INPUT_CONFIRM).unwrap();
        assert_eq!(next.context.language_id, id);
        assert_eq!(next.state.phase, 2);
        assert_eq!(next.context.progress_flags, context().progress_flags);
    }
}

#[test]
fn intro_and_exit_each_take_twenty_six_calls() {
    let mut update = initialize_language(context());
    for timer in (0..25).rev() {
        update = tick_language(update.state, update.context, INPUT_CONFIRM).unwrap();
        assert_eq!((update.state.phase, update.state.timer), (0, timer));
        assert_eq!(update.callback_result, 0);
    }
    update = tick_language(update.state, update.context, INPUT_CONFIRM).unwrap();
    assert_eq!((update.state.phase, update.state.timer), (1, 0));
    update = tick_language(update.state, update.context, INPUT_CONFIRM).unwrap();
    assert_eq!((update.state.phase, update.state.timer), (2, 0));
    for timer in 1..=25 {
        update = tick_language(update.state, update.context, INPUT_NEXT).unwrap();
        assert_eq!(update.state.timer, timer);
        assert_eq!(update.callback_result, 0);
        assert_eq!(update.context.progress_flags, context().progress_flags);
    }
    update = tick_language(update.state, update.context, 0).unwrap();
    assert_eq!(update.callback_result, 0x100);
    assert_eq!(update.state.phase, 2);
    assert_eq!(update.context.progress_flags, context().progress_flags | 1);
    assert_eq!(
        update.effects.first(),
        Some(&Effect::DrawLanguageFade { shade: 255 })
    );
    assert_eq!(
        update.effects.last(),
        Some(&Effect::SelectVisualPage { page: 0 })
    );
    let menu = apply_menu_callback_result(
        MenuState {
            state: 1,
            substate: 1,
            ..MenuState::default()
        },
        update.context,
        update.callback_result as i32,
        &MenuTables::PAL,
    )
    .unwrap();
    assert_eq!(
        (
            menu.state.state,
            menu.state.substate,
            menu.state.previous_state
        ),
        (2, 0, 1)
    );
}

#[test]
fn confirm_wins_over_both_directions_and_next_wins_over_previous() {
    let next = tick_language(
        active(1),
        context(),
        INPUT_CONFIRM | INPUT_NEXT | INPUT_PREVIOUS,
    )
    .unwrap();
    assert_eq!(next.state.selection, 1);
    assert_eq!(next.state.phase, 2);
    assert_eq!(next.context.language_id, 2);
    assert_eq!(next.effects.last(), Some(&Effect::PlaySound { id: 0x3c }));
    let next = tick_language(active(1), context(), INPUT_NEXT | INPUT_PREVIOUS).unwrap();
    assert_eq!(next.state.selection, 2);
    assert_eq!(next.context.language_id, context().language_id);
    assert_eq!(next.effects.last(), Some(&Effect::PlaySound { id: 0x3d }));
}

#[test]
fn navigation_wraps_and_render_effect_uses_old_selection() {
    let next = tick_language(active(4), context(), INPUT_NEXT).unwrap();
    assert_eq!(next.state.selection, 0);
    let Effect::DrawLanguageRows { rows } = &next.effects[0] else {
        panic!("missing drawing effect")
    };
    assert_eq!((rows[4].style, rows[4].x), (1, 88));
    assert_eq!((rows[0].style, rows[0].x), (2, 80));
    let next = tick_language(active(0), context(), INPUT_PREVIOUS).unwrap();
    assert_eq!(next.state.selection, 4);
}

#[test]
fn unusual_signed_values_follow_original_comparisons_without_panics() {
    for value in [-1, 5, i32::MIN, i32::MAX] {
        let init = initialize_language(MenuContext {
            language_id: value,
            ..context()
        });
        assert_eq!(init.state.selection, value);
        let confirm = tick_language(active(value), context(), INPUT_CONFIRM).unwrap();
        assert_eq!(confirm.context.language_id, context().language_id);
        assert_eq!(confirm.state.phase, 2);
    }
    assert_eq!(
        tick_language(active(i32::MIN), context(), INPUT_NEXT)
            .unwrap()
            .state
            .selection,
        i32::MIN + 1
    );
    assert_eq!(
        tick_language(active(i32::MIN), context(), INPUT_PREVIOUS)
            .unwrap()
            .state
            .selection,
        4
    );
    assert_eq!(
        tick_language(active(i32::MAX), context(), INPUT_NEXT)
            .unwrap()
            .state
            .selection,
        0
    );
    for timer in [i32::MIN, -1, 0, 25, i32::MAX] {
        let state = LanguageState {
            phase: 2,
            timer,
            selection: 0,
        };
        let next = tick_language(state, context(), 0).unwrap();
        assert_eq!(next.state.timer, if timer < 25 { timer + 1 } else { timer });
        assert_eq!(next.callback_result, if timer < 25 { 0 } else { 0x100 });
    }
    assert_eq!(
        tick_language(
            LanguageState {
                phase: -1,
                ..active(0)
            },
            context(),
            0
        ),
        Err(Error::InvalidLanguagePhase(-1))
    );
}

#[test]
fn original_row_trajectory_matches_known_positions() {
    let start = language_rows(0, 25);
    assert_eq!(
        (start[0].visual_x, start[0].x, start[0].y),
        (-400, -312, 120)
    );
    assert_eq!(
        (start[4].visual_x, start[4].x, start[4].y),
        (-576, -496, 356)
    );
    let end = language_rows(4, 0);
    assert_eq!((end[0].x, end[4].x), (80, 88));
    assert_eq!(end.map(|row| row.visual_slot), [1, 2, 3, 4, 5]);
    let _ = language_rows(0, i32::MAX);
}

#[test]
fn language_memory_view_roundtrips_signed_words_and_preserves_tail() {
    let state = LanguageState {
        phase: 2,
        timer: -5,
        selection: i32::MIN,
    };
    let mut bytes = [0xaa; 16];
    state.write_into(&mut bytes).unwrap();
    assert_eq!(LanguageState::from_bytes(&bytes).unwrap(), state);
    assert_eq!(&bytes[12..], &[0xaa; 4]);
    let mut short = [0xaa; 11];
    assert!(state.write_into(&mut short).is_err());
    assert_eq!(short, [0xaa; 11]);
}
