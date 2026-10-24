//! Effects command ring and particle effects (explosions, smoke, debris)
//! requested by the flight code.
//!
//! Commands are appended to the ring `[gp+0x544]` through 8007DF28
//! (`init_38818`); particles are allocated from the pools in the effect block
//! `[gp+0x41C]` (+0x600 flashes, +0x1098 smoke/debris, +0x33C0 shock waves)
//! through 8007FDB4 (`replay`), which returns the particle or 0. Both are
//! called by address. Conventions as in `player.rs`: original `a0..a3` as
//! `u32`, `v0` returned where used, every routine claimed in [`dispatch`].
use crate::{
    dispatch,
    machine::{Machine, SmallData},
};

/// 8007DF28: reserve `words` command words in the ring; returns their
/// address.
fn reserve(m: &mut Machine, words: u32) -> u32 {
    dispatch::call(m, 0x8007_df28, [words, 0, 0, 0])
}

/// 8007FDB4: a particle from the pool at effect block +`pool`
/// (`count`, `kind` as the original passes them); 0 when none is free.
fn particle(m: &mut Machine, pool: u32, count: u32, kind: u32) -> u32 {
    let block = m.gw(0x41c);
    dispatch::call(m, 0x8007_fdb4, [block.wrapping_add(pool), count, kind, 0])
}

fn rand(m: &mut Machine) -> u32 {
    crate::sdk::rand(m)
}

/// `((byte - 0x80) << shift)`, the signed jitter the effects derive from a
/// random byte.
fn jitter(byte: u32, shift: u32) -> u32 {
    (byte & 0xff).wrapping_sub(0x80) << shift
}

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}

/// Command header word: type `kind` in bits 0..3, 3 in bits 4..6, bits 7..8
/// clear, the rest kept.
fn header(word: u32, kind: u32) -> u32 {
    ((((word & 0xffff_fff0) | kind) & 0xffff_ff8f) | 0x30) & 0xffff_fe7f
}

/// 8007ED9C: append a 4-word command of `kind` (word 0 byte 3) with the
/// position (VECTOR >> 12 in the low 20 bits of words 1..3) and velocity
/// (>> 12 in their high 12 bits). Returns the command address.
pub fn command_8007d720(m: &mut Machine, kind: u32, position: u32, velocity: u32) -> u32 {
    let command = reserve(m, 4);
    let word0 = m.lw(command);
    let word1 = m.lw(command.wrapping_add(4));
    let word2 = m.lw(command.wrapping_add(8));
    let word3 = m.lw(command.wrapping_add(0xc));
    m.sw(command, header(word0, 4));
    m.sb(command.wrapping_add(3), kind as u8);
    let x = m.lw(position);
    let y = m.lw(position.wrapping_add(4));
    let z = m.lw(position.wrapping_add(8));
    let word1 = (word1 & 0xfff0_0000) | (x >> 12);
    let word2 = (word2 & 0xfff0_0000) | (y >> 12);
    let word3 = (word3 & 0xfff0_0000) | (z >> 12);
    m.sw(command.wrapping_add(4), word1);
    m.sw(command.wrapping_add(0xc), word3);
    m.sw(command.wrapping_add(8), word2);
    let vx = m.lw(velocity);
    let vy = m.lw(velocity.wrapping_add(4));
    let vz = m.lw(velocity.wrapping_add(8));
    let word1 = (word1 & 0xf_ffff) | sra(vx, 12) << 20;
    let word2 = (word2 & 0xf_ffff) | sra(vy, 12) << 20;
    let word3 = m.lw(command.wrapping_add(0xc));
    m.sw(command.wrapping_add(4), word1);
    m.sw(command.wrapping_add(8), word2);
    m.sw(
        command.wrapping_add(0xc),
        (word3 & 0xf_ffff) | sra(vz, 12) << 20,
    );
    command
}

/// 8007ECE8: append a 3-word command of `kind` (word 1 byte 0) at
/// `position` (the top 20 bits of each coordinate above the 12-bit fields).
/// Returns the command address.
pub fn command_8007d66c(m: &mut Machine, kind: u32, position: u32) -> u32 {
    let command = reserve(m, 3);
    let word0 = m.lw(command);
    m.sb(command.wrapping_add(4), kind as u8);
    let word1 = m.lw(command.wrapping_add(4)) & 0xfff;
    let word2 = m.lw(command.wrapping_add(8)) & 0xfff;
    let word0 = header(word0, 3);
    m.sw(command, word0);
    let x = m.lw(position) & 0xffff_f000;
    let y = m.lw(position.wrapping_add(4)) & 0xffff_f000;
    let z = m.lw(position.wrapping_add(8)) & 0xffff_f000;
    m.sw(command, (word0 & 0xfff) | x);
    m.sw(command.wrapping_add(4), word1 | y);
    m.sw(command.wrapping_add(8), word2 | z);
    command
}

