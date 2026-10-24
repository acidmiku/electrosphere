//! Targets of the replay/sequence script commands 5 (effects, jump table
//! 8002398C) and 8 (overlay tracer) of 80038B60 (`replay.rs`): the 8009xxxx
//! routines that spawn flash, smoke, debris and shock-wave particles from a
//! command record (+2 seed/size halfword, +4..+8 velocity halfwords, +0xA
//! scale, +0xC..+0x14 position words).
//!
//! Conventions as in `ai.rs`: every routine with an original stack frame
//! allocates it on the guest stack, and routines owned elsewhere (the
//! particle pools 8007E76C, `rand`/`srand`, the tracer claim 800819A8) are
//! reached by original address or through their owners.
use crate::{
    dispatch,
    machine::{Machine, SmallData},
    sdk,
};

/// `[gp+0x40C]`: the effect block (flash pools at +0x600: three in-use
/// masks, fresh masks at +0xA8C, 0x1C-byte particles from +0x60C in banks
/// of 0x380; smoke/debris pool +0x1098; shock waves +0x33C0).
const GP_EFFECTS: u32 = 0x40c;
/// `[gp+0x2D0]`: gameplay state (+0xC frame counter).
const GP_GAMEPLAY: u32 = 0x2d0;

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}
fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// `mult` + `mflo`.
fn lo(a: u32, b: u32) -> u32 {
    (a as i32).wrapping_mul(b as i32) as u32
}
fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}
fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
/// `value / 2^shift` rounded toward zero (`bgez`/`addiu 2^shift - 1`/`sra`).
fn div_pow2(value: u32, shift: u32) -> u32 {
    let value = if lt(value, 0) {
        value.wrapping_add((1 << shift) - 1)
    } else {
        value
    };
    sra(value, shift)
}
/// Run `body` inside an original stack frame of `size` bytes.
fn with_frame<T>(m: &mut Machine, size: u32, body: impl FnOnce(&mut Machine, u32) -> T) -> T {
    let frame = m.push_frame(size);
    let result = body(m, frame);
    m.pop_frame(size);
    result
}
/// 8007E76C (`replay`): a particle from the pool at effect block +`pool`,
/// or 0.
fn particle(m: &mut Machine, pool: u32, count: u32, kind: u32) -> u32 {
    let block = m.gw(GP_EFFECTS);
    dispatch::call(m, 0x8007_e76c, [block.wrapping_add(pool), count, kind, 0])
}
/// A random byte centred on zero: `(rand() & 0xFF) - 0x80`.
fn random_byte(m: &mut Machine) -> u32 {
    (sdk::rand(m) & 0xff).wrapping_sub(0x80)
}
/// The frame counter `[gp+0x2D0]` +0xC.
fn frame_counter(m: &mut Machine) -> u32 {
    let gameplay = m.gw(GP_GAMEPLAY);
    m.lw(gameplay + 0xc)
}

/// 80091294 () -> particle or 0: claim the first free flash particle of the
/// three banks at effect block +0x600 (in-use and fresh bits set).
pub fn claim_flash_91294(m: &mut Machine) -> u32 {
    let base = m.gw(GP_EFFECTS).wrapping_add(0x600);
    let mut offset = 0xcu32;
    for bank in 0..3u32 {
        let at = base.wrapping_add(bank * 4);
        let mask = m.lw(at);
        if mask != u32::MAX {
            let mut flash = base.wrapping_add(offset);
            let mut bit = 1u32;
            if mask & 1 != 0 {
                loop {
                    bit <<= 1;
                    flash = flash.wrapping_add(0x1c);
                    if mask & bit == 0 {
                        break;
                    }
                }
            }
            let fresh = m.lw(at.wrapping_add(0xa8c));
            m.sw(at, mask | bit);
            m.sw(at.wrapping_add(0xa8c), fresh | bit);
            return flash;
        }
        offset = offset.wrapping_add(0x380);
    }
    0
}

