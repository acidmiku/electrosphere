//! PsyQ SDK routines of the JP resident that no wave part owns and the
//! patch layer calls, translated from the JP instructions (`ACE3.BIN`):
//! strcmp (800ADE38, exact; untranslated in PAL), LoadImage2 (800AA8E0,
//! near: only the address of its debug name differs), CdFlush and CD_flush
//! (800A2F50, 800A43F8), PadStopCom (8009CD50), and the JP-only GetClut
//! (800AC708), SetDrawLoad (800AC788), DecDCTBufSize (800AD398),
//! ResetRCnt's shared epilogue (800AF5EC) and the BIOS stub Krom2RawAdd
//! (800AF438). The game routines of the same part are in
//! [`res_sdk_game`](super::res_sdk_game) and
//! [`res_sdk_format`](super::res_sdk_format).
//!
//! Device work follows the PAL crate's conventions: LoadImage2 reaches the
//! VRAM through libgpu's LoadImage transfer (`sdk`, host call
//! `LOAD_IMAGE`), as the generated StoreImage2 does; PadStopCom is the host
//! call `PAD_STOP_COM`, as PadStartCom is `PAD_START_COM`; CD_flush's
//! register accesses are device only, as in the generated CD_init.
//!
//! Every routine is `pub`, so patch code can call it natively; [`dispatch`]
//! claims exactly these addresses.
use crate::machine::Machine;
use crate::mainloop::pending_sdk::{CD_END_STATUS, CD_READY_STATUS, CD_SYNC_STATUS};
use crate::{dispatch, host_ids, sdk};

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

/// 800ADE38 (JP; PAL 800AC268, exact) strcmp(a, b): the difference of the
/// first differing characters (signed bytes, `a`'s minus `b`'s), 0 for
/// equal strings. A null pointer compares below a string: 0 when both are
/// null, -1 when `a` is, 1 when `b` is.
pub fn strcmp(m: &mut Machine, a: u32, b: u32) -> u32 {
    if a == 0 || b == 0 {
        return if a == b {
            0
        } else if a == 0 {
            -1i32 as u32
        } else {
            1
        };
    }
    let (mut a, mut b) = (a, b);
    loop {
        let right = m.lb(b);
        let left = m.lb(a);
        let end = m.lbu(a);
        b = b.wrapping_add(1);
        if left != right {
            let left = i32::from(m.lb(a));
            let right = i32::from(m.lb(b.wrapping_sub(1)));
            return left.wrapping_sub(right) as u32;
        }
        a = a.wrapping_add(1);
        if end == 0 {
            return 0;
        }
    }
}

/// 800BB95C: pointer to libgpu's low-level function table.
const GPU_TABLE: u32 = 0x800b_b95c;
/// 80025278: "LoadImage2", the name libgpu's argument trace prints.
const LOAD_IMAGE2_NAME: u32 = 0x8002_5278;
/// 800A81FC: libgpu's argument trace `(name, rect)`.
const GPU_CHECK_RECT: u32 = 0x800a_81fc;
/// 800AACF8: the DMA channel 2 callback LoadImage2 installs (a routine
/// address stored in RAM; the port runs no DMA completion callbacks).
const GPU_DMA_DONE: u32 = 0x800a_acf8;

/// 800AA8E0 (JP; PAL 800A8DB0, near: the debug name's address) LoadImage2
/// (RECT* rect, u_long* pixels): LoadImage without libgpu's queue, like the
/// generated StoreImage2. Traces the rectangle (800A81FC), arms the DMA
/// timeout (VSync(-1) + 0xF0, 0 polls), waits until DMA channel 2 is idle
/// and the GPU takes commands (device only: the host GPU is ready at once;
/// the poll's timeout path, 800AA6FC's -1, needs a GPU that never gets
/// ready), installs DMACallback(2, 800AACF8) and runs libgpu's table +0x20
/// transfer (800A995C: the host call LOAD_IMAGE `[rect, pixels]`). Returns
/// 0.
pub fn load_image2(m: &mut Machine, rect: u32, pixels: u32) -> u32 {
    m.push_frame(0x20);
    dispatch::call(m, GPU_CHECK_RECT, [LOAD_IMAGE2_NAME, rect, 0, 0]);
    let now = sdk::vsync(m, -1i32 as u32);
    m.sw(sdk::GPU_TIMEOUT_DEADLINE, now.wrapping_add(0xf0));
    m.sw(sdk::GPU_TIMEOUT_POLLS, 0);
    sdk::dma_callback(m, 2, GPU_DMA_DONE);
    let table = m.lw(GPU_TABLE);
    let transfer = m.lw(at(table, 0x20));
    dispatch::call(m, transfer, [rect, pixels, 0, 0]);
    m.pop_frame(0x20);
    0
}

/// 800AC708 (JP only) GetClut(x, y): the CLUT field of a primitive for a
/// CLUT at VRAM `x`, `y` (`y << 6 | x >> 4 & 0x3F`, 16 bits).
pub fn get_clut(x: u32, y: u32) -> u32 {
    ((y << 6) | (((x as i32) >> 4) as u32 & 0x3f)) & 0xffff
}

