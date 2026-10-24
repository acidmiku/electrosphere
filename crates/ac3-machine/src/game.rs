//! The game-version interface: everything the host takes from a version's
//! translation crate (`ac3-flight` for PAL; the JP crate later).
//!
//! The host drives one [`Machine`] type for every version. A version crate
//! implements [`Game`] for a unit type and exposes a `static` of it; the
//! player picks that `&'static dyn Game` when it starts (from the disc) and
//! goes through it for the boot, each main-loop pass, the CD interrupt work,
//! libpad's VBlank handler, and the RAM words the host reads and writes as
//! the hardware interrupts would ([`Layout`]).
//!
//! Every call is per pass or per VBlank, never per instruction: the
//! translated code inside a pass calls its own crate directly, so dynamic
//! dispatch here costs nothing measurable. Host devices that read RAM in
//! the hot path copy the [`Layout`] (plain addresses) at construction.
use crate::cd::VBlankClock;
use crate::machine::Machine;
use crate::sio::Sio;
use std::time::Duration;

/// The TV standard a release runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoStandard {
    /// 50 fields per second, 256/512 lines (the European release).
    Pal,
    /// 59.94 fields per second, 240/480 lines (the Japanese release).
    Ntsc,
}

impl VideoStandard {
    pub fn name(self) -> &'static str {
        match self {
            Self::Pal => "PAL",
            Self::Ntsc => "NTSC",
        }
    }
    /// Its field rate.
    pub fn field_rate(self) -> FieldRate {
        match self {
            Self::Pal => FieldRate::PAL,
            Self::Ntsc => FieldRate::NTSC,
        }
    }
}

/// VBlanks (fields) per second as an exact fraction: `fields` per
/// `seconds`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FieldRate {
    pub fields: u64,
    pub seconds: u64,
}

impl FieldRate {
    /// 50 Hz.
    pub const PAL: Self = Self::new(50, 1);
    /// 60000 / 1001 Hz (59.94).
    pub const NTSC: Self = Self::new(60_000, 1_001);

    pub const fn new(fields: u64, seconds: u64) -> Self {
        Self { fields, seconds }
    }
    /// Fields per second.
    pub fn hz(self) -> f64 {
        self.fields as f64 / self.seconds as f64
    }
    /// Whole fields per second, rounded (a drive's VBlank rate).
    pub fn rounded_hz(self) -> u32 {
        ((self.fields + self.seconds / 2) / self.seconds) as u32
    }
    /// Wall time of `vblanks` fields, rounded down to a nanosecond (PAL:
    /// exactly 20 ms each).
    pub fn duration(self, vblanks: u64) -> Duration {
        let nanos = u128::from(vblanks) * u128::from(self.seconds) * 1_000_000_000
            / u128::from(self.fields.max(1));
        Duration::new(
            (nanos / 1_000_000_000) as u64,
            (nanos % 1_000_000_000) as u32,
        )
    }
}

/// The game context's state words (`[context + offset]`), as offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextFields {
    /// The resident state (2: flight).
    pub state: u32,
    pub substate: u32,
    /// The overlay module the state table asks for, and the one loaded
    /// (`0xFFFFFFFF` while a resident state runs).
    pub requested_module: u32,
    pub loaded_module: u32,
}

/// RAM words of a version that the host reads or writes (absolute
/// addresses; all inside the version's executable image).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The executable's `$gp` (small-data base).
    pub gp: u32,
    /// libetc's VBlank counter: its VBlank handler adds one per VBlank.
    pub vblank_counter: u32,
    /// VSync's record of the VBlank count at its previous waiting call.
    pub vsync_record: u32,
    /// libpad's driver state word (1 once set up).
    pub pad_state: u32,
    /// libpad's port records pointer, and the records it points to once
    /// set up.
    pub pad_records_pointer: u32,
    pub pad_records: u32,
    /// The game's pad block pointer (`[gp+..]`; the libpad receive buffers
    /// of ports 1 and 2 at +0x00 and +0x22).
    pub pad_block: u32,
    /// The game context pointer (`[gp+..]`).
    pub context: u32,
    /// The context's state words.
    pub context_fields: ContextFields,
    /// libcd's file cache (24-byte CdlFILE entries): CdSearchFile returns
    /// one ([`crate::disc::DiscDevice`]).
    pub cd_file_cache: u32,
}

