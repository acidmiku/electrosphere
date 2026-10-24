use ac3_mission::*;
use std::cell::RefCell;
use std::collections::VecDeque;

#[test]
fn script_state_sparse_roundtrip_preserves_unmodeled_fields_and_checks_address() {
    let mut bytes = [0xa5; 0x554];
    bytes[0xc..0x10].copy_from_slice(&0x80100040_u32.to_le_bytes());
    let state = BriefingScriptState::from_bytes(&bytes, 0x80100000).unwrap();
    assert_eq!(state.cursor, 4);
    let original = bytes;
    state.write_into(&mut bytes, 0x80100000).unwrap();
    assert_eq!(bytes, original);
    assert_eq!(
        state.write_into(&mut bytes, u32::MAX),
        Err(BriefingScriptError::ScriptAddressOverflow)
    );
    assert_eq!(bytes, original);
    assert!(matches!(
        BriefingScriptState::from_bytes(&bytes, 0x80100001),
        Err(BriefingScriptError::InvalidScriptPointer(_))
    ));
}

fn command(time: i16, opcode: u8, slot: u8, arguments: [i16; 6]) -> BriefingCommand {
    BriefingCommand {
        time,
        opcode,
        slot,
        arguments,
    }
}
fn script(commands: &[BriefingCommand]) -> BriefingScript {
    let mut bytes = vec![];
    for command in commands {
        bytes.extend(command.time.to_le_bytes());
        bytes.extend([command.opcode, command.slot]);
        for value in command.arguments {
            bytes.extend(value.to_le_bytes());
        }
    }
    BriefingScript::parse(&bytes).unwrap()
}
struct MathInputs(RefCell<VecDeque<(BriefingMathRequest, i32)>>);
impl BriefingMath for MathInputs {
    fn evaluate(&self, request: BriefingMathRequest) -> Result<i32, BriefingScriptError> {
        let (expected, result) = self
            .0
            .borrow_mut()
            .pop_front()
            .expect("unexpected math call");
        assert_eq!(request, expected);
        Ok(result)
    }
}

#[test]
fn parser_rejects_partial_records_unknown_opcodes_and_cursor_overrun() {
    assert!(matches!(
        BriefingScript::parse(&[0; 17]),
        Err(BriefingScriptError::PartialRecord { .. })
    ));
    let mut bytes = [0; 16];
    bytes[2] = 8;
    assert_eq!(
        BriefingScript::parse(&bytes),
        Err(BriefingScriptError::InvalidOpcode(8))
    );
    let state = BriefingScriptState::default();
    assert_eq!(
        dispatch_briefing_due(
            state,
            &BriefingScript::parse(&[]).unwrap(),
            &MissingBriefingMath
        ),
        Err(BriefingScriptError::CursorOutsideScript(0))
    );
    assert_eq!(BriefingObject::default().halfword(usize::MAX - 1), None);
}

#[test]
fn initialization_prefix_and_same_time_records_preserve_lookahead_semantics() {
    let s = script(&[
        command(-1, 6, 0, [0, 129, 0, 127, 0, 0]),
        command(-1, 1, 2, [3, 130, 15, 125, 90, 0]),
        command(0, 7, 0, [17, 129, 0, 127, 0, 0]),
        command(0, 4, 255, [0; 6]),
        command(1, 0, 0, [0; 6]),
    ]);
    let prefix =
        dispatch_briefing_prefix(BriefingScriptState::default(), &s, &MissingBriefingMath).unwrap();
    assert_eq!(prefix.records_executed, 2);
    assert_eq!(prefix.state.cursor, 2);
    assert_eq!(prefix.state.camera, [128, 0, -128]);
    assert_eq!(prefix.state.objects[2].halfword(8), Some(14));
    assert_eq!(prefix.state.objects[2].halfword(0x10), Some(-1));
    assert_eq!(prefix.state.objects[2].halfword(0x18), Some(1024));
    let due = dispatch_briefing_due(prefix.state, &s, &MissingBriefingMath).unwrap();
    assert_eq!(
        (due.records_executed, due.state.cursor, due.state.clock),
        (2, 4, 0)
    );
    assert_eq!(due.state.flags & 0x20, 0x20);
    assert_eq!(due.state.marker, [128, -300, -128]);
    assert_eq!(due.state.marker_id, 17);
    assert_eq!(
        dispatch_briefing_due(due.state, &s, &MissingBriefingMath)
            .unwrap()
            .records_executed,
        0
    );
}

