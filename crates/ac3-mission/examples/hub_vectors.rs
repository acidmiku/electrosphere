//! Line protocol used by tools/verify_hub.py; no emulator or captured GPU replay.
use ac3_mission::{Effect, HubState, MissionContext, MissionState, initialize_hub, tick_hub};
use std::io::{self, BufRead};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        let values = line?
            .split_whitespace()
            .map(str::parse::<i64>)
            .collect::<Result<Vec<_>, _>>()?;
        if values.len() != 11 {
            return Err("expected eleven input integers".into());
        }
        let state = HubState {
            flags: values[1].try_into()?,
            selection: values[2].try_into()?,
            previous_selection: values[3].try_into()?,
            phase: values[4].try_into()?,
            timer: values[5].try_into()?,
            reveal_timer: values[6].try_into()?,
            exit_offset: values[7].try_into()?,
        };
        let mission = MissionState {
            choice: u16::try_from(values[8])? as i16,
            ..MissionState::default()
        };
        let context = MissionContext {
            options: values[9].try_into()?,
            ..MissionContext::default()
        };
        let update = match values[0] {
            0 => initialize_hub(mission, context),
            1 => tick_hub(state, mission, context, values[10].try_into()?)?,
            _ => return Err("unsupported case kind".into()),
        };
        let state = update.state;
        print!(
            "{} {} {} {} {} {} {} {} {} {}",
            state.flags,
            state.selection,
            state.previous_selection,
            state.phase,
            state.timer,
            state.reveal_timer,
            state.exit_offset,
            update.mission.choice as u16,
            update.context.options,
            update.callback_result
        );
        for effect in update.effects {
            if let Effect::PlaySound { id } = effect {
                print!(" {id}");
            }
        }
        println!();
    }
    Ok(())
}
