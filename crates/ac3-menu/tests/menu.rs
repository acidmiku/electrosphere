use ac3_menu::*;

fn context() -> MenuContext {
    MenuContext {
        return_flags: 0x8000_0000,
        progress_flags: 0x4000_0000,
        language_id: 2,
        current_pair: [0x1234, 0xabcd],
        saved_pair: [0x5678, 0xef90],
        mode: 7,
        status: 3,
    }
}

#[test]
fn menu_initialization_obeys_flag_priority_and_preserves_other_fields() {
    let fresh = initialize_menu(MenuContext {
        progress_flags: 0x10,
        return_flags: 6,
        ..context()
    });
    assert_eq!(fresh.state, MenuState::default());
    assert_eq!(fresh.context.progress_flags, 0);
    assert_eq!(fresh.context.return_flags, 1);
    assert_eq!(fresh.context.current_pair, context().current_pair);
    let resume = initialize_menu(MenuContext {
        return_flags: 6,
        ..context()
    });
    assert_eq!(resume.state.state, 0);
    assert_eq!(resume.context.return_flags, 4);
    assert_eq!(resume.context.current_pair, context().saved_pair);
    let alternate = initialize_menu(MenuContext {
        return_flags: 4,
        ..context()
    });
    assert_eq!(alternate.state.state, 8);
    assert_eq!(alternate.context.return_flags, 0);
    assert_eq!(alternate.callback_result, 0x0400_0000);
    assert_eq!(
        alternate.effects.first(),
        Some(&Effect::AllocateMenuBlock { bytes: 0x809c })
    );
}

#[test]
fn callback_results_are_exact_codes_and_halfword_increment_wraps() {
    let state = MenuState {
        state: 3,
        substate: i16::MAX,
        ..MenuState::default()
    };
    let next = apply_menu_callback_result(state, context(), 1, &MenuTables::PAL).unwrap();
    assert_eq!(next.state.substate, i16::MIN);
    for result in [3, 5, 9, 0x110, -1] {
        let next = apply_menu_callback_result(state, context(), result, &MenuTables::PAL).unwrap();
        assert_eq!(next.state, state);
        assert_eq!(next.context, context());
        assert_eq!(next.callback_result, 0);
    }
}

#[test]
fn state_zero_chooses_language_only_if_progress_bit_is_clear() {
    for (flags, expected) in [(0, 1), (0x10, 1), (1, 2), (0x11, 2)] {
        let next = apply_menu_callback_result(
            MenuState {
                substate: 1,
                ..MenuState::default()
            },
            MenuContext {
                progress_flags: flags,
                ..context()
            },
            0x100,
            &MenuTables::PAL,
        )
        .unwrap();
        assert_eq!(next.state.state, expected);
        assert_eq!(next.state.substate, 0);
        assert_eq!(next.state.previous_state, 0);
    }
}

#[test]
fn menu_forward_branches_and_backward_restore_keep_original_rules() {
    for (choice, expected) in [(-1, 3), (0, 4), (1, 5), (2, 6), (3, 3)] {
        let next = apply_menu_callback_result(
            MenuState {
                state: 3,
                substate: 1,
                choice3: choice,
                ..MenuState::default()
            },
            context(),
            0x100,
            &MenuTables::PAL,
        )
        .unwrap();
        assert_eq!(next.state.state, expected);
        assert_eq!(next.state.previous_state, 3);
        assert_eq!(
            next.context.return_flags & 0x10,
            if choice == 2 { 0x10 } else { 0 }
        );
    }
    for (choice, expected) in [(-1, 6), (0, 6), (6, 7), (7, 6)] {
        let next = apply_menu_callback_result(
            MenuState {
                state: 5,
                choice4: choice,
                ..MenuState::default()
            },
            context(),
            0x100,
            &MenuTables::PAL,
        )
        .unwrap();
        assert_eq!(next.state.state, expected);
        assert_eq!(
            next.context.progress_flags & 0x10,
            if choice == 7 { 0x10 } else { 0 }
        );
    }
    let mut tables = MenuTables::PAL;
    tables.backward[8] = 0xfe;
    let next = apply_menu_callback_result(
        MenuState {
            state: 8,
            substate: 1,
            previous_state: 4,
            ..MenuState::default()
        },
        context(),
        0x200,
        &tables,
    )
    .unwrap();
    assert_eq!((next.state.state, next.state.substate), (4, 0));
}

