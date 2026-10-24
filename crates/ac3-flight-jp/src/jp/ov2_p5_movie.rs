//! JP overlay 2, part 5: the movie (MDEC) player, 800FC5A8..800FD48C, the
//! movie quad builder (800FD48C) and the two step routines 800FD634 and
//! 800FD698, translated from the JP instructions.
//!
//! The player state [`PLAYER`] (`[0x800BD044]`) runs the steps of the jump
//! table 0x800CD690 in [`movie_update`]; [`movie_callback`] (800FD408) is
//! the state callback that creates it ([`movie_init`]), runs it, and frees
//! the playback block ([`movie_finish`]), switching on `[HUB]+0x114`
//! (0x4000 create, 0x8000 run, 0x10000 finish).
//!
//! The playback block [`MOVIE`] (`[0x80112E18]`) holds: +0 the run-level
//! buffer toggle, +1 the image slice (0..9), +2 the display buffer toggle,
//! +3 the frame-decoded flag, +4 the end flag, +5 the 24-bit mode, +8/+0xC
//! the two run-level buffers, +0x10..+0x34 the ten slice buffers,
//! +0x38/+0x40 the two display rectangles (x, y, w, h), +0x48 the slice
//! rectangle, +0x50 the VLC table, +0x11050/+0x11052 the frame width and
//! height, +0x11054 the frame number, +0x11056 the slice size, +0x11058..
//! +0x11060 the stream parameters, +0x11064 the started flag, +0x11068 the
//! time-out counter, +0x1106C the buffers.
use super::ov2_p5::{
    MODULE, SOUND, add_prim, alloc, call, ctx, framed, hub, lbu, lh, lhu, lw, sb, sh, slt, sra, sw,
    sx16,
};
use crate::common::loaded_module;
use crate::machine::Machine;

/// `[0x80112E18]`: the movie playback block (see the module doc).
pub const MOVIE: u32 = 0x8011_2e18;
/// `[0x80112E14]`: the sector header of the current frame (written by
/// 8002B9EC).
pub const HEADER: u32 = 0x8011_2e14;
/// `[0x800BD044]` (gp+0x4D0): the movie player state (0x44 bytes): +0 the
/// step, +4/+8 two 0x400-byte buffers, +0x24/+0x28 two (x, y) display
/// positions, +0x2C/+0x2E the width and height, +0x30/+0x32 the offset,
/// +0x34 the stream position, +0x38 the 24-bit flag, +0x40.
pub const PLAYER: u32 = 0x800b_d044;

/// 800FC5A8: reset the MDEC (800AC898(0)), install the DecDCTout callback
/// 800FCB44 (800ACB34), build the VLC table at `MOVIE + 0x50` (800AD2A8),
/// clear the end flag +4 and set +3. Returns 1.
pub fn mdec_start(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        call(m, 0x800a_c898, [0, 0, 0, 0]);
        call(m, 0x800a_cb34, [0x800f_cb44, 0, 0, 0]);
        let movie = m.lw(MOVIE);
        call(m, 0x800a_d2a8, [movie.wrapping_add(0x50), 0, 0, 0]);
        let movie = m.lw(MOVIE);
        sb(m, movie, 4, 0);
        let movie = m.lw(MOVIE);
        sb(m, movie, 3, 1);
        1
    })
}