/// The debris chunk shared by 80091310/80091520: with record flag 0x2000
/// and an even chance, a smoke-pool particle thrown along the record's
/// velocity (+4..+8) from `position`.
fn debris_chunk(m: &mut Machine, record: u32, position: [u32; 3]) {
    if (m.lw(record) >> 13) & 1 == 0 || sdk::rand(m) & 1 == 0 {
        return;
    }
    let chunk = particle(m, 0x1098, 10, 7);
    if chunk == 0 {
        return;
    }
    let velocity = [
        lhu(m, record + 4) << 16,
        lhu(m, record + 6) << 16,
        lhu(m, record + 8) << 16,
    ];
    m.sh(chunk, 0);
    let life = (sdk::rand(m) & 7) + 0x14;
    m.sh(chunk + 2, life as u16);
    m.sh(chunk + 0x18, 0);
    let size = (sdk::rand(m) & 7) + 0xc;
    m.sh(chunk + 0x1a, size as u16);
    for (axis, (&start, &speed)) in position.iter().zip(velocity.iter()).enumerate() {
        let offset = axis as u32 * 4;
        m.sw(chunk + 4 + offset, start.wrapping_add(sra(speed, 16)));
    }
    for (axis, &speed) in velocity.iter().enumerate() {
        let drift = sra(speed, 17)
            .wrapping_add(sdk::rand(m) & 0xff)
            .wrapping_sub(0x80);
        m.sh(chunk + 0x10 + axis as u32 * 2, drift as u16);
    }
    m.sh(chunk + 0x16, 0x188);
}

/// A flash at `position`: life 10 (or `life` frames for kind -3), size
/// `scale * (base + rand & mask) >> 12`.
#[allow(clippy::too_many_arguments)]
fn flash(
    m: &mut Machine,
    position: [u32; 3],
    head: u32,
    life: u32,
    size: u32,
    scale: u32,
    mask: u32,
    base: u32,
) {
    let flash = particle(m, 0x600, 3, 7);
    if flash == 0 {
        return;
    }
    if head == 0 {
        m.sh(flash + 2, life as u16);
        m.sh(flash, 0);
    } else {
        m.sh(flash, head as u16);
        m.sh(flash + 2, life as u16);
    }
    for (axis, &start) in position.iter().enumerate() {
        m.sw(flash + 4 + axis as u32 * 4, start);
    }
    m.sh(flash + 0x16, 0);
    m.sh(flash + 0x18, size as u16);
    let random = (sdk::rand(m) & mask).wrapping_add(base);
    m.sh(flash + 0x1a, sra(lo(scale, random), 12) as u16);
}

/// 80091310 (record, case): command-5 explosion: a flash scattered by the
/// scale (+0xA) and the record's debris chunk.
pub fn explosion_91310(m: &mut Machine, record: u32) {
    with_frame(m, 0x30, |m, _| {
        let scale = lh(m, record + 0xa);
        let mut position = [0u32; 3];
        for (axis, slot) in position.iter_mut().enumerate() {
            let spread = random_byte(m);
            let base = m.lw(record + 0xc + axis as u32 * 4);
            *slot = base.wrapping_add(sra(lo(scale, spread), 3));
        }
        flash(m, position, 0, 0xa, 0x100, scale, 0x3f, 0x4e);
        debris_chunk(m, record, position);
    });
}

/// 80091520 (record, case): command-5 large explosion on alternate frames
/// (frame counter + seed +2 odd skips): with record flag 0x4000 three in
/// four times a flash, always a long flash (life 0x28), and the debris
/// chunk.
pub fn large_explosion_91520(m: &mut Machine, record: u32) {
    with_frame(m, 0x30, |m, _| {
        let seed = lh(m, record + 2);
        if frame_counter(m).wrapping_add(seed) & 1 != 0 {
            return;
        }
        let scale = lh(m, record + 0xa);
        let mut position = [0u32; 3];
        for (axis, slot) in position.iter_mut().enumerate() {
            let spread = random_byte(m);
            let base = m.lw(record + 0xc + axis as u32 * 4);
            *slot = base.wrapping_add(sra(lo(scale, spread), 2));
        }
        if (m.lw(record) >> 14) & 1 != 0 && sdk::rand(m) & 3 != 0 {
            flash(m, position, 0, 0xa, 0x100, scale, 0x3f, 0x3a);
        }
        flash(m, position, 0xfffd, 0x28, 0x210, scale, 0x7f, 0x4c);
        debris_chunk(m, record, position);
    });
}

