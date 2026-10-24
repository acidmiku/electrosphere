//! Compare pure menu logic with natural original-game entry/return captures.
//! This validates logical fields, return values and sound request IDs. It does
//! not compare rendered pixels, playback, pad sampling or elapsed frame timing.
use crate::{Result, sha256};
use ac3_menu::{
    DifficultyState, Effect, LanguageState, MainMenuState, MenuContext, MenuState, TitleState,
    initialize_difficulty, initialize_language, initialize_main_menu, initialize_title,
    tick_difficulty, tick_language, tick_main_menu, tick_title,
};
use serde_json::{Value, json};
use std::{fs, path::Path};

const PAL_DISC_SHA256: &str = "5d59ef5bf2b905cde40cf8a14647b409228a4443eb94e9a0444351544ca87276";

pub fn verify_language_trace(path: &Path) -> Result<Value> {
    let (trace, provenance) = read_capture(path)?;
    let mut report = compare_trace(&trace)?;
    append_provenance(&mut report, provenance);
    Ok(report)
}

/// Validate title/main/difficulty callback fields and sound IDs from natural runs.
pub fn verify_front_end_trace(path: &Path) -> Result<Value> {
    let (trace, provenance) = read_capture(path)?;
    let mut report = compare_front_end_trace(&trace)?;
    append_provenance(&mut report, provenance);
    Ok(report)
}

fn read_capture(path: &Path) -> Result<(Value, Value)> {
    let raw = fs::read(path)?;
    let trace: Value = serde_json::from_slice(&raw)?;
    let directory = path.parent().ok_or("menu trace needs a parent directory")?;
    let metadata_raw = fs::read(directory.join("metadata.json"))?;
    let metadata: Value = serde_json::from_slice(&metadata_raw)?;
    let engine = fs::read(directory.join("capture-engine.lua"))?;
    if metadata["original_disc_sha256"] != PAL_DISC_SHA256
        || metadata["guest_code_patched"] != false
        || metadata["guest_ram_patched"] != false
        || metadata["engine_sha256"] != sha256(&engine)
    {
        return Err("menu capture provenance does not identify the unpatched PAL game and saved capture engine".into());
    }
    let mut report = json!({});
    report["trace"] = json!(path);
    report["trace_sha256"] = json!(sha256(&raw));
    report["capture_metadata_sha256"] = json!(sha256(&metadata_raw));
    report["capture_engine_sha256"] = json!(sha256(&engine));
    report["declared_disc_sha256"] = metadata["original_disc_sha256"].clone();
    report["checkpoint_sha256"] = metadata["checkpoint_sha256"].clone();
    report["emulator"] = metadata["emulator"].clone();
    Ok((trace, report))
}

fn append_provenance(report: &mut Value, provenance: Value) {
    if let (Some(target), Value::Object(source)) = (report.as_object_mut(), provenance) {
        target.extend(source);
    }
}

