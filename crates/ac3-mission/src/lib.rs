//! Pure control logic from the PAL game's second overlay.
//! Graphics, resource loading, streaming and unported routines remain effects.
mod aircraft;
mod briefing;
mod briefing_script;
mod dispatcher;
mod hub;
mod launch;
mod loader;
mod wrappers;

pub use aircraft::*;
pub use briefing::*;
pub use briefing_script::*;
pub use dispatcher::*;
pub use hub::*;
pub use launch::*;
pub use loader::*;
pub use wrappers::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// `0x800CB8B4`: reset display bytes +0x10..12, word +4=1, +0x14=0,
    /// and OR 9 into flags in the frame block `[0x800BACB8]`.
    InitializeFrameState,
    AllocateScene {
        bytes: u32,
    },
    SetSceneWord {
        offset: u32,
        value: u32,
    },
    PlaySound {
        id: u32,
    },
    StartStream {
        resource: u16,
    },
    StopStream,
    ClearTextVisibility,
    SetTextVisibility {
        entry: i32,
        value: u8,
    },
    SelectVisualPage {
        page: u32,
    },
    SetVisualStyle {
        slot: i32,
        style: u32,
    },
    SetVisualOffset {
        slot: i32,
        x: i32,
        y: i32,
    },
    SetVisualParameter {
        slot: i32,
        value: i32,
    },
    SetSceneColor {
        red: i32,
        green: i32,
        blue: i32,
    },
    DrawHubBackdrop {
        selection: i32,
    },
    InitializeHubMap {
        mission: u16,
        placeholder: bool,
    },
    DrawHubMap {
        reveal_timer: i32,
        exit_offset: i32,
    },
    /// Remainder of `0x800E7CE8` after its explicit sound-0x62 request:
    /// five voice fades through 0x8002DB18 and background flags +0x14/+0x20.
    BeginHubBackgroundFade,
    /// Remainder of 0x800E7B50(3) after its explicit sound-0x63 request:
    /// background +0x14=3/+0x20=0 and five voice fades to zero over 60 ticks.
    BeginLaunchBackgroundFade,
    /// An unported original routine. Not an instruction to execute guest code.
    PendingOriginalCall {
        address: u32,
    },
    /// Address and scalar arguments of an unported dependency.
    PendingOriginalCallWithArgs {
        address: u32,
        arguments: Vec<u32>,
    },
    /// The two 0x800CE978 requests in aircraft selection: scene +0x200AC
    /// (ULZ output capacity 0x3820, supplied VRAM rectangle) and +0x200B0
    /// (capacity 0x828). These are indexed TIM tables, not fixed record strides.
    LoadAircraftResources {
        index: i32,
        rectangle: [i16; 4],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidMissionState(i16),
    InvalidMissionSubstate { state: i16, substate: i16 },
    InvalidHubPhase(i32),
    InvalidBriefingPhase { phase: i16, substate: i16 },
    InvalidAircraftPhase { phase: i32, substate: i32 },
    InvalidAircraftId(i32),
    InvalidAircraftIndex(i32),
    InvalidWeaponRow(i32),
    InvalidWeaponId { row: usize, id: i32 },
    MissingAircraftLoadout(i16),
    InvalidLoadoutCount { row: usize, count: u8 },
    BufferTooShort { required: usize, actual: usize },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

fn require_bytes(bytes: &[u8], required: usize) -> Result<(), Error> {
    if bytes.len() < required {
        Err(Error::BufferTooShort {
            required,
            actual: bytes.len(),
        })
    } else {
        Ok(())
    }
}
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
