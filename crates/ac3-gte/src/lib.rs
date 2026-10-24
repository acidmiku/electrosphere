//! Native GTE (COP2) register file and command set.
//!
//! Semantics follow PCSX-Redux `e1693b7f` (`src/core/gte-*.cc`), the emulator
//! that produced every original capture in this project, so raw register
//! storage matches captured dumps bit for bit:
//!
//! - registers are stored as raw 32-bit words; commands write only the low
//!   halfword of 16-bit fields (IR0-3, SZ0-3, OTZ, SXY halves);
//! - `mfc2` normalizes the stored word of sign/zero-extended registers before
//!   returning it, exactly like Redux, so a read has a storage side effect;
//! - `mtc2` to SXYP pushes the screen FIFO, IRGB expands into IR1-3, LZCS
//!   updates LZCR; `ctc2` sign-extends the 16-bit control registers;
//! - MAC1-3 use 44-bit accumulation with overflow checked after each add.
//!
//! The transfer methods and [`Gte::command`] assume every earlier write has
//! landed, which holds when at least two instructions separate a COP2 write
//! from the next COP2 read or command. For a command issued sooner, Redux's
//! write-to-use pipeline may read the old value; [`Gte::command_after_writes`]
//! reproduces it (see [`pipeline`]). `tests/redux_oracle.rs` replays the Redux
//! GTE oracle (`docs/GTE.md`) against all of this. PGXP and the widescreen
//! hack are off, as in every capture.
//!
//! Optionally ([`Gte::enable_subpixel`]), RTPS and RTPT also keep the
//! unrounded screen position of every vertex they project, in a
//! [`subpixel::Window`] beside the registers. It is not GTE state: it never
//! changes a register, is skipped by serde and ignored by `==`, and costs one
//! branch per projected vertex while disabled.
//!
//! The anamorphic widescreen hack ([`Gte::set_projection_x_scale`]) is host
//! configuration of the same kind: while set, RTPS and RTPT scale the
//! projected X offset `IR1 * H / SZ` by a fraction before adding OFX. It is
//! off (`None`) by default, which is the exact Redux behaviour.
#![forbid(unsafe_code)]

pub mod reg {
    pub const VXY0: usize = 0;
    pub const VZ0: usize = 1;
    pub const VXY1: usize = 2;
    pub const VZ1: usize = 3;
    pub const VXY2: usize = 4;
    pub const VZ2: usize = 5;
    pub const RGBC: usize = 6;
    pub const OTZ: usize = 7;
    pub const IR0: usize = 8;
    pub const IR1: usize = 9;
    pub const IR2: usize = 10;
    pub const IR3: usize = 11;
    pub const SXY0: usize = 12;
    pub const SXY1: usize = 13;
    pub const SXY2: usize = 14;
    pub const SXYP: usize = 15;
    pub const SZ0: usize = 16;
    pub const SZ1: usize = 17;
    pub const SZ2: usize = 18;
    pub const SZ3: usize = 19;
    pub const RGB0: usize = 20;
    pub const RGB1: usize = 21;
    pub const RGB2: usize = 22;
    pub const RES1: usize = 23;
    pub const MAC0: usize = 24;
    pub const MAC1: usize = 25;
    pub const MAC2: usize = 26;
    pub const MAC3: usize = 27;
    pub const IRGB: usize = 28;
    pub const ORGB: usize = 29;
    pub const LZCS: usize = 30;
    pub const LZCR: usize = 31;
}

pub mod ctrl {
    pub const R11R12: usize = 0;
    pub const R13R21: usize = 1;
    pub const R22R23: usize = 2;
    pub const R31R32: usize = 3;
    pub const R33: usize = 4;
    pub const TRX: usize = 5;
    pub const TRY: usize = 6;
    pub const TRZ: usize = 7;
    pub const L11L12: usize = 8;
    pub const L33: usize = 12;
    pub const RBK: usize = 13;
    pub const LR1LR2: usize = 16;
    pub const LB3: usize = 20;
    pub const RFC: usize = 21;
    pub const GFC: usize = 22;
    pub const BFC: usize = 23;
    pub const OFX: usize = 24;
    pub const OFY: usize = 25;
    pub const H: usize = 26;
    pub const DQA: usize = 27;
    pub const DQB: usize = 28;
    pub const ZSF3: usize = 29;
    pub const ZSF4: usize = 30;
    pub const FLAG: usize = 31;
}

pub mod flag {
    pub const ERROR: u32 = 1 << 31;
    pub const MAC1_POS: u32 = ERROR | 1 << 30;
    pub const MAC1_NEG: u32 = ERROR | 1 << 27;
    pub const MAC2_POS: u32 = ERROR | 1 << 29;
    pub const MAC2_NEG: u32 = ERROR | 1 << 26;
    pub const MAC3_POS: u32 = ERROR | 1 << 28;
    pub const MAC3_NEG: u32 = ERROR | 1 << 25;
    pub const IR1_SAT: u32 = ERROR | 1 << 24;
    pub const IR2_SAT: u32 = ERROR | 1 << 23;
    pub const IR3_SAT: u32 = 1 << 22;
    pub const COLOR_R_SAT: u32 = 1 << 21;
    pub const COLOR_G_SAT: u32 = 1 << 20;
    pub const COLOR_B_SAT: u32 = 1 << 19;
    pub const SZ_SAT: u32 = ERROR | 1 << 18;
    pub const DIV_OVER: u32 = ERROR | 1 << 17;
    pub const MAC0_POS: u32 = ERROR | 1 << 16;
    pub const MAC0_NEG: u32 = ERROR | 1 << 15;
    pub const SX_SAT: u32 = ERROR | 1 << 14;
    pub const SY_SAT: u32 = ERROR | 1 << 13;
    pub const IR0_SAT: u32 = 1 << 12;
    pub const ERROR_BITS: u32 = 0x7f87_e000;
}