/// 80082BAC: ground-contact burst a quarter of the way from `from` to `to`
/// (both VECTORs; the point lives in this routine's frame): 80082B54 with
/// `velocity` and `kind`. Returns its `v0`.
pub fn contact_burst_80081564(
    m: &mut Machine,
    from: u32,
    velocity: u32,
    to: u32,
    kind: u32,
) -> u32 {
    const FRAME: u32 = 0x28;
    let sp = m.push_frame(FRAME);
    let point = sp.wrapping_add(0x10);
    for i in 0..3 {
        let start = m.lw(from.wrapping_add(i * 4));
        let end = m.lw(to.wrapping_add(i * 4));
        let value = start.wrapping_add(sra(end.wrapping_sub(start), 2));
        m.sw(point.wrapping_add(i * 4), value);
    }
    let result = debris_8008150c(m, point, velocity, kind);
    m.pop_frame(FRAME);
    result
}

/// 80082B54: debris command 7 at `position`/`velocity` with byte 2 =
/// `kind`, then a smoke puff (80082A3C). Returns 80082A3C's `v0`.
pub fn debris_8008150c(m: &mut Machine, position: u32, velocity: u32, kind: u32) -> u32 {
    let command = command_8007d720(m, 7, position, velocity);
    m.sb(command.wrapping_add(2), kind as u8);
    smoke_800813f4(m, position, velocity)
}

/// 80082A3C: one smoke particle (pool +0x1098, 0xA/7) at `position` +
/// `velocity` + random jitter, drifting with velocity >> 10 (y - 0x90).
/// Returns the last z jitter (`v0`), or 0 when no particle is free.
pub fn smoke_800813f4(m: &mut Machine, position: u32, velocity: u32) -> u32 {
    let p = particle(m, 0x1098, 0xa, 7);
    if p == 0 {
        return 0;
    }
    let r = rand(m);
    m.sw(p, ((r & 3) + 9) << 16);
    m.sh(p.wrapping_add(0x18), 0x1c04);
    m.sh(p.wrapping_add(0x1a), ((r & 0x3f) + 0x32) as u16);
    let r = rand(m);
    let vx = m.lw(velocity);
    let px = m.lw(position);
    let vy = m.lw(velocity.wrapping_add(4));
    let vz = m.lw(velocity.wrapping_add(8));
    let x = px.wrapping_add(vx).wrapping_add(jitter(r, 8));
    m.sw(p.wrapping_add(4), x);
    let py = m.lw(position.wrapping_add(4));
    let y = py.wrapping_add(vy).wrapping_add(jitter(r >> 3, 8));
    m.sw(p.wrapping_add(8), y);
    let pz = m.lw(position.wrapping_add(8));
    m.sh(p.wrapping_add(0x14), sra(vz, 10) as u16);
    m.sh(p.wrapping_add(0x16), 0x8a);
    let z_jitter = jitter(r >> 6, 8);
    m.sh(p.wrapping_add(0x10), sra(vx, 10) as u16);
    m.sh(p.wrapping_add(0x12), sra(vy, 10).wrapping_sub(0x90) as u16);
    m.sw(
        p.wrapping_add(0xc),
        pz.wrapping_add(vz).wrapping_add(z_jitter),
    );
    z_jitter
}

/// 80081F94: crash explosion: command 3 at `position`/`velocity`, then the
/// particles of 80081D4C.
pub fn crash_explosion_8008094c(m: &mut Machine, position: u32, velocity: u32) {
    command_8007d720(m, 3, position, velocity);
    explosion_80080704(m, position, velocity);
}