/// 800917D0 (record, case): command-5 smoke puff every fourth frame
/// (frame counter + seed): a rising flash scattered by the scale.
pub fn smoke_917d0(m: &mut Machine, record: u32) {
    with_frame(m, 0x20, |m, _| {
        let seed = lh(m, record + 2);
        if frame_counter(m).wrapping_add(seed) & 3 != 0 {
            return;
        }
        let scale = lh(m, record + 0xa);
        let puff = particle(m, 0x600, 3, 7);
        if puff == 0 {
            return;
        }
        m.sw(puff, 0x50_0000);
        for axis in 0..3u32 {
            let spread = (sdk::rand(m) & 0x3f).wrapping_sub(0x20);
            let base = m.lw(record + 0xc + axis * 4);
            m.sw(
                puff + 4 + axis * 4,
                base.wrapping_add(sra(lo(scale, spread), 1)),
            );
        }
        let spread = (sdk::rand(m) & 0x3f).wrapping_sub(0x20);
        let rise = sra(scale.wrapping_mul(0x90).wrapping_neg(), 12);
        m.sh(puff + 0x12, rise as u16);
        m.sh(puff + 0x10, sra(lo(scale, spread), 12) as u16);
        let spread = (sdk::rand(m) & 0x3f).wrapping_sub(0x20);
        m.sh(puff + 0x16, 0x80);
        m.sh(puff + 0x18, 0x210);
        m.sh(puff + 0x14, sra(lo(scale, spread), 12) as u16);
        let size = (sdk::rand(m) & 0x3f).wrapping_add(0x58);
        m.sh(puff + 0x1a, sra(lo(scale, size), 12) as u16);
    });
}

/// 8009192C (record, case): command-5 trail flash on even frames: thrown
/// along the velocity (+4..+8, ahead by 512 frames' worth) with a life
/// from the seed and scattered by the scale.
pub fn trail_flash_9192c(m: &mut Machine, record: u32) {
    with_frame(m, 0x30, |m, _| {
        if frame_counter(m) & 1 != 0 {
            return;
        }
        let flash = particle(m, 0x600, 3, 7);
        if flash == 0 {
            return;
        }
        let seed = lh(m, record + 2);
        let scale = lh(m, record + 0xa);
        m.sh(flash, div_pow2(seed, 10) as u16);
        let life = (sdk::rand(m) & 0xf).wrapping_add(0x40);
        m.sh(flash + 2, div_pow2(lo(life, seed), 12) as u16);
        let velocity = [lh(m, record + 4), lh(m, record + 6), lh(m, record + 8)];
        m.sh(flash + 0x16, 6);
        for (axis, &speed) in velocity.iter().enumerate() {
            m.sh(flash + 0x10 + axis as u32 * 2, speed as u16);
        }
        let mut position = [0u32; 3];
        for (axis, slot) in position.iter_mut().enumerate() {
            let base = m.lw(record + 0xc + axis as u32 * 4);
            *slot = base.wrapping_add(velocity[axis] << 9);
        }
        for (axis, &start) in position.iter().enumerate() {
            let spread = random_byte(m);
            if axis == 2 {
                m.sh(flash + 0x18, 0x111);
            }
            m.sw(
                flash + 4 + axis as u32 * 4,
                start.wrapping_add(sra(lo(spread, scale), 3)),
            );
        }
        let size = (sdk::rand(m) & 0xf).wrapping_add(0xf0);
        m.sh(flash + 0x1a, sra(lo(size, scale), 12) as u16);
    });
}

/// 80091AB4 (record, mode): command-5 smoke column (mode 0/1 on the ground,
/// 2 airborne): one smoke-pool particle moving with the velocity, started
/// 320 frames back along it.
pub fn smoke_column_91ab4(m: &mut Machine, record: u32, mode: u32) {
    with_frame(m, 0x20, |m, _| {
        let column = particle(m, 0x1098, 10, 7);
        if column == 0 {
            return;
        }
        m.sh(column, 0);
        let velocity = [lh(m, record + 4), lh(m, record + 6), lh(m, record + 8)];
        for (axis, &speed) in velocity.iter().enumerate() {
            m.sh(column + 0x10 + axis as u32 * 2, speed as u16);
        }
        for (axis, &speed) in velocity.iter().enumerate() {
            let offset = axis as u32 * 4;
            let base = m.lw(record + 0xc + offset);
            m.sw(
                column + 4 + offset,
                base.wrapping_sub(speed << 8).wrapping_sub(speed << 6),
            );
        }
        let random = (sdk::rand(m) & 0x7f).wrapping_add(0xa0);
        let scale = lh(m, record + 0xa);
        m.sh(column + 0x1a, div_pow2(lo(random, scale), 12) as u16);
        let seed = lh(m, record + 2);
        let size = if mode == 2 {
            m.sh(column + 2, div_pow2(seed, 8) as u16);
            m.sh(column + 0x16, 0x8c);
            0xa04
        } else {
            m.sh(column + 2, div_pow2(seed.wrapping_mul(0x50), 12) as u16);
            m.sh(column + 0x16, 0x18d);
            if mode != 0 { 0x1603 } else { 0x1600 }
        };
        m.sh(column + 0x18, size);
    });
}