/// Command function codes (low six bits of the COP2 imm25 word).
pub mod op {
    pub const RTPS: u32 = 0x01;
    pub const NCLIP: u32 = 0x06;
    pub const OP: u32 = 0x0c;
    pub const DPCS: u32 = 0x10;
    pub const INTPL: u32 = 0x11;
    pub const MVMVA: u32 = 0x12;
    pub const NCDS: u32 = 0x13;
    pub const CDP: u32 = 0x14;
    pub const NCDT: u32 = 0x16;
    pub const NCCS: u32 = 0x1b;
    pub const CC: u32 = 0x1c;
    pub const NCS: u32 = 0x1e;
    pub const NCT: u32 = 0x20;
    pub const SQR: u32 = 0x28;
    pub const DCPL: u32 = 0x29;
    pub const DPCT: u32 = 0x2a;
    pub const AVSZ3: u32 = 0x2d;
    pub const AVSZ4: u32 = 0x2e;
    pub const RTPT: u32 = 0x30;
    pub const GPF: u32 = 0x3d;
    pub const GPL: u32 = 0x3e;
    pub const NCCT: u32 = 0x3f;
}

/// Redux's COP2 write-to-use timing (`gte-write-to-use.h` and
/// `gte-write-to-use-model.cc`, from the nugget `gte-write-to-use` and
/// `gte-store-delay` hardware tests).
///
/// A `mtc2`, `ctc2` or `lwc2` lands two instructions after it issues. A
/// command issued before then reads the old value of a register unless enough
/// instructions separate the write from it ([`write_need`]). Such a late write
/// still lands while the command runs: one by the instruction right before the
/// command (distance 0) overwrites the command's result in that register, an
/// older one (distance 1) lands before the results, and registers the command
/// does not write take the written value either way. Reads (`mfc2`, `cfc2`,
/// `swc2`) see landed writes only.
///
/// All of this applies only while the GTE is idle. A command or read issued
/// while an earlier command is still running ([`still_busy`]) stalls the CPU,
/// and every write in flight lands during the stall: the command then sees
/// all of them, exactly as [`Gte::command`] after [`Gte::apply_write`].
pub mod pipeline {
    /// A transfer write still in flight when a command issues.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PendingWrite {
        /// `ctc2` (control register) rather than `mtc2`/`lwc2`.
        pub control: bool,
        pub index: usize,
        /// The written word, before `ctc2` sign extension or masking.
        pub value: u32,
        /// Instructions between the write and the command: 0 when the write is
        /// the instruction right before it, 1 for the one before that.
        pub distance: u32,
    }
    impl PendingWrite {
        /// Register number in Redux's numbering: data 0-31, control 32-63.
        pub fn register(&self) -> usize {
            usize::from(self.control) * 32 + self.index
        }
    }

    /// `(funct, register, need)` for every command except MVMVA.
    #[rustfmt::skip]
    const NEEDS: [(u8, u8, u8); 69] = [
        (0x01, 35, 2), (0x01, 39, 2),
        (0x06, 12, 2), (0x06, 13, 1), (0x06, 14, 2),
        (0x0c, 10, 1), (0x0c, 11, 2), (0x0c, 32, 1), (0x0c, 34, 2), (0x0c, 36, 1),
        (0x10, 6, 2), (0x10, 53, 2), (0x10, 54, 1),
        (0x11, 9, 2), (0x11, 10, 1), (0x11, 53, 2), (0x11, 54, 1),
        (0x13, 0, 2), (0x13, 40, 2), (0x13, 41, 1),
        (0x14, 9, 2), (0x14, 45, 2), (0x14, 46, 1), (0x14, 48, 2), (0x14, 49, 1),
        (0x16, 0, 2),
        (0x1b, 0, 2), (0x1b, 40, 2), (0x1b, 41, 1),
        (0x1c, 9, 2), (0x1c, 45, 2), (0x1c, 46, 1), (0x1c, 48, 2), (0x1c, 49, 1),
        (0x1e, 0, 2), (0x1e, 40, 2), (0x1e, 41, 1),
        (0x20, 0, 2),
        (0x28, 9, 2), (0x28, 10, 1),
        (0x29, 6, 2), (0x29, 9, 2), (0x29, 10, 1), (0x29, 53, 2), (0x29, 54, 1),
        (0x2a, 20, 2), (0x2a, 21, 2), (0x2a, 53, 2), (0x2a, 54, 1),
        (0x2d, 17, 2), (0x2d, 18, 1), (0x2d, 61, 2),
        (0x2e, 17, 2), (0x2e, 18, 1), (0x2e, 62, 2),
        (0x30, 0, 1), (0x30, 32, 1), (0x30, 35, 2), (0x30, 37, 1), (0x30, 39, 2),
        (0x3d, 8, 2), (0x3d, 9, 2), (0x3d, 10, 1),
        (0x3e, 8, 2), (0x3e, 9, 2), (0x3e, 10, 1), (0x3e, 25, 2), (0x3e, 26, 1),
        (0x3f, 0, 2),
    ];
    /// MVMVA by role: vector x, y, z; matrix pairs 0-4; translation x, y, z.
    const MVMVA_VECTOR: [u32; 3] = [2, 0, 0];
    const MVMVA_MATRIX: [u32; 5] = [2, 1, 0, 0, 0];
    const MVMVA_TRANSLATION: [u32; 3] = [2, 1, 0];

    /// Instructions that must separate a write to `register` (data 0-31,
    /// control 32-63) from command `code` for the command to read the new value.
    pub fn write_need(code: u32, register: usize) -> u32 {
        let funct = code & 0x3f;
        if funct != super::op::MVMVA {
            return NEEDS
                .iter()
                .find(|&&(f, r, _)| u32::from(f) == funct && usize::from(r) == register)
                .map_or(0, |&(_, _, need)| u32::from(need));
        }
        let mx = (code >> 17 & 3) as usize;
        let v = (code >> 15 & 3) as usize;
        let cv = (code >> 13 & 3) as usize;
        if let Some(c) = register.checked_sub(32) {
            if mx < 3 && (mx * 8..mx * 8 + 5).contains(&c) {
                return MVMVA_MATRIX[c - mx * 8];
            }
            if cv < 3 && (cv * 8 + 5..cv * 8 + 8).contains(&c) {
                return MVMVA_TRANSLATION[c - cv * 8 - 5];
            }
            return 0;
        }
        if v == 3 {
            return if (9..=11).contains(&register) {
                MVMVA_VECTOR[register - 9]
            } else {
                0
            };
        }
        if register == v * 2 {
            MVMVA_VECTOR[0].max(MVMVA_VECTOR[1])
        } else if register == v * 2 + 1 {
            MVMVA_VECTOR[2]
        } else {
            0
        }
    }

    /// Cycles a command keeps the GTE busy after its instruction (Redux, equal
    /// to the nocash timings); 0 for an unknown command.
    pub fn latency(code: u32) -> u32 {
        match code & 0x3f {
            0x01 => 15,
            0x06 | 0x10 | 0x11 | 0x12 | 0x29 => 8,
            0x0c | 0x2e => 6,
            0x13 => 19,
            0x14 => 13,
            0x16 => 44,
            0x1b | 0x2a => 17,
            0x1c => 11,
            0x1e => 14,
            0x20 => 30,
            0x28 | 0x2d | 0x3d | 0x3e => 5,
            0x30 => 23,
            0x3f => 39,
            _ => 0,
        }
    }
    /// Whether a COP2 command or read issued `instructions` after command `code`
    /// (1 for the next instruction) still finds the GTE busy and stalls.
    /// Redux's interpreter charges two cycles per instruction; this holds when
    /// the instructions in between are not loads or stores, which may cost more.
    pub fn still_busy(code: u32, instructions: u32) -> bool {
        2 * instructions < latency(code) + 2
    }

    const fn bits(first: u32, last: u32) -> u64 {
        (u64::MAX >> (63 - last)) & (u64::MAX << first)
    }
    /// Registers (data 0-31, control 32-63) a command writes; 0 when unknown.
    pub(crate) fn command_outputs(code: u32) -> u64 {
        const FLAG: u64 = 1 << 63;
        const MAC0: u64 = 1 << 24;
        const VECTOR: u64 = bits(9, 11) | bits(25, 27);
        const COLOR: u64 = VECTOR | bits(20, 22);
        match code & 0x3f {
            0x01 | 0x30 => FLAG | bits(8, 19) | bits(24, 27),
            0x06 => FLAG | MAC0,
            0x0c | 0x12 | 0x28 => FLAG | VECTOR,
            0x2d | 0x2e => FLAG | MAC0 | 1 << 7,
            0x10 | 0x11 | 0x13 | 0x14 | 0x16 | 0x1b | 0x1c | 0x1e | 0x20 | 0x29 | 0x2a | 0x3d
            | 0x3e | 0x3f => FLAG | COLOR,
            _ => 0,
        }
    }
    /// Registers a write sets: SXYP shifts the screen FIFO, IRGB sets IR1-3,
    /// LZCS sets LZCR, and LZCR itself is read-only.
    pub(crate) fn write_targets(write: &PendingWrite) -> u64 {
        if write.control {
            return 1 << (32 + write.index);
        }
        match write.index {
            15 => bits(12, 15),
            28 => bits(9, 11) | 1 << 28,
            30 => bits(30, 31),
            31 => 0,
            index => 1 << index,
        }
    }
}