/// 800AC788 (JP only) SetDrawLoad(DR_LOAD* p, RECT* r): a GP0(A0h)
/// CPU-to-VRAM packet for the RECT's pixels inline: length byte (+3) the
/// words of `(w * h + 1) / 2` pixels plus 4 when there are at most 12 of
/// them, else 0; +4 the command 0xA0000000, +8/+0xC the RECT's position and
/// size words; and 0x01000000 at the word after the packet (`p` + 4 x the
/// length: over the tag itself for a length of 0). Returns the address of
/// that word.
pub fn set_draw_load(m: &mut Machine, packet: u32, rect: u32) -> u32 {
    let w = i32::from(m.lh(at(rect, 4)));
    let h = i32::from(m.lh(at(rect, 6)));
    let rounded = w.wrapping_mul(h).wrapping_add(1);
    let words = rounded.wrapping_add(((rounded as u32) >> 31) as i32) >> 1;
    let length = if (words as u32) < 0xd {
        words.wrapping_add(4) as u32
    } else {
        0
    };
    m.sb(at(packet, 3), length as u8);
    m.sw(at(packet, 4), 0xa000_0000);
    let position = m.lw(rect);
    m.sw(at(packet, 8), position);
    let size = m.lw(at(rect, 4));
    m.sw(at(packet, 0xc), size);
    let end = at(packet, length << 2);
    m.sw(end, 0x0100_0000);
    end
}

/// 800AD398 (JP only) DecDCTBufSize(u_long* bs): the low halfword of the
/// bitstream's first word (its length in words).
pub fn dec_dct_buf_size(m: &mut Machine, bitstream: u32) -> u32 {
    u32::from(m.lhu(bitstream))
}

/// 800AF5EC (JP only, `jr ra; nop`): the epilogue ResetRCnt (800AF5C0)
/// jumps to, a routine of its own in the JP map. It writes nothing and
/// leaves the caller's `v0`.
pub fn reset_rcnt_epilogue(_m: &mut Machine) {}

/// 800AF438 (JP only) Krom2RawAdd(sjis), BIOS B0h:51h: the address of the
/// 16x15 kanji ROM glyph of a Shift-JIS code, or -1 for a code the ROM
/// lacks. The port has no BIOS ROM (none is shipped or read): every code is
/// one the ROM lacks, -1. Its only caller is overlay 1's glyph routine
/// 800D783C (from 800D7868), which no code reaches; with -1 it uploads a
/// blank cell.
pub fn krom2_raw_add(_sjis: u32) -> u32 {
    u32::MAX
}

/// 800A43F8 (JP; PAL 800A28C8, exact) CD_flush: libcd's drain of the
/// drive's pending interrupts, as the generated CD_init does it: the index
/// register, the acknowledge loop over the interrupt flag (bits 0..2) and
/// the closing index, interrupt-enable and COM_DELAY (0x1325) writes are
/// device only (the host drive delivers its interrupts through libcd's
/// handler); RAM: the status bytes end 0, ready = end, sync 2. Returns
/// 0x1325 (the COM_DELAY value).
pub fn cd_flush_drive(m: &mut Machine) -> u32 {
    m.sb(CD_END_STATUS, 0);
    let end = m.lbu(CD_END_STATUS);
    m.sb(CD_READY_STATUS, end);
    m.sb(CD_SYNC_STATUS, 2);
    0x1325
}

/// 800A2F50 (JP; PAL 800A1420, exact) CdFlush(): [`cd_flush_drive`]
/// (800A43F8); returns its result.
pub fn cd_flush(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let v0 = cd_flush_drive(m);
    m.pop_frame(0x18);
    v0
}

/// 8009CD50 (JP; PAL 8009B220, exact) PadStopCom(): _padStopCom
/// (8009BD2C): in a critical section, ChangeClearRCnt(3, 1) and
/// SysDeqIntRP(2, 800C7BF8), removing libpad's VBlank handler. That is
/// kernel and interrupt state only, so it is the host call `PAD_STOP_COM`
/// (the host stops running libpad's VBlank handler), as PadStartCom is
/// `PAD_START_COM`. No RAM is written; the original's `v0` is the
/// kernel's.
pub fn pad_stop_com(m: &mut Machine) {
    m.host_call(host_ids::PAD_STOP_COM, [0; 4]);
}

/// The routines of this file by JP address.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, ..] = args;
    Some(match target {
        0x800a_de38 => strcmp(m, a0, a1),
        0x800a_a8e0 => load_image2(m, a0, a1),
        0x800a_c708 => get_clut(a0, a1),
        0x800a_c788 => set_draw_load(m, a0, a1),
        0x800a_d398 => dec_dct_buf_size(m, a0),
        0x800a_f5ec => {
            reset_rcnt_epilogue(m);
            0
        }
        0x800a_f438 => krom2_raw_add(a0),
        0x800a_43f8 => cd_flush_drive(m),
        0x800a_2f50 => cd_flush(m),
        0x8009_cd50 => {
            pad_stop_com(m);
            0
        }
        _ => return None,
    })
}