/// 800FC600: set up the playback block from the parameter record `a0`
/// (+0 the 24-bit flag, +4/+8/+0xC the stream parameters, +0x10/+0x14 the
/// run-level buffers, +0x18.. the ten slice buffers, +0x40..+0x46 the
/// display rectangle) and reset its counters. Returns `MOVIE + 0x10000`.
pub fn movie_setup(m: &mut Machine, a0: u32) -> u32 {
    let p = a0;
    let movie = m.lw(MOVIE);
    let high = movie.wrapping_add(0x1_0000);
    for (from, to) in [(4u32, 0x1058u32), (8, 0x105c), (0xc, 0x1060)] {
        let v = lw(m, p, from);
        sw(m, high, to, v);
    }
    let v = lw(m, p, 0x10);
    sw(m, movie, 8, v);
    let v = lw(m, p, 0x14);
    sw(m, movie, 0xc, v);
    for i in 0..10u32 {
        let v = lw(m, p, 0x18 + 4 * i);
        sw(m, movie, 0x10 + 4 * i, v);
    }
    let movie = m.lw(MOVIE);
    let mode = lbu(m, p, 0);
    sb(m, movie, 5, mode);
    let movie = m.lw(MOVIE);
    for (from, to) in [(0x40u32, 0x38u32), (0x42, 0x3a), (0x44, 0x40)] {
        let v = lhu(m, p, from);
        sh(m, movie, to, v);
    }
    let v = lhu(m, p, 0x46);
    for at in [0x3c, 0x3e, 0x44, 0x46] {
        sh(m, movie, at, 0);
    }
    sh(m, movie, 0x42, v);
    let v = lhu(m, p, 0x40);
    sh(m, movie, 0x48, v);
    let v = lhu(m, p, 0x42);
    sh(m, movie, 0x4a, v);
    let width = if lw(m, p, 0) != 0 { 0x18 } else { 0x10 };
    sh(m, movie, 0x4c, width);
    let movie = m.lw(MOVIE);
    sb(m, movie, 1, 0);
    sb(m, movie, 0, 0);
    let again = m.lw(MOVIE);
    sh(m, movie, 0x4e, 0);
    let high = again.wrapping_add(0x1_0000);
    sh(m, high, 0x1052, 0);
    sh(m, high, 0x1050, 0);
    sh(m, high, 0x1056, 0);
    sb(m, again, 2, 0);
    let movie = m.lw(MOVIE);
    sb(m, movie, 3, 0);
    let movie = m.lw(MOVIE);
    sb(m, movie, 4, 0);
    let movie = m.lw(MOVIE);
    let high = movie.wrapping_add(0x1_0000);
    sw(m, high, 0x1064, 0);
    sw(m, high, 0x1068, 0x12c);
    high
}

/// 800FC750: the stream-end callback: set the end flag `MOVIE + 4`.
/// Returns 1.
pub fn movie_end(m: &mut Machine) -> u32 {
    let movie = m.lw(MOVIE);
    sb(m, movie, 4, 1);
    1
}

/// 800FC764: open a movie: the playback block at `a2` (it becomes
/// [`MOVIE`]), the buffers after it (`a3` bytes of ring buffer, two
/// run-level buffers of the 5th argument's size, ten slices of the 6th's),
/// the stream position `a0`, the 24-bit flag `a1` and the display rectangle
/// (7th..10th arguments); then [`movie_setup`], [`mdec_start`] and
/// 8002C6A8(0, 0). Returns the end of the buffers.
pub fn movie_open(m: &mut Machine, a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {
    framed(m, 0x60, |m, sp| {
        let t0 = lw(m, sp, 0x70);
        let t1 = lw(m, sp, 0x74);
        m.sw(MOVIE, a2);
        let mut s0 = a2.wrapping_add(0x1_106c);
        let ring = (a3.wrapping_add(3) >> 2) << 2;
        sw(m, sp, 0x1c, s0);
        s0 = s0.wrapping_add(ring);
        sw(m, sp, 0x14, a0);
        sw(m, sp, 0x18, ring);
        sw(m, sp, 0x20, s0);
        let run_level = (t0.wrapping_add(3) >> 2) << 2;
        let slice = (t1.wrapping_add(3) >> 2) << 2;
        s0 = s0.wrapping_add(run_level);
        sw(m, sp, 0x24, s0);
        s0 = s0.wrapping_add(run_level);
        for i in 0..10u32 {
            sw(m, sp, 0x28 + 4 * i, s0);
            s0 = s0.wrapping_add(slice);
        }
        for (from, to) in [(0x78u32, 0x50u32), (0x7c, 0x52), (0x80, 0x54)] {
            let v = lw(m, sp, from);
            sh(m, sp, to, v);
        }
        let v = lw(m, sp, 0x84);
        sw(m, sp, 0x10, a1);
        sh(m, sp, 0x56, v);
        movie_setup(m, sp + 0x10);
        mdec_start(m);
        call(m, 0x8002_c6a8, [0, 0, 0, 0]);
        s0
    })
}

/// 800FC854: start the stream: 8002C928(+0x11058, +0x11060, +0x1105C,
/// 0x1000) with the end callback 800FC750 as the 5th argument. Returns its
/// `v0`.
pub fn movie_stream_start(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, sp| {
        sw(m, sp, 0x10, 0x800f_c750);
        let movie = m.lw(MOVIE);
        let high = movie.wrapping_add(0x1_0000);
        let a0 = lw(m, high, 0x1058);
        let a1 = lw(m, high, 0x1060);
        let a2 = lw(m, high, 0x105c);
        call(m, 0x8002_c928, [a0, a1, a2, 0x1000])
    })
}

