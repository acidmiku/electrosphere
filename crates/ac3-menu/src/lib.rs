//! Pure logic from the PAL overlay 1 menu and language-selection callbacks.
//!
//! These functions return host effects; they do not execute GPU, audio, file or
//! save operations. See `docs/MENU_LOGIC.md` for original addresses and coverage.
#![forbid(unsafe_code)]

mod front_end;
mod language;
mod menu;

pub use front_end::*;
pub use language::*;
pub use menu::*;

/// A host-side effect requested by translated logic, in original call order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Original `0x800D0E78` layout. A host still needs fonts and visual assets.
    DrawLanguageRows {
        rows: [LanguageRow; 5],
    },
    /// Original exit-phase rectangle, shade before incrementing the timer.
    DrawLanguageFade {
        shade: u8,
    },
    /// The same full-screen subtraction rectangle used by the title fade.
    DrawScreenFade {
        shade: u8,
    },
    /// Original `0x8002D724`; numeric IDs are preserved without inventing audio.
    PlaySound {
        id: u32,
    },
    /// Original `0x800D54D8`.
    SelectVisualPage {
        page: i32,
    },
    /// Original `0x800D5458`.
    SetVisualOffset {
        slot: i32,
        x: i32,
        y: i32,
    },
    /// Original `0x800D52DC`.
    SetVisualStyle {
        slot: i32,
        style: u8,
    },
    /// Original `0x800D0548` clears all 256 text visibility bytes.
    ClearTextVisibility,
    /// Original `0x800D050C`.
    SetTextVisibility {
        entry: i32,
        value: u8,
    },
    /// Original `0x800D0524`, the first halfword of a 24-byte text descriptor.
    SetTextFont {
        entry: i32,
        font: u16,
    },
    /// Original `0x800D532C`; use `pulse_color` with context frame counter +0x20.
    PulseVisualColor {
        slot: i32,
        from: [u8; 3],
        to: [u8; 3],
    },
    /// Original `0x800D0280` followed by `0x800D0290`; no audio is decoded here.
    StartStream {
        resource: u16,
    },
    /// Original `0x800D02B0`, including its resource queue cancellation call.
    StopStream,
    /// Sparse context effects not included in the MenuContext view.
    SetContextByte {
        offset: u32,
        value: u8,
    },
    SetContextWord {
        offset: u32,
        value: u32,
    },
    /// Original `0x800D1D0C`; apply `reset_new_game_progress` to context memory.
    ResetNewGameProgress,
    /// Original allocation at `0x80026F70`. The host owns allocation and linking
    /// its result to `0x800D8948`; this effect does not allocate game memory.
    AllocateMenuBlock {
        bytes: u32,
    },
    /// Graphics/resource functions outside the translated initialization slice.
    /// Addresses identify missing implementations, not callable native pointers.
    PendingOriginalCall {
        address: u32,
        arguments: Vec<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    BufferTooShort { required: usize, actual: usize },
    InvalidMenuState(i16),
    InvalidMenuSubstate(i16),
    InvalidLanguagePhase(i32),
    InvalidTitlePhase(i32),
    InvalidDifficultyPhase(i32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BufferTooShort { required, actual } => {
                write!(f, "buffer requires {required} bytes, received {actual}")
            }
            Self::InvalidMenuState(value) => write!(f, "unknown menu state {value}"),
            Self::InvalidMenuSubstate(value) => write!(f, "unknown menu substate {value}"),
            Self::InvalidLanguagePhase(value) => write!(f, "unknown language phase {value}"),
            Self::InvalidTitlePhase(value) => write!(f, "unknown title phase {value}"),
            Self::InvalidDifficultyPhase(value) => write!(f, "unknown difficulty phase {value}"),
        }
    }
}
impl std::error::Error for Error {}

pub(crate) fn require_bytes(bytes: &[u8], required: usize) -> Result<(), Error> {
    if bytes.len() < required {
        Err(Error::BufferTooShort {
            required,
            actual: bytes.len(),
        })
    } else {
        Ok(())
    }
}
pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