#[test]
fn object_motion_uses_wide_heading_but_halfword_distance_delta() {
    let mut state = BriefingScriptState::default();
    state.objects[0].0[0xe..0x10].copy_from_slice(&i16::MIN.to_le_bytes());
    let math = MathInputs(RefCell::new(VecDeque::from([
        (BriefingMathRequest::Atan2 { y: 65408, x: 0 }, 1024),
        (BriefingMathRequest::SquareRoot { value: 16384 }, 128),
    ])));
    let update =
        apply_briefing_command(state, command(1, 2, 0, [2, 383, 0, 128, 0, 0]), &math).unwrap();
    assert_eq!(update.state.objects[0].halfword(12), Some(64));
    assert_eq!(update.state.objects[0].halfword(0x18), Some(1024));
    assert!(math.0.borrow().is_empty());
    assert_eq!(
        apply_briefing_command(state, command(1, 2, 0, [0; 6]), &MissingBriefingMath),
        Err(BriefingScriptError::DivisionByZero)
    );
    assert!(matches!(
        apply_briefing_command(
            state,
            command(1, 2, 0, [2, 383, 0, 128, 0, 0]),
            &MissingBriefingMath
        ),
        Err(BriefingScriptError::MissingMath(_))
    ));
}

#[test]
fn camera_motion_keeps_word_deltas_and_divides_before_angle_request() {
    let state = BriefingScriptState {
        camera: [10, 20, 30],
        ..BriefingScriptState::default()
    };
    let math = MathInputs(RefCell::new(VecDeque::from([
        (
            BriefingMathRequest::SquareRoot {
                value: 118 * 118 + 98 * 98,
            },
            150,
        ),
        (BriefingMathRequest::Atan2 { y: 118, x: 98 }, 512),
    ])));
    let update =
        apply_briefing_command(state, command(1, 5, 0, [7, 129, 0, 129, 0, 0]), &math).unwrap();
    assert_eq!(
        (
            update.state.camera_duration,
            update.state.camera_speed,
            update.state.camera_angles[1]
        ),
        (7, 21, 512)
    );
    assert_eq!(update.state.camera, state.camera);
}

#[test]
fn label_append_preserves_unsigned_style_and_animates_previous_labels() {
    let mut state = BriefingScriptState::default();
    for index in 0..3 {
        let update = apply_briefing_command(
            state,
            command(0, 3, index, [255, 0, 0, 0, 0, 0]),
            &MissingBriefingMath,
        )
        .unwrap();
        assert_eq!(
            update.effects[1],
            Effect::SetVisualStyle {
                slot: i32::from(index) + 1,
                style: 255
            }
        );
        state = update.state;
    }
    assert_eq!(
        [
            state.labels[0].flags,
            state.labels[1].flags,
            state.labels[2].flags
        ],
        [31, 15, 7]
    );
    assert_eq!(state.label_count, 3);
    state.flags |= 2;
    let update =
        apply_briefing_command(state, command(0, 3, 3, [0; 6]), &MissingBriefingMath).unwrap();
    assert!(
        !update
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::PlaySound { .. }))
    );
}

#[test]
fn movement_truncates_toward_zero_and_does_not_decrement_appearance_timer() {
    let mut state = BriefingScriptState::default();
    state.objects[0].0[6..8].copy_from_slice(&2_i16.to_le_bytes());
    state.objects[0].0[8..10].copy_from_slice(&2_i16.to_le_bytes());
    state.objects[0].0[12..14].copy_from_slice(&(-1_i16).to_le_bytes());
    state.objects[1].0[6..8].copy_from_slice(&1_i16.to_le_bytes());
    state.objects[1].0[8..10].copy_from_slice(&12_i16.to_le_bytes());
    let math = MathInputs(RefCell::new(VecDeque::from([
        (BriefingMathRequest::Sin { angle: 0 }, 1),
        (BriefingMathRequest::Cos { angle: 0 }, 4096),
    ])));
    let next = tick_briefing_movement(state, &math).unwrap();
    assert_eq!(next.objects[0].halfword(0xe), Some(0));
    assert_eq!(next.objects[0].halfword(0x12), Some(-1));
    assert_eq!(next.objects[0].halfword(8), Some(1));
    assert_eq!(next.objects[1].halfword(8), Some(12));
}

#[test]
fn drawing_outcomes_are_explicit_sparse_and_reject_duplicates_atomically() {
    let mut state = BriefingScriptState::default();
    state.objects[0].0 = [0x55; 36];
    let request = briefing_draw_requests(&state);
    assert_eq!(request.len(), 1);
    assert_eq!(request[0].object, state.objects[0]);
    let outcome = BriefingObjectDrawOutcome {
        index: 0,
        flags: 1,
        appearance_remaining: 7,
        screen: [-1, 320],
    };
    let next = apply_briefing_draw_outcomes(state, &[outcome]).unwrap();
    assert_eq!(next.objects[0].0[0x22..], [0x55, 0x55]);
    assert_eq!(next.objects[0].halfword(8), Some(7));
    assert_eq!(next.objects[0].halfword(0x20), Some(320));
    assert_eq!(
        apply_briefing_draw_outcomes(state, &[outcome, outcome]),
        Err(BriefingScriptError::DuplicateDrawOutcome(0))
    );
}