/// Unrounded screen positions of projected vertices, for renderers that draw
/// at a higher resolution than the PlayStation (the "wobble fix").
///
/// RTPS/RTPT compute `SX = (OFX + IR1 * ratio) >> 16` (and SY likewise), so
/// the screen FIFO holds whole pixels. The unrounded value is the same sum
/// divided by 2^16 instead of shifted: the GTE's own inputs and arithmetic
/// (IR1, the reciprocal `ratio`), without the final truncation. Depth is SZ3's
/// sum divided by 2^12. Each position therefore floors exactly to the integer
/// the GTE wrote; the f32 conversion is kept below the next integer.
pub mod subpixel {
    /// Default [`Window`] capacity: 32 entries.
    pub const DEFAULT_CAPACITY: usize = 32;
    /// Buckets of the window's membership filter.
    const BUCKETS: usize = 256;

    /// The most recent screen words with their unrounded positions, newest
    /// last: one entry per vertex RTPS/RTPT projected, plus any a memory
    /// tracker adds ([`Window::push`]) when it reloads a tracked word. A
    /// vertex whose SX or SY saturated has no position (`None`).
    #[derive(Clone, Debug)]
    pub struct Window {
        words: Box<[u32]>,
        positions: Box<[Option<[f32; 3]>]>,
        /// Entries per hash bucket of their word, so most lookups of a word
        /// that is not in the window end without a scan.
        filter: Box<[u16; BUCKETS]>,
        /// Slot of the next entry.
        next: usize,
        len: usize,
        projected: u64,
    }

    fn bucket(word: u32) -> usize {
        (word.wrapping_mul(0x9e37_79b1) >> 24) as usize
    }