/// 80081D4C: explosion particles at `position` moving against `velocity`:
/// with gameplay word 0 bit 0x100 the 80089504 burst instead; otherwise a
/// flash (pool +0x600), a shock wave (+0x33C0) and up to 20 smoke/debris
/// particles (+0x1098) with random spread.
pub fn explosion_80080704(m: &mut Machine, position: u32, velocity: u32) {
    let x = m.lw(position);
    let y = m.lw(position.wrapping_add(4));
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let flags = m.lw(gameplay);
    let z = m.lw(position.wrapping_add(8));
    if flags & 0x100 != 0 {
        burst_80087d94(m, x, y, z);
        return;
    }
    let p = particle(m, 0x600, 3, 7);
    if p != 0 {
        m.sw(p, 0xf_ffff);
        m.sw(p.wrapping_add(4), x);
        m.sw(p.wrapping_add(8), y);
        m.sw(p.wrapping_add(0xc), z);
        m.sh(p.wrapping_add(0x16), 0);
        m.sw(p.wrapping_add(0x18), 0x0190_0100);
    }
    let p = particle(m, 0x33c0, 1, 6);
    if p != 0 {
        m.sw(p, 0xc_0000);
        m.sw(p.wrapping_add(4), x);
        m.sw(p.wrapping_add(8), y);
        m.sw(p.wrapping_add(0xc), z);
        m.sw(p.wrapping_add(0x14), 0x02bc_0000);
    }
    let vy = m.lw(velocity.wrapping_add(4));
    let vz = m.lw(velocity.wrapping_add(8));
    let back_y = sra(vy.wrapping_neg(), 12);
    let vx = m.lw(velocity);
    let back_z = sra(vz.wrapping_neg(), 12);
    let back_x = sra(vx.wrapping_neg(), 12);
    let life = (back_x & 0x3f) + 0x258;
    for i in 0..0x14u32 {
        let p = particle(m, 0x1098, 0xa, 7);
        if p == 0 {
            return;
        }
        m.sh(p, 0);
        m.sh(p.wrapping_add(2), (i + 0x14) as u16);
        let r = rand(m);
        let spin = (r >> 8) | (r << 7);
        let px = x.wrapping_add(jitter(r, 8));
        let py = y.wrapping_add(jitter(r >> 3, 8));
        let pz = z.wrapping_add(jitter(r >> 6, 8));
        m.sh(
            p.wrapping_add(0x10),
            back_x.wrapping_add(jitter(spin, 1)) as u16,
        );
        m.sh(
            p.wrapping_add(0x12),
            back_y.wrapping_add(((spin >> 2) & 0x1fe).wrapping_sub(0x120)) as u16,
        );
        m.sh(
            p.wrapping_add(0x14),
            back_z.wrapping_add(jitter(spin >> 6, 1)) as u16,
        );
        m.sw(p.wrapping_add(4), px);
        m.sw(p.wrapping_add(8), py);
        m.sw(p.wrapping_add(0xc), pz);
        if i < 6 {
            m.sh(p.wrapping_add(0x16), 0x8a);
            m.sh(p.wrapping_add(0x18), 0x1804);
            m.sh(p.wrapping_add(0x1a), life as u16);
        } else {
            m.sh(p.wrapping_add(0x16), 0x188);
            m.sh(p.wrapping_add(0x18), 0);
            m.sh(p.wrapping_add(0x1a), ((i >> 1) + 0x12) as u16);
        }
    }
}