/// 80091C14 (record, ring): command-5 shock wave at the record's position
/// (ring: radius word +4, flag 1), seed +2 and scale +0xA.
pub fn shock_wave_91c14(m: &mut Machine, record: u32, ring: u32) {
    with_frame(m, 0x20, |m, _| {
        let wave = particle(m, 0x33c0, 1, 6);
        if wave == 0 {
            return;
        }
        let position = [m.lw(record + 0xc), m.lw(record + 0x10), m.lw(record + 0x14)];
        for (axis, &word) in position.iter().enumerate() {
            m.sw(wave + 4 + axis as u32 * 4, word);
        }
        if ring != 0 {
            let radius = m.lw(record + 4);
            m.sh(wave + 0x14, 1);
            m.sw(wave + 0x10, radius);
        } else {
            m.sh(wave + 0x14, 0);
        }
        let seed = lh(m, record + 2);
        let scale = lh(m, record + 0xa);
        m.sw(wave, seed | 0x1000_0000);
        m.sh(wave + 0x16, scale as u16);
    });
}

/// 80091CAC (record, case): command-5 flash (cases 0-3 kind `case`, larger
/// cases kind 1/2 with flag 0x10) at the record's position with the word
/// +4 as its header and scale +0xA.
pub fn flash_91cac(m: &mut Machine, record: u32, case: u32) {
    with_frame(m, 0x20, |m, _| {
        let flash = claim_flash_91294(m);
        if flash == 0 {
            return;
        }
        let x = m.lw(record + 0xc);
        let y = m.lw(record + 0x10);
        m.sw(flash + 4, x);
        m.sw(flash + 8, y);
        let z = m.lw(record + 0x14);
        let head = m.lw(record + 4);
        m.sw(flash + 0xc, z);
        m.sw(flash, head);
        let scale = lh(m, record + 0xa);
        m.sh(flash + 0x16, 0);
        m.sh(flash + 0x1a, scale as u16);
        let kind = if lt(case, 4) {
            (case & 3) << 8
        } else {
            (((case & 1) + 1) << 8) | 0x10
        };
        m.sh(flash + 0x18, kind as u16);
    });
}

/// 80091D3C (record, count, mode): command-5 burst of `count` flashes with
/// the RNG seeded by +2 (restored afterwards): each kept with a random
/// stagger against +4 frames, scattered by +6 and sized by +0xA; mode 0
/// mixes small sparks, 1 and 2 pick the flash kind.
pub fn burst_91d3c(m: &mut Machine, record: u32, count: u32, mode: u32) {
    with_frame(m, 0x48, |m, frame| {
        if !lt(0, count) {
            return;
        }
        let mut index = 0u32;
        let saved = sdk::rand(m);
        let seed = lh(m, record + 2);
        m.sw(frame + 0x1c, saved);
        sdk::srand(m, seed);
        for axis in 0..3u32 {
            let word = m.lw(record + 0xc + axis * 4);
            m.sw(frame + 0x10 + axis * 4, word);
        }
        let delay = lh(m, record + 4);
        let spread = lh(m, record + 6);
        let scale = lh(m, record + 0xa);
        while lt(index, count) {
            let random = sdk::rand(m);
            let (start, life) = if mode != 0 {
                (
                    delay.wrapping_sub(index.wrapping_add(random & 3)),
                    (sra(random, 2) & 7) + 0x14,
                )
            } else {
                (
                    delay.wrapping_sub(index.wrapping_add(random & 1)),
                    (random & 7) + 8,
                )
            };
            if lt(start, 0) || !lt(start, life) {
                index = index.wrapping_add(1);
                sdk::rand(m);
                sdk::rand(m);
                continue;
            }
            let flash = claim_flash_91294(m);
            if flash != 0 {
                m.sw(flash, start | (life << 16));
                let random = sdk::rand(m);
                let dx = lo((random & 0x1ff).wrapping_sub(0x100), spread);
                let dy = lo((sra(random, 2) & 0x1ff).wrapping_sub(0x100), spread);
                let dz = lo((sra(random, 4) & 0x1ff).wrapping_sub(0x100), spread);
                m.sh(flash + 0x16, 0);
                for (axis, delta) in [dx, dy, dz].into_iter().enumerate() {
                    let offset = axis as u32 * 4;
                    let base = m.lw(frame + 0x10 + offset);
                    m.sw(flash + 4 + offset, base.wrapping_add(delta << 4));
                }
                let random = sdk::rand(m);
                let size = lo(scale, (sra(random, 3) & 0x7ff).wrapping_add(0xc00));
                m.sh(flash + 0x1a, sra(size, 12) as u16);
                let kind = match mode {
                    0 => {
                        if random & 3 != 0 {
                            0x100
                        } else {
                            0
                        }
                    }
                    1 => 0x110,
                    _ => 0x210,
                };
                m.sh(flash + 0x18, kind);
            }
            index = index.wrapping_add(1);
        }
        let saved = m.lw(frame + 0x1c);
        sdk::srand(m, saved);
    });
}

