//! Whole original briefing state checks with an explicit observed renderer boundary.
use ac3_mission::*;
use ac3_port::{Result, assets::GameData, mission_math::NativeBriefingMath};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

fn unhex(v: &Value) -> Result<Vec<u8>> {
    let text = v.as_str().ok_or("expected hex string")?;
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err("invalid hex".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(Into::into))
        .collect()
}
fn uint(v: &Value) -> Result<u32> {
    Ok(u32::try_from(v.as_u64().ok_or("expected unsigned word")?)?)
}
fn differences(actual: &[u8], expected: &[u8]) -> Vec<Value> {
    actual
        .iter()
        .zip(expected)
        .enumerate()
        .filter(|(_, (a, e))| a != e)
        .take(30)
        .map(|(i, (a, e))| json!({"offset":format!("0x{i:x}"),"actual":a,"expected":e}))
        .collect()
}
fn check(case: &Value, script: &BriefingScript, math: &NativeBriefingMath) -> Result<Value> {
    let mut bytes = unhex(&case["input_state_hex"])?;
    let expected = unhex(&case["expected_state_hex"])?;
    if bytes.len() != 0x554 || expected.len() != 0x554 {
        return Err("incorrect wrapper state size".into());
    }
    let base = uint(&case["script_base"])?;
    let mut update = if case["kind"] == "init" {
        initialize_briefing(
            FullBriefingState::from_bytes_for_initialization(&bytes)?,
            script,
            uint(&case["text_count"])? as i32,
            math,
        )?
    } else {
        let offset = unhex(&case["visual_offset2_hex"])?;
        if offset.len() != 4 {
            return Err("invalid visual offsets".into());
        }
        prepare_briefing_tick(
            FullBriefingState::from_bytes(&bytes, base)?,
            script,
            BriefingFrameInput {
                pressed: uint(&case["pressed"])?,
                visual_address: uint(&case["visual_address"])?,
                visual_offset2: [
                    i16::from_le_bytes([offset[0], offset[1]]),
                    i16::from_le_bytes([offset[2], offset[3]]),
                ],
            },
            math,
        )?
    };
    update.state.write_into(&mut bytes, base)?;
    let mut before_diff = vec![];
    let mut rendering_objects = 0;
    if update.render_pending {
        let before = unhex(&case["render_before_hex"])?;
        let after = unhex(&case["render_after_hex"])?;
        if before.len() != 0x554 || after.len() != 0x554 {
            return Err("invalid rendering state size".into());
        }
        before_diff = differences(&bytes, &before);
        let observed = FullBriefingState::from_bytes(&after, base)?;
        let mut outcomes = vec![];
        if update.state.script.flags & 1 != 0 {
            for request in briefing_draw_requests(&update.state.script) {
                let object = observed.script.objects[usize::from(request.index)];
                outcomes.push(BriefingObjectDrawOutcome {
                    index: request.index,
                    flags: object.flags(),
                    appearance_remaining: object.halfword(8).unwrap(),
                    screen: [
                        object.halfword(0x1e).unwrap(),
                        object.halfword(0x20).unwrap(),
                    ],
                });
            }
        }
        rendering_objects = outcomes.len();
        update = finish_briefing_render(update, &outcomes)?;
        update.state.write_into(&mut bytes, base)?;
    } else if case.get("render_before_hex").is_some() {
        return Err("native returned before observed render".into());
    }
    let sounds: Vec<u32> = update
        .effects
        .iter()
        .filter_map(|e| {
            if let BriefingEffect::Common(Effect::PlaySound { id }) = e {
                Some(*id)
            } else {
                None
            }
        })
        .collect();
    let expected_sounds: Vec<u32> = match case["sounds"].as_array() {
        Some(a) => a.iter().map(uint).collect::<Result<_>>()?,
        None if case["sounds"] == json!({}) => vec![],
        _ => return Err("invalid sounds".into()),
    };
    let after_diff = differences(&bytes, &expected);
    let expected_return = uint(&case["callback_result"])?;
    Ok(
        json!({"passed":before_diff.is_empty()&&after_diff.is_empty()&&sounds==expected_sounds&&update.callback_result==expected_return,
        "before_render_differences":before_diff,"final_differences":after_diff,"sounds":sounds,"expected_sounds":expected_sounds,
        "callback_result":update.callback_result,"expected_callback_result":expected_return,"rendering_objects_supplied":rendering_objects}),
    )
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("expected extracted disc directory and capture directory".into());
    }
    let data = GameData::load(Path::new(&args[1]))?;
    let math = NativeBriefingMath::load(&data)?;
    let capture = Path::new(&args[2]);
    let mut scripts = BTreeMap::new();
    let mut failures = vec![];
    let mut count = 0;
    let mut excluded = 0;
    let mut sounds = 0;
    let mut rendering_objects = 0;
    let mut phases = BTreeMap::<String, usize>::new();
    for (index, line) in fs::read_to_string(capture.join("briefing-wrapper-vectors.jsonl"))?
        .lines()
        .enumerate()
    {
        let case: Value = serde_json::from_str(line)?;
        if case.get("visual_address").is_none()
            || (case["kind"] == "init" && case.get("text_count").is_none())
        {
            excluded += 1;
            continue;
        }
        let base = uint(&case["script_base"])?;
        if let std::collections::btree_map::Entry::Vacant(entry) = scripts.entry(base) {
            entry.insert(BriefingScript::parse(&fs::read(
                capture.join(format!("script-{base:08x}.bin")),
            )?)?);
        }
        count += 1;
        let bytes = unhex(&case["input_state_hex"])?;
        let phase = format!(
            "{}:{}/{}",
            case["kind"].as_str().unwrap_or("?"),
            i16::from_le_bytes([bytes[4], bytes[5]]),
            i16::from_le_bytes([bytes[6], bytes[7]])
        );
        *phases.entry(phase).or_default() += 1;
        match check(&case,&scripts[&base],&math){
            Ok(result)=>{sounds+=result["expected_sounds"].as_array().map_or(0,Vec::len);rendering_objects+=result["rendering_objects_supplied"].as_u64().unwrap_or(0);
                if result["passed"]!=true {failures.push(json!({"index":index,"frame":case["frame"],"kind":case["kind"],"details":result}));}}
            Err(error)=>failures.push(json!({"index":index,"frame":case["frame"],"kind":case["kind"],"error":error.to_string()})),
        }
    }
    println!(
        "{}",
        json!({"passed":count>0&&failures.is_empty(),"cases":count,"matched":count-failures.len(),"excluded_legacy_input_cases":excluded,
        "phase_counts":phases,"sound_requests_compared":sounds,"rendering_object_outcomes_supplied":rendering_objects,"failures":failures})
    );
    Ok(())
}
