//! Std-only protocol for saved original first-mission loader observations.
use ac3_mission::*;
use std::io::{self, BufRead};

fn unhex(text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err("invalid hexadecimal bytes".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(Into::into))
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn visual(kind: &str, a0: i32, a1: i32) -> String {
    format!("{{\"kind\":\"{kind}\",\"a0\":{a0},\"a1\":{a1}}}")
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tables = AircraftTables::from_overlay(&std::fs::read(
        std::env::args().nth(1).ok_or("overlay path required")?,
    )?)?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let v: Vec<_> = line.split_whitespace().collect();
        if v.len() != 9 {
            return Err("expected kind/control/loader/context and five scalar inputs".into());
        }
        let mut control = unhex(v[1])?;
        let mut loader = unhex(v[2])?;
        let mut context = unhex(v[3])?;
        let state = MissionLoaderState::from_bytes(&control, &loader)?;
        let ctx = LoaderContext::from_bytes(&context)?;
        let input = LoaderInputs {
            mission_resource_bytes: v[4].parse()?,
            common_resource_bytes: v[5].parse()?,
            transferred_total_bytes: v[6].parse()?,
            read_complete: v[7] == "1",
            texture_poll_result: v[8].parse()?,
        };
        let update = match v[0] {
            "init" => initialize_mission_loader(state, ctx),
            "tick" => tick_mission_loader(state, ctx, input, &tables)?,
            _ => return Err("unknown loader kind".into()),
        };
        update.state.write_into(&mut control, &mut loader)?;
        update.context.write_into(&mut context)?;
        let mut calls = Vec::new();
        let mut sounds = Vec::new();
        let mut packet = None;
        for effect in update.effects {
            match effect {
                LoaderEffect::Common(Effect::PlaySound { id }) => sounds.push(id.to_string()),
                LoaderEffect::InitializeBackgroundVoices => {
                    sounds.extend((0x5d..=0x61).map(|id| id.to_string()))
                }
                LoaderEffect::Common(Effect::SelectVisualPage { page }) => {
                    calls.push(visual("page", page as i32, 0))
                }
                LoaderEffect::Common(Effect::SetVisualStyle { slot, style }) => {
                    calls.push(visual("style", slot, style as i32))
                }
                LoaderEffect::Common(Effect::SetVisualParameter { slot, value }) => {
                    calls.push(visual("parameter", slot, value))
                }
                LoaderEffect::BindBuiltinScripts { address } => {
                    calls.push(visual("scripts", address as i32, 0))
                }
                LoaderEffect::BindCommonScripts { resource, child } => {
                    calls.push(visual("common_scripts", resource as i32, i32::from(child)))
                }
                LoaderEffect::DrawProgressBar { width } => {
                    let words = [0x60d5e5e5_u32, 0x01a400c0, (6 << 16) | u32::from(width)];
                    packet = Some(hex(&words
                        .iter()
                        .flat_map(|w| w.to_le_bytes())
                        .collect::<Vec<_>>()));
                }
                _ => {}
            }
        }
        println!(
            "{{\"state_hex\":\"{}\",\"loader_hex\":\"{}\",\"context_hex\":\"{}\",\"callback_result\":{},\"sounds\":[{}],\"visual_calls\":[{}],\"progress_packet_hex\":{}}}",
            hex(&control),
            hex(&loader),
            hex(&context),
            update.callback_result,
            sounds.join(","),
            calls.join(","),
            packet.map(|p| format!("\"{p}\"")).unwrap_or("null".into())
        );
    }
    Ok(())
}
