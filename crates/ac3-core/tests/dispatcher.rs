use ac3_core::{
    Context, Dispatch, Error, StateEntry, apply_callback_result, boot_callback_result,
    parse_state_table, reset_arena, select_dispatch, transition::*,
};

fn entries() -> Vec<StateEntry> {
    (0..10)
        .map(|index| StateEntry {
            callback_table: 0x800a_0000 + index * 16,
            module_id: index as i32,
            load_address: 0x800c_9dc0,
            arena_limit: 0x800d_0000 + index * 0x1000,
        })
        .collect()
}

fn context() -> Context {
    Context {
        flags: 0x8000_0001,
        arena_current: 0x800c_0000,
        arena_base: 0x800c_1000,
        state: 3,
        substate: 7,
        requested_module: 2,
        loaded_module: 2,
        load_address: 0x800c_9dc0,
        arena_limit: 0x800f_9e18,
    }
}

#[test]
fn result_zero_and_unrecognized_bits_preserve_context() {
    for result in [0, 1, 0x8000_0000, 0x01ff_ffff] {
        assert_eq!(
            apply_callback_result(context(), result, false, &[]),
            Ok(context())
        );
    }
}

#[test]
fn compound_flags_obey_original_priority_and_reset_after_state_change() {
    let table = entries();
    let result = SET_STATE | NEXT_STATE | SET_SUBSTATE | NEXT_SUBSTATE | RESET_ARENA | 4;
    let next = apply_callback_result(context(), result, false, &table).unwrap();
    assert_eq!(next.state, 4);
    assert_eq!(next.substate, 0);
    assert_eq!(next.requested_module, 4);
    assert_eq!(next.loaded_module, 2);
    assert_eq!(next.arena_limit, table[4].arena_limit);
    assert_eq!(next.arena_current, table[4].arena_limit);
    assert_eq!(next.arena_base, table[4].arena_limit);
    assert_eq!(next.flags, context().flags | 8);

    let next = apply_callback_result(
        context(),
        NEXT_STATE | SET_SUBSTATE | NEXT_SUBSTATE | 9,
        false,
        &table,
    )
    .unwrap();
    assert_eq!((next.state, next.substate), (4, 0));
    let next =
        apply_callback_result(context(), SET_SUBSTATE | NEXT_SUBSTATE | 9, false, &[]).unwrap();
    assert_eq!((next.state, next.substate), (3, 9));
}

#[test]
fn loading_complete_precedes_increment_but_is_ignored_on_normal_path() {
    let original = Context {
        requested_module: 5,
        loaded_module: 2,
        substate: 1,
        ..context()
    };
    let next = apply_callback_result(original, LOAD_COMPLETE | NEXT_SUBSTATE, true, &[]).unwrap();
    assert_eq!((next.loaded_module, next.substate), (5, 1));
    let next = apply_callback_result(original, LOAD_COMPLETE, false, &[]).unwrap();
    assert_eq!(next, original);
}

#[test]
fn loading_complete_uses_old_request_before_state_transition() {
    let next = apply_callback_result(
        Context {
            requested_module: 5,
            loaded_module: 2,
            ..context()
        },
        LOAD_COMPLETE | SET_STATE | 4,
        true,
        &entries(),
    )
    .unwrap();
    assert_eq!(next.loaded_module, 5);
    assert_eq!(next.requested_module, 4);
    assert_eq!(next.substate, 0);
}

#[test]
fn negative_selected_module_ids_set_loaded_to_minus_one() {
    for module_id in [-1, -2, i32::MIN] {
        let mut table = entries();
        table[4].module_id = module_id;
        let next = apply_callback_result(context(), SET_STATE | 4, false, &table).unwrap();
        assert_eq!(next.requested_module, module_id);
        assert_eq!(next.loaded_module, -1);
    }
    let original = Context {
        requested_module: -2,
        ..context()
    };
    let next = apply_callback_result(original, LOAD_COMPLETE, true, &[]).unwrap();
    assert_eq!(next.loaded_module, -2);
}

#[test]
fn increments_wrap_like_mips_addiu() {
    let next = apply_callback_result(
        Context {
            substate: u32::MAX,
            ..context()
        },
        NEXT_SUBSTATE,
        false,
        &[],
    )
    .unwrap();
    assert_eq!(next.substate, 0);
    let next = apply_callback_result(
        Context {
            state: u32::MAX,
            ..context()
        },
        NEXT_STATE,
        false,
        &entries(),
    )
    .unwrap();
    assert_eq!(next.state, 0);
    assert_eq!(next.substate, 0);
}

