//! JP overlay 3, part 1: the credits' picture player (8011BD94..8011CB5B):
//! the player's phases, the decoder block `[0x800BD054]`, the stream and
//! its chunks, translated from the JP instructions. The decoder is the
//! resident picture player's format (`states.rs`) in a copy of its own:
//! 40x240 texture tiles, the CLUT row at y 510/511.
use super::ov3_p1::{
    ARENA_PEEK, CLEAR_IMAGE, DECODER, DISPLAY, LOAD_IMAGE, MODULE, NEXT_CHUNK, PROCESS_CD_QUEUE,
    QUEUE_STREAM, RELEASE_CHUNK, RESET_CD_QUEUE, VSYNC, block, call, ctx, decoder, framed, hi_s,
    lbu, lh, lhu, sb, sh, sra, sx16,
};
use super::ov3_p1_draw::credits_frame;
use crate::common::loaded_module;
use crate::machine::Machine;

/// 0x8011EA3C / 0x8011EA44: the clear rectangles of phases 3 and 4.
const CLEAR_RECTS: [u32; 2] = [0x8011_ea3c, 0x8011_ea44];
/// 0x8011EA4C: the 10 tile rectangles (8 bytes per page); 0x8011EA9C: the
/// tiles' pixel sources (a word per page).
const TILE_RECTS: u32 = 0x8011_ea4c;
const TILE_SOURCES: u32 = 0x8011_ea9c;
/// 8011C2A0: the stream's completion routine.
const STREAM_DONE: u32 = 0x8011_c2a0;

/// 8011BD94 (JP; PAL 800D69EC, exact): the credits player (block phase
/// +0): 0 lay out the decoder (ring 0xD740 bytes) and start the stream; 1
/// on buffer 0 wait for the first chunk (END ends); 2 show frames (START
/// ends); 3/4 clear the screen; 5 restore the display (phase 0, interval
/// 2, requests 1|4, context bit 0x10000000 cleared) and return 1. Returns
/// 0 otherwise.
pub fn player(m: &mut Machine) -> u32 {
    framed(m, 0x38, |m, sp| {
        let b = block(m);
        let set_phase = |m: &mut Machine, phase: u32| {
            let b = block(m);
            m.sw(b, phase);
        };
        match m.lw(b) {
            0 => {
                let buffer = call(m, ARENA_PEEK, [0, 0, 0, 0]);
                let b = block(m);
                for i in 0..8u32 {
                    let value = lh(m, b.wrapping_add(0x44 + 2 * i));
                    m.sw(sp.wrapping_add(0x10 + 4 * i), value);
                }
                let file = m.lw(b.wrapping_add(0x58));
                let mode = m.lw(b.wrapping_add(0x5c));
                decoder_init(m, file, mode, buffer, 0xd740);
                stream_start(m);
                set_phase(m, 1);
                0
            }
            1 => {
                let context = ctx(m);
                if m.lw(context.wrapping_add(4)) == 0 {
                    m.sw(context.wrapping_add(4), 0);
                    if wait_frame(m, 1) != 0 {
                        set_phase(m, 2);
                    } else {
                        let d = decoder(m);
                        if lbu(m, d.wrapping_add(0xa0)) == 1 {
                            set_phase(m, 3);
                        }
                    }
                }
                let d = decoder(m);
                let count = m.lw(d.wrapping_add(4)).wrapping_add(1);
                m.sw(d.wrapping_add(4), count);
                0
            }
            2 => {
                let context = ctx(m);
                if m.lw(context.wrapping_add(0x12c)) & 0x40 != 0 {
                    let d = decoder(m);
                    sb(m, d.wrapping_add(0xa0), 1);
                }
                if credits_frame(m) != 0 {
                    set_phase(m, 3);
                }
                0
            }
            phase @ (3 | 4) => {
                if phase == 3 {
                    stop(m);
                }
                bank_zero(m);
                let rect = CLEAR_RECTS[(phase - 3) as usize];
                sh(m, rect, 0);
                sh(m, rect + 4, 0x140);
                sh(m, rect + 2, 0);
                sh(m, rect + 6, 0xf0);
                call(m, CLEAR_IMAGE, [rect, 0, 0, 0]);
                if phase == 4 {
                    set_phase(m, 5);
                } else {
                    let context = ctx(m);
                    if m.lw(context.wrapping_add(4)) == 0 {
                        set_phase(m, 4);
                    }
                }
                0
            }
            5 => {
                let context = ctx(m);
                let word = m.lw(context) & 0xefff_ffff;
                let display = m.lw(DISPLAY);
                m.sw(context, word);
                let flags = m.lw(display) | 5;
                m.sw(display.wrapping_add(4), 0);
                m.sw(display.wrapping_add(0x14), 2);
                m.sw(display, flags);
                1
            }
            _ => 0,
        }
    })
}

