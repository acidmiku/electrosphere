//! This crate's game as the host sees it: [`GAME`], its
//! [`Game`](ac3_machine::Game) (boot, main-loop pass, CD interrupt work,
//! libpad's VBlank handler, the flight-state query, and the RAM words the
//! host reads and writes as the hardware interrupts would).
//!
//! The names are version-neutral: a crate generated from this one (the JP
//! crate) has the same items. Every address is named from the translated
//! modules where they define it, or is a literal the generator re-maps; the
//! release facts that no address map knows are the constants below.
use crate::{boot, common, machine::Machine, mainloop, sdk};
use ac3_machine::game::{ContextFields, Executable, Game, Layout, Overlays, VideoStandard};
use ac3_machine::sio::Sio;

/// The release: the European one (SCES-02066).
pub const NAME: &str = "PAL";
pub const VIDEO: VideoStandard = VideoStandard::Pal;
pub const DISCS: u32 = 1;
/// The resident entry point (`start`).
pub const ENTRY: u32 = 0x8002_572c;
/// Overlay modules, and the state table loading them (10 records).
pub const MODULES: u32 = 5;
pub const STATE_TABLE: u32 = 0x800b_0134;
pub const STATES: u32 = 10;

/// This crate's release (a unit type; [`GAME`] is its value).
#[derive(Clone, Copy, Debug, Default)]
pub struct Release;

/// The game of this crate, for the host (`&ac3_flight::game::GAME`).
pub static GAME: Release = Release;

/// The RAM words the host uses (see [`Layout`]).
pub const LAYOUT: Layout = Layout {
    gp: crate::GP,
    vblank_counter: sdk::VSYNC_COUNTER,
    vsync_record: sdk::VSYNC_LAST_COUNT,
    pad_state: mainloop::libpad::STATE,
    pad_records_pointer: mainloop::libpad::RECORDS_POINTER,
    pad_records: mainloop::libpad::RECORDS,
    pad_block: crate::GP + mainloop::PAD_BLOCK,
    context: crate::GP + common::CONTEXT,
    context_fields: ContextFields {
        state: 0x2c,
        substate: 0x30,
        requested_module: 0x34,
        loaded_module: 0x38,
    },
    cd_file_cache: boot::CD_FILE_CACHE,
};

impl Game for Release {
    fn name(&self) -> &'static str {
        NAME
    }
    fn video(&self) -> VideoStandard {
        VIDEO
    }
    fn discs(&self) -> u32 {
        DISCS
    }
    fn executable(&self) -> Executable {
        Executable {
            load_address: boot::PAYLOAD_ADDRESS,
            bytes: boot::PAYLOAD_BYTES,
            entry: ENTRY,
        }
    }
    fn layout(&self) -> Layout {
        LAYOUT
    }
    fn overlays(&self) -> Overlays {
        Overlays {
            base: common::OVERLAY_BASE,
            modules: MODULES,
            state_table: STATE_TABLE,
            states: STATES,
        }
    }
    fn boot(&self, m: &mut Machine, payload: &[u8]) {
        boot::boot(m, payload);
    }
    fn main_loop_pass(&self, m: &mut Machine) {
        mainloop::main_loop_iteration(m);
    }
    fn cd_service(&self, m: &mut Machine) -> u32 {
        // `cdstream::cd_service`: libcd's interrupt handler over the drive.
        mainloop::pending_sdk::cd_interrupt_handler(m)
    }
    fn pad_vblank(&self, ram: &mut [u8], sio: &mut Sio) {
        mainloop::libpad::vblank(ram, sio);
    }
    fn in_flight(&self, m: &Machine) -> bool {
        // Flight is resident state 2 (as `widescreen::in_flight` reads it).
        self.states(m).state == 2
    }
}