/// 800FC89C: wait for the next frame ([`movie_poll`]): with `a0` != 0 poll
/// once and count the time-out +0x11068 down (below 0: set the end flag);
/// with `a0` == 0 poll up to 121 times with 8002C4DC and VSync(0) between,
/// then set the end flag. Returns 1 when a frame came (or the wait ran
/// out), else 0.
pub fn movie_poll_wait(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        if a0 != 0 {
            let movie = m.lw(MOVIE);
            if movie_poll(m, movie) != 0 {
                return 1;
            }
            let movie = m.lw(MOVIE);
            let high = movie.wrapping_add(0x1_0000);
            let count = lw(m, high, 0x1068).wrapping_sub(1);
            sw(m, high, 0x1068, count);
            if (count as i32) < 0 {
                sb(m, movie, 4, 1);
            }
            return 0;
        }
        // the counter is the stack local +0x10 (0x78 down to -1)
        let mut counter = 0x78u32;
        loop {
            let movie = m.lw(MOVIE);
            if movie_poll(m, movie) != 0 {
                return 1;
            }
            call(m, 0x8002_c4dc, [0; 4]);
            call(m, 0x800a_2098, [0, 0, 0, 0]);
            counter = counter.wrapping_sub(1);
            if (counter as i32) < 0 {
                break;
            }
        }
        let movie = m.lw(MOVIE);
        sb(m, movie, 4, 1);
        1
    })
}

/// 800FC970: decode the next frame: when [`movie_poll`] has one, clear +3,
/// start DecDCTin (800AC9F0) on the run-level buffer (mode +5 | 2) and
/// DecDCTout (800ACA6C) into the current slice, wait for it
/// ([`movie_wait_decode`] with `a0`) and reset the time-out to 300; else
/// count the time-out down (at 0: set the end flag). Returns 1 when the end
/// flag was already set, else 0.
pub fn movie_step(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let movie = m.lw(MOVIE);
        if lbu(m, movie, 4) != 0 {
            return 1;
        }
        if movie_poll(m, movie) != 0 {
            let movie = m.lw(MOVIE);
            sb(m, movie, 3, 0);
            let movie = m.lw(MOVIE);
            let run_level = lbu(m, movie, 0);
            let mode = lbu(m, movie, 5);
            let buffer = lw(m, movie.wrapping_add(run_level << 2), 8);
            call(m, 0x800a_c9f0, [buffer, mode | 2, 0, 0]);
            let movie = m.lw(MOVIE);
            let slice = lbu(m, movie, 1);
            let size = lh(m, movie.wrapping_add(0x1_0000), 0x1056);
            let buffer = lw(m, movie.wrapping_add(slice << 2), 0x10);
            call(m, 0x800a_ca6c, [buffer, size, 0, 0]);
            let movie = m.lw(MOVIE);
            movie_wait_decode(m, movie, a0);
            let movie = m.lw(MOVIE);
            sw(m, movie.wrapping_add(0x1_0000), 0x1068, 0x12c);
        } else {
            let movie = m.lw(MOVIE);
            let high = movie.wrapping_add(0x1_0000);
            let count = lw(m, high, 0x1068);
            if (count as i32) > 0 {
                sw(m, high, 0x1068, count.wrapping_sub(1));
            } else {
                sb(m, movie, 4, 1);
            }
        }
        0
    })
}

/// 800FCA70: with `a1` != 0 just read the frame-decoded flag `a0 + 3`;
/// else busy-wait (up to 0x40000 rounds, re-reading `[MOVIE]+4` each round)
/// until it is set: when the count runs out or the end flag is set, mark
/// the frame decoded, flip the display buffer +2 and move the slice
/// rectangle +0x48 to that buffer's corner. Returns the flag `a0 + 3`.
pub fn movie_wait_decode(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    framed(m, 8, |m, _| {
        if a1 != 0 {
            return lbu(m, a0, 3);
        }
        // the stack locals +0 (count, 0x40000) and +4 (end flag)
        let mut counter = 0x4_0000u32;
        let mut done = lbu(m, a0, 3);
        while done == 0 {
            let movie = m.lw(MOVIE);
            let end = lbu(m, movie, 4);
            counter = counter.wrapping_sub(1);
            if (counter as i32) <= 0 || end != 0 {
                let buffer = lbu(m, a0, 2);
                sb(m, a0, 3, 1);
                let buffer = u32::from(buffer == 0);
                sb(m, a0, 2, buffer);
                let x = lhu(m, a0.wrapping_add(buffer << 3), 0x38);
                let buffer = lbu(m, a0, 2);
                sh(m, a0, 0x48, x);
                let y = lhu(m, a0.wrapping_add(buffer << 3), 0x3a);
                sh(m, a0, 0x4a, y);
            }
            done = lbu(m, a0, 3);
        }
        done
    })
}