/// 80089504: large burst at (`x`, `y`, `z`): up to 6 flashes (pool +0x600)
/// spread by 11-bit jitter, a shock wave (+0x33C0) and up to 24 smoke and
/// debris particles (+0x1098), alternating long-lived smoke and debris.
pub fn burst_80087d94(m: &mut Machine, x: u32, y: u32, z: u32) {
    for i in 0..6u32 {
        let p = particle(m, 0x600, 3, 7);
        if p == 0 {
            break;
        }
        let r = rand(m);
        let life = (r & 7) + 0x18;
        let spin = (r << 8) | (r >> 7);
        m.sw(p.wrapping_add(4), x.wrapping_add(jitter(r, 11)));
        m.sh(p, !i as u16);
        m.sh(p.wrapping_add(0x10), jitter(spin, 1) as u16);
        m.sh(p.wrapping_add(0x12), jitter(spin >> 3, 1) as u16);
        m.sh(p.wrapping_add(0x14), jitter(spin >> 6, 1) as u16);
        m.sh(p.wrapping_add(0x16), 0x85);
        m.sh(p.wrapping_add(0x18), 0x100);
        m.sh(p.wrapping_add(2), life as u16);
        m.sw(p.wrapping_add(8), y.wrapping_add(jitter(r >> 3, 11)));
        m.sw(p.wrapping_add(0xc), z.wrapping_add(jitter(r >> 6, 11)));
        m.sh(p.wrapping_add(0x1a), ((spin & 0xff) + 0x12c) as u16);
    }
    let p = particle(m, 0x33c0, 1, 6);
    if p != 0 {
        let r = rand(m);
        m.sh(p, 0);
        m.sh(p.wrapping_add(2), ((r & 3) + 0x14) as u16);
        m.sw(p.wrapping_add(4), x);
        m.sw(p.wrapping_add(8), y);
        m.sw(p.wrapping_add(0xc), z);
        m.sh(p.wrapping_add(0x14), 0);
        m.sh(p.wrapping_add(0x16), ((r & 0xff) + 0x640) as u16);
    }
    let mut life = 0x244u32;
    for i in 0..0x18u32 {
        let p = particle(m, 0x1098, 0xa, 7);
        if p == 0 {
            return;
        }
        m.sh(p, 0);
        m.sh(p.wrapping_add(2), (i + 0x32) as u16);
        let r = rand(m);
        let spin = (r >> 8) | (r << 7);
        let px = x.wrapping_add(jitter(r, 11));
        let py = y.wrapping_add(jitter(r >> 3, 11));
        let pz = z.wrapping_add(jitter(r >> 6, 11));
        m.sh(p.wrapping_add(0x10), jitter(spin, 0) as u16);
        m.sh(p.wrapping_add(0x12), jitter(spin >> 3, 0) as u16);
        m.sh(p.wrapping_add(0x14), jitter(spin >> 6, 0) as u16);
        m.sw(p.wrapping_add(4), px);
        m.sw(p.wrapping_add(8), py);
        m.sw(p.wrapping_add(0xc), pz);
        if i & 1 != 0 {
            m.sh(p.wrapping_add(0x16), 0x87);
            m.sh(p.wrapping_add(0x18), 0x1804);
            m.sh(p.wrapping_add(0x1a), life as u16);
        } else {
            m.sh(p.wrapping_add(0x16), 0x180);
            m.sh(p.wrapping_add(0x18), 0);
            m.sh(p.wrapping_add(0x1a), (i + 0x1e) as u16);
        }
        life = life.wrapping_add(0x20);
    }
}

/// 800821EC: wreck debris of `kind` at `position`/`velocity`: with gameplay
/// word 0 bit 0x100 fragments (800829BC kind 9) and sparks (80081C30 kind
/// 1); otherwise, for even kinds, command 4 with bits 9..15 = `kind` & 0x7F
/// and the debris cloud 80081FDC.
pub fn wreck_debris_80080ba4(m: &mut Machine, position: u32, velocity: u32, kind: u32) {
    let gameplay = m.gw(crate::common::GAMEPLAY);
    if m.lw(gameplay) & 0x100 != 0 {
        fragments_80081374(m, position, velocity, 9);
        sparks_800805e8(m, position, velocity, 0, 1);
        return;
    }
    if kind & 1 != 0 {
        return;
    }
    let command = command_8007d720(m, 4, position, velocity);
    let word = (m.lw(command) & 0xffff_01ff) | (kind & 0x7f) << 9;
    m.sw(command, word);
    debris_cloud_80080994(m, position, velocity, kind);
}

// JP-REVIEW(R0046): near function: PAL 80081374 -> JP 800829BC, imm:1, 1 instructions differ (whole routine)
/// 800829BC: fragments command 6 with byte 2 = `kind` (kind 1 becomes 0xA
/// unless the context's stage (+0x390) is 0x2A), then the fragment particle
/// 800827E0.
// JP-PATCH: replaced by `crate::jp::res_p3_effects::fragments_80081374` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn fragments_80081374(m: &mut Machine, position: u32, velocity: u32, kind: u32) {
    crate::jp::res_p3_effects::fragments_80081374(m, position, velocity, kind)
}

