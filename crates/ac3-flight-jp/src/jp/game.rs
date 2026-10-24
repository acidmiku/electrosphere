//! The Japanese release as the host sees it: [`GAME`], this crate's
//! [`Game`](ac3_machine::Game) (SLPS-02020/02021, Rev 1, two discs;
//! JP_PORT_PLAN §1.1, §1.4, §2.1, §6).
//!
//! The generated [`crate::game`] is the PAL module re-addressed. Its
//! [`LAYOUT`](crate::game::LAYOUT) names the JP words and is reused here;
//! its release facts (name, video standard, discs, modules, states, the
//! payload size) are PAL's, because the generator maps addresses, not
//! facts. This module states those facts for JP and goes through the JP
//! crate for everything the host runs: the boot ([`crate::boot`]), the main
//! loop pass, libcd's interrupt handler and libpad's VBlank handler
//! ([`crate::mainloop`]).
//!
//! **The layout, checked against the JP code.** Every word of
//! [`LAYOUT`](crate::game::LAYOUT) was compared with the JP instructions
//! (`mips.normalize` of every aligned exact or near function pair of
//! `jp_pal_map.tsv`: the PAL instruction naming the PAL word, and the same
//! instruction of the JP function, give the JP word; 2–220 sites each, no
//! disagreement):
//!
//! | Word | PAL | JP | Sites (examples) |
//! | --- | --- | --- | --- |
//! | VBlank counter | 800B3FF4 | 800B6174 | 8 (VSync) |
//! | VSync's record | 800B2EC8 | 800B5048 | 3 (VSync) |
//! | libpad state | 800B253C | 800B46BC | 7 (PadEnableCom) |
//! | libpad records pointer | 800B2538 | 800B46B8 | 10 |
//! | libpad records | 800C5A90 | 800C7CB8 | 4 (PadInitDirect) |
//! | libcd file cache | 800C5CD0 | 800C7EF8 | 2 (CdSearchFile) |
//! | pad block `[gp+0x10]` | 800BAA04 | 800BCB84 | 15 |
//! | context `[gp+0x2EC]` (PAL gp+0x2DC) | 800BACD0 | 800BCE60 | 220 |
//!
//! The context's state words keep their offsets (+0x2C..+0x38): the state
//! dispatcher (JP 80026350 = PAL 80025EB0) and `main` (JP 800261A8 = PAL
//! 80025D08) are exact matches.
//!
//! **Field rate.** NTSC, [`FieldRate::NTSC`]: 60000/1001 = 59.94 fields
//! per second. See [`Release::field_rate`] for the number and its source.
use crate::{boot, common, game::LAYOUT, machine::Machine, mainloop};
use ac3_machine::game::{Executable, FieldRate, Game, Layout, Overlays, VideoStandard};
use ac3_machine::sio::Sio;

/// The release: the Japanese one (SLPS-02020 disc 1, SLPS-02021 disc 2).
pub const NAME: &str = "JP";
pub const VIDEO: VideoStandard = VideoStandard::Ntsc;
pub const DISCS: u32 = 2;
/// The resident `ACE3.BIN`: a plain PS-X EXE whose 0xAD000-byte payload
/// loads at 0x80010000 (its zero tail from 0x800BCE40 is the start of the
/// BSS, which ends at 0x800CBFE8).
pub const PAYLOAD_ADDRESS: u32 = 0x8001_0000;
pub const PAYLOAD_BYTES: usize = 0xad000;
/// The resident entry point (`start`).
pub const ENTRY: u32 = 0x8002_572c;
/// Overlay modules (BPB entries 1..=4, all loaded at
/// [`common::OVERLAY_BASE`] 0x800CBFE8), and the state table loading them
/// (9 records; PAL has 10).
pub const MODULES: u32 = 4;
pub const STATE_TABLE: u32 = 0x800b_0134;
pub const STATES: u32 = 9;

/// The Japanese release (a unit type; [`GAME`] is its value).
#[derive(Clone, Copy, Debug, Default)]
pub struct Release;

/// The game of the Japanese release, for the host
/// (`&ac3_flight_jp::jp::game::GAME`).
pub static GAME: Release = Release;