    impl Window {
        /// An empty window of `capacity` entries (1 to 65535).
        pub fn new(capacity: usize) -> Self {
            let capacity = capacity.clamp(1, usize::from(u16::MAX));
            Self {
                words: vec![0; capacity].into_boxed_slice(),
                positions: vec![None; capacity].into_boxed_slice(),
                filter: Box::new([0; BUCKETS]),
                next: 0,
                len: 0,
                projected: 0,
            }
        }
        pub fn capacity(&self) -> usize {
            self.words.len()
        }
        pub fn len(&self) -> usize {
            self.len
        }
        pub fn is_empty(&self) -> bool {
            self.len == 0
        }
        /// Vertices RTPS/RTPT projected since the window was created.
        pub fn projected(&self) -> u64 {
            self.projected
        }
        /// Forget every entry (the projection count stays).
        pub fn clear(&mut self) {
            self.next = 0;
            self.len = 0;
            self.filter.fill(0);
        }
        /// Append `word` with its position; the oldest entry drops out once
        /// the window is full.
        pub fn push(&mut self, word: u32, position: Option<[f32; 3]>) {
            let capacity = self.capacity();
            if self.len == capacity {
                self.filter[bucket(self.words[self.next])] -= 1;
            }
            self.filter[bucket(word)] += 1;
            self.words[self.next] = word;
            self.positions[self.next] = position;
            self.next = (self.next + 1) % capacity;
            self.len = (self.len + 1).min(capacity);
        }
        /// The position of the newest entry holding `word`: `None` when no
        /// entry holds it, or when the newest that does has no position.
        pub fn find(&self, word: u32) -> Option<[f32; 3]> {
            if self.filter[bucket(word)] == 0 {
                return None;
            }
            let (newer, older) = self.words.split_at(self.next);
            if let Some(at) = newer.iter().rposition(|&w| w == word) {
                return self.positions[at];
            }
            if self.len == self.capacity()
                && let Some(at) = older.iter().rposition(|&w| w == word)
            {
                return self.positions[self.next + at];
            }
            None
        }
        /// The newest entry.
        pub fn newest(&self) -> Option<(u32, Option<[f32; 3]>)> {
            let at = (self.next + self.capacity() - 1) % self.capacity();
            (self.len > 0).then(|| (self.words[at], self.positions[at]))
        }
        /// Entries newest first.
        pub fn entries(&self) -> impl Iterator<Item = (u32, Option<[f32; 3]>)> + '_ {
            let capacity = self.capacity();
            (1..=self.len).map(move |back| {
                let at = (self.next + capacity - back) % capacity;
                (self.words[at], self.positions[at])
            })
        }
        /// One projected vertex: `word` is the SXY2 word written from the
        /// 16.16 sums `sx`, `sy`; `sz` is SZ3's sum (before `>> 12`).
        /// `overflow`: the divide overflowed (SZ3 < H/2, a vertex nearer
        /// than the GTE can project), so the screen position is clamped
        /// arithmetic, not a projection; it gets no position.
        pub(crate) fn project(&mut self, word: u32, sx: i64, sy: i64, sz: i64, overflow: bool) {
            self.projected += 1;
            let screen = -0x400..=0x3ff;
            let position =
                (!overflow && screen.contains(&(sx >> 16)) && screen.contains(&(sy >> 16)))
                    .then(|| [unrounded(sx, 16), unrounded(sy, 16), depth(sz)]);
            self.push(word, position);
        }
    }

    /// `value / 2^bits` as f32, never rounded up to the next integer, so it
    /// floors to `value >> bits` (exact in f64 below 2^53).
    fn unrounded(value: i64, bits: u32) -> f32 {
        let exact = value as f64 / f64::from(1_u32 << bits);
        let rounded = exact as f32;
        let ceiling = ((value >> bits) + 1) as f32;
        if rounded >= ceiling {
            ceiling.next_down()
        } else {
            rounded
        }
    }
    /// SZ3 unrounded: its sum / 2^12, saturated like SZ3 (0..=0xFFFF).
    fn depth(sz: i64) -> f32 {
        match sz >> 12 {
            z if z < 0 => 0.0,
            z if z > 0xffff => 65535.0,
            _ => unrounded(sz, 12),
        }
    }
}

/// The optional [`subpixel::Window`] of a [`Gte`]: not part of its state,
/// so every two of them compare equal.
#[derive(Clone, Default)]
struct SubpixelSlot(Option<Box<subpixel::Window>>);
impl PartialEq for SubpixelSlot {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for SubpixelSlot {}
impl std::fmt::Debug for SubpixelSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(window) => write!(f, "on ({} entries)", window.len()),
            None => f.write_str("off"),
        }
    }
}

/// The projection X scale of a [`Gte`] ([`Gte::set_projection_x_scale`]):
/// host configuration, not part of its state, so every two of them compare
/// equal.
#[derive(Clone, Copy, Debug, Default)]
struct XScaleSlot(Option<(u32, u32)>);
impl PartialEq for XScaleSlot {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for XScaleSlot {}

/// The whole GTE state (save states serialize it as is). The optional
/// sub-pixel window and the projection X scale are not state: serde skips
/// them and `==` ignores them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Gte {
    pub data: [u32; 32],
    pub control: [u32; 32],
    #[serde(skip)]
    subpixel: SubpixelSlot,
    #[serde(skip)]
    x_scale: XScaleSlot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownCommand(pub u32);
impl std::fmt::Display for UnknownCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown GTE command {:#09x}", self.0)
    }
}
impl std::error::Error for UnknownCommand {}

/// 44-bit accumulator with per-addition overflow tracking.
#[derive(Clone, Copy)]
struct Int44 {
    value: i64,
    pos: bool,
    neg: bool,
}
impl Int44 {
    fn new(value: i64) -> Self {
        Self {
            value,
            pos: value > 0x7ff_ffff_ffff,
            neg: value < -0x800_0000_0000,
        }
    }
    fn add(self, rhs: i64) -> Self {
        let result = (self.value.wrapping_add(rhs) << 20) >> 20;
        Self {
            value: result,
            pos: self.pos || (result < 0 && self.value >= 0 && rhs >= 0),
            neg: self.neg || (result >= 0 && self.value < 0 && rhs < 0),
        }
    }
}

const fn unr_table() -> [u8; 257] {
    let mut table = [0; 257];
    let mut i = 0;
    while i < 257 {
        let value = ((0x40000 / (i as i32 + 0x100)) + 1) / 2 - 0x101;
        table[i] = if value < 0 { 0 } else { value as u8 };
        i += 1;
    }
    table
}
const UNR: [u8; 257] = unr_table();

fn shift(sf: bool, value: i64) -> i64 {
    if sf { value >> 12 } else { value }
}

impl Gte {
    // ---- sub-pixel window -------------------------------------------------
    /// Keep the unrounded position of every projected vertex in a new, empty
    /// window of `capacity` entries (see [`subpixel`]). Registers and flags
    /// are unaffected.
    pub fn enable_subpixel(&mut self, capacity: usize) {
        self.subpixel.0 = Some(Box::new(subpixel::Window::new(capacity)));
    }
    pub fn disable_subpixel(&mut self) {
        self.subpixel.0 = None;
    }
    pub fn subpixel(&self) -> Option<&subpixel::Window> {
        self.subpixel.0.as_deref()
    }
    pub fn subpixel_mut(&mut self) -> Option<&mut subpixel::Window> {
        self.subpixel.0.as_deref_mut()
    }

    // ---- anamorphic widescreen ----------------------------------------------
    /// Scale the X of every vertex RTPS/RTPT project by `num / den` about
    /// OFX (`SX = OFX + floor(IR1 * ratio * num / den)`, in exact integers),
    /// or project normally with `None`. A 16:9 view squeezed into the 4:3
    /// frame uses `(3, 4)`. The sub-pixel window gets the same scaled sum,
    /// so its positions still floor to the words. A zero numerator or
    /// denominator means `None`. Not GTE state: serde skips it and `==`
    /// ignores it.
    pub fn set_projection_x_scale(&mut self, scale: Option<(u32, u32)>) {
        self.x_scale.0 = scale.filter(|&(num, den)| num > 0 && den > 0);
    }
    /// The projection X scale ([`Self::set_projection_x_scale`]).
    pub fn projection_x_scale(&self) -> Option<(u32, u32)> {
        self.x_scale.0
    }