/// 800827E0: one fragment particle (pool +0x1098) of `kind` < 11 from the
/// 11-entry (word, word) table at 0x800247AC (copied to the frame by the
/// original): position - 1.25 velocity, drift velocity >> 8, word +0 =
/// (entry0 >> 24) << 16, +0x16 = entry1 >> 16, +0x18 = entry1, +0x1A =
/// entry0 + (rand & entry0 byte 2).
pub fn fragment_80081198(m: &mut Machine, position: u32, velocity: u32, kind: u32) {
    const TABLE: u32 = 0x8002_47ac;
    if kind >= 0xb {
        return;
    }
    let p = particle(m, 0x1098, 0xa, 7);
    if p == 0 {
        return;
    }
    let vx = m.lw(velocity);
    let px = m.lw(position);
    let vy = m.lw(velocity.wrapping_add(4));
    let vz = m.lw(velocity.wrapping_add(8));
    m.sw(
        p.wrapping_add(4),
        px.wrapping_sub(vx).wrapping_sub(sra(vx, 2)),
    );
    let py = m.lw(position.wrapping_add(4));
    m.sw(
        p.wrapping_add(8),
        py.wrapping_sub(vy).wrapping_sub(sra(vy, 2)),
    );
    let pz = m.lw(position.wrapping_add(8));
    m.sh(p.wrapping_add(0x10), sra(vx, 8) as u16);
    m.sh(p.wrapping_add(0x12), sra(vy, 8) as u16);
    m.sh(p.wrapping_add(0x14), sra(vz, 8) as u16);
    m.sw(
        p.wrapping_add(0xc),
        pz.wrapping_sub(vz).wrapping_sub(sra(vz, 2)),
    );
    let first = m.lw(TABLE.wrapping_add(kind * 8));
    let second = m.lw(TABLE.wrapping_add(kind * 8 + 4));
    let r = rand(m);
    m.sw(p, sra(first, 24) << 16);
    m.sh(p.wrapping_add(0x16), (second >> 16) as u16);
    let mask = (first << 8) >> 24;
    m.sh(p.wrapping_add(0x18), second as u16);
    m.sh(p.wrapping_add(0x1a), first.wrapping_add(r & mask) as u16);
}

/// 80081C30: sparks of `kind` (with gameplay word 0 bit 0x100, kinds 0/1
/// become 1 and `phase` 0): kind 0 every 4th `phase`, kind 1 every 2nd,
/// kind 2 every 4th phase at random 1/2, kind 3 every 2nd phase at random
/// 3/4, kinds 4 and up always: command 2 with byte 2 = `kind`, then the
/// spark handler 80081BC8.
pub fn sparks_800805e8(m: &mut Machine, position: u32, velocity: u32, phase: u32, kind: u32) {
    let mut kind = kind;
    let mut phase = phase;
    if kind < 2 {
        let gameplay = m.gw(crate::common::GAMEPLAY);
        if m.lw(gameplay) & 0x100 != 0 {
            kind = 1;
            phase = 0;
        }
    }
    let emit = match kind {
        1 => phase & 1 == 0,
        0 => phase & 3 == 0,
        2 => phase & 3 == 0 && rand(m) & 1 == 0,
        3 => phase & 1 == 0 && rand(m) & 3 != 0,
        _ => true,
    };
    if !emit {
        return;
    }
    let command = command_8007d720(m, 2, position, velocity);
    m.sb(command.wrapping_add(2), kind as u8);
    spark_handler_80080580(m, position, velocity, kind);
}

/// 80081BC8: spark handler `kind` < 4 from the code-pointer table at
/// 0x8002479C (copied to the frame by the original), called with
/// (`position`, `velocity`, `kind`, table entry 3 in a3).
pub fn spark_handler_80080580(m: &mut Machine, position: u32, velocity: u32, kind: u32) {
    const TABLE: u32 = 0x8002_479c;
    let last = m.lw(TABLE + 0xc);
    if kind < 4 {
        let handler = m.lw(TABLE.wrapping_add(kind << 2));
        dispatch::call(m, handler, [position, velocity, kind, last]);
    }
}