impl Layout {
    /// The words a CD drive paces sectors by ([`crate::cd::CdDrive`]).
    pub fn vblank_clock(&self) -> VBlankClock {
        VBlankClock {
            counter: self.vblank_counter,
            vsync_record: self.vsync_record,
        }
    }
}

/// The resident executable the host loads (the boot loader's work).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Executable {
    /// Where the unpacked payload goes, and its size.
    pub load_address: u32,
    pub bytes: usize,
    /// The resident entry point.
    pub entry: u32,
}

/// The overlay modules and the state table that loads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overlays {
    /// Where every overlay module loads (their addresses overlap).
    pub base: u32,
    /// Overlay modules of the release (archive entries 1..=modules).
    pub modules: u32,
    /// The state table: 16-byte records {callback table, module, load
    /// address, arena start}, `states` of them.
    pub state_table: u32,
    pub states: u32,
}

/// The context's state words of the running game ([`Game::states`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct States {
    pub state: u32,
    pub substate: u32,
    pub requested_module: u32,
    pub loaded_module: u32,
}

/// One game version: what the host takes from its translation crate.
pub trait Game: Sync {
    /// Short name for reports and messages ("PAL").
    fn name(&self) -> &'static str;
    /// The TV standard of the release.
    fn video(&self) -> VideoStandard;
    /// The host's field clock: VBlanks per second of game time.
    fn field_rate(&self) -> FieldRate {
        self.video().field_rate()
    }
    /// Discs of the release (the host mounts them all).
    fn discs(&self) -> u32;
    fn executable(&self) -> Executable;
    fn layout(&self) -> Layout;
    fn overlays(&self) -> Overlays;

    /// Load the unpacked resident `payload` and run the boot up to the main
    /// loop (`m.device` supplies the CD data the boot reads). A fault stays
    /// in the machine.
    fn boot(&self, m: &mut Machine, payload: &[u8]);
    /// One pass of the resident main loop.
    fn main_loop_pass(&self, m: &mut Machine);
    /// The CD interrupt work of a pass (libcd's handler polls the drive);
    /// returns what the handler returns.
    fn cd_service(&self, m: &mut Machine) -> u32;
    /// libpad's VBlank handler on `ram`, with the port hardware `sio`.
    fn pad_vblank(&self, ram: &mut [u8], sio: &mut Sio);
    /// Whether the game is in flight (widescreen, the presenter). Reads RAM
    /// without side effects.
    fn in_flight(&self, m: &Machine) -> bool;

    /// The context's state words, read without side effects (no fault, no
    /// precision tracking).
    fn states(&self, m: &Machine) -> States {
        let layout = self.layout();
        let context = m.peek(layout.context);
        let field = |offset: u32| m.peek(context.wrapping_add(offset));
        let fields = layout.context_fields;
        States {
            state: field(fields.state),
            substate: field(fields.substate),
            requested_module: field(fields.requested_module),
            loaded_module: field(fields.loaded_module),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_rates_are_exact() {
        assert_eq!(FieldRate::PAL.duration(1), Duration::from_millis(20));
        assert_eq!(
            FieldRate::PAL.duration(123_456),
            Duration::from_millis(123_456 * 20)
        );
        assert_eq!(FieldRate::PAL.rounded_hz(), 50);
        assert_eq!(FieldRate::NTSC.rounded_hz(), 60);
        // 60000 fields of NTSC take 1001 seconds.
        assert_eq!(FieldRate::NTSC.duration(60_000), Duration::from_secs(1001));
        assert_eq!(
            FieldRate::NTSC.duration(1),
            Duration::from_nanos(16_683_333)
        );
        assert!((FieldRate::NTSC.hz() - 59.94).abs() < 0.001);
        assert_eq!(VideoStandard::Pal.field_rate(), FieldRate::PAL);
    }
}