/// 800FCB44, the DecDCTout callback: LoadImage (800A8440) the finished
/// slice at the slice rectangle +0x48, step the slice (0..9) and the
/// rectangle's x by the slice width +0x4C; while inside the display
/// rectangle start DecDCTout (800ACA6C) on the next slice (returns its
/// `v0`), else mark the frame decoded, flip the display buffer and move the
/// slice rectangle to its corner (returns the new y).
pub fn movie_slice_done(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let movie = m.lw(MOVIE);
        let slice = lbu(m, movie, 1);
        let buffer = lw(m, movie.wrapping_add(slice << 2), 0x10);
        call(m, 0x800a_8440, [movie.wrapping_add(0x48), buffer, 0, 0]);
        let movie = m.lw(MOVIE);
        let slice = lbu(m, movie, 1);
        sb(m, movie, 1, if slice < 9 { slice + 1 } else { 0 });
        let movie = m.lw(MOVIE);
        let x = lhu(m, movie, 0x48);
        let width = lhu(m, movie, 0x4c);
        let x = x.wrapping_add(width);
        let buffer = lbu(m, movie, 2);
        sh(m, movie, 0x48, x);
        let rect = movie.wrapping_add(buffer << 3);
        let left = lh(m, rect, 0x38);
        let span = lh(m, rect, 0x3c);
        if slt(sx16(x), left.wrapping_add(span)) {
            let slice = lbu(m, movie, 1);
            let size = lh(m, movie.wrapping_add(0x1_0000), 0x1056);
            let buffer = lw(m, movie.wrapping_add(slice << 2), 0x10);
            return call(m, 0x800a_ca6c, [buffer, size, 0, 0]);
        }
        sb(m, movie, 3, 1);
        let movie = m.lw(MOVIE);
        let buffer = lbu(m, movie, 2) ^ 1;
        sb(m, movie, 2, buffer);
        let movie = m.lw(MOVIE);
        let buffer = lbu(m, movie, 2);
        let x = lhu(m, movie.wrapping_add(buffer << 3), 0x38);
        let buffer = lbu(m, movie, 2);
        sh(m, movie, 0x48, x);
        let y = lhu(m, movie.wrapping_add(buffer << 3), 0x3a);
        sh(m, movie, 0x4a, y);
        y
    })
}

/// 800FCC7C: fetch and decode the next frame of the playback block `a0`.
/// While `[HUB]+0x118` == 1 (idle): get the next frame (8002B9EC into the
/// local +0x10 and [`HEADER`]; none: return 0), clear it, take the frame
/// number, height (slice size, rectangle heights) and width (x1.5 in 24-bit
/// mode) from the header, flip the run-level buffer and start the VLC
/// decode (800AD398, 800ACF28, 800ACF58); when `[HUB]+0x118` == 0 continue
/// the VLC decode (800ACF58(0, 0, table)). A finished decode sets
/// `[HUB]+0x118` = 1 and frees the ring sector (8002BBE0); returns 1. An
/// unfinished one (800ACF58 returned 1) returns 0; another `[HUB]+0x118`
/// returns 1.
pub fn movie_poll(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x30, |m, sp| {
        let movie = a0;
        let hb = hub(m);
        let state = lw(m, hb, 0x118);
        if state == 1 {
            if call(m, 0x8002_b9ec, [sp + 0x10, HEADER, 0, 0]) != 0 {
                return 0;
            }
            let hb = hub(m);
            let header = m.lw(HEADER);
            sw(m, hb, 0x118, 0);
            if lhu(m, header, 6) == 0 {
                sb(m, movie, 4, 1);
            }
            let high = movie.wrapping_add(0x1_0000);
            let header = m.lw(HEADER);
            let height = lh(m, high, 0x1052);
            let frame = lhu(m, header, 6);
            sh(m, high, 0x1054, frame);
            let v1 = lh(m, header, 0xa);
            if height != v1 {
                sh(m, high, 0x1052, v1);
                let width = lh(m, movie, 0x4c);
                let size = v1.wrapping_mul(width);
                sh(m, high, 0x1056, sra(size, 1));
                sh(m, movie, 0x4e, v1);
                sh(m, movie, 0x46, v1);
                sh(m, movie, 0x3e, v1);
            }
            let header = m.lw(HEADER);
            let width = lh(m, high, 0x1050);
            let v0 = lh(m, header, 8);
            if width != v0 {
                sh(m, high, 0x1050, v0);
                let v1 = if lbu(m, movie, 5) != 0 {
                    sra((v0 << 1).wrapping_add(v0), 1)
                } else {
                    v0
                };
                sh(m, movie, 0x44, v1);
                sh(m, movie, 0x3c, v1);
            }
            let run_level = lbu(m, movie, 0) ^ 1;
            sb(m, movie, 0, run_level);
            let stream = lw(m, sp, 0x10);
            let size = call(m, 0x800a_d398, [stream, 0, 0, 0]);
            let size = if (size as i32) < 0 {
                size.wrapping_add(3)
            } else {
                size
            };
            call(m, 0x800a_cf28, [sra(size, 2).wrapping_add(1), 0, 0, 0]);
            let run_level = lbu(m, movie, 0);
            let stream = lw(m, sp, 0x10);
            let buffer = lw(m, movie.wrapping_add(run_level << 2), 8);
            let table = movie.wrapping_add(0x50);
            if call(m, 0x800a_cf58, [stream, buffer, table, 0]) == 1 {
                return 0;
            }
        } else {
            if state != 0 {
                return 1;
            }
            if call(m, 0x800a_cf58, [0, 0, movie.wrapping_add(0x50), 0]) == 1 {
                return 0;
            }
        }
        let header = m.lw(HEADER);
        let hb = hub(m);
        sw(m, hb, 0x118, 1);
        call(m, 0x8002_bbe0, [header, 0, 0, 0]);
        1
    })
}