/// 80081FDC: debris cloud at `position` + `velocity` + random 11-bit
/// offsets: a flash (pool +0x600) when `kind` & 7 is 0, a second flash, and
/// at random 3/4 a smoke or debris particle (+0x1098).
pub fn debris_cloud_80080994(m: &mut Machine, position: u32, velocity: u32, kind: u32) {
    let vx = m.lw(velocity);
    let vy = m.lw(velocity.wrapping_add(4));
    let vz = m.lw(velocity.wrapping_add(8));
    let mut point = [0u32; 3];
    for (i, (slot, v)) in point.iter_mut().zip([vx, vy, vz]).enumerate() {
        let r = rand(m);
        let base = m.lw(position.wrapping_add(i as u32 * 4));
        *slot = base.wrapping_add(v).wrapping_add((r & 0x3f) << 11);
    }
    let [x, y, z] = point;
    let low = rand(m);
    let high = rand(m);
    let r = low | high << 15;
    let flash = |m: &mut Machine, word: u32, size: u16, life: u32| {
        let p = particle(m, 0x600, 3, 7);
        if p != 0 {
            m.sw(p, word);
            m.sh(p.wrapping_add(0x18), size);
            m.sw(p.wrapping_add(4), x);
            m.sw(p.wrapping_add(8), y);
            m.sw(p.wrapping_add(0xc), z);
            m.sh(p.wrapping_add(0x16), 0);
            m.sh(p.wrapping_add(0x1a), life as u16);
        }
    };
    if kind & 7 == 0 {
        flash(m, 0xe_0000, 0x100, (r & 0x3f) + 0x3a);
    }
    flash(m, 0x10_0000, 0x210, (r & 0x3f) + 0x44);
    if r & 3 == 0 {
        return;
    }
    let p = particle(m, 0x1098, 0xa, 7);
    if p == 0 {
        return;
    }
    m.sw(p, 0x18_0000);
    m.sh(
        p.wrapping_add(0x10),
        sra(vx, 9).wrapping_sub(0x40).wrapping_add(r & 0x7f) as u16,
    );
    m.sh(
        p.wrapping_add(0x12),
        sra(vy, 9).wrapping_add(((r >> 3) & 0x7f).wrapping_sub(0x60)) as u16,
    );
    let drift_z = sra(vz, 9).wrapping_sub(0x40).wrapping_add((r >> 6) & 0x7f);
    m.sw(p.wrapping_add(4), x);
    m.sw(p.wrapping_add(8), y);
    m.sw(p.wrapping_add(0xc), z);
    m.sh(p.wrapping_add(0x14), drift_z as u16);
    if r & 1 != 0 {
        m.sh(p.wrapping_add(0x16), 0x188);
        m.sw(p.wrapping_add(0x18), 0xe_0000);
    } else {
        m.sh(p.wrapping_add(0x16), 0x8b);
        m.sh(p.wrapping_add(0x18), 0x1704);
        m.sh(p.wrapping_add(0x1a), ((r & 0x1f) + 0x3c) as u16);
    }
}

/// 8008276C: burning wreck at `position` (none with gameplay word 0 bit
/// 0x100): command 2 (8007ECE8) with word 1 bits 8..11 = `kind` & 0xF, then
/// the smoke of 80082594.
pub fn burning_80081124(m: &mut Machine, position: u32, kind: u32) {
    let gameplay = m.gw(crate::common::GAMEPLAY);
    if m.lw(gameplay) & 0x100 != 0 {
        return;
    }
    let command = command_8007d66c(m, 2, position);
    let word = (m.lw(command.wrapping_add(4)) & 0xffff_f0ff) | (kind & 0xf) << 8;
    m.sw(command.wrapping_add(4), word);
    wreck_smoke_80080f4c(m, position, kind);
}

/// 80082594: wreck smoke at `position` with random 12-bit offsets: for odd
/// `kind` a rising flame (pool +0x600) and at random 1/2 a smoke particle
/// (+0x1098); for even kinds a glow (+0x600).
pub fn wreck_smoke_80080f4c(m: &mut Machine, position: u32, kind: u32) {
    let mut point = [0u32; 3];
    for (i, slot) in point.iter_mut().enumerate() {
        let r = rand(m);
        let base = m.lw(position.wrapping_add(i as u32 * 4));
        *slot = base.wrapping_add(((r & 0x3f).wrapping_sub(0x20)) << 12);
    }
    let [x, y, z] = point;
    let r = rand(m);
    let place = |m: &mut Machine, p: u32| {
        m.sw(p.wrapping_add(4), x);
        m.sw(p.wrapping_add(8), y);
        m.sw(p.wrapping_add(0xc), z);
    };
    if kind & 1 != 0 {
        let p = particle(m, 0x600, 3, 7);
        if p != 0 {
            m.sw(p, 0x82_0000);
            m.sh(p.wrapping_add(0x10), (r & 0x3f) as u16);
            m.sh(p.wrapping_add(0x12), 0xff90);
            m.sh(p.wrapping_add(0x14), ((r >> 6) & 0x3f) as u16);
            m.sh(p.wrapping_add(0x16), 0x80);
            m.sh(p.wrapping_add(0x18), 0x210);
            place(m, p);
            m.sh(p.wrapping_add(0x1a), (((r >> 3) & 0x3f) + 0x4e) as u16);
        }
    } else {
        let p = particle(m, 0x600, 3, 7);
        if p != 0 {
            m.sw(p, 0xa_fffc);
            m.sh(p.wrapping_add(0x18), 0x100);
            place(m, p);
            m.sh(p.wrapping_add(0x16), 0);
            m.sh(p.wrapping_add(0x1a), ((r & 0x3f) + 0x30) as u16);
        }
    }
    if kind & 1 == 0 || r & 1 == 0 {
        return;
    }
    let p = particle(m, 0x1098, 0xa, 7);
    if p == 0 {
        return;
    }
    m.sh(p, 0xa);
    m.sh(p.wrapping_add(2), ((r & 0x1f) + 0x28) as u16);
    m.sh(p.wrapping_add(0x10), (r & 0x3f) as u16);
    m.sh(p.wrapping_add(0x12), 0xff90);
    m.sh(p.wrapping_add(0x14), ((r >> 6) & 0x3f) as u16);
    m.sh(p.wrapping_add(0x16), 0x80);
    m.sh(p.wrapping_add(0x18), 0x1604);
    place(m, p);
    m.sh(p.wrapping_add(0x1a), (((r >> 3) & 0x3f) + 0x64) as u16);
}