/// 80091F68 (record, count): command-5 spray of `count` smoke-pool
/// particles with staggered seeds (+4 + 2i), scattered by +6, sized by
/// +0xA.
pub fn spray_91f68(m: &mut Machine, record: u32, count: u32) {
    with_frame(m, 0x38, |m, _| {
        if !lt(0, count) {
            return;
        }
        let x = m.lw(record + 0xc);
        let y = m.lw(record + 0x10);
        let z = m.lw(record + 0x14);
        let mut seed = lh(m, record + 4);
        let spread = lh(m, record + 6);
        let scale = lh(m, record + 0xa);
        let mut index = 0u32;
        while lt(index, count) {
            let drop = particle(m, 0x1098, 10, 7);
            if drop == 0 {
                return;
            }
            m.sw(drop, seed << 16);
            let random = sdk::rand(m);
            let vy = lo((sra(random, 2) & 0xff).wrapping_sub(0x80), spread);
            let vz = lo((sra(random, 4) & 0xff).wrapping_sub(0x80), spread);
            let vx = lo((random & 0xff).wrapping_sub(0x80), spread);
            let high = sra(vy, 8);
            let low = sra(vy, 10);
            m.sh(drop + 0x16, 0x188);
            m.sw(drop + 4, x.wrapping_add(high << 9));
            let depth = sra(vz, 8);
            m.sw(drop + 8, y.wrapping_add(depth << 9));
            m.sh(drop + 0x12, high.wrapping_sub(low) as u16);
            m.sh(drop + 0x14, depth as u16);
            let side = sra(vx, 8);
            m.sw(drop + 0xc, z.wrapping_add(side << 9));
            m.sh(drop + 0x10, side as u16);
            let size = (sdk::rand(m) & 0xfff).wrapping_add(0x800);
            m.sh(drop + 0x1a, div_pow2(lo(scale, size), 12) as u16);
            index = index.wrapping_add(1);
            seed = seed.wrapping_add(2);
        }
    });
}

/// 800920D4 (record): command 8, an overlay tracer (kind 3, length +2)
/// from the packed points +8..+0x10 (low bytes from +4/+6) to +0x14..+0x1C.
pub fn overlay_tracer_920d4(m: &mut Machine, record: u32) {
    with_frame(m, 0x18, |m, _| {
        let tracer = dispatch::call(m, 0x8008_19a8, [record, 0, 0, 0]);
        if tracer == 0 {
            return;
        }
        let length = lh(m, record + 2);
        m.sh(tracer + 2, 3);
        m.sh(tracer, length as u16);
        let x = m.lw(record + 8);
        let y = m.lw(record + 0xc);
        let z = m.lw(record + 0x10);
        let low_x = lbu(m, record + 4);
        let low_y = lh(m, record + 6);
        m.sw(tracer + 4, (x & 0xffff_ff00) | low_x);
        m.sw(tracer + 0xc, z);
        m.sw(tracer + 8, (y & 0xffff_ff00) | (low_y & 0xff));
        for offset in [0u32, 4, 8] {
            let word = m.lw(record + 0x14 + offset);
            m.sw(tracer + 0x10 + offset, word);
        }
    });
}

/// The routines of this module by original address, with the original
/// argument registers; `v0` is the routine's result (0 for routines whose
/// callers ignore it).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, _] = args;
    let v0 = match target {
        0x8009_1294 => claim_flash_91294(m),
        0x8009_1310 => {
            explosion_91310(m, a0);
            0
        }
        0x8009_1520 => {
            large_explosion_91520(m, a0);
            0
        }
        0x8009_17d0 => {
            smoke_917d0(m, a0);
            0
        }
        0x8009_192c => {
            trail_flash_9192c(m, a0);
            0
        }
        0x8009_1ab4 => {
            smoke_column_91ab4(m, a0, a1);
            0
        }
        0x8009_1c14 => {
            shock_wave_91c14(m, a0, a1);
            0
        }
        0x8009_1cac => {
            flash_91cac(m, a0, a1);
            0
        }
        0x8009_1d3c => {
            burst_91d3c(m, a0, a1, a2);
            0
        }
        0x8009_1f68 => {
            spray_91f68(m, a0, a1);
            0
        }
        0x8009_20d4 => {
            overlay_tracer_920d4(m, a0);
            0
        }
        _ => return None,
    };
    Some(v0)
}