/// 800FCE40: create the player: set bit 28 of context +0, clear
/// `[SOUND]+0x14`, allocate [`PLAYER`] (0x44 bytes) and its two 0x400-byte
/// buffers, set the default rectangles (0x340,0x100 / 0x340,0x180, 0xC0 x
/// 0x80), take the stream position from `[HUB]+0x12C` when set, clear the
/// two 0xC0 x 0x80 areas at (0x340, 0x100) and (0x340, 0x180) (ClearImage),
/// and set `[HUB]+0x114` = 0x8000 (run), `+0x118` = 1. Returns 1.
pub fn movie_init(m: &mut Machine) -> u32 {
    framed(m, 0x30, |m, sp| {
        let context = ctx(m);
        let flags = lw(m, context, 0);
        let sound = m.lw(SOUND);
        sw(m, context, 0, flags | 0x1000_0000);
        sw(m, sound, 0x14, 0);
        let block = alloc(m, 0x44);
        m.sw(PLAYER, block);
        let buffer = alloc(m, 0x400);
        let player = m.lw(PLAYER);
        sw(m, player, 4, buffer);
        let buffer = alloc(m, 0x400);
        let player = m.lw(PLAYER);
        sw(m, player, 8, buffer);
        let hb = hub(m);
        sw(m, player, 0, 0);
        for (at, v) in [
            (0x24u32, 0x340u32),
            (0x26, 0x100),
            (0x28, 0x340),
            (0x2a, 0x180),
            (0x2c, 0xc0),
            (0x2e, 0x80),
            (0x30, 0),
            (0x32, 0),
        ] {
            sh(m, player, at, v);
        }
        let position = lw(m, hb, 0x12c);
        if position != 0 {
            sw(m, player, 0x34, position);
        }
        let player = m.lw(PLAYER);
        sw(m, player, 0x38, 0);
        sw(m, player, 0x3c, 0);
        sw(m, player, 0x40, 0x78);
        sh(m, sp, 0x10, 0x340);
        sh(m, sp, 0x12, 0x100);
        sh(m, sp, 0x14, 0xc0);
        sh(m, sp, 0x16, 0x80);
        call(m, 0x800a_8318, [sp + 0x10, 0, 0, 0]);
        sh(m, sp, 0x10, 0x340);
        sh(m, sp, 0x12, 0x180);
        call(m, 0x800a_8318, [sp + 0x10, 0, 0, 0]);
        let hb = hub(m);
        sw(m, hb, 0x114, 0x8000);
        sw(m, hb, 0x118, 1);
        1
    })
}

/// 800FCF80: finish the movie: clear bit 28 of context +0, clear the two
/// 0xC0 x 0x80 areas at (0x340, 0x100) and (0x340, 0x180) (ClearImage), set
/// `[HUB]+0x128` = 1 and free [`MOVIE`] (80027538). Returns its `v0`.
pub fn movie_finish(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, sp| {
        let context = ctx(m);
        let flags = lw(m, context, 0) & 0xefff_ffff;
        sw(m, context, 0, flags);
        sh(m, sp, 0x12, 0x100);
        sh(m, sp, 0x14, 0xc0);
        sh(m, sp, 0x10, 0x340);
        sh(m, sp, 0x16, 0x80);
        call(m, 0x800a_8318, [sp + 0x10, 0, 0, 0]);
        sh(m, sp, 0x10, 0x340);
        sh(m, sp, 0x12, 0x180);
        call(m, 0x800a_8318, [sp + 0x10, 0, 0, 0]);
        let movie = m.lw(MOVIE);
        let hb = hub(m);
        sw(m, hb, 0x128, 1);
        call(m, 0x8002_7538, [movie, 0, 0, 0])
    })
}

