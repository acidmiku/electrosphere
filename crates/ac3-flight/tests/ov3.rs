//! Overlay module 3 (state 7, the ending credits, `ac3_flight::ov3`)
//! against original entry/return snapshots.
//!
//! Capture `ovmisc-018`: from the title (`overlay-entry.sav`, CROSS at
//! 301) the documented one-register state selection (`.ovmisc/
//! snapshots.lua`: the first state-callback result 0 on or after frame 450
//! becomes 0x60000007) enters state 7; module 3 loads (frames 451..472) and
//! the credits run without input. Snapshots are taken only while module 3
//! is loaded: setup (frame 472), the first frames, the title, the layered
//! sequence with the reticle (from 1000) and the scrolling list (from 1900).
//!
//! CD-interrupt state (monitor/driver blocks, ring state, the stream ring)
//! is interrupt-owned.
mod support;

use ac3_flight::{
    Fault, Machine, dispatch, ov3,
    snapshot::{self, Snapshot},
    states::picture as pic,
};
use std::path::{Path, PathBuf};

type Call<'a> = dyn FnMut(&mut Machine, u32, [u32; 4]) -> u32 + 'a;

/// The credits' stream ring (decoder +0xD4, +0xD0 bytes; CD DMA).
fn credits_ring(entry: &Snapshot) -> (u32, u32) {
    let mut m = entry.machine();
    let decoder = m.lw(0x800b_a9f4 + 0x4e8);
    if decoder == 0 {
        return (0, 0);
    }
    let ring = m.lw(decoder + 0xd4);
    let size = m.lw(decoder + 0xd0);
    (ring, ring.wrapping_add(size))
}

fn capture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(name)
        .join("snapshots.jsonl")
}

const OWNED: &[(u32, u32, &str)] = &[
    (0x8000_0000, 0x8001_0000, "BIOS kernel RAM"),
    (0x800b_256c, 0x800b_2570, "sound interrupt flag"),
    (0x800b_2ec4, 0x800b_2ec8, "VSync root-counter reading"),
    (
        0x800b_2f2c,
        0x800b_3f44,
        "libetc interrupt context and stack",
    ),
    (0x800b_3ff4, 0x800b_3ff8, "VSync counter"),
    (0x800b_4010, 0x800b_4014, "GPU DMA callback slot"),
    (0x800b_990c, 0x800b_9914, "libgpu queue head/tail"),
    (0x800b_9914, 0x800b_9918, "libgpu saved interrupt mask"),
    (0x800b_9920, 0x800b_9928, "libgpu timeout"),
    (0x800b_f150, 0x800b_f1d0, "CD monitor and driver state"),
    (0x800c_31d0, 0x800c_3420, "CD ring state and sector headers"),
    (0x800c_5bcc, 0x800c_5bce, "sound driver tick state"),
    (0x800c_5c70, 0x800c_5c72, "sound driver tick state"),
    (0x800c_8230, 0x800c_9a30, "libgpu queue entries"),
    // ClearImage's packet words read from the GPU (`sdk.rs` clear
    // transfer: draw-mode status, drawing area of 800A83A8), device values.
    (
        0x800c_80f4,
        0x800c_80f8,
        "ClearImage draw mode (GPU status)",
    ),
    (
        0x800c_810c,
        0x800c_8118,
        "ClearImage drawing area (GPU status)",
    ),
];
fn owner(address: u32) -> Option<&'static str> {
    OWNED
        .iter()
        .find(|(s, e, _)| (*s..*e).contains(&address))
        .map(|(_, _, n)| *n)
}

struct Pair {
    entry: Snapshot,
    exit: Snapshot,
}
fn pair(index: &Path, callee: u32, occurrence: usize) -> Pair {
    let directory = index.parent().unwrap();
    let all = snapshot::read_index(index).unwrap();
    let entry = all
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{callee:08x}: no entry #{occurrence}"));
    let exit = all
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap_or_else(|| panic!("{callee:08x}: no return"));
    Pair {
        entry: Snapshot::load(directory, entry).unwrap(),
        exit: Snapshot::load(directory, exit).unwrap(),
    }
}
fn count(index: &Path, callee: u32) -> usize {
    snapshot::read_index(index)
        .unwrap()
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .count()
}

