//! Differential checks use separate original-game captures, never generated
//! expected outputs from the Rust implementation being tested.
use crate::{
    Result,
    assets::{FIRST_CALLBACKS, GameData, OVERLAY_BASE},
    bootstrap::Bootstrap,
    sha256,
};
use ac3_core::{Context, StateEntry, apply_callback_result};
use serde_json::{Value, json};
use std::{fs, path::Path};

pub fn context_json(c: Context) -> Value {
    json!({"flags":c.flags,"arena_current":c.arena_current,"arena_base":c.arena_base,
        "state":c.state,"substate":c.substate,"requested_module":c.requested_module,
        "loaded_module":c.loaded_module,"load_address":c.load_address,"arena_limit":c.arena_limit})
}

fn unsigned(v: &Value, key: &str) -> Result<u32> {
    v[key]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("missing/invalid u32 oracle field {key}").into())
}
fn signed(v: &Value, key: &str) -> Result<i32> {
    v[key]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| format!("missing/invalid i32 oracle field {key}").into())
}
fn context(v: &Value) -> Result<Context> {
    Ok(Context {
        flags: unsigned(v, "flags")?,
        arena_current: unsigned(v, "arena_current")?,
        arena_base: unsigned(v, "arena_base")?,
        state: unsigned(v, "state")?,
        substate: unsigned(v, "substate")?,
        requested_module: signed(v, "requested_module")?,
        loaded_module: signed(v, "loaded_module")?,
        load_address: unsigned(v, "load_address")?,
        arena_limit: unsigned(v, "arena_limit")?,
    })
}

pub fn verify_oracle(data: &GameData, path: &Path) -> Result<Value> {
    let raw = fs::read(path).map_err(|e| {
        format!(
            "{}: {e}; capture the state oracle (PCSX-Redux) first",
            path.display()
        )
    })?;
    let oracle: Value = serde_json::from_slice(&raw)?;
    let oracle_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let capture_text = fs::read_to_string(oracle_dir.join("capture-metadata.json"))?;
    let capture_metadata: Value =
        serde_json::from_str(capture_text.trim_start_matches('\u{feff}'))?;
    if capture_metadata["result_sha256"] != sha256(&raw)
        || capture_metadata["recovered_resident_sha256"] != sha256(&data.executable.payload)
    {
        return Err("oracle hash does not match its capture provenance".into());
    }
    for (name, address, length) in [
        ("dispatcher-code.bin", 0x80025eb0_u32, 0x1fc),
        ("arena-reset-code.bin", 0x80026f5c_u32, 0x14),
    ] {
        let instructions = fs::read(oracle_dir.join(name))?;
        let offset = (address - crate::assets::RESIDENT_BASE) as usize;
        if instructions != data.executable.payload[offset..offset + length] {
            return Err(format!(
                "oracle instruction capture {name} differs from the original resident"
            )
            .into());
        }
    }
    let cases = oracle["cases"]
        .as_array()
        .ok_or("oracle has no cases array")?;
    if cases.is_empty() {
        return Err("oracle must contain at least one case".into());
    }
    let metadata = &oracle["metadata"];
    if metadata["case_count"].as_u64() != Some(cases.len() as u64)
        || metadata["code_patched"] != false
        || metadata["isolation"] != "full"
        || metadata["emulator_changeset"] != "e1693b7f756ebdc4ccae1230ed09992079b51f90"
    {
        return Err("oracle provenance/isolation metadata is missing or unexpected".into());
    }
    let entries = oracle["state_table"]
        .as_array()
        .ok_or("oracle has no original state_table")?;
    let entries: Vec<StateEntry> = entries
        .iter()
        .map(|e| {
            Ok(StateEntry {
                callback_table: unsigned(e, "callback_table")?,
                module_id: signed(e, "module_id")?,
                load_address: unsigned(e, "load_address")?,
                arena_limit: unsigned(e, "arena_limit")?,
            })
        })
        .collect::<Result<_>>()?;
    if entries != data.states {
        return Err("oracle state table differs from original PAL payload".into());
    }
    let mut failures = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        if case["stores_dropped"] != false
            || case["ram_and_registers_restored"] != true
            || case["non_ram_accesses"].as_u64() != Some(0)
            || case["return_value"].as_u64() != Some(0)
            || !matches!(case["cycles"].as_u64(), Some(1..=10000))
        {
            return Err(
                format!("oracle case {index} lacks a successful isolated CPU execution").into(),
            );
        }
        let input = context(&case["input"])?;
        let expected = context(&case["expected"])?;
        let value = unsigned(case, "callback_result")?;
        let loading = case["loading_path"]
            .as_bool()
            .ok_or("oracle loading_path must be boolean")?;
        let actual = apply_callback_result(input, value, loading, &data.states)?;
        if actual != expected {
            failures.push(json!({"index":index,"name":case["name"],"expected":context_json(expected),"actual":context_json(actual)}));
        }
    }
    Ok(
        json!({"passed":failures.is_empty(),"cases":cases.len(),"matched":cases.len()-failures.len(),
        "oracle":path,"oracle_sha256":sha256(&raw),"source_metadata":metadata,
        "capture_metadata":capture_metadata,"instruction_bytes_match":true,
        "scope":"Original post-callback transition instructions and arena reset; callbacks and gameplay are not executed.","failures":failures}),
    )
}

