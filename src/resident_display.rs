//! Resident 80025720: frame-start processing of display-environment requests.
//!
//! The main loop runs this before state dispatch. Request bits set by callbacks
//! (`LoadingRenderStatus::flags`, original display word +0) are consumed here.
//! Bit 1 selects the display pair and defers bit 0x40, which selects the
//! drawing pair on the following frame. Bits 4/8 disable/enable the background
//! clear of all three drawing environments, using RGB +0x10..+0x12.
//! Offsets (bit 2) and custom environments (0x10/0x20) are not on the
//! connected path and fail explicitly instead of being approximated.
use crate::{Result, flight_loader::LoadingRenderStatus};

pub const SELECT_DISPLAY: u32 = 1;
pub const SET_OFFSETS: u32 = 2;
pub const DISABLE_CLEAR: u32 = 4;
pub const ENABLE_CLEAR: u32 = 8;
pub const CUSTOM_DRAW: u32 = 0x10;
pub const CUSTOM_DISPLAY: u32 = 0x20;
pub const SELECT_DRAW: u32 = 0x40;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackgroundClear {
    /// Drawing environments retain state from before the connected path.
    #[default]
    Inherited,
    Disabled,
    Enabled([u8; 3]),
}

/// Selections use the status phase (+4). Phases other than 0/1 leave the
/// previous selection unchanged, as in the original branches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResidentDisplay {
    pub display_pair: Option<u32>,
    pub draw_pair: Option<u32>,
    pub clear: BackgroundClear,
}

impl ResidentDisplay {
    pub fn update(&mut self, status: &mut LoadingRenderStatus) -> Result<()> {
        let mut flags = status.flags;
        if flags == 0 {
            return Ok(());
        }
        if flags & (SET_OFFSETS | CUSTOM_DRAW | CUSTOM_DISPLAY) != 0 {
            return Err("display offset/custom environment requests are not ported".into());
        }
        if flags & SELECT_DRAW != 0 {
            flags &= !SELECT_DRAW;
            if status.phase <= 1 {
                self.draw_pair = Some(status.phase);
            }
        }
        if flags & SELECT_DISPLAY != 0 {
            flags = (flags & !SELECT_DISPLAY) | SELECT_DRAW;
            if status.phase <= 1 {
                self.display_pair = Some(status.phase);
            }
        }
        if flags & DISABLE_CLEAR != 0 {
            flags &= !DISABLE_CLEAR;
            self.clear = BackgroundClear::Disabled;
        }
        if flags & ENABLE_CLEAR != 0 {
            flags &= !ENABLE_CLEAR;
            let [r, g, b, _] = status.background;
            self.clear = BackgroundClear::Enabled([r, g, b]);
        }
        status.flags = flags;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn status(flags: u32, phase: u32) -> LoadingRenderStatus {
        LoadingRenderStatus {
            flags,
            phase,
            background: [1, 12, 3, 9],
            parameter: 0,
        }
    }
    #[test]
    fn display_request_defers_draw_selection_one_frame() {
        let mut display = ResidentDisplay::default();
        let mut s = status(SELECT_DISPLAY | ENABLE_CLEAR, 0);
        display.update(&mut s).unwrap();
        assert_eq!(s.flags, SELECT_DRAW);
        assert_eq!(display.display_pair, Some(0));
        assert_eq!(display.draw_pair, None);
        assert_eq!(display.clear, BackgroundClear::Enabled([1, 12, 3]));
        display.update(&mut s).unwrap();
        assert_eq!((s.flags, display.draw_pair), (0, Some(0)));
    }
    #[test]
    fn enable_follows_disable_and_unknown_phase_keeps_selection() {
        let mut display = ResidentDisplay::default();
        let mut s = status(SELECT_DISPLAY | DISABLE_CLEAR | ENABLE_CLEAR | 0x80, 7);
        display.update(&mut s).unwrap();
        assert_eq!(s.flags, 0x80 | SELECT_DRAW);
        assert_eq!(display.display_pair, None);
        assert_eq!(display.clear, BackgroundClear::Enabled([1, 12, 3]));
        let mut s = status(DISABLE_CLEAR, 1);
        display.update(&mut s).unwrap();
        assert_eq!(display.clear, BackgroundClear::Disabled);
    }
    #[test]
    fn unported_requests_fail_without_mutation() {
        for bit in [SET_OFFSETS, CUSTOM_DRAW, CUSTOM_DISPLAY] {
            let mut display = ResidentDisplay::default();
            let mut s = status(bit | SELECT_DISPLAY, 0);
            assert!(display.update(&mut s).is_err());
            assert_eq!(s.flags, bit | SELECT_DISPLAY);
            assert_eq!(display, ResidentDisplay::default());
        }
    }
}