fn compare_front_end_trace(trace: &Value) -> Result<Value> {
    let metadata = &trace["metadata"];
    if metadata["oracle"] != "normal original game execution"
        || metadata["code_patched"] != false
        || metadata["ram_patched"] != false
    {
        return Err("front-end trace must observe unpatched normal original game execution".into());
    }
    let cases = trace["cases"]
        .as_array()
        .ok_or("front-end trace needs a cases array")?;
    if cases.is_empty() {
        return Err("front-end trace has no cases".into());
    }
    let mut failures = vec![];
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut sound_requests = 0;
    for (index, case) in cases.iter().enumerate() {
        let input = &case["input"];
        let expected = &case["expected"];
        let kind = case["kind"].as_str().ok_or("missing front-end case kind")?;
        let frame = u32_field(case, "frame")?;
        let context = MenuContext {
            language_id: i32_field(input, "language_id")?,
            progress_flags: u32_field(input, "progress_flags")?,
            ..MenuContext::default()
        };
        let mut expected_fields = json!({"language_id":i32_field(expected,"language_id")?,
            "progress_flags":u32_field(expected,"progress_flags")?});
        let (actual_fields, callback_result, effects) = match kind {
            "title_init" | "title_tick" => {
                for field in ["title_phase", "title_timer", "title_idle_timer"] {
                    expected_fields[field] = json!(i32_field(expected, field)?);
                }
                let update = if kind == "title_init" {
                    initialize_title(context)
                } else {
                    tick_title(
                        TitleState {
                            phase: i32_field(input, "title_phase")?,
                            timer: i32_field(input, "title_timer")?,
                            idle_timer: i32_field(input, "title_idle_timer")?,
                        },
                        context,
                        u32_field(input, "input_pressed")?,
                    )?
                };
                (
                    json!({"language_id":update.context.language_id,"progress_flags":update.context.progress_flags,
                    "title_phase":update.state.phase,"title_timer":update.state.timer,"title_idle_timer":update.state.idle_timer}),
                    update.callback_result,
                    update.effects,
                )
            }
            "main_init" | "main_tick" => {
                for field in ["main_selection", "main_choice_count"] {
                    expected_fields[field] = json!(i32_field(expected, field)?);
                }
                expected_fields["menu_choice"] = json!(halfword_field(expected, "menu_choice")?);
                let menu = MenuState {
                    choice3: halfword_field(input, "menu_choice")? as i16,
                    ..MenuState::default()
                };
                let update = if kind == "main_init" {
                    initialize_main_menu(menu, context)
                } else {
                    tick_main_menu(
                        MainMenuState {
                            selection: i32_field(input, "main_selection")?,
                            choice_count: i32_field(input, "main_choice_count")?,
                        },
                        menu,
                        context,
                        u32_field(input, "input_pressed")?,
                        u32_field(input, "input_repeat")?,
                    )
                };
                (
                    json!({"language_id":update.context.language_id,"progress_flags":update.context.progress_flags,
                    "main_selection":update.state.selection,"main_choice_count":update.state.choice_count,
                    "menu_choice":update.menu.choice3 as u16}),
                    update.callback_result,
                    update.effects,
                )
            }
            "difficulty_init" | "difficulty_tick" => {
                for field in ["difficulty_phase", "difficulty_selection"] {
                    expected_fields[field] = json!(i32_field(expected, field)?);
                }
                for field in ["current_pair0", "current_pair1"] {
                    expected_fields[field] = json!(halfword_field(expected, field)?);
                }
                expected_fields["context_difficulty"] =
                    json!(u32_field(expected, "context_difficulty")?);
                let context = MenuContext {
                    current_pair: [
                        halfword_field(input, "current_pair0")?,
                        halfword_field(input, "current_pair1")?,
                    ],
                    ..context
                };
                let state = DifficultyState {
                    phase: i32_field(input, "difficulty_phase")?,
                    selection: i32_field(input, "difficulty_selection")?,
                };
                let update = if kind == "difficulty_init" {
                    initialize_difficulty(state, context)
                } else {
                    tick_difficulty(state, context, u32_field(input, "input_pressed")?)?
                };
                // The pure callback emits the +0x398 store for the host. Apply
                // just that observed effect; the trace has no progress-region
                // memory snapshot, so ResetNewGameProgress is not validated.
                let mut context_difficulty = u32_field(input, "context_difficulty")?;
                for effect in &update.effects {
                    if let Effect::SetContextWord {
                        offset: 0x398,
                        value,
                    } = effect
                    {
                        context_difficulty = *value;
                    }
                }
                (
                    json!({"language_id":update.context.language_id,"progress_flags":update.context.progress_flags,
                    "difficulty_phase":update.state.phase,"difficulty_selection":update.state.selection,
                    "current_pair0":update.context.current_pair[0],"current_pair1":update.context.current_pair[1],
                    "context_difficulty":context_difficulty}),
                    update.callback_result,
                    update.effects,
                )
            }
            _ => return Err(format!("unsupported front-end case kind {kind}").into()),
        };
        *counts.entry(kind.to_owned()).or_default() += 1;
        let expected_result = u32_field(case, "callback_result")?;
        let expected_sounds = sounds(&case["sounds"])?;
        sound_requests += expected_sounds.len();
        let actual_sounds: Vec<u32> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::PlaySound { id } => Some(*id),
                _ => None,
            })
            .collect();
        if actual_fields != expected_fields
            || callback_result != expected_result
            || actual_sounds != expected_sounds
        {
            failures.push(json!({"index":index,"kind":kind,"frame":frame,"expected":expected_fields,
                "actual":actual_fields,"expected_result":expected_result,"actual_result":callback_result,
                "expected_sounds":expected_sounds,"actual_sounds":actual_sounds}));
        }
    }
    Ok(
        json!({"passed":failures.is_empty(),"cases":cases.len(),"matched":cases.len()-failures.len(),
        "case_kinds":counts,"sound_requests":sound_requests,
        "compared_fields":{"shared":["language_id","progress_flags"],
            "title":["title_phase","title_timer","title_idle_timer"],
            "main_menu":["main_selection","main_choice_count","menu_choice"],
            "difficulty":["difficulty_phase","difficulty_selection","current_pair0","current_pair1","context_difficulty"]},
        "compared_outputs":["callback_result","sound_request_a0"],
        "source_functions":["0x800D18B0","0x800D1900","0x800D1578","0x800D1794","0x800D1B58","0x800D1C1C","0x800D1EF4","0x800D1F68","0x800D1D78","0x800D1D80","0x800D1EAC"],
        "scope":"Natural title/main/difficulty logical fields, callback results and sound request IDs, including difficulty context +0x398 effect; progress-memory reset, rendering, other text/resource effects, playback, sampling and timing are excluded.",
        "failures":failures}),
    )
}