#[test]
fn menu_exit_translates_to_resident_transition_and_saves_pair() {
    let next =
        apply_menu_callback_result(MenuState::default(), context(), 8, &MenuTables::PAL).unwrap();
    assert_eq!(next.callback_result, 0x6000_0005);
    assert_eq!(next.context.saved_pair, context().current_pair);
    assert_eq!(next.context.mode, 4);
    assert_eq!(next.context.status, 0);
    assert_eq!(next.context.return_flags, context().return_flags | 2);
    let next =
        apply_menu_callback_result(MenuState::default(), context(), 4, &MenuTables::PAL).unwrap();
    assert_eq!(next.callback_result, 0x6000_0003);
}

#[test]
fn callback_selection_rejects_data_words_and_negative_indices() {
    assert_eq!(
        select_menu_callback(
            MenuState {
                state: 1,
                substate: 1,
                ..MenuState::default()
            },
            &MenuTables::PAL
        ),
        Ok(0x800d14e0)
    );
    for state in [-1, 9, i16::MAX] {
        assert_eq!(
            select_menu_callback(
                MenuState {
                    state,
                    ..MenuState::default()
                },
                &MenuTables::PAL
            ),
            Err(Error::InvalidMenuState(state))
        );
    }
    for substate in [-1, 2] {
        assert_eq!(
            select_menu_callback(
                MenuState {
                    substate,
                    ..MenuState::default()
                },
                &MenuTables::PAL
            ),
            Err(Error::InvalidMenuSubstate(substate))
        );
    }
    let original = MenuState {
        state: -1,
        substate: 1,
        ..MenuState::default()
    };
    assert_eq!(
        apply_menu_callback_result(original, context(), 0x100, &MenuTables::PAL),
        Err(Error::InvalidMenuState(-1))
    );
    assert_eq!(original.substate, 1);
}

#[test]
fn cleanup_waits_three_calls_and_completes_on_fourth() {
    let (mut state, result, effects) = initialize_exit();
    assert_eq!(result, 1);
    assert_eq!(effects.len(), 2);
    for expected in 1..=3 {
        let (next, ctx, result, effects) = tick_exit(state, context());
        assert_eq!(next.counter, expected);
        assert_eq!(ctx, context());
        assert_eq!(result, 0);
        assert!(effects.is_empty());
        state = next;
    }
    let (_, ctx, result, effects) = tick_exit(state, context());
    assert_eq!(result, 4);
    assert_eq!(ctx.return_flags, context().return_flags | 0x10);
    assert_eq!(effects.len(), 2);
}

#[test]
fn memory_views_preserve_unknown_bytes_and_reject_short_output_atomically() {
    let mut bytes = [0xa5; MenuContext::BYTE_LEN + 1];
    context().write_into(&mut bytes).unwrap();
    assert_eq!(MenuContext::from_bytes(&bytes).unwrap(), context());
    assert_eq!(bytes[0x38b], 0xa5);
    assert_eq!(bytes[0x3ad], 0xa5);
    assert_eq!(bytes[MenuContext::BYTE_LEN], 0xa5);
    let mut short = [0xa5; 16];
    assert!(context().write_into(&mut short).is_err());
    assert_eq!(short, [0xa5; 16]);
    let state = MenuState {
        state: -1,
        substate: i16::MIN,
        previous_state: 4,
        choice3: 2,
        choice4: -3,
        reserved: 8,
    };
    state.write_into(&mut short).unwrap();
    assert_eq!(MenuState::from_bytes(&short).unwrap(), state);
    assert_eq!(&short[12..], &[0xa5; 4]);
}