fn comparison(expected: &[u8], actual: &[u8]) -> Value {
    json!({"match":expected==actual,"expected_size":expected.len(),"actual_size":actual.len(),
        "expected_sha256":sha256(expected),"actual_sha256":sha256(actual),
        "first_difference":expected.iter().zip(actual).position(|(a,b)|a!=b)
            .or_else(|| (expected.len()!=actual.len()).then_some(expected.len().min(actual.len())))})
}

pub fn verify_captures(data: &GameData, directory: &Path) -> Result<Value> {
    let mut runs = Vec::new();
    let mut passed = true;
    for module in 1..=5 {
        let dir = directory.join(format!("final-{module:03}"));
        let resident = fs::read(dir.join("resident.bin"))?;
        let overlay = fs::read(dir.join(format!("overlay_{module:03}.bin")))?;
        let ram = fs::read(dir.join("ram.bin"))?;
        if ram.len() != crate::memory::RAM_SIZE {
            return Err(format!("{}: RAM capture must be 2 MiB", dir.display()).into());
        }
        let word =
            |offset| u32::from_le_bytes(ram[offset..offset + 4].try_into().expect("bounded word"));
        let ctx_offset = (word(0xbacd0) & 0x1fffffff) as usize;
        let ctx = Context::from_bytes(
            ram.get(
                ctx_offset
                    ..ctx_offset
                        .checked_add(Context::BYTE_LEN)
                        .ok_or("context overflow")?,
            )
            .ok_or("RAM context outside capture")?,
        )?;
        let native = Bootstrap::run(data, module)?;
        let source = data.overlay(module)?;
        let resident_cmp = comparison(&data.executable.payload, &resident);
        let overlay_cmp = comparison(source, &overlay);
        let overlay_offset = (OVERLAY_BASE & 0x1fff_ffff) as usize;
        let captured_overlay = &ram[overlay_offset..overlay_offset + source.len()];
        let live_cmp = comparison(source, captured_overlay);
        let native_cmp = comparison(
            captured_overlay,
            native.ram.read(OVERLAY_BASE, source.len())?,
        );
        let context_matches = (
            native.context.state,
            native.context.substate,
            native.context.requested_module,
            native.context.loaded_module,
            native.context.load_address,
            native.context.arena_limit,
        ) == (
            ctx.state,
            ctx.substate,
            ctx.requested_module,
            ctx.loaded_module,
            ctx.load_address,
            ctx.arena_limit,
        );
        let state = data
            .states
            .get(ctx.state as usize)
            .ok_or("captured state outside table")?;
        let pointer = state
            .callback_table
            .checked_add(
                ctx.substate
                    .checked_mul(4)
                    .ok_or("captured substate overflow")?,
            )
            .ok_or("captured callback pointer overflow")?;
        let offset = (pointer & 0x1fff_ffff) as usize;
        let callback_bytes: [u8; 4] = ram
            .get(offset..offset.checked_add(4).ok_or("callback offset overflow")?)
            .ok_or("callback pointer outside captured RAM")?
            .try_into()?;
        let captured_callback = u32::from_le_bytes(callback_bytes);
        let callback_matches =
            native.callback == FIRST_CALLBACKS[module - 1] && native.callback == captured_callback;
        let run_passed = resident_cmp["match"] == true
            && overlay_cmp["match"] == true
            && live_cmp["match"] == true
            && native_cmp["match"] == true
            && context_matches
            && callback_matches;
        passed &= run_passed;
        runs.push(json!({"module":module,"passed":run_passed,"resident":resident_cmp,
            "overlay":overlay_cmp,"captured_ram_overlay":live_cmp,"native_loaded_overlay":native_cmp,"state_fields_match":context_matches,
            "first_callback_matches":callback_matches,"first_callback":format!("0x{:08X}",native.callback),
            "captured_callback":format!("0x{captured_callback:08X}")}));
    }
    Ok(json!({"passed":passed,"unique_images":6,"runs":runs,
        "scope":"PS-Y output, raw overlay data and native bootstrap state fields. Rendering/audio/CD timing are not modeled."}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_comparison_detects_content_and_length_mismatches() {
        assert_eq!(comparison(&[1, 2], &[1, 3])["first_difference"], 1);
        assert_eq!(comparison(&[1, 2], &[1])["first_difference"], 1);
        assert_eq!(comparison(&[1], &[1, 2])["match"], false);
        assert_eq!(comparison(&[1], &[1])["match"], true);
    }
    #[test]
    fn malformed_oracle_words_are_rejected_instead_of_truncated() {
        assert!(unsigned(&json!({"x":4294967296_u64}), "x").is_err());
        assert!(signed(&json!({"x":2147483648_u64}), "x").is_err());
        assert!(context(&json!({})).is_err());
    }
}
