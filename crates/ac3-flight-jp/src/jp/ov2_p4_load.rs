//! JP overlay 2: resource helpers (800ECD20..800ED15B), translated from the
//! JP instructions. 800ECD20 is class `changed` (PAL 800D73B8, a different
//! routine now); the rest are `new`.
//!
//! The upload helpers take a resource that is either raw or Ulz-compressed
//! (first word 0x1A7A6C55): a compressed one is decoded into an arena
//! buffer first, which is released (arena reset to it) at the end.
use super::ov2_p4::{At, call, lt, sext16, sra};
use crate::common::{RESOURCES, loaded_module};
use crate::machine::{Machine, SmallData};

/// `[0x80112DC4]`: the hub block (+0xD0 palette-copy rectangle, +0xD4 its
/// pixels, +0xD8 the rectangle the palette is restored to).
pub const HUB: u32 = 0x8011_2dc4;
/// Resident arena allocation and reset.
const ALLOC: u32 = 0x8002_7410;
const RESET: u32 = 0x8002_7538;
/// Resident loaders: relocate a directory, upload its TIMs, upload one
/// TIM, upload an image into a rectangle (8002_7084), Ulz decode.
const RELOCATE: u32 = 0x8002_7338;
const UPLOAD_TIMS: u32 = 0x8002_722c;
const UPLOAD_TIM: u32 = 0x8002_71a0;
const UPLOAD_RECT: u32 = 0x8002_7084;
const DECOMPRESS: u32 = 0x800a_fd08;
/// libgpu DrawSync, StoreImage, LoadImage.
const DRAW_SYNC: u32 = 0x800a_8194;
const STORE_IMAGE: u32 = 0x800a_84a0;
const LOAD_IMAGE: u32 = 0x800a_8440;
/// The first word of an Ulz stream.
const ULZ: u32 = 0x1a7a_6c55;

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != 2 {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800e_cd20 => release_ecd20(m),
        0x800e_cd4c => children_ecd4c(m, a0, a1),
        0x800e_cda8 => upload_tims_ecda8(m, a0),
        0x800e_ce38 => upload_tims_sized_ece38(m, a0, a1),
        0x800e_cec8 => upload_tim_list_ecec8(m, a0),
        0x800e_cf88 => upload_tim_ecf88(m, a0),
        0x800e_d000 => upload_rect_ed000(m, a0, a1, a2, a3),
        0x800e_d0d4 => upload_and_restore_ed0d4(m, a0),
        _ => return None,
    })
}

/// 800ECD20 (JP; PAL 800D73B8, changed): release the resource loaded at
/// `[resources + 0x60]` (arena reset to it). Returns the reset's result.
pub fn release_ecd20(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x18);
    let _ = frame;
    let resources = m.gw(RESOURCES);
    let resource = m.lw(resources.at(0x60));
    let v = call(m, RESET, [resource, 0, 0, 0]);
    m.pop_frame(0x18);
    v
}

/// 800ECD4C: the children of the directory `[holder]` (word 0 the count,
/// then byte offsets, rounded toward zero to words) as addresses into
/// `out`. Returns the count.
pub fn children_ecd4c(m: &mut Machine, holder: u32, out: u32) -> u32 {
    let directory = m.lw(holder);
    let count = m.lw(directory);
    if lt(0, count) {
        let (mut out, mut entry, mut i) = (out, directory, 0u32);
        loop {
            let offset = m.lw(entry.at(4));
            let offset = if lt(offset, 0) {
                offset.wrapping_add(3)
            } else {
                offset
            };
            m.sw(out, directory.wrapping_add(sra(offset, 2) << 2));
            out = out.at(4);
            i = i.wrapping_add(1);
            entry = entry.at(4);
            if !lt(i, count) {
                break;
            }
        }
    }
    count
}

/// Decode `resource` into `buffer` when it is compressed; the data to use.
fn decoded(m: &mut Machine, resource: u32, buffer: u32) -> (bool, u32) {
    let head = m.lw(resource);
    if head == ULZ {
        call(m, DECOMPRESS, [resource, buffer, 0, 0]);
        (true, buffer)
    } else {
        (false, resource)
    }
}

/// The TIM directory `resource` uploaded through an arena buffer of
/// `bytes` (800ECDA8 / 800ECE38).
fn upload_tims(m: &mut Machine, resource: u32, bytes: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let _ = frame;
    let buffer = call(m, ALLOC, [bytes, 0, 0, 0]);
    let (_, data) = decoded(m, resource, buffer);
    let directory = call(m, RELOCATE, [data, 0, 0, 0]);
    let count = m.lw(data);
    call(m, UPLOAD_TIMS, [directory, count, 0, 0]);
    call(m, DRAW_SYNC, [0, 0, 0, 0]);
    call(m, RESET, [buffer, 0, 0, 0]);
    m.pop_frame(0x20);
    1
}