    // ---- raw field access -------------------------------------------------
    fn lo(word: u32) -> i16 {
        word as u16 as i16
    }
    fn hi(word: u32) -> i16 {
        (word >> 16) as u16 as i16
    }
    fn set_lo(word: &mut u32, value: i32) {
        *word = (*word & 0xffff_0000) | (value as u32 & 0xffff);
    }
    fn set_hi(word: &mut u32, value: i32) {
        *word = (*word & 0x0000_ffff) | ((value as u32 & 0xffff) << 16);
    }
    fn set_byte(word: &mut u32, index: u32, value: i32) {
        let shift = index * 8;
        *word = (*word & !(0xff << shift)) | ((value as u32 & 0xff) << shift);
    }
    fn byte(word: u32, index: u32) -> u8 {
        (word >> (index * 8)) as u8
    }

    pub fn flag(&self) -> u32 {
        self.control[ctrl::FLAG]
    }
    fn raise(&mut self, bits: u32) {
        self.control[ctrl::FLAG] |= bits;
    }
    pub fn ir(&self, index: usize) -> i16 {
        Self::lo(self.data[reg::IR0 + index])
    }
    fn set_ir(&mut self, index: usize, value: i32) {
        Self::set_lo(&mut self.data[reg::IR0 + index], value);
    }
    pub fn mac(&self, index: usize) -> i32 {
        self.data[reg::MAC0 + index] as i32
    }
    fn set_mac(&mut self, index: usize, value: i32) {
        self.data[reg::MAC0 + index] = value as u32;
    }
    pub fn sz(&self, index: usize) -> u16 {
        self.data[reg::SZ0 + index] as u16
    }
    fn vertex(&self, v: usize) -> [i16; 3] {
        if v < 3 {
            [
                Self::lo(self.data[v * 2]),
                Self::hi(self.data[v * 2]),
                Self::lo(self.data[v * 2 + 1]),
            ]
        } else {
            [self.ir(1), self.ir(2), self.ir(3)]
        }
    }
    fn rgb(&self) -> [u8; 4] {
        self.data[reg::RGBC].to_le_bytes()
    }
    fn cs(&self, index: usize) -> i64 {
        i64::from(self.control[index] as i32)
    }
    fn matrix_element(&self, mx: usize, row: usize, col: usize) -> i32 {
        if mx < 3 {
            let linear = row * 3 + col;
            let word = self.control[mx * 8 + linear / 2];
            i32::from(if linear & 1 == 1 {
                Self::hi(word)
            } else {
                Self::lo(word)
            })
        } else {
            // Garbage matrix: {-R<<4, R<<4, IR0, R13 x3, R22 x3}.
            let r = i32::from(Self::byte(self.data[reg::RGBC], 0));
            match row * 3 + col {
                0 => (-r) << 4,
                1 => r << 4,
                2 => i32::from(self.ir(0)),
                3..=5 => i32::from(Self::lo(self.control[ctrl::R13R21])),
                _ => i32::from(Self::lo(self.control[ctrl::R22R23])),
            }
        }
    }
    fn control_vector(&self, cv: usize, component: usize) -> i64 {
        if cv == 3 {
            0
        } else {
            self.cs(cv * 8 + 5 + component)
        }
    }

    // ---- limiters -----------------------------------------------------------
    fn lim(&mut self, value: i32, max: i32, min: i32, bits: u32) -> i32 {
        if value > max {
            self.raise(bits);
            max
        } else if value < min {
            self.raise(bits);
            min
        } else {
            value
        }
    }
    fn bounds(&mut self, sf: bool, value: Int44, pos: u32, neg: u32) -> i32 {
        if value.pos {
            self.raise(pos);
        }
        if value.neg {
            self.raise(neg);
        }
        shift(sf, value.value) as i32
    }
    fn a(&mut self, index: usize, sf: bool, value: Int44) -> i32 {
        match index {
            1 => self.bounds(sf, value, flag::MAC1_POS, flag::MAC1_NEG),
            2 => self.bounds(sf, value, flag::MAC2_POS, flag::MAC2_NEG),
            _ => self.bounds(sf, value, flag::MAC3_POS, flag::MAC3_NEG),
        }
    }
    fn f(&mut self, value: i64) -> i64 {
        if value > 0x7fff_ffff {
            self.raise(flag::MAC0_POS);
        }
        if value < -0x8000_0000 {
            self.raise(flag::MAC0_NEG);
        }
        value
    }
    fn lim_b(&mut self, index: usize, lm: bool, value: i32) -> i32 {
        let bits = [0, flag::IR1_SAT, flag::IR2_SAT, flag::IR3_SAT][index];
        self.lim(value, 0x7fff, if lm { 0 } else { -0x8000 }, bits)
    }
    fn lim_b3_sf(&mut self, sf: bool, lm: bool, raw: i64) -> i32 {
        let value_sf = shift(sf, raw) as i32;
        let value12 = (raw >> 12) as i32;
        if !(-0x8000..=0x7fff).contains(&value12) {
            self.raise(flag::IR3_SAT);
        }
        value_sf.clamp(if lm { 0 } else { -0x8000 }, 0x7fff)
    }
    fn lim_c(&mut self, index: usize, value: i32) -> i32 {
        let bits = [flag::COLOR_R_SAT, flag::COLOR_G_SAT, flag::COLOR_B_SAT][index];
        self.lim(value, 0xff, 0, bits)
    }
    fn lim_d(&mut self, raw: i64) -> i32 {
        self.lim((raw >> 12) as i32, 0xffff, 0, flag::SZ_SAT)
    }
    fn lim_g(&mut self, value: i64, bits: u32) -> i32 {
        if value > 0x3ff {
            self.raise(bits);
            0x3ff
        } else if value < -0x400 {
            self.raise(bits);
            -0x400
        } else {
            value as i32
        }
    }
    fn lim_h(&mut self, raw: i64) -> i32 {
        let value_sf = raw >> 12;
        let value12 = (raw >> 12) as i32;
        if !(0..=0x1000).contains(&value_sf) {
            self.raise(flag::IR0_SAT);
        }
        value12.clamp(0, 0x1000)
    }