/// 8011C05C (JP; PAL 800D6CB4, exact; `file`, `mode`, `buffer`, `ring`,
/// eight layout words on the stack): point [`DECODER`] at `buffer`, the
/// ring after its 0xD8 bytes (`ring` rounded up to words), set the decoder
/// up ([`decoder_setup`] of the parameter record in the frame) and reset
/// the CD queue. Returns the end of the ring.
pub fn decoder_init(m: &mut Machine, file: u32, mode: u32, buffer: u32, ring: u32) -> u32 {
    let caller = m.sp;
    let layout: [u32; 8] = std::array::from_fn(|i| m.lw(caller.wrapping_add(0x10 + 4 * i as u32)));
    framed(m, 0x38, |m, sp| {
        m.sw(DECODER, buffer);
        let start = buffer.wrapping_add(0xd8);
        let size = (ring.wrapping_add(3) >> 2) << 2;
        m.sw(sp.wrapping_add(0x1c), start);
        let end = start.wrapping_add(size);
        m.sw(sp.wrapping_add(0x14), file);
        m.sw(sp.wrapping_add(0x18), size);
        m.sw(sp.wrapping_add(0x10), mode);
        for (i, value) in layout.into_iter().enumerate() {
            sh(m, sp.wrapping_add(0x20 + 2 * i as u32), value);
        }
        decoder_setup(m, sp.wrapping_add(0x10));
        call(m, RESET_CD_QUEUE, [0, 0, 0, 0]);
        end
    })
}

/// 8011C100 (JP; PAL 800D6D58, exact): copy the parameter record `params`
/// (+0 mode, +4 file, +8 ring size, +0xC ring) into the decoder and reset
/// it: no step, bank 0, frame 0, both banks' five effect slots cleared
/// (start/end 0, the four words 0xFFFF), the tile flags cleared. Returns 0.
pub fn decoder_setup(m: &mut Machine, params: u32) -> u32 {
    let d = decoder(m);
    for (from, to) in [(4u32, 0xccu32), (8, 0xd0), (0xc, 0xd4)] {
        let value = m.lw(params.wrapping_add(from));
        m.sw(d.wrapping_add(to), value);
    }
    let mode = lbu(m, params);
    sb(m, d.wrapping_add(0xa1), mode);
    let mut last = d;
    for offset in [0x9e, 0x9f, 0xa0, 0xa, 1] {
        last = decoder(m);
        sb(m, last.wrapping_add(offset), 0);
    }
    let again = decoder(m);
    m.sw(last.wrapping_add(0xc), 0);
    m.sw(last.wrapping_add(4), 0);
    sb(m, again, 0);
    let d = decoder(m);
    sb(m, d.wrapping_add(0x10), 0);
    let d = decoder(m);
    sh(m, d.wrapping_add(8), 0);
    sb(m, d.wrapping_add(0x10), 0);
    for bank in 0..2u32 {
        for slot in 0..5u32 {
            let at = bank * 0x46 + slot * 0xe;
            let d = decoder(m);
            sb(m, d.wrapping_add(at).wrapping_add(0x12), 0);
            let d = decoder(m);
            sh(m, d.wrapping_add(at).wrapping_add(0x14), 0);
            sh(m, d.wrapping_add(at).wrapping_add(0x16), 0);
            for k in 0..4u32 {
                sh(m, d.wrapping_add(at + 2 * k).wrapping_add(0x18), 0xffff);
            }
        }
    }
    for bank in 0..2u32 {
        for i in 0..0x14u32 {
            let d = decoder(m);
            sb(m, d.wrapping_add(bank * 0x14 + i).wrapping_add(0xa2), 0);
        }
    }
    0
}

/// 8011C260 (JP; PAL 800D6EB8, exact): start streaming: 8002C928(file,
/// ring, ring size, 0x2000, completion 8011C2A0). Returns its result.
pub fn stream_start(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, sp| {
        let d = decoder(m);
        m.sw(sp.wrapping_add(0x10), STREAM_DONE);
        let file = m.lw(d.wrapping_add(0xcc));
        let ring = m.lw(d.wrapping_add(0xd4));
        let size = m.lw(d.wrapping_add(0xd0));
        call(m, QUEUE_STREAM, [file, ring, size, 0x2000])
    })
}

/// 8011C2A0 (JP; PAL 800D6EF8, exact): the stream's completion: END.
/// Returns 1.
pub fn stream_done(m: &mut Machine) -> u32 {
    let d = decoder(m);
    sb(m, d.wrapping_add(0xa0), 1);
    1
}

/// 8011C2B4 (JP; PAL 800D6F0C, exact): stop: reset the CD queue. Returns 1.
pub fn stop(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        call(m, RESET_CD_QUEUE, [0, 0, 0, 0]);
        1
    })
}