fn halfword_field(value: &Value, field: &str) -> Result<u16> {
    u16::try_from(u32_field(value, field)?)
        .map_err(|_| format!("invalid u16 menu trace field {field}").into())
}

fn compare_trace(trace: &Value) -> Result<Value> {
    let metadata = &trace["metadata"];
    if metadata["oracle"] != "normal original game execution"
        || metadata["code_patched"] != false
        || metadata["ram_patched"] != false
        || u32_field(metadata, "wrapper_entry")? != 0x800d14e0
        || u32_field(metadata, "wrapper_return")? != 0x800d1568
        || u32_field(metadata, "context_input_offset")? != 0x12c
    {
        return Err("language trace is not the declared original entry/return observation".into());
    }
    let cases = trace["cases"]
        .as_array()
        .ok_or("language trace needs a cases array")?;
    let initializations = trace["initializations"]
        .as_array()
        .ok_or("language trace needs an initializations array")?;
    if cases.is_empty() {
        return Err("language trace must contain at least one update observation".into());
    }
    let mut failures = Vec::new();
    let mut phase_counts = [0_usize; 3];
    let mut legacy_empty_sounds = 0;
    let mut sound_requests = 0;
    let mut last_frame = None;
    for (index, case) in cases.iter().enumerate() {
        let (state, context) = logical(&case["input"])?;
        let (expected_state, expected_context) = logical(&case["expected"])?;
        let frame = u32_field(case, "frame")?;
        if last_frame.is_some_and(|last| frame <= last) {
            return Err("language trace update frames must increase strictly".into());
        }
        last_frame = Some(frame);
        let expected_result = u32_field(case, "callback_result")?;
        let update = tick_language(state, context, u32_field(case, "input_bits")?)?;
        phase_counts[state.phase as usize] += 1;
        let expected_sounds = sounds(&case["sounds"])?;
        if case["sounds"].is_object() {
            legacy_empty_sounds += 1;
        }
        sound_requests += expected_sounds.len();
        let actual_sounds: Vec<u32> = update
            .effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::PlaySound { id } => Some(*id),
                _ => None,
            })
            .collect();
        if update.state != expected_state
            || update.context.language_id != expected_context.language_id
            || update.context.progress_flags != expected_context.progress_flags
            || update.callback_result != expected_result
            || actual_sounds != expected_sounds
        {
            failures.push(json!({"kind":"update","index":index,"frame":frame,
                "expected":logical_json(expected_state, expected_context),
                "actual":logical_json(update.state, update.context),
                "expected_result":expected_result,"actual_result":update.callback_result,
                "expected_sounds":expected_sounds,"actual_sounds":actual_sounds}));
        }
    }
    for (index, case) in initializations.iter().enumerate() {
        let (_, context) = logical(&case["input"])?;
        let (expected_state, expected_context) = logical(&case["expected"])?;
        let expected_result = u32_field(case, "callback_result")?;
        let update = initialize_language(context);
        if update.state != expected_state
            || update.context.language_id != expected_context.language_id
            || update.context.progress_flags != expected_context.progress_flags
            || update.callback_result != expected_result
        {
            failures.push(json!({"kind":"initialization","index":index,
                "expected":logical_json(expected_state, expected_context),
                "actual":logical_json(update.state, update.context),
                "expected_result":expected_result,"actual_result":update.callback_result}));
        }
    }
    let count = cases.len() + initializations.len();
    Ok(
        json!({"passed":failures.is_empty(),"cases":count,"matched":count-failures.len(),
        "update_cases":cases.len(),"initialization_cases":initializations.len(),
        "phase_cases":{"intro":phase_counts[0],"input":phase_counts[1],"exit":phase_counts[2]},
        "sound_requests":sound_requests,"legacy_empty_sound_objects":legacy_empty_sounds,
        "compared_fields":["phase","timer","selection","language_id","progress_flags"],
        "compared_outputs":["callback_result","sound_request_a0"],
        "source_functions":["0x800D13E4","0x800D14E0","0x800D0FA4","0x800D103C","0x800D11B4"],
        "scope":"Natural language initialization/update logical fields, callback results and sound request IDs; drawing, sound playback, pad sampling and timing are excluded.",
        "failures":failures}),
    )
}