    fn divide(&mut self, numerator: u16, denominator: u16) -> u32 {
        if u32::from(numerator) >= u32::from(denominator) * 2 {
            self.raise(flag::DIV_OVER);
            return 0x1ffff;
        }
        let shift = denominator.leading_zeros();
        let r1 = ((u32::from(denominator) << shift) & 0x7fff) as i64;
        let r2 = i64::from(UNR[((r1 + 0x40) >> 7) as usize]) + 0x101;
        let r3 = ((0x80 - r2 * (r1 + 0x8000)) >> 8) & 0x1ffff;
        let reciprocal = ((r2 * r3 + 0x80) >> 8) as u64;
        let result = (reciprocal * u64::from(u32::from(numerator) << shift) + 0x8000) >> 16;
        result.min(0x1ffff) as u32
    }

    fn push_z(&mut self, z: i32) {
        for i in 0..3 {
            let next = self.data[reg::SZ0 + i + 1] & 0xffff;
            Self::set_lo(&mut self.data[reg::SZ0 + i], next as i32);
        }
        Self::set_lo(&mut self.data[reg::SZ3], z);
    }
    fn push_color(&mut self) {
        self.data[reg::RGB0] = self.data[reg::RGB1];
        self.data[reg::RGB1] = self.data[reg::RGB2];
        let code = Self::byte(self.data[reg::RGBC], 3);
        Self::set_byte(&mut self.data[reg::RGB2], 3, i32::from(code));
        for i in 0..3 {
            let value = self.lim_c(i, self.mac(i + 1) >> 4);
            Self::set_byte(&mut self.data[reg::RGB2], i as u32, value);
        }
    }
    fn set_irs_from_macs(&mut self, lm: bool) {
        for i in 1..=3 {
            let value = self.lim_b(i, lm, self.mac(i));
            self.set_ir(i, value);
        }
    }

    // ---- pipeline stages ----------------------------------------------------
    fn matrix_vector(&mut self, sf: bool, lm: bool, mx: usize, v: usize, cv: usize) {
        let vector = self.vertex(v).map(i64::from);
        let m = |gte: &Self, row, col| i64::from(gte.matrix_element(mx, row, col));
        if cv == 2 {
            // FC bug: columns 1-2 form the result; column 0 only sets flags.
            for row in 0..3 {
                let sum = Int44::new(m(self, row, 1) * vector[1]).add(m(self, row, 2) * vector[2]);
                let value = self.a(row + 1, sf, sum);
                self.set_mac(row + 1, value);
            }
            for row in 0..3 {
                let sum =
                    Int44::new(self.control_vector(cv, row) << 12).add(m(self, row, 0) * vector[0]);
                let value = self.a(row + 1, sf, sum);
                self.lim_b(row + 1, false, value);
            }
        } else {
            for row in 0..3 {
                let sum = Int44::new(self.control_vector(cv, row) << 12)
                    .add(m(self, row, 0) * vector[0])
                    .add(m(self, row, 1) * vector[1])
                    .add(m(self, row, 2) * vector[2]);
                let value = self.a(row + 1, sf, sum);
                self.set_mac(row + 1, value);
            }
        }
        self.set_irs_from_macs(lm);
    }
    fn depth_cue(&mut self, sf: bool, lm: bool, input: [i64; 3]) {
        let far = [ctrl::RFC, ctrl::GFC, ctrl::BFC];
        for i in 0..3 {
            let difference = self.a(i + 1, sf, Int44::new((self.cs(far[i]) << 12) - input[i]));
            let limited = self.lim_b(i + 1, false, difference);
            let product = i64::from(i32::from(self.ir(0)).wrapping_mul(limited));
            let value = self.a(i + 1, sf, Int44::new(input[i] + product));
            self.set_mac(i + 1, value);
        }
        self.set_irs_from_macs(lm);
    }
    fn depth_cue_color(&mut self, sf: bool, lm: bool) {
        let rgb = self.rgb();
        let input =
            std::array::from_fn(|i| i64::from(i32::from(rgb[i]) << 4) * i64::from(self.ir(i + 1)));
        self.depth_cue(sf, lm, input);
    }
    fn color_apply(&mut self, sf: bool, lm: bool) {
        let rgb = self.rgb();
        for (i, &channel) in rgb.iter().take(3).enumerate() {
            let product = i64::from(i32::from(channel) << 4) * i64::from(self.ir(i + 1));
            let value = self.a(i + 1, sf, Int44::new(product));
            self.set_mac(i + 1, value);
        }
        self.set_irs_from_macs(lm);
    }
    fn rtps_vertex(&mut self, sf: bool, lm: bool, v: usize, last: bool) {
        let vector = self.vertex(v).map(i64::from);
        let mut raw3 = 0;
        for row in 0..3 {
            let sum = Int44::new(self.cs(ctrl::TRX + row) << 12)
                .add(i64::from(self.matrix_element(0, row, 0)) * vector[0])
                .add(i64::from(self.matrix_element(0, row, 1)) * vector[1])
                .add(i64::from(self.matrix_element(0, row, 2)) * vector[2]);
            if row == 2 {
                raw3 = sum.value;
            }
            let value = self.a(row + 1, sf, sum);
            self.set_mac(row + 1, value);
        }
        let ir1 = self.lim_b(1, lm, self.mac(1));
        self.set_ir(1, ir1);
        let ir2 = self.lim_b(2, lm, self.mac(2));
        self.set_ir(2, ir2);
        let ir3 = self.lim_b3_sf(sf, lm, raw3);
        self.set_ir(3, ir3);
        let z = self.lim_d(raw3);
        self.push_z(z);
        let h = Self::lo(self.control[ctrl::H]) as u16;
        let ratio = i64::from(self.divide(h, self.sz(3)) as i32);
        self.data[reg::SXY0] = self.data[reg::SXY1];
        self.data[reg::SXY1] = self.data[reg::SXY2];
        let offset = i64::from(self.ir(1)) * ratio;
        let offset = match self.x_scale.0 {
            None => offset,
            Some((num, den)) => (offset * i64::from(num)).div_euclid(i64::from(den)),
        };
        let sx = self.cs(ctrl::OFX) + offset;
        let x = self.f(sx) >> 16;
        let x = self.lim_g(x, flag::SX_SAT);
        Self::set_lo(&mut self.data[reg::SXY2], x);
        let sy = self.cs(ctrl::OFY) + i64::from(self.ir(2)) * ratio;
        let y = self.f(sy) >> 16;
        let y = self.lim_g(y, flag::SY_SAT);
        Self::set_hi(&mut self.data[reg::SXY2], y);
        if let Some(window) = self.subpixel.0.as_deref_mut() {
            let overflow = u32::from(h) >= u32::from(Self::lo(self.data[reg::SZ3]) as u16) * 2;
            window.project(self.data[reg::SXY2], sx, sy, raw3, overflow);
        }
        if last {
            let dqa = i64::from(Self::lo(self.control[ctrl::DQA]));
            let raw0 = self.f(self.cs(ctrl::DQB) + dqa * ratio);
            self.set_mac(0, raw0 as i32);
            let ir0 = self.lim_h(raw0);
            self.set_ir(0, ir0);
        }
    }

