//! Native translations of the PAL resident state dispatcher at `0x80025EB0`.
//!
//! This module separates selecting a callback from applying its return value.
//! It does not run MIPS instructions or implement the callbacks themselves.
//! Addresses remain PSX virtual addresses until the host resolves them.
//!
//! The observed source is the recovered resident executable, including branch
//! delay slots; see `analysis/main/disassembly.txt` and `docs/CODE_MAP.md`.

#![forbid(unsafe_code)]

use std::fmt;

/// Callback result bits consumed by the resident dispatcher.
pub mod transition {
    pub const LOAD_COMPLETE: u32 = 0x0200_0000;
    pub const NEXT_SUBSTATE: u32 = 0x0400_0000;
    pub const NEXT_STATE: u32 = 0x0800_0000;
    pub const SET_SUBSTATE: u32 = 0x1000_0000;
    pub const SET_STATE: u32 = 0x2000_0000;
    pub const RESET_ARENA: u32 = 0x4000_0000;
}

/// Context words read or written by the dispatcher and arena-reset routine.
///
/// This is a semantic view, not a packed copy of the original structure.
/// [`Context::write_into`] preserves the unmodeled bytes between these fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// Original byte offset `0x00`.
    pub flags: u32,
    /// Original byte offset `0x18`.
    pub arena_current: u32,
    /// Original byte offset `0x1C`.
    pub arena_base: u32,
    /// Original byte offset `0x2C`.
    pub state: u32,
    /// Original byte offset `0x30`.
    pub substate: u32,
    /// Original byte offset `0x34`. Negative values denote resident states.
    pub requested_module: i32,
    /// Original byte offset `0x38`.
    pub loaded_module: i32,
    /// Original byte offset `0x3C`.
    pub load_address: u32,
    /// Original byte offset `0x40`; passed to the arena-reset routine.
    pub arena_limit: u32,
}

impl Context {
    /// Minimum byte length covering every modeled word.
    pub const BYTE_LEN: usize = 0x44;

    /// Read little-endian PSX context words. A longer input is permitted.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_length(bytes.len(), Self::BYTE_LEN)?;
        Ok(Self {
            flags: word(bytes, 0),
            arena_current: word(bytes, 0x18),
            arena_base: word(bytes, 0x1c),
            state: word(bytes, 0x2c),
            substate: word(bytes, 0x30),
            requested_module: word(bytes, 0x34) as i32,
            loaded_module: word(bytes, 0x38) as i32,
            load_address: word(bytes, 0x3c),
            arena_limit: word(bytes, 0x40),
        })
    }

    /// Write only modeled words, leaving all other bytes unchanged.
    ///
    /// Validation happens before any write, so a short buffer is unchanged.
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), Error> {
        require_length(bytes.len(), Self::BYTE_LEN)?;
        for (offset, value) in [
            (0, self.flags),
            (0x18, self.arena_current),
            (0x1c, self.arena_base),
            (0x2c, self.state),
            (0x30, self.substate),
            (0x34, self.requested_module as u32),
            (0x38, self.loaded_module as u32),
            (0x3c, self.load_address),
            (0x40, self.arena_limit),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}

/// One 16-byte record in the resident table beginning at `0x800AE544`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateEntry {
    pub callback_table: u32,
    pub module_id: i32,
    pub load_address: u32,
    pub arena_limit: u32,
}

impl StateEntry {
    pub const BYTE_LEN: usize = 16;

    /// Parse a little-endian table record. A longer input is permitted.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_length(bytes.len(), Self::BYTE_LEN)?;
        Ok(Self {
            callback_table: word(bytes, 0),
            module_id: word(bytes, 4) as i32,
            load_address: word(bytes, 8),
            arena_limit: word(bytes, 12),
        })
    }
}

/// Parse a complete sequence of state records, with no trailing partial record.
pub fn parse_state_table(bytes: &[u8]) -> Result<Vec<StateEntry>, Error> {
    if !bytes.len().is_multiple_of(StateEntry::BYTE_LEN) {
        return Err(Error::InvalidStateTableLength(bytes.len()));
    }
    bytes
        .chunks_exact(StateEntry::BYTE_LEN)
        .map(StateEntry::from_bytes)
        .collect()
}

/// The callback that the host must invoke before applying its result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    /// Resolve the indexed callback in the selected table using game memory.
    /// Its bounds cannot be inferred from a state record alone.
    Normal { callback_table: u32, substate: u32 },
    /// Original callback `0x80099BC8`, shared loading-table substate zero.
    LoadBegin { module_id: i32, load_address: u32 },
    /// Original callback `0x80099CD8`, shared loading-table substate one.
    LoadPoll { module_id: i32, load_address: u32 },
}