/// 8011C2D8 (JP; PAL 800D6F30, exact): whether the decoding bank (+0x9E)
/// is 0.
pub fn bank_zero(m: &mut Machine) -> u32 {
    let d = decoder(m);
    u32::from(lbu(m, d.wrapping_add(0x9e)) == 0)
}

/// 8011C2F0 (JP; PAL 800D6F48, exact): [`next_chunk`]; with `poll` 0
/// retry for up to 121 frames (the CD queue, `VSync(0)`), then END.
/// Returns 1 once a chunk was taken or the wait gave up, else 0.
pub fn wait_frame(m: &mut Machine, poll: u32) -> u32 {
    framed(m, 0x28, |m, sp| {
        if poll != 0 {
            let d = decoder(m);
            return u32::from(next_chunk(m, d) != 0);
        }
        let counter = sp.wrapping_add(0x10);
        m.sw(counter, 0x78);
        loop {
            let d = decoder(m);
            if next_chunk(m, d) != 0 {
                return 1;
            }
            call(m, PROCESS_CD_QUEUE, [0; 4]);
            call(m, VSYNC, [0, 0, 0, 0]);
            let count = m.lw(counter).wrapping_sub(1);
            m.sw(counter, count);
            if (m.lw(counter) as i32) < 0 {
                break;
            }
        }
        let d = decoder(m);
        sb(m, d.wrapping_add(0xa0), 1);
        1
    })
}

/// The effect slot `slot` of bank `bank` of the decoder `d`.
fn slot_at(d: u32, bank: u32, slot: u32) -> u32 {
    d.wrapping_add(bank.wrapping_mul(0x46))
        .wrapping_add(slot.wrapping_mul(0xe))
}