    // ---- transfers --------------------------------------------------------
    pub fn mfc2(&mut self, index: usize) -> u32 {
        let word = self.data[index];
        let component = |value: i16| (i32::from(value) >> 7).clamp(0, 0x1f) as u32;
        self.data[index] = match index {
            1 | 3 | 5 | 8..=11 => Self::lo(word) as i32 as u32,
            7 | 16..=19 => word & 0xffff,
            15 => self.data[reg::SXY2],
            28 | 29 => {
                component(self.ir(1)) | component(self.ir(2)) << 5 | component(self.ir(3)) << 10
            }
            _ => word,
        };
        self.data[index]
    }
    pub fn cfc2(&self, index: usize) -> u32 {
        self.control[index]
    }
    pub fn mtc2(&mut self, index: usize, value: u32) {
        match index {
            15 => {
                self.data[reg::SXY0] = self.data[reg::SXY1];
                self.data[reg::SXY1] = self.data[reg::SXY2];
                self.data[reg::SXY2] = value;
            }
            28 => {
                self.set_ir(1, ((value & 0x1f) << 7) as i32);
                self.set_ir(2, ((value & 0x3e0) << 2) as i32);
                self.set_ir(3, ((value & 0x7c00) >> 3) as i32);
            }
            30 => {
                let bits = if value & 0x8000_0000 != 0 {
                    !value
                } else {
                    value
                };
                self.data[reg::LZCR] = bits.leading_zeros();
            }
            31 => return,
            _ => {}
        }
        self.data[index] = value;
    }
    pub fn ctc2(&mut self, index: usize, value: u32) {
        let value = match index {
            4 | 12 | 20 | 26 | 27 | 29 | 30 => value as u16 as i16 as i32 as u32,
            31 => {
                let v = value & 0x7fff_f000;
                if v & flag::ERROR_BITS != 0 {
                    v | flag::ERROR
                } else {
                    v
                }
            }
            _ => value,
        };
        self.control[index] = value;
    }