/// The display block `[gp+0x2D4]` ([`SOUND`]) after the movie: +0x28 = 0,
/// +0x29 = the player's 24-bit flag, +0x20/+0x22 from +8/+0xC, +0x44 = 1,
/// +0x45..+0x47 = 0 (the same sequence at 800FD170 and 800FD368).
fn restore_display(m: &mut Machine) {
    let sound = m.lw(SOUND);
    sb(m, sound, 0x28, 0);
    let player = m.lw(PLAYER);
    let sound = m.lw(SOUND);
    let deep = lbu(m, player, 0x38);
    sb(m, sound, 0x29, deep);
    let sound = m.lw(SOUND);
    let x = lhu(m, sound, 8);
    let y = lhu(m, sound, 0xc);
    sh(m, sound, 0x20, x);
    sh(m, sound, 0x22, y);
    sb(m, sound, 0x44, 1);
    for at in [0x45, 0x46, 0x47] {
        let sound = m.lw(SOUND);
        sb(m, sound, at, 0);
    }
}

/// 800FD028: one step of the player (`[PLAYER]+0`, jump table 0x800CD690):
/// 0 open the movie (buffer from 80027478(0), [`movie_open`] with 0xC404
/// run-level and 0x1000/0x1800 slice bytes) and start the stream; 1 on
/// buffer 0 wait for the first frame ([`movie_poll_wait`]), then step 2,
/// 80056B1C and [`restore_display`]; at the end flag 80056E8C and step 3;
/// 2 play: the pad bits `context +0x12C & a0` set the end flag; at the end
/// flag 80056E8C and step 3; decode ([`movie_step`]) until started, then
/// at each decoded frame [`movie_wait_decode`], 80056E8C, 80056B1C and
/// clear the shown buffer's rectangle (step 3); otherwise 80056B1C and
/// [`restore_display`]; 3 step 4; 4 done. Returns 1 at step 4, else 0.
pub fn movie_update(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x40, |m, sp| {
        let player = m.lw(PLAYER);
        let step = lw(m, player, 0);
        // Jump table 0x800CD690 (step < 5): 0 800FD074, 1 800FD134,
        // 2 800FD204, 3 800FD3D0, 4 800FD3E4; any other step returns 0.
        match step {
            0 => {
                let buffer = call(m, 0x8002_7478, [0, 0, 0, 0]);
                sw(m, sp, 0x10, 0xc404);
                let first = m.lw(PLAYER);
                let deep = lw(m, first, 0x38);
                sw(m, sp, 0x14, if deep != 0 { 0x1800 } else { 0x1000 });
                let player = m.lw(PLAYER);
                for (at, from, offset) in [
                    (0x18u32, 0x24u32, 0x30u32),
                    (0x1c, 0x26, 0x32),
                    (0x20, 0x28, 0x30),
                    (0x24, 0x2a, 0x32),
                ] {
                    let v = lh(m, player, from);
                    let d = lh(m, player, offset);
                    sw(m, sp, at, v.wrapping_add(d));
                }
                let position = lw(m, first, 0x34);
                let deep = lw(m, first, 0x38);
                movie_open(m, position, deep, buffer, 0x1_2300);
                movie_stream_start(m);
                let player = m.lw(PLAYER);
                sw(m, player, 0, 1);
                0
            }
            1 => {
                let context = ctx(m);
                if lw(m, context, 4) == 0 && movie_poll_wait(m, 1) != 0 {
                    let player = m.lw(PLAYER);
                    sw(m, player, 0, 2);
                    call(m, 0x8005_6b1c, [0; 4]);
                    restore_display(m);
                }
                let movie = m.lw(MOVIE);
                if lbu(m, movie, 4) != 1 {
                    return 0;
                }
                call(m, 0x8005_6e8c, [0; 4]);
                let player = m.lw(PLAYER);
                sw(m, player, 0, 3);
                0
            }
            2 => {
                let context = ctx(m);
                if lw(m, context, 0x12c) & a0 != 0 {
                    let movie = m.lw(MOVIE);
                    sb(m, movie, 4, 1);
                }
                let movie = m.lw(MOVIE);
                if lbu(m, movie, 4) == 1 {
                    call(m, 0x8005_6e8c, [0; 4]);
                    let player = m.lw(PLAYER);
                    sw(m, player, 0, 3);
                }
                let movie = m.lw(MOVIE);
                if lw(m, movie.wrapping_add(0x1_0000), 0x1064) == 0 {
                    if movie_step(m, 1) != 0 {
                        let movie = m.lw(MOVIE);
                        sw(m, movie.wrapping_add(0x1_0000), 0x1064, 1);
                    }
                } else if lbu(m, movie, 3) == 1 {
                    movie_wait_decode(m, movie, 0);
                    call(m, 0x8005_6e8c, [0; 4]);
                    let index = call(m, 0x8005_6b1c, [0; 4]);
                    let player = m.lw(PLAYER);
                    let corner = player.wrapping_add(index << 2);
                    let x = lhu(m, corner, 0x24);
                    sh(m, sp, 0x28, x);
                    let y = lhu(m, corner, 0x26);
                    sh(m, sp, 0x2a, y);
                    let h = lhu(m, player, 0x2e);
                    sh(m, sp, 0x2e, h);
                    if lw(m, player, 0x38) != 0 {
                        let w = lh(m, player, 0x2c);
                        sh(m, sp, 0x2c, sra((w << 1).wrapping_add(w), 1));
                    } else {
                        let w = lhu(m, player, 0x2c);
                        sh(m, sp, 0x2c, w);
                    }
                    call(m, 0x800a_8318, [sp + 0x28, 0, 0, 0]);
                    let player = m.lw(PLAYER);
                    sw(m, player, 0, 3);
                    return 0;
                }
                call(m, 0x8005_6b1c, [0; 4]);
                restore_display(m);
                0
            }
            3 => {
                let player = m.lw(PLAYER);
                sw(m, player, 0, 4);
                0
            }
            4 => 1,
            _ => 0,
        }
    })
}