/// 8011C39C (JP; PAL 800D6FF4, near: 40x240 tiles, the CLUT row at y
/// 510/511): take the next credits chunk of decoder `d` (8002B9EC; 0 when
/// there is none). The chunk header (+6 the last picture when 0, +0xC kind
/// and step, +0xD restart, +0xF page, +0x10 effect record, +0x11 effect,
/// +0x13 the picture's first page and the tile flags, +0x14.. the effect
/// times): page 0 sets bit 15 of the CLUT's non-zero colours, other pages
/// drop every 574th word (0x1426 words compacted in place); a picture's
/// first page (+0x13 & 0xF, + 7) flips the decoding bank and clears its
/// slots; pages 2..17 set their tile flags; an effect record fills its
/// slot; kind 0 uploads the page (the CLUT 256x1 at 0,510/511, tiles 40x240
/// from x 0x140/0x200); step 1 sets the decoder step (2 on a restart). The
/// chunk is released (8002BBE0). Returns 1.
pub fn next_chunk(m: &mut Machine, d: u32) -> u32 {
    framed(m, 0x28, |m, sp| {
        let data_out = sp.wrapping_add(0x10);
        let header_out = sp.wrapping_add(0x14);
        if call(m, NEXT_CHUNK, [data_out, header_out, 0, 0]) != 0 {
            return 0;
        }
        let header = m.lw(header_out);
        if lhu(m, header.wrapping_add(6)) == 0 {
            sb(m, d.wrapping_add(0xa0), 1);
        }
        let header = m.lw(header_out);
        let kinds = lbu(m, header.wrapping_add(0xc));
        let first = lbu(m, header.wrapping_add(0x13)) & 0xf;
        let page = lbu(m, header.wrapping_add(0xf));
        let restart = lbu(m, header.wrapping_add(0xd));
        let kind = kinds & 0xf;
        let step = kinds >> 4;
        if page == 0 {
            let mut entry = m.lw(data_out).wrapping_add(0x14);
            for _ in 0..0x100 {
                let value = lh(m, entry);
                let half = lhu(m, entry);
                if value != 0 {
                    sh(m, entry, half | 0xffff_8000);
                }
                entry = entry.wrapping_add(2);
            }
        } else {
            let data = m.lw(data_out);
            let mut out = data;
            for i in 0..0x1426u32 {
                let q = sra(hi_s(i, 0x725f_9bed), 8).wrapping_sub(sra(i, 31));
                let from = i.wrapping_add(sx16(q)) << 2;
                let word = m.lw(from.wrapping_add(data));
                m.sw(out, word);
                out = out.wrapping_add(4);
            }
        }
        if page == first + 7 {
            let bank = lbu(m, d.wrapping_add(0x9e));
            sb(m, d.wrapping_add(0x9e), u32::from(bank == 0));
            if restart == 1 {
                sb(m, d.wrapping_add(0x9e), 0);
            }
            for slot in 0..5u32 {
                let bank = lbu(m, d.wrapping_add(0x9e));
                sb(m, slot_at(d, bank, slot).wrapping_add(0x12), 0);
                let bank = lbu(m, d.wrapping_add(0x9e));
                sh(m, slot_at(d, bank, slot).wrapping_add(0x14), 0xffff);
                let bank = lbu(m, d.wrapping_add(0x9e));
                sh(m, slot_at(d, bank, slot).wrapping_add(0x16), 0xffff);
                for k in 0..4u32 {
                    let bank = lbu(m, d.wrapping_add(0x9e));
                    let at = slot_at(d, bank, slot).wrapping_add(2 * k);
                    sh(m, at.wrapping_add(0x18), 0);
                }
            }
        }
        // the switch table 0x8011B2E8: pages 2..17 set the tile flag at
        // 0xA2 + 17 - page (pages 2..5 also the one 4 bytes on)
        if (2..18).contains(&page) {
            let offset = 0xa2 + 17 - page;
            let offsets: &[u32] = if page < 6 {
                &[offset, offset + 4]
            } else {
                &[offset]
            };
            for &offset in offsets {
                let bank = lbu(m, d.wrapping_add(0x9e));
                let flags = d.wrapping_add(bank * 0x14);
                let header = m.lw(header_out);
                let value = lbu(m, header.wrapping_add(0x13)) >> 4;
                sb(m, flags.wrapping_add(offset), value);
            }
        }
        let header = m.lw(header_out);
        let record = lbu(m, header.wrapping_add(0x10));
        if record & 0xf == 1 {
            let s = record >> 4;
            let bank = lbu(m, d.wrapping_add(0x9e));
            let effect = lbu(m, header.wrapping_add(0x11)) & 0xf;
            sb(m, slot_at(d, bank, s).wrapping_add(0x12), effect);
            let bank = lbu(m, d.wrapping_add(0x9e));
            let header = m.lw(header_out);
            let start = lhu(m, header.wrapping_add(0x14));
            sh(m, slot_at(d, bank, s).wrapping_add(0x14), start);
            let bank = lbu(m, d.wrapping_add(0x9e));
            let end = lhu(m, header.wrapping_add(0x16));
            sh(m, slot_at(d, bank, s).wrapping_add(0x16), end);
            for k in 0..4u32 {
                let value = lhu(m, header.wrapping_add(0x18 + 2 * k));
                let bank = lbu(m, d.wrapping_add(0x9e));
                let at = slot_at(d, bank, s).wrapping_add(2 * k);
                sh(m, at.wrapping_add(0x18), value);
            }
        }
        if kind == 0 {
            if page == 0 {
                sh(m, TILE_RECTS, 0);
                sh(m, TILE_RECTS + 4, 0x100);
                sh(m, TILE_RECTS + 6, 1);
                let y = if lbu(m, d.wrapping_add(0x9e)) != 0 {
                    0x1ff
                } else {
                    0x1fe
                };
                sh(m, TILE_RECTS + 2, y);
            } else {
                let x0: u32 = if lbu(m, d.wrapping_add(0x9e)) != 0 {
                    0x200
                } else {
                    0x140
                };
                let tile = page - 1;
                // `bgez`: the page is a non-zero byte here, so the tile
                // index is never negative and the rounding never applies
                let rounded = if (tile as i32) < 0 { page + 2 } else { tile };
                let rect = TILE_RECTS + page * 8;
                let row = sra(rounded, 2);
                let column = tile.wrapping_sub(row << 2).wrapping_sub(3);
                sh(m, rect, x0.wrapping_sub(column.wrapping_mul(40)));
                sh(
                    m,
                    rect + 2,
                    0u32.wrapping_sub(row.wrapping_sub(1).wrapping_mul(240)),
                );
                sh(m, rect + 4, 0x28);
                sh(m, rect + 6, 0xf0);
            }
            let rect = TILE_RECTS + page * 8;
            let source = m.lw(data_out).wrapping_add(0x14);
            m.sw(TILE_SOURCES + page * 4, source);
            call(m, LOAD_IMAGE, [rect, source, 0, 0]);
        }
        if step == 1 {
            sb(m, d, if restart == 1 { 2 } else { 1 });
        }
        let header = m.lw(header_out);
        call(m, RELEASE_CHUNK, [header, 0, 0, 0]);
        1
    })
}

/// The routines of this file by JP address, while module 3 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8011_bd94 => player(m),
        0x8011_c05c => decoder_init(m, a0, a1, a2, a3),
        0x8011_c100 => decoder_setup(m, a0),
        0x8011_c260 => stream_start(m),
        0x8011_c2a0 => stream_done(m),
        0x8011_c2b4 => stop(m),
        0x8011_c2d8 => bank_zero(m),
        0x8011_c2f0 => wait_frame(m, a0),
        0x8011_c39c => next_chunk(m, a0),
        _ => return None,
    })
}