/// Select a callback according to the dispatcher at `0x80025EB0..0x80025F30`.
///
/// The original can index outside both callback tables. The native boundary
/// rejects unknown state records and loading substates instead. Normal callback
/// indices are returned unresolved because their table lengths vary by state.
pub fn select_dispatch(context: &Context, entries: &[StateEntry]) -> Result<Dispatch, Error> {
    if context.requested_module == context.loaded_module {
        let entry = state_entry(entries, context.state)?;
        Ok(Dispatch::Normal {
            callback_table: entry.callback_table,
            substate: context.substate,
        })
    } else {
        match context.substate {
            0 => Ok(Dispatch::LoadBegin {
                module_id: context.requested_module,
                load_address: context.load_address,
            }),
            1 => Ok(Dispatch::LoadPoll {
                module_id: context.requested_module,
                load_address: context.load_address,
            }),
            value => Err(Error::InvalidLoadingSubstate(value)),
        }
    }
}

/// Apply the original dispatcher's callback-result semantics.
///
/// Pass the context **after** the callback's own changes. `loading_path` records
/// which branch selected that callback; it must not be recomputed from context
/// fields the callback might have changed. The loading-complete action precedes
/// transition selection. Transition priority is: set state, next state, set
/// substate, next substate. Arena reset then uses the resulting state's pointer.
///
/// State/substate increments wrap as MIPS `addiu` does. An invalid selected
/// state returns an error without exposing a partially updated context.
pub fn apply_callback_result(
    context: Context,
    result: u32,
    loading_path: bool,
    entries: &[StateEntry],
) -> Result<Context, Error> {
    use transition::*;

    let mut next = context;
    if loading_path && result & LOAD_COMPLETE != 0 {
        next.loaded_module = next.requested_module;
        next.substate = 0;
    }

    if result & SET_STATE != 0 {
        next.state = result & 0xff;
        next.substate = 0;
    } else if result & NEXT_STATE != 0 {
        next.state = next.state.wrapping_add(1);
        next.substate = 0;
    } else if result & SET_SUBSTATE != 0 {
        next.substate = result & 0xff;
    } else if result & NEXT_SUBSTATE != 0 {
        next.substate = next.substate.wrapping_add(1);
    }

    if result & (SET_STATE | NEXT_STATE) != 0 {
        let entry = state_entry(entries, next.state)?;
        next.requested_module = entry.module_id;
        next.load_address = entry.load_address;
        next.arena_limit = entry.arena_limit;
        if next.requested_module < 0 {
            // The original stores -1 even for another negative module ID.
            next.loaded_module = -1;
        }
    }

    if result & RESET_ARENA != 0 {
        next = reset_arena(next, next.arena_limit);
        next.flags |= 8;
    }

    Ok(next)
}

/// Native translation of `0x80026F5C`, including the store in its delay slot.
/// The routine itself does not set the dispatcher's draw-synchronization bit.
pub const fn reset_arena(mut context: Context, address: u32) -> Context {
    context.arena_current = address;
    context.arena_base = address;
    context
}

/// Original resident startup callback `0x80025BA4`: state 4 plus arena reset.
pub const fn boot_callback_result() -> u32 {
    0x6000_0004
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    BufferTooShort { required: usize, actual: usize },
    InvalidStateTableLength(usize),
    InvalidState(u32),
    InvalidLoadingSubstate(u32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooShort { required, actual } => {
                write!(f, "buffer requires {required} bytes, received {actual}")
            }
            Self::InvalidStateTableLength(length) => {
                write!(f, "state table length {length} is not a multiple of 16")
            }
            Self::InvalidState(state) => write!(f, "state {state} is outside the supplied table"),
            Self::InvalidLoadingSubstate(substate) => {
                write!(f, "loading substate must be 0 or 1, received {substate}")
            }
        }
    }
}

impl std::error::Error for Error {}

fn require_length(actual: usize, required: usize) -> Result<(), Error> {
    if actual < required {
        Err(Error::BufferTooShort { required, actual })
    } else {
        Ok(())
    }
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn state_entry(entries: &[StateEntry], state: u32) -> Result<&StateEntry, Error> {
    usize::try_from(state)
        .ok()
        .and_then(|index| entries.get(index))
        .ok_or(Error::InvalidState(state))
}