/// 80083CA4: ground-unit trail at `position`: `mode` 0 emits when `phase`
/// & 0xF is 0 (variant 0 when phase bits 4..6 are set, else variant 1 at
/// random 7/8), mode 1 when `phase` & 3 is 0, other modes always: command 6
/// (8007ECE8) with word 2 bits 4..11 = variant, word 1 bits 8..11 = `mode`,
/// then the trail handler 80083C3C.
pub fn unit_trail_8008265c(m: &mut Machine, position: u32, phase: u32, mode: u32) {
    if (mode == 0 && phase & 0xf != 0) || (mode == 1 && phase & 3 != 0) {
        return;
    }
    let variant = if mode != 0 {
        phase
    } else if sra(phase, 4) & 7 != 0 {
        0
    } else {
        u32::from(rand(m) & 7 != 0)
    };
    let command = command_8007d66c(m, 6, position);
    let word2 = (m.lw(command.wrapping_add(8)) & 0xffff_f00f) | (variant & 0xff) << 4;
    let word1 = (m.lw(command.wrapping_add(4)) & 0xffff_f0ff) | (mode & 0xf) << 8;
    m.sw(command.wrapping_add(4), word1);
    m.sw(command.wrapping_add(8), word2);
    trail_handler_800825f4(m, position, variant, mode);
}

/// 80083C3C: trail handler `mode` < 4 from the code-pointer table at
/// 0x80024804 (copied to the frame by the original), called with
/// (`position`, `variant`, `mode`, table entry 3 in a3).
pub fn trail_handler_800825f4(m: &mut Machine, position: u32, variant: u32, mode: u32) {
    const TABLE: u32 = 0x8002_4804;
    let last = m.lw(TABLE + 0xc);
    if mode < 4 {
        let handler = m.lw(TABLE.wrapping_add(mode << 2));
        dispatch::call(m, handler, [position, variant, mode, last]);
    }
}

/// 8008130C: effect `kind` at `position`: command 4 (8007ECE8) with word 2
/// bits 4..11 = `kind`, then the effect handler 80089BB4.
pub fn effect_8007fcc4(m: &mut Machine, position: u32, kind: u32) {
    let command = command_8007d66c(m, 4, position);
    let word = (m.lw(command.wrapping_add(8)) & 0xffff_f00f) | (kind & 0xff) << 4;
    m.sw(command.wrapping_add(8), word);
    effect_handler_80088444(m, position, kind);
}

/// 80089BB4: effect handler `kind` < 21 from the code-pointer table at
/// 0x80024874 (copied to the frame by the original), called with the
/// position's x, y, z and table entry 20 in a3.
pub fn effect_handler_80088444(m: &mut Machine, position: u32, kind: u32) {
    const TABLE: u32 = 0x8002_4874;
    let last = m.lw(TABLE + 0x50);
    if kind < 0x15 {
        let handler = m.lw(TABLE.wrapping_add(kind << 2));
        let x = m.lw(position);
        let y = m.lw(position.wrapping_add(4));
        let z = m.lw(position.wrapping_add(8));
        dispatch::call(m, handler, [x, y, z, last]);
    }
}

/// 8008350C: guided-weapon beam: command 9 at `position`/`velocity`, then
/// the beam geometry 800833C8.
pub fn beam_80081ec4(m: &mut Machine, position: u32, velocity: u32) {
    command_8007d720(m, 9, position, velocity);
    beam_geometry_80081d80(m, position, velocity);
}

