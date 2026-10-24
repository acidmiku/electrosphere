//! Std-only protocol for saved original-function observations.
use ac3_mission::*;
use std::io::{self, BufRead};

fn unhex(text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err("invalid hexadecimal bytes".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&text[offset..offset + 2], 16).map_err(Into::into))
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn print_sounds(effects: &[Effect]) {
    for effect in effects {
        if let Effect::PlaySound { id } = effect {
            print!(" {id}");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tables = AircraftTables::from_overlay(&std::fs::read(
        std::env::args().nth(1).ok_or("overlay path required")?,
    )?)?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let values: Vec<_> = line.split_whitespace().collect();
        if values.len() != 4 {
            return Err("expected kind/state/context/pressed".into());
        }
        let pressed = values[3].parse()?;
        if values[0].starts_with("launch_") {
            let state = LaunchState {
                timer: values[1].parse()?,
            };
            let context = MissionContext {
                options: values[2].parse()?,
                ..MissionContext::default()
            };
            let update = match values[0] {
                "launch_init" => initialize_launch(context),
                "launch_tick" => tick_launch(state, context, pressed),
                _ => return Err("unknown launch kind".into()),
            };
            print!(
                "{} {} {}",
                update.state.timer, update.context.options, update.callback_result
            );
            print_sounds(&update.effects);
        } else {
            let state = AircraftSelectionState::from_bytes(&unhex(values[1])?)?;
            let mut context_bytes = unhex(values[2])?;
            let context = AircraftContext::from_bytes(&context_bytes)?;
            let update = match values[0] {
                "aircraft_init" => initialize_aircraft(state, context),
                "aircraft_tick" => tick_aircraft(state, context, &tables, pressed)?,
                "aircraft_leaf" => tick_aircraft_leaf(state, context, &tables, pressed)?,
                _ => return Err("unknown aircraft kind".into()),
            };
            update.context.write_into(&mut context_bytes)?;
            print!(
                "{} {} {}",
                hex(&update.state.to_bytes()),
                hex(&context_bytes),
                update.callback_result
            );
            print_sounds(&update.effects);
        }
        println!();
    }
    Ok(())
}