    // ---- commands -----------------------------------------------------------
    /// Execute a COP2 command word (`imm25`, as in the instruction).
    pub fn command(&mut self, code: u32) -> Result<(), UnknownCommand> {
        let code = code & 0x1ff_ffff;
        let sf = code >> 19 & 1 != 0;
        let lm = code >> 10 & 1 != 0;
        match code & 0x3f {
            op::RTPS => self.rtps(sf, lm),
            op::NCLIP => self.nclip(),
            op::OP => self.op(sf, lm),
            op::DPCS => self.dpcs(sf, lm),
            op::INTPL => self.intpl(sf, lm),
            op::MVMVA => self.mvmva(
                sf,
                lm,
                (code >> 17 & 3) as usize,
                (code >> 15 & 3) as usize,
                (code >> 13 & 3) as usize,
            ),
            op::NCDS => self.ncds(sf, lm),
            op::CDP => self.cdp(sf, lm),
            op::NCDT => self.ncdt(sf, lm),
            op::NCCS => self.nccs(sf, lm),
            op::CC => self.cc(sf, lm),
            op::NCS => self.ncs(sf, lm),
            op::NCT => self.nct(sf, lm),
            op::SQR => self.sqr(sf, lm),
            op::DCPL => self.dcpl(sf, lm),
            op::DPCT => self.dpct(sf, lm),
            op::AVSZ3 => self.avsz3(),
            op::AVSZ4 => self.avsz4(),
            op::RTPT => self.rtpt(sf, lm),
            op::GPF => self.gpf(sf, lm),
            op::GPL => self.gpl(sf, lm),
            op::NCCT => self.ncct(sf, lm),
            _ => return Err(UnknownCommand(code)),
        }
        Ok(())
    }
    /// Land a transfer write (`mtc2`/`lwc2`, or `ctc2` for a control register).
    pub fn apply_write(&mut self, write: &pipeline::PendingWrite) {
        if write.control {
            self.ctc2(write.index, write.value);
        } else {
            self.mtc2(write.index, write.value);
        }
    }
    /// Execute a command while `writes` (in issue order) are still in flight,
    /// exactly as Redux's `gteRunCommand`: a write far enough ahead for this
    /// command and register lands first; the others are read as their old
    /// values and then land as described in [`pipeline`]. With no writes this
    /// is [`Gte::command`].
    pub fn command_after_writes(
        &mut self,
        code: u32,
        writes: &[pipeline::PendingWrite],
    ) -> Result<(), UnknownCommand> {
        let code = code & 0x1ff_ffff;
        if pipeline::command_outputs(code) == 0 {
            return Err(UnknownCommand(code));
        }
        let mut late = Vec::with_capacity(writes.len());
        for write in writes {
            if write.distance >= pipeline::write_need(code, write.register()) {
                self.apply_write(write);
            } else {
                late.push(*write);
            }
        }
        if late.is_empty() {
            return self.command(code);
        }
        let before = self.clone();
        let (mut targets, mut over_results) = (0_u64, 0_u64);
        for write in &late {
            self.apply_write(write);
            let set = pipeline::write_targets(write);
            targets |= set;
            if write.distance == 0 {
                over_results |= set;
            }
        }
        let written = std::mem::replace(self, before);
        self.command(code)?;
        targets &= !pipeline::command_outputs(code) | over_results;
        for r in 0..32 {
            if targets >> r & 1 != 0 {
                self.data[r] = written.data[r];
            }
            if targets >> (32 + r) & 1 != 0 {
                self.control[r] = written.control[r];
            }
        }
        Ok(())
    }
    fn clear_flag(&mut self) {
        self.control[ctrl::FLAG] = 0;
    }
    pub fn rtps(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        self.rtps_vertex(sf, lm, 0, true);
    }
    pub fn rtpt(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        for v in 0..3 {
            self.rtps_vertex(sf, lm, v, v == 2);
        }
    }
    pub fn nclip(&mut self) {
        self.clear_flag();
        let s = |gte: &Self, i: usize| {
            let word = gte.data[reg::SXY0 + i];
            (i64::from(Self::lo(word)), i64::from(Self::hi(word)))
        };
        let ((x0, y0), (x1, y1), (x2, y2)) = (s(self, 0), s(self, 1), s(self, 2));
        let value = self.f(x0 * y1 + x1 * y2 + x2 * y0 - x0 * y2 - x1 * y0 - x2 * y1);
        self.set_mac(0, value as i32);
    }
    pub fn op(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        let d =
            [ctrl::R11R12, ctrl::R22R23, ctrl::R33].map(|r| i64::from(Self::lo(self.control[r])));
        let ir = [1, 2, 3].map(|i| i64::from(self.ir(i)));
        let values = [
            d[1] * ir[2] - d[2] * ir[1],
            d[2] * ir[0] - d[0] * ir[2],
            d[0] * ir[1] - d[1] * ir[0],
        ];
        for (i, value) in values.into_iter().enumerate() {
            let mac = self.a(i + 1, sf, Int44::new(value));
            self.set_mac(i + 1, mac);
        }
        self.set_irs_from_macs(lm);
    }
    pub fn dpcs(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        let rgb = self.rgb();
        self.depth_cue(sf, lm, std::array::from_fn(|i| i64::from(rgb[i]) << 16));
        self.push_color();
    }
    pub fn intpl(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        let input = std::array::from_fn(|i| i64::from(self.ir(i + 1)) << 12);
        self.depth_cue(sf, lm, input);
        self.push_color();
    }
    pub fn mvmva(&mut self, sf: bool, lm: bool, mx: usize, v: usize, cv: usize) {
        self.clear_flag();
        self.matrix_vector(sf, lm, mx, v, cv);
    }
    fn ncds_core(&mut self, sf: bool, lm: bool, v: usize) {
        self.matrix_vector(sf, lm, 1, v, 3);
        self.matrix_vector(sf, lm, 2, 3, 1);
        self.depth_cue_color(sf, lm);
        self.push_color();
    }
    pub fn ncds(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        self.ncds_core(sf, lm, 0);
    }
    pub fn ncdt(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        for v in 0..3 {
            self.ncds_core(sf, lm, v);
        }
    }
    pub fn cdp(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        self.matrix_vector(sf, lm, 2, 3, 1);
        self.depth_cue_color(sf, lm);
        self.push_color();
    }
    fn nccs_core(&mut self, sf: bool, lm: bool, v: usize) {
        self.matrix_vector(sf, lm, 1, v, 3);
        self.matrix_vector(sf, lm, 2, 3, 1);
        self.color_apply(sf, lm);
        self.push_color();
    }
    pub fn nccs(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        self.nccs_core(sf, lm, 0);
    }
    pub fn ncct(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        for v in 0..3 {
            self.nccs_core(sf, lm, v);
        }
    }
    pub fn cc(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        self.matrix_vector(sf, lm, 2, 3, 1);
        self.color_apply(sf, lm);
        self.push_color();
    }
    fn ncs_core(&mut self, sf: bool, lm: bool, v: usize) {
        self.matrix_vector(sf, lm, 1, v, 3);
        self.matrix_vector(sf, lm, 2, 3, 1);
        self.push_color();
    }
    pub fn ncs(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        self.ncs_core(sf, lm, 0);
    }
    pub fn nct(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        for v in 0..3 {
            self.ncs_core(sf, lm, v);
        }
    }
    pub fn sqr(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        for i in 1..=3 {
            let value = i64::from(self.ir(i)) * i64::from(self.ir(i));
            let mac = self.a(i, sf, Int44::new(value));
            self.set_mac(i, mac);
        }
        self.set_irs_from_macs(lm);
    }
    pub fn dcpl(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        self.depth_cue_color(sf, lm);
        self.push_color();
    }
    pub fn dpct(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        for _ in 0..3 {
            let rgb0 = self.data[reg::RGB0].to_le_bytes();
            self.depth_cue(sf, lm, std::array::from_fn(|i| i64::from(rgb0[i]) << 16));
            self.push_color();
        }
    }
    pub fn avsz3(&mut self) {
        self.clear_flag();
        let zsf = i64::from(Self::lo(self.control[ctrl::ZSF3]));
        let sum = (1..=3).map(|i| zsf * i64::from(self.sz(i))).sum::<i64>();
        let raw = self.f(sum);
        self.set_mac(0, raw as i32);
        let otz = self.lim_d(raw);
        Self::set_lo(&mut self.data[reg::OTZ], otz);
    }
    pub fn avsz4(&mut self) {
        self.clear_flag();
        let zsf = i64::from(Self::lo(self.control[ctrl::ZSF4]));
        let sum = (0..=3).map(|i| zsf * i64::from(self.sz(i))).sum::<i64>();
        let raw = self.f(sum);
        self.set_mac(0, raw as i32);
        let otz = self.lim_d(raw);
        Self::set_lo(&mut self.data[reg::OTZ], otz);
    }
    pub fn gpf(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        for i in 1..=3 {
            let value = i64::from(i32::from(self.ir(0)) * i32::from(self.ir(i)));
            let mac = self.a(i, sf, Int44::new(value));
            self.set_mac(i, mac);
        }
        self.set_irs_from_macs(lm);
        self.push_color();
    }
    pub fn gpl(&mut self, sf: bool, lm: bool) {
        self.clear_flag();
        for i in 1..=3 {
            let base = if sf {
                i64::from(self.mac(i)) << 12
            } else {
                i64::from(self.mac(i))
            };
            let value = base + i64::from(i32::from(self.ir(0)) * i32::from(self.ir(i)));
            let mac = self.a(i, sf, Int44::new(value));
            self.set_mac(i, mac);
        }
        self.set_irs_from_macs(lm);
        self.push_color();
    }
}

#[cfg(test)]
mod tests;