fn u32_field(value: &Value, field: &str) -> Result<u32> {
    value[field]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("missing/invalid language trace u32 field {field}").into())
}
fn i32_field(value: &Value, field: &str) -> Result<i32> {
    value[field]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| format!("missing/invalid language trace i32 field {field}").into())
}
fn logical(value: &Value) -> Result<(LanguageState, MenuContext)> {
    Ok((
        LanguageState {
            phase: i32_field(value, "phase")?,
            timer: i32_field(value, "timer")?,
            selection: i32_field(value, "selection")?,
        },
        MenuContext {
            language_id: i32_field(value, "language_id")?,
            progress_flags: u32_field(value, "progress_flags")?,
            ..MenuContext::default()
        },
    ))
}
fn logical_json(state: LanguageState, context: MenuContext) -> Value {
    json!({"phase":state.phase,"timer":state.timer,"selection":state.selection,
        "language_id":context.language_id,"progress_flags":context.progress_flags})
}
fn sounds(value: &Value) -> Result<Vec<u32>> {
    // The first capture engine encoded empty Lua tables as {}. Accept only an
    // actually empty object as that legacy encoding; reject arbitrary objects.
    if value.as_object().is_some_and(|object| object.is_empty()) {
        return Ok(vec![]);
    }
    value
        .as_array()
        .ok_or("language sounds must be an array (or legacy empty object)")?
        .iter()
        .map(|sound| u32_field(sound, "a0"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sound_trace_accepts_only_arrays_or_legacy_empty_objects() {
        assert_eq!(sounds(&json!({})).unwrap(), Vec::<u32>::new());
        assert_eq!(sounds(&json!([])).unwrap(), Vec::<u32>::new());
        assert_eq!(sounds(&json!([{"a0":61}])).unwrap(), vec![61]);
        assert!(sounds(&json!({"a0":61})).is_err());
        assert!(sounds(&json!([{"a0":4294967296_u64}])).is_err());
        assert!(sounds(&Value::Null).is_err());
    }
    #[test]
    fn malformed_signed_and_unsigned_fields_are_rejected() {
        assert!(logical(&json!({})).is_err());
        assert!(i32_field(&json!({"x":2147483648_u64}), "x").is_err());
        assert!(u32_field(&json!({"x":-1}), "x").is_err());
    }
}