/// 800ECDA8: upload the TIMs of directory `resource` (0x30000-byte buffer
/// for a compressed one), DrawSync. Returns 1.
pub fn upload_tims_ecda8(m: &mut Machine, resource: u32) -> u32 {
    upload_tims(m, resource, 0x30000)
}

/// 800ECE38: as 800ECDA8 with a `bytes` buffer.
pub fn upload_tims_sized_ece38(m: &mut Machine, resource: u32, bytes: u32) -> u32 {
    upload_tims(m, resource, bytes)
}

/// 800ECEC8: upload each TIM of directory `list` (each child decoded into a
/// 0x9000-byte buffer when compressed), DrawSync after each. Returns 1.
pub fn upload_tim_list_ecec8(m: &mut Machine, list: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let _ = frame;
    let count = m.lw(list);
    let buffer = call(m, ALLOC, [0x9000, 0, 0, 0]);
    let first = m.lw(list);
    let children = call(m, RELOCATE, [list, first, 0, 0]);
    if lt(0, count) {
        let (mut child, mut i) = (children, 0u32);
        loop {
            let tim = m.lw(child);
            let (_, data) = decoded(m, tim, buffer);
            child = child.at(4);
            call(m, UPLOAD_TIM, [data, 0, 0, 0]);
            call(m, DRAW_SYNC, [0, 0, 0, 0]);
            i = i.wrapping_add(1);
            if !lt(i, count) {
                break;
            }
        }
    }
    call(m, RESET, [buffer, 0, 0, 0]);
    m.pop_frame(0x28);
    1
}

/// 800ECF88: upload one TIM (decoded into a 0x9000-byte buffer when
/// compressed), DrawSync. Returns the arena reset's result.
pub fn upload_tim_ecf88(m: &mut Machine, tim: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let _ = frame;
    let buffer = call(m, ALLOC, [0x9000, 0, 0, 0]);
    let (_, data) = decoded(m, tim, buffer);
    call(m, UPLOAD_TIM, [data, 0, 0, 0]);
    call(m, DRAW_SYNC, [0, 0, 0, 0]);
    let v = call(m, RESET, [buffer, 0, 0, 0]);
    m.pop_frame(0x20);
    v
}

/// 800ED000 (image, x, y, w, h on the stack): upload `image` (decoded when
/// compressed) into the rectangle through 80027084 (the halfword arguments
/// sign-extended, h passed on the stack), DrawSync. Returns the arena
/// reset's result.
pub fn upload_rect_ed000(m: &mut Machine, image: u32, x: u32, y: u32, w: u32) -> u32 {
    let h = m.lw(m.sp.at(0x10));
    let frame = m.push_frame(0x38);
    let buffer = call(m, ALLOC, [0x9000, 0, 0, 0]);
    let head = m.lw(image);
    let data = if head == ULZ {
        call(m, DECOMPRESS, [image, buffer, 0, 0]);
        m.sw(frame + 0x10, sext16(h));
        buffer
    } else {
        m.sw(frame + 0x10, sext16(h));
        image
    };
    call(m, UPLOAD_RECT, [data, sext16(x), sext16(y), sext16(w)]);
    call(m, DRAW_SYNC, [0, 0, 0, 0]);
    let v = call(m, RESET, [buffer, 0, 0, 0]);
    m.pop_frame(0x38);
    v
}

/// 800ED0D4: upload the TIM directory `resource` (800ECE38, 0x10000-byte
/// buffer), save the palette rectangle `[HUB] + 0xD8` into `[HUB + 0xD0]`
/// (StoreImage, DrawSync), then fill the 64 words at `[HUB + 0xD4]` with
/// 0xCE73CE73 and load them into the rectangle (LoadImage). Returns
/// LoadImage's result.
pub fn upload_and_restore_ed0d4(m: &mut Machine, resource: u32) -> u32 {
    let frame = m.push_frame(0x18);
    let _ = frame;
    upload_tims_sized_ece38(m, resource, 0x10000);
    let hub = m.lw(HUB);
    let pixels = m.lw(hub.at(0xd0));
    call(m, STORE_IMAGE, [hub.at(0xd8), pixels, 0, 0]);
    call(m, DRAW_SYNC, [0, 0, 0, 0]);
    for i in 0..0x40u32 {
        let hub = m.lw(HUB);
        let pixels = m.lw(hub.at(0xd4));
        m.sw(pixels.wrapping_add(i << 2), 0xce73_ce73);
    }
    let hub = m.lw(HUB);
    let pixels = m.lw(hub.at(0xd4));
    let v = call(m, LOAD_IMAGE, [hub.at(0xd8), pixels, 0, 0]);
    m.pop_frame(0x18);
    v
}