impl Game for Release {
    fn name(&self) -> &'static str {
        NAME
    }
    fn video(&self) -> VideoStandard {
        VIDEO
    }
    /// 60000/1001 Hz (59.94), [`FieldRate::NTSC`].
    ///
    /// The source: NTSC is 525 lines at 30000/1001 frames per second, i.e.
    /// 60000/1001 fields per second (SMPTE 170M / ITU-R BT.470 System M).
    /// The PlayStation's NTSC video clock is 15 × the 315/88 MHz colour
    /// subcarrier (53.693182 MHz, psx-spx "GPU Timings"); with the
    /// standard line of 227.5 subcarrier cycles (3412.5 video clocks) and
    /// 262.5 lines per field, an interlaced NTSC picture runs at exactly
    /// 60000/1001 Hz. In 240-line progressive mode the GPU draws 263 lines
    /// per field instead, 0.19 % slower (about 59.83 Hz; psx-spx gives
    /// "ca. 59.82 Hz" from 263 lines of 3413 clocks).
    ///
    /// PAL is chosen the same way: the port runs PAL at exactly 50 Hz, the
    /// PAL standard's field rate and its interlaced hardware rate, not the
    /// slightly slower progressive rate (psx-spx: ca. 49.76 Hz). So JP
    /// takes NTSC's standard rate. The game's own timing does not depend
    /// on the choice: it counts VBlanks.
    fn field_rate(&self) -> FieldRate {
        FieldRate::NTSC
    }
    fn discs(&self) -> u32 {
        DISCS
    }
    fn executable(&self) -> Executable {
        Executable {
            load_address: PAYLOAD_ADDRESS,
            bytes: PAYLOAD_BYTES,
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
        // `boot::boot` copies the whole payload it is given (0xAD000
        // bytes) and runs JP `start` (8002572C) through main's prologue.
        boot::boot(m, payload);
    }
    fn main_loop_pass(&self, m: &mut Machine) {
        mainloop::main_loop_iteration(m);
    }
    fn cd_service(&self, m: &mut Machine) -> u32 {
        mainloop::pending_sdk::cd_interrupt_handler(m)
    }
    fn pad_vblank(&self, ram: &mut [u8], sio: &mut Sio) {
        mainloop::libpad::vblank(ram, sio);
    }
    fn in_flight(&self, m: &Machine) -> bool {
        // Flight is resident state 2, as in PAL (the state table's record 2
        // is the resident flight callbacks, JP_PORT_PLAN §2.1).
        self.states(m).state == 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_facts_are_japanese() {
        assert_eq!(GAME.name(), "JP");
        assert_eq!(GAME.video(), VideoStandard::Ntsc);
        assert_eq!(GAME.field_rate(), FieldRate::NTSC);
        assert_eq!(GAME.field_rate().rounded_hz(), 60);
        assert_eq!(GAME.discs(), 2);
        let exe = GAME.executable();
        assert_eq!(
            (exe.load_address, exe.bytes, exe.entry),
            (0x8001_0000, 0xad000, 0x8002_572c)
        );
        // The BSS `start` clears covers the payload's zero tail and ends
        // where the overlays load.
        let end = exe.load_address + exe.bytes as u32;
        assert!((boot::BSS_START..end).contains(&0x800b_cfff));
        assert_eq!(boot::BSS_END, common::OVERLAY_BASE);
        let overlays = GAME.overlays();
        assert_eq!(
            (
                overlays.base,
                overlays.modules,
                overlays.state_table,
                overlays.states
            ),
            (0x800c_bfe8, 4, 0x800b_0134, 9)
        );
        // The generated module states the PAL facts; only its addresses are
        // JP. This module must not drift from them.
        assert_eq!(crate::game::ENTRY, ENTRY);
        assert_eq!(crate::game::STATE_TABLE, STATE_TABLE);
    }

    #[test]
    fn the_layout_names_the_jp_words() {
        let layout = GAME.layout();
        assert_eq!(layout.gp, 0x800b_cb74);
        assert_eq!(layout.vblank_counter, 0x800b_6174);
        assert_eq!(layout.vsync_record, 0x800b_5048);
        assert_eq!(layout.pad_state, 0x800b_46bc);
        assert_eq!(layout.pad_records_pointer, 0x800b_46b8);
        assert_eq!(layout.pad_records, 0x800c_7cb8);
        assert_eq!(layout.pad_block, 0x800b_cb84);
        assert_eq!(layout.context, 0x800b_ce60);
        assert_eq!(layout.cd_file_cache, 0x800c_7ef8);
        let fields = layout.context_fields;
        assert_eq!(
            (
                fields.state,
                fields.substate,
                fields.requested_module,
                fields.loaded_module
            ),
            (0x2c, 0x30, 0x34, 0x38)
        );
        assert_eq!(layout.vblank_clock(), crate::cdstream::CLOCK);
        let mut m = Machine::default();
        m.sw(layout.context, 0x8010_0000);
        m.sw(0x8010_002c, 2);
        m.sw(0x8010_0038, 0xffff_ffff);
        let states = GAME.states(&m);
        assert_eq!(states.state, 2);
        assert_eq!(states.loaded_module, 0xffff_ffff);
        assert!(GAME.in_flight(&m));
        m.sw(0x8010_002c, 4);
        assert!(!GAME.in_flight(&m));
    }
}