#[test]
fn bad_state_rejected_without_exposing_partially_changed_context() {
    let original = context();
    assert_eq!(
        apply_callback_result(original, SET_STATE | RESET_ARENA | 255, true, &entries()),
        Err(Error::InvalidState(255))
    );
    assert_eq!(original, context());
}

#[test]
fn arena_reset_stores_both_words_without_setting_dispatch_flag() {
    let next = reset_arena(context(), 0x1234_5678);
    assert_eq!(next.arena_current, 0x1234_5678);
    assert_eq!(next.arena_base, 0x1234_5678);
    assert_eq!(next.flags, context().flags);
    assert_eq!(next.state, context().state);
}

#[test]
fn dispatch_selects_normal_and_two_loading_callbacks() {
    let table = entries();
    assert_eq!(
        select_dispatch(&context(), &table),
        Ok(Dispatch::Normal {
            callback_table: table[3].callback_table,
            substate: 7
        })
    );
    let mut original = Context {
        requested_module: 4,
        substate: 0,
        ..context()
    };
    assert_eq!(
        select_dispatch(&original, &[]),
        Ok(Dispatch::LoadBegin {
            module_id: 4,
            load_address: 0x800c_9dc0
        })
    );
    original.substate = 1;
    assert_eq!(
        select_dispatch(&original, &[]),
        Ok(Dispatch::LoadPoll {
            module_id: 4,
            load_address: 0x800c_9dc0
        })
    );
    original.substate = 2;
    assert_eq!(
        select_dispatch(&original, &table),
        Err(Error::InvalidLoadingSubstate(2))
    );
    assert_eq!(
        select_dispatch(&context(), &[]),
        Err(Error::InvalidState(3))
    );
}

#[test]
fn context_roundtrip_preserves_unmodeled_bytes_and_signs() {
    let original = Context {
        requested_module: -1,
        loaded_module: i32::MIN,
        ..context()
    };
    let mut bytes = [0xa5; Context::BYTE_LEN + 4];
    original.write_into(&mut bytes).unwrap();
    assert_eq!(Context::from_bytes(&bytes).unwrap(), original);
    assert!(bytes[4..0x18].iter().all(|value| *value == 0xa5));
    assert!(bytes[0x20..0x2c].iter().all(|value| *value == 0xa5));
    assert!(
        bytes[Context::BYTE_LEN..]
            .iter()
            .all(|value| *value == 0xa5)
    );
    assert_eq!(&bytes[0x34..0x38], &[0xff; 4]);
    let mut short = [0xa5; Context::BYTE_LEN - 1];
    assert!(original.write_into(&mut short).is_err());
    assert_eq!(short, [0xa5; Context::BYTE_LEN - 1]);
    assert!(Context::from_bytes(&short).is_err());
}

#[test]
fn table_parser_reads_little_endian_records_and_rejects_partial_tail() {
    let words: [u32; 8] = [
        0x800a_e540,
        0xffff_ffff,
        0x8001_0000,
        0x800c_9d98,
        0x800d_597c,
        1,
        0x800c_9dc0,
        0x800d_b010,
    ];
    let bytes: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
    let parsed = parse_state_table(&bytes).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].module_id, -1);
    assert_eq!(
        parsed[1],
        StateEntry {
            callback_table: 0x800d_597c,
            module_id: 1,
            load_address: 0x800c_9dc0,
            arena_limit: 0x800d_b010,
        }
    );
    assert_eq!(
        parse_state_table(&bytes[..31]),
        Err(Error::InvalidStateTableLength(31))
    );
    assert_eq!(parse_state_table(&[]).unwrap(), []);
}

#[test]
fn original_boot_callback_selects_state_four_and_resets_arena() {
    let mut table = entries();
    table[4].module_id = 1;
    let original = Context {
        state: 0,
        substate: 0,
        requested_module: -1,
        loaded_module: -1,
        ..Context::default()
    };
    let next = apply_callback_result(original, boot_callback_result(), false, &table).unwrap();
    assert_eq!(next.state, 4);
    assert_eq!(next.requested_module, 1);
    assert_eq!(next.loaded_module, -1);
    assert_eq!(next.arena_current, table[4].arena_limit);
    assert_eq!(next.flags, 8);
    assert_eq!(
        select_dispatch(&next, &table),
        Ok(Dispatch::LoadBegin {
            module_id: 1,
            load_address: 0x800c_9dc0,
        })
    );
}