/// A recorded call: callee, RAM and scratchpad bytes it changed, GTE
/// registers at its return, `v0`, used.
type Recording = (u32, Vec<(usize, u8)>, Vec<(usize, u8)>, Gte, u32, bool);
type Gte = ([u32; 32], [u32; 32]);

/// Recorded calls of `pending` inside one call.
fn recorded(index: &Path, p: &Pair, pending: &[u32]) -> Vec<Recording> {
    let directory = index.parent().unwrap();
    let all = snapshot::read_index(index).unwrap();
    let window = p.entry.info.sequence..p.exit.info.sequence;
    let mut out = Vec::new();
    for e in all.iter().filter(|s| {
        s.stage == "entry" && pending.contains(&s.callee) && window.contains(&s.sequence)
    }) {
        let Some(x) = all.iter().find(|s| {
            s.callee == e.callee
                && s.stage == "return"
                && s.sequence > e.sequence
                && s.ra == e.ra
                && s.registers.gpr[29] == e.registers.gpr[29]
        }) else {
            continue;
        };
        let a = Snapshot::load(directory, e).unwrap();
        let b = Snapshot::load(directory, x).unwrap();
        let diff = (0..snapshot::STACK_START)
            .filter(|&i| a.ram[i] != b.ram[i])
            .map(|i| (i, b.ram[i]))
            .collect();
        let scratch = (0..a.scratch.len())
            .filter(|&i| a.scratch[i] != b.scratch[i])
            .map(|i| (i, b.scratch[i]))
            .collect();
        let gte = (b.info.registers.cp2d, b.info.registers.cp2c);
        out.push((e.callee, diff, scratch, gte, b.gpr(2), false));
    }
    out
}

/// Run `native` (with the pending routines replayed) and compare.
fn check(
    index: &Path,
    callee: u32,
    occurrence: usize,
    pending: &[u32],
    native: impl FnOnce(&mut Machine, &Snapshot, &mut Call) -> Option<u32>,
) -> usize {
    let p = pair(index, callee, occurrence);
    let ring = credits_ring(&p.entry);
    let mut calls = recorded(index, &p, pending);
    let mut m = p.entry.machine();
    let v0 = {
        let mut c = |m: &mut Machine, t: u32, a: [u32; 4]| -> u32 {
            if !pending.contains(&t) {
                return dispatch::call(m, t, a);
            }
            let call = calls
                .iter_mut()
                .find(|c| c.0 == t && !c.5)
                .unwrap_or_else(|| panic!("{t:08x}: no recording"));
            call.5 = true;
            for &(i, b) in &call.1 {
                let address = 0x8000_0000 + i as u32;
                if owner(address).is_none() {
                    m.sb(address, b);
                }
            }
            for &(i, b) in &call.2 {
                m.sb(0x1f80_0000 + i as u32, b);
            }
            (m.gte.data, m.gte.control) = call.3;
            call.4
        };
        native(&mut m, &p.entry, &mut c)
    };
    m.take_fault()
        .unwrap_or_else(|f| panic!("{callee:08x}#{occurrence}: {f}"));
    let d = snapshot::compare(&m, &p.exit);
    assert!(
        d.scratch.is_empty() && d.gte_data.is_empty() && d.gte_control.is_empty(),
        "{callee:08x}#{occurrence}: {}",
        d.summary()
    );
    let foreign: Vec<u32> = d
        .ram
        .iter()
        .copied()
        .filter(|&a| owner(a).is_none() && !(ring.0..ring.1).contains(&a))
        .collect();
    assert!(
        foreign.is_empty(),
        "{callee:08x}#{occurrence}: {} bytes differ: {:08x?}",
        foreign.len(),
        &foreign[..foreign.len().min(24)]
    );
    if let Some(v0) = v0 {
        assert_eq!(v0, p.exit.gpr(2), "{callee:08x}#{occurrence}: v0");
    }
    let changed = snapshot::changed_bytes(&p.entry, &p.exit);
    assert!(changed > 0 || v0.is_some(), "{callee:08x}: vacuous");
    changed
}