/// 800FD408, the movie state callback: `[HUB]+0x114` 0x4000 creates the
/// player ([`movie_init`], returns 0), 0x10000 finishes it
/// ([`movie_finish`], returns 1); otherwise one [`movie_update`] step with
/// the pad mask `a0` (passed through), and when it is done set
/// `[HUB]+0x114` = 0x10000. Returns 0.
pub fn movie_callback(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let hb = hub(m);
        let phase = lw(m, hb, 0x114);
        if phase == 0x4000 {
            movie_init(m);
            return 0;
        }
        if phase == 0x1_0000 {
            movie_finish(m);
            return 1;
        }
        if movie_update(m, a0) != 0 {
            let hb = hub(m);
            sw(m, hb, 0x114, 0x1_0000);
        }
        0
    })
}

/// 800FD48C: a POLY_FT4 packet (code 0x2C, grey 0x80, tpage 0x11D, no
/// CLUT) at `a0` for the rectangle `a2` (x, y, w, h halfwords) and the
/// texture rectangle `a3` (u +0, v +2, w +4, h +6 bytes; v moved 128 lines
/// down for display buffer 0), linked into the ordering-table word `a1`.
/// Returns the next packet (`a0 + 0x28`).
pub fn movie_quad(m: &mut Machine, a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {
    let (p, ot, rect, uv) = (a0, a1, a2, a3);
    sb(m, p, 3, 9);
    sb(m, p, 7, 0x2c);
    sb(m, p, 4, 0x80);
    sb(m, p, 5, 0x80);
    sb(m, p, 6, 0x80);
    let x = lhu(m, rect, 0);
    sh(m, p, 8, x);
    let y = lhu(m, rect, 2);
    sh(m, p, 0xa, y);
    let x = lhu(m, rect, 0);
    let w = lhu(m, rect, 4);
    sh(m, p, 0x10, x.wrapping_add(w));
    let y = lhu(m, rect, 2);
    sh(m, p, 0x12, y);
    let x = lhu(m, rect, 0);
    sh(m, p, 0x18, x);
    let y = lhu(m, rect, 2);
    let h = lhu(m, rect, 6);
    sh(m, p, 0x1a, y.wrapping_add(h));
    let x = lhu(m, rect, 0);
    let w = lhu(m, rect, 4);
    sh(m, p, 0x20, x.wrapping_add(w));
    let y = lhu(m, rect, 2);
    let h = lhu(m, rect, 6);
    sh(m, p, 0x22, y.wrapping_add(h));
    // the texture rows: (1 - [MOVIE]+2) * 128 lines down
    let offset = |buffer: u32| 1u32.wrapping_sub(buffer) << 7;
    let u = lbu(m, uv, 0);
    sb(m, p, 0xc, u);
    let movie = m.lw(MOVIE);
    let buffer = lbu(m, movie, 2);
    let v = lbu(m, uv, 2);
    sb(m, p, 0xd, v.wrapping_add(offset(buffer)));
    let u = lbu(m, uv, 0);
    let w = lbu(m, uv, 4);
    sb(m, p, 0x14, u.wrapping_add(w));
    let movie = m.lw(MOVIE);
    let buffer = lbu(m, movie, 2);
    let v = lbu(m, uv, 2);
    sb(m, p, 0x15, v.wrapping_add(offset(buffer)));
    let u = lbu(m, uv, 0);
    sb(m, p, 0x1c, u);
    let movie = m.lw(MOVIE);
    let h = lbu(m, uv, 6);
    let buffer = lbu(m, movie, 2);
    let v = lbu(m, uv, 2);
    sb(m, p, 0x1d, h.wrapping_add(v.wrapping_add(offset(buffer))));
    let u = lbu(m, uv, 0);
    let w = lbu(m, uv, 4);
    sb(m, p, 0x24, u.wrapping_add(w));
    let movie = m.lw(MOVIE);
    let v = lbu(m, uv, 2);
    let h = lbu(m, uv, 6);
    let buffer = lbu(m, movie, 2);
    sh(m, p, 0x16, 0x11d);
    sb(m, p, 0x25, h.wrapping_add(v.wrapping_add(offset(buffer))));
    add_prim(m, ot, p);
    p.wrapping_add(0x28)
}

/// 800FD634: reset the hub step bytes +0x7C/+0x7D; with context +0x96C
/// bit 5 set the mission (context +0x38C) to 0x3B, else with bit 3 set the
/// hub phase +0x1C6 to 3. Returns NEXT_SUBSTATE (0x04000000).
pub fn steps_initialize(m: &mut Machine) -> u32 {
    let hb = hub(m);
    sb(m, hb, 0x7c, 0);
    let hb = hub(m);
    sb(m, hb, 0x7d, 0);
    let context = ctx(m);
    let flags = lw(m, context, 0x96c);
    if flags & 0x20 != 0 {
        sh(m, context, 0x38c, 0x3b);
    } else if flags & 8 != 0 {
        let hb = hub(m);
        sh(m, hb, 0x1c6, 3);
    }
    0x0400_0000
}

/// 800FD698: unless the hub phase +0x1C6 is set without context +0x96C
/// bit 3 (then NEXT_SUBSTATE), run the step routine
/// `[0x80111710 + 4 * [HUB]+0x7D]` (with this routine's own arguments,
/// which reach the `jalr` unchanged): 0 returns 0, 1 advances the step
/// (returns 0), 2 clears hub +0x88/+0x8A, sets the phase to 1 and returns
/// NEXT_SUBSTATE (0x04000000); any other result returns 2.
pub fn steps_update(m: &mut Machine, args: [u32; 4]) -> u32 {
    framed(m, 0x18, |m, _| {
        let hb = hub(m);
        if lh(m, hb, 0x1c6) != 0 {
            let context = ctx(m);
            if lw(m, context, 0x96c) & 8 == 0 {
                return 0x0400_0000;
            }
        }
        let hb = hub(m);
        let step = lbu(m, hb, 0x7d);
        let routine = lw(m, step << 2, 0x8011_1710);
        match call(m, routine, args) {
            0 => 0,
            1 => {
                let hb = hub(m);
                let step = lbu(m, hb, 0x7d).wrapping_add(1);
                sb(m, hb, 0x7d, step);
                0
            }
            2 => {
                let hb = hub(m);
                sh(m, hb, 0x88, 0);
                sh(m, hb, 0x8a, 0);
                sh(m, hb, 0x1c6, 1);
                0x0400_0000
            }
            _ => 2,
        }
    })
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800f_c5a8 => mdec_start(m),
        0x800f_c600 => movie_setup(m, a0),
        0x800f_c750 => movie_end(m),
        0x800f_c764 => movie_open(m, a0, a1, a2, a3),
        0x800f_c854 => movie_stream_start(m),
        0x800f_c89c => movie_poll_wait(m, a0),
        0x800f_c970 => movie_step(m, a0),
        0x800f_ca70 => movie_wait_decode(m, a0, a1),
        0x800f_cb44 => movie_slice_done(m),
        0x800f_cc7c => movie_poll(m, a0),
        0x800f_ce40 => movie_init(m),
        0x800f_cf80 => movie_finish(m),
        0x800f_d028 => movie_update(m, a0),
        0x800f_d408 => movie_callback(m, a0),
        0x800f_d48c => movie_quad(m, a0, a1, a2, a3),
        0x800f_d634 => steps_initialize(m),
        0x800f_d698 => steps_update(m, args),
        _ => return None,
    })
}