/// 800833C8: beam geometry in the effect block (+0x3B28 = 1, +0x3B2E = 0):
/// end = `position` + 8 `velocity` (+0x3B30..), a cross segment (+0x3B3C..
/// +0x3B50) around it, cut by the ground ray 8009A800 when it hits (the hit
/// point from this routine's frame replaces the segment end), then the
/// final segment start.
pub fn beam_geometry_80081d80(m: &mut Machine, position: u32, velocity: u32) {
    const FRAME: u32 = 0x40;
    let sp = m.push_frame(FRAME);
    let block = m.gw(0x41c);
    let beam = block.wrapping_add(0x3b28);
    m.sw(beam, 1);
    m.sh(beam.wrapping_add(6), 0);
    let vx = m.lw(velocity);
    let vy = m.lw(velocity.wrapping_add(4));
    let vz = m.lw(velocity.wrapping_add(8));
    let x = m.lw(position).wrapping_add(vx << 3);
    let y = m.lw(position.wrapping_add(4)).wrapping_add(vy << 3);
    let x_eighth = sra(x, 8);
    let z = m.lw(position.wrapping_add(8)).wrapping_add(vz << 3);
    m.sw(beam.wrapping_add(8), x);
    m.sw(beam.wrapping_add(0xc), y);
    m.sw(beam.wrapping_add(0x14), x.wrapping_sub(x_eighth));
    m.sw(beam.wrapping_add(0x18), y.wrapping_add(0xfd8f_0000));
    let z_eighth = sra(z, 8);
    m.sw(beam.wrapping_add(0x1c), z.wrapping_sub(z_eighth));
    m.sw(beam.wrapping_add(0x20), x.wrapping_add(x_eighth));
    m.sw(beam.wrapping_add(0x24), y.wrapping_add(0x0271_0000));
    m.sw(beam.wrapping_add(0x10), z);
    m.sw(beam.wrapping_add(0x28), z.wrapping_add(z_eighth));
    let hit = sp.wrapping_add(0x10);
    let result = dispatch::call(
        m,
        0x8009_a800,
        [
            block.wrapping_add(0x3b3c),
            block.wrapping_add(0x3b48),
            hit,
            0x1f80_0000,
        ],
    );
    if result as i32 <= 0x7fff_fffe {
        for i in 0..3 {
            let value = m.lw(hit.wrapping_add(i * 4));
            m.sw(beam.wrapping_add(0x20 + i * 4), value);
        }
    }
    m.sw(beam.wrapping_add(0x14), x.wrapping_sub(x_eighth << 3));
    m.sw(beam.wrapping_add(0x18), y.wrapping_add(0xec78_0000));
    m.sw(beam.wrapping_add(0x1c), z.wrapping_sub(z_eighth << 3));
    m.pop_frame(FRAME);
}

/// Native targets owned by this module, by original address: original
/// `a0..a3`, returns `v0` (0 for routines without a result).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8007_ed9c => command_8007d720(m, a0, a1, a2),
        0x8007_ece8 => command_8007d66c(m, a0, a1),
        0x8008_2bac => contact_burst_80081564(m, a0, a1, a2, a3),
        0x8008_2b54 => debris_8008150c(m, a0, a1, a2),
        0x8008_2a3c => smoke_800813f4(m, a0, a1),
        0x8008_1f94 => {
            crash_explosion_8008094c(m, a0, a1);
            0
        }
        0x8008_1d4c => {
            explosion_80080704(m, a0, a1);
            0
        }
        0x8008_9504 => {
            burst_80087d94(m, a0, a1, a2);
            0
        }
        0x8008_21ec => {
            wreck_debris_80080ba4(m, a0, a1, a2);
            0
        }
        0x8008_29bc => {
            fragments_80081374(m, a0, a1, a2);
            0
        }
        0x8008_27e0 => {
            fragment_80081198(m, a0, a1, a2);
            0
        }
        0x8008_1c30 => {
            sparks_800805e8(m, a0, a1, a2, a3);
            0
        }
        0x8008_1bc8 => {
            spark_handler_80080580(m, a0, a1, a2);
            0
        }
        0x8008_1fdc => {
            debris_cloud_80080994(m, a0, a1, a2);
            0
        }
        0x8008_276c => {
            burning_80081124(m, a0, a1);
            0
        }
        0x8008_2594 => {
            wreck_smoke_80080f4c(m, a0, a1);
            0
        }
        0x8008_3ca4 => {
            unit_trail_8008265c(m, a0, a1, a2);
            0
        }
        0x8008_3c3c => {
            trail_handler_800825f4(m, a0, a1, a2);
            0
        }
        0x8008_130c => {
            effect_8007fcc4(m, a0, a1);
            0
        }
        0x8008_9bb4 => {
            effect_handler_80088444(m, a0, a1);
            0
        }
        0x8008_350c => {
            beam_80081ec4(m, a0, a1);
            0
        }
        0x8008_33c8 => {
            beam_geometry_80081d80(m, a0, a1);
            0
        }
        _ => return None,
    })
}
