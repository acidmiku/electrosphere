//! Native execution of the first dispatcher/load sequence. Rendering, hardware
//! initialization and overlay callbacks are deliberately outside this boundary.
use crate::{
    Result,
    assets::{GameData, OVERLAY_STATES, RESIDENT_BASE},
    memory::Ram,
};
use ac3_core::{Context, Dispatch, apply_callback_result, boot_callback_result, select_dispatch};

pub struct Bootstrap {
    pub ram: Ram,
    pub context: Context,
    pub callback: u32,
    pub loaded_bytes: usize,
    pub trace: Vec<String>,
}

impl Bootstrap {
    /// Module 1 follows the observed startup choice. Other IDs are loader probes.
    pub fn run(data: &GameData, module: usize) -> Result<Self> {
        let bytes = data.overlay(module)?;
        let mut ram = Ram::default();
        ram.write(RESIDENT_BASE, &data.executable.payload)?;
        // This reproduces resident_start's observed BSS extent, including its
        // overlap with the payload tail. It does not initialize PSX hardware.
        ram.clear(0x800bacb0, (0x800c9dc0_u32 - 0x800bacb0) as usize)?;
        let initial = data.states.first().ok_or("missing initial state")?;
        let context = Context {
            flags: 0,
            arena_current: initial.arena_limit,
            arena_base: initial.arena_limit,
            state: 0,
            substate: 0,
            requested_module: -1,
            loaded_module: -1,
            load_address: initial.load_address,
            arena_limit: initial.arena_limit,
        };
        let mut trace =
            vec!["Initialized the ported dispatcher fields (0x800252D8 subset)".to_owned()];
        match select_dispatch(&context, &data.states)? {
            Dispatch::Normal {
                callback_table,
                substate: 0,
            } if ram.read_u32(callback_table)? == 0x80025ba4 => (),
            _ => return Err("initial callback did not resolve to 0x80025BA4".into()),
        }
        let result = if module == 1 {
            boot_callback_result()
        } else {
            0x60000000 | OVERLAY_STATES[module - 1]
        };
        let context = apply_callback_result(context, result, false, &data.states)?;
        let selection = if module == 1 {
            "Boot callback result"
        } else {
            "Controlled probe transition"
        };
        trace.push(format!(
            "{selection} {result:#010X} selected state {}",
            context.state
        ));
        let Dispatch::LoadBegin {
            module_id,
            load_address,
        } = select_dispatch(&context, &data.states)?
        else {
            return Err("dispatcher did not request a module load".into());
        };
        if module_id != module as i32 {
            return Err("unexpected requested module".into());
        }
        // Desktop file IO replaces the CD queue for this slice. Completion is
        // synchronous; no claim of equivalent CD/frame timing is made.
        ram.write(load_address, bytes)?;
        let context = apply_callback_result(context, 0x04000000, true, &data.states)?;
        if !matches!(
            select_dispatch(&context, &data.states)?,
            Dispatch::LoadPoll { .. }
        ) {
            return Err("module load did not advance to polling".into());
        }
        let context = apply_callback_result(context, 0x02000000, true, &data.states)?;
        let Dispatch::Normal {
            callback_table,
            substate,
        } = select_dispatch(&context, &data.states)?
        else {
            return Err("completed module did not dispatch normally".into());
        };
        let pointer = callback_table
            .checked_add(substate.checked_mul(4).ok_or("callback index overflow")?)
            .ok_or("callback address overflow")?;
        let callback = ram.read_u32(pointer)?;
        trace.push(format!(
            "Loaded module {module}: {} bytes at {load_address:#010X}",
            bytes.len()
        ));
        trace.push(format!(
            "Next original callback: {callback:#010X}; implementation pending"
        ));
        Ok(Self {
            ram,
            context,
            callback,
            loaded_bytes: bytes.len(),
            trace,
        })
    }
}