fn run_all(
    index: &Path,
    callee: u32,
    native: impl Fn(&mut Machine, &Snapshot, &mut Call) -> Option<u32>,
) {
    for occurrence in 0..count(index, callee) {
        let changed = check(index, callee, occurrence, &[], |m, e, c| native(m, e, c));
        println!("{callee:08X} #{occurrence}: {changed} bytes");
    }
}

#[test]
fn claims_only_while_loaded() {
    let mut m = Machine::default();
    m.sw(0x800b_a9f4 + 0x2dc, 0x800b_d158);
    m.sw(0x800b_d158 + 0x38, 5);
    assert_eq!(ov3::dispatch(&mut m, 0x800d_97c8, [0; 4]), None);
    m.sw(0x800b_d158 + 0x38, 3);
    m.sw(0x800b_d158 + 0x38c, 1);
    m.sw(0x8010_0008, 0x20);
    m.sw(0x8010_0028, 0x11);
    assert_eq!(
        ov3::dispatch(&mut m, 0x800d_97c8, [1, 0x8010_0000, 0, 0]),
        Some(0x8010_0030)
    );
    assert!(m.take_fault().is_ok());
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-018"]
fn credits_setup_matches_original() {
    let index = capture("ovmisc-018");
    let exit = pair(&index, 0x800d_67d4, 0).exit.ram;
    let changed = check(&index, 0x800d_67d4, 0, &[], |m, _, c| {
        m.device = Some(Box::new(StoreFromExit(exit)));
        Some(ov3::start(m, c))
    });
    println!("800D67D4: {changed} bytes");
    run_all(&index, 0x800d_653c, |m, _, _| {
        ov3::upload_images(m);
        None
    });
    run_all(&index, 0x800d_6488, |m, e, c| {
        ov3::load_credits(m, c, e.gpr(4));
        None
    });
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-018"]
fn credits_frames_match_original() {
    let index = capture("ovmisc-018");
    run_all(&index, 0x800d_94a0, |m, _, c| Some(ov3::frame(m, c)));
    run_all(&index, 0x800d_77b4, |m, _, c| {
        Some(ov3::credits_frame(m, c))
    });
    run_all(&index, 0x800d_6ff4, |m, e, c| {
        Some(ov3::next_chunk(m, c, e.gpr(4)))
    });
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-018"]
fn credits_drawing_matches_original() {
    let index = capture("ovmisc-018");
    run_all(&index, 0x800d_80a4, |m, e, _| {
        Some(ov3::layers(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    run_all(&index, 0x800d_8530, |m, e, _| {
        Some(ov3::draw_reticle(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    run_all(&index, 0x800d_90b0, |m, e, c| {
        Some(ov3::draw_credits(m, c, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    run_all(&index, 0x800d_95b0, |m, e, c| {
        let sp = e.gpr(29);
        let stack = std::array::from_fn(|i| m.lw(sp + 0x10 + 4 * i as u32));
        Some(ov3::draw_text(
            m,
            c,
            e.gpr(4),
            e.gpr(5),
            e.gpr(6),
            e.gpr(7),
            stack,
        ))
    });
    run_all(&index, 0x800d_98cc, |m, e, c| {
        Some(ov3::text_width(m, c, e.gpr(4), e.gpr(5)))
    });
}

/// Test device: StoreImage (host call 800A6970) delivers the VRAM words
/// the original stored, taken from the return snapshot.
struct StoreFromExit(Vec<u8>);
impl ac3_flight::machine::Device for StoreFromExit {
    fn call(
        &mut self,
        ram: &mut [u8],
        _scratch: &mut [u8],
        call: ac3_flight::machine::HostCall,
    ) -> Option<u32> {
        if call.function == 0x800a_6970 {
            let [rect, pixels, ..] = call.args;
            let half = |a: u32| {
                let i = (a & 0x1f_ffff) as usize;
                u32::from(u16::from_le_bytes([ram[i], ram[i + 1]]))
            };
            let bytes = (half(rect + 4) * half(rect + 6)).div_ceil(2) * 4;
            let start = (pixels & 0x1f_ffff) as usize;
            let end = start + bytes as usize;
            ram[start..end].copy_from_slice(&self.0[start..end]);
        }
        None
    }
}

/// 800D97C8 / 800D9814 / 800D9878 (no caller) with the differential oracle
/// (`tests/support/oracle.rs`) on the 800D95B0 entry of `ovmisc-018`
/// (module 3 loaded): random language tables (offsets with low bits the
/// lookups mask) and indices.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-018"]
fn text_tables_match_original() {
    let base = support::oracle::entries("ovmisc-018", 0x800d_95b0)
        .first()
        .expect("800D95B0 entry")
        .machine();
    let mut rng = support::oracle::Rng(0x000d_97c8);
    let table = 0x8014_0000;
    let mut cases = 0;
    for case in 0..300u32 {
        let mut m = base.clone();
        for i in (0..0x2000).step_by(4) {
            m.sw(table + i, 4 * (rng.next() % 0x300) + (rng.next() & 3));
        }
        let context = m.lw(0x800b_acd0);
        m.sw(context + 0x38c, rng.next() % 6);
        let a = rng.next() % 20;
        let b = rng.next() % 20;
        let changed = match case % 3 {
            0 => support::oracle::diff(
                "800D97C8",
                &m,
                (0x800d_97c8, 0x800d_9814),
                [a, table, 0, 0],
                |m| Some(ov3::text_entry(m, a, table)),
            ),
            1 => support::oracle::diff(
                "800D9814",
                &m,
                (0x800d_9814, 0x800d_9878),
                [a, b, table, 0],
                |m| Some(ov3::text_entry2(m, a, b, table)),
            ),
            _ => support::oracle::diff(
                "800D9878",
                &m,
                (0x800d_9878, 0x800d_98cc),
                [a, table, 0, 0],
                |m| Some(ov3::text_word(m, a, table)),
            ),
        };
        assert_eq!(changed, 0);
        cases += 1;
    }
    println!("800D97C8/800D9814/800D9878: {cases} cases (v0 compared)");
}

/// One `background` case: the decoder at `d` with `effect`, the shown
/// `bank` and `slot`, the slot's start and end (+0x14/+0x16), the effect
/// time and the shown picture.
struct Background {
    effect: u8,
    bank: u8,
    slot: u8,
    start: u16,
    end: u16,
    time: u32,
    shown: u16,
}
impl Background {
    fn set(&self, m: &mut Machine, d: u32) {
        m.sb(d + pic::EFFECT, self.effect);
        m.sb(d + pic::SHOWN_BANK, self.bank);
        m.sb(d + pic::SLOT, self.slot);
        let s = d + u32::from(self.bank) * pic::BANK_SIZE + u32::from(self.slot) * pic::SLOT_SIZE;
        m.sh(s + 0x14, self.start);
        m.sh(s + 0x16, self.end);
        m.sw(d + pic::EFFECT_TIME, self.time);
        m.sh(d + pic::SHOWN, self.shown);
    }
    /// The `break` the original reaches: (address, code), if any. Effect 4
    /// divides -(time << 7) by the span at 800D8224, effect 5 time << 7 at
    /// 800D82A4; each `div` is followed by the compiler's checks, `break 7`
    /// at +0xC and `break 6` at +0x24.
    fn trap(&self) -> Option<(u32, u32)> {
        let span = i32::from(self.end as i16) - i32::from(self.start as i16);
        let (n, div) = match self.effect {
            4 => ((self.time << 7).wrapping_neg() as i32, 0x800d_8224),
            5 => ((self.time << 7) as i32, 0x800d_82a4),
            _ => return None,
        };
        if span == 0 {
            Some((div + 0xc, 7))
        } else if span == -1 && n == i32::MIN {
            Some((div + 0x24, 6))
        } else {
            None
        }
    }
}

/// 800D81CC `background` (no capture reaches its fades) with the
/// differential oracle on the 800D95B0 entry of `ovmisc-018` (module 3
/// loaded). Random decoders (effects 4 fade out, 5 fade in, others; both
/// banks, slots 0..4, random slot spans and times, the shown picture 0x13
/// or another) are compared in full: RAM, the four SPRT packets and their
/// links, `v0`. Then the compiler's division checks, where only the first
/// fault is compared, because the original stops at its `break`: a zero
/// span (`break 7` at 800D8230 / 800D82B0) and the time 0x01000000 with
/// the span -1, `0x80000000 / -1` (`break 6` at 800D8248 / 800D82C8).
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-018"]
fn background_matches_original() {
    let base = support::oracle::entries("ovmisc-018", 0x800d_95b0)
        .first()
        .expect("800D95B0 entry")
        .machine();
    let range = (0x800d_81cc, 0x800d_8530);
    let (d, packets, ot) = (0x8014_0000, 0x8014_1000, 0x8014_2000);
    let args = [d, packets, ot, 0];
    let mut rng = support::oracle::Rng(0x000d_81cc);
    let mut cases = 0;
    for case in 0..600u32 {
        let start = rng.next() as u16;
        let c = Background {
            effect: [4, 5, (rng.next() % 8) as u8][(case % 3) as usize],
            bank: (rng.next() & 1) as u8,
            slot: (rng.next() % 5) as u8,
            start,
            end: if case % 7 == 0 {
                start.wrapping_add((rng.next() % 0x200) as u16)
            } else {
                rng.next() as u16
            },
            time: if case % 5 == 0 {
                rng.next() % 0x400
            } else {
                (rng.next() << 8) | (rng.next() & 0xff)
            },
            shown: if case % 4 == 0 {
                0x13
            } else {
                (rng.next() % 0x40) as u16
            },
        };
        if c.trap().is_some() {
            continue;
        }
        let mut m = base.clone();
        c.set(&mut m, d);
        let changed = support::oracle::diff("800D81CC", &m, range, args, |m| {
            Some(ov3::background(m, d, packets, ot))
        });
        assert!(changed > 0, "800D81CC case {case}: vacuous");
        cases += 1;
    }
    let traps = [
        (4, 0x0010, 0x0010, 0x0000_0123, (0x800d_8230, 7)),
        (5, 0xff80, 0xff80, 0x0000_0123, (0x800d_82b0, 7)),
        (4, 0x0001, 0x0000, 0x0100_0000, (0x800d_8248, 6)),
        (5, 0x0000, 0xffff, 0x0100_0000, (0x800d_82c8, 6)),
    ];
    for (effect, start, end, time, (address, code)) in traps {
        let c = Background {
            effect,
            bank: 1,
            slot: 2,
            start,
            end,
            time,
            shown: 0,
        };
        assert_eq!(c.trap(), Some((address, code)));
        let mut m = base.clone();
        c.set(&mut m, d);
        let mut original = m.clone();
        support::oracle::run(&mut original, range.0, range.1, args);
        let mut native = m;
        ov3::background(&mut native, d, packets, ot);
        let expected = Err(Fault::Trap { address, code });
        assert_eq!(original.take_fault(), expected, "original, effect {effect}");
        assert_eq!(native.take_fault(), expected, "native, effect {effect}");
        cases += 1;
    }
    println!("800D81CC: {cases} cases (4 of them at the division traps)");
}
