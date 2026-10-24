//! Sub-pixel vertex positions for a renderer that draws above 1× (the
//! "wobble fix", Milestone 1 item 6).
//!
//! The GTE writes whole-pixel screen words (SXY), and the GPU draws from
//! them, so geometry snaps to the 320-pixel grid. With
//! [`Machine::set_precision`](crate::Machine::set_precision) the GTE also
//! keeps the unrounded position of every vertex it projects
//! ([`ac3_gte::subpixel`]), and the machine follows those words into RAM:
//!
//! - **store:** a word store (`sw`, so `swc2` too) whose value equals an
//!   entry of the GTE's recent-word window records `(address -> word,
//!   position)` here. Any other store to the word (`sw` of another value,
//!   `sh`, `sb`) clears it.
//! - **load:** a word load (`lw`, so `lwc2` too) of a recorded word, while
//!   RAM still holds that word, puts it back into the window as the newest
//!   entry. Translated code that copies screen words through CPU registers
//!   (the vertex buffer → packet copies of the mesh renderers) keeps them.
//!
//! The window is short (32 entries by default): a stored word must equal a
//! GTE output or a reloaded word from the last few dozen. Words that were
//! never projected (HUD and sprites typed in screen space) find nothing.
//! [`VertexPrecision::precise_vertex`] also checks that RAM still holds the
//! recorded word, so a stale entry never leaks.
//!
//! Not covered: screen positions the game computes from a projected word
//! (sprite corners around a projected centre, ribbon strips, sky-band
//! corners). They find nothing and stay on the integer grid.
//!
//! None of this is game state. RAM, the GTE registers and timing are the
//! same with it on or off; save states, `Machine::snapshot` and
//! `Machine::clone` leave it out, and `Machine::restore` clears the table.
//!
//! Reading it: the host keeps a clone of the [`SharedPrecision`] it gave the
//! machine. In `Device::call` (a DMA list, before the GPU draws it) it
//! borrows the table and looks vertex words up through
//! [`PrecisionTable::view`] over the `ram` slice it was given; between
//! main-loop passes the [`Machine`](crate::Machine) itself is a
//! [`VertexPrecision`].
use crate::machine::{RAM_BYTES, SCRATCH_BASE, SCRATCH_BYTES};
use std::{cell::RefCell, rc::Rc};

/// Sub-pixel position of the vertex word at `address` (the RAM address of a
/// GP0 vertex word inside a DMA-list packet): x, y in the same coordinate
/// space as the integer word (before the GPU's drawing offset), and z (the
/// GTE's SZ, for optional perspective-correct texturing later).
pub trait VertexPrecision {
    fn precise_vertex(&self, address: u32) -> Option<[f32; 3]>;
}

/// Main RAM words, then scratchpad words.
const RAM_WORDS: usize = RAM_BYTES / 4;
const WORDS: usize = RAM_WORDS + SCRATCH_BYTES / 4;

/// The table shared by the machine (writer) and the host (reader).
pub type SharedPrecision = Rc<RefCell<PrecisionTable>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Entries of the GTE's recent-word window.
    pub window: usize,
    /// Put reloaded recorded words back into the window (see the module
    /// documentation). Off only to measure what it adds.
    pub follow_loads: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            window: ac3_gte::subpixel::DEFAULT_CAPACITY,
            follow_loads: true,
        }
    }
}

/// Counters since the table was created (diagnostics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Word stores that recorded a position.
    pub recorded: u64,
    /// Recorded words put back into the window by a load.
    pub reloaded: u64,
    /// Stores that cleared a recorded word.
    pub cleared: u64,
}

#[derive(Clone, Copy)]
struct Slot {
    word: u32,
    position: [f32; 3],
}
const EMPTY: Slot = Slot {
    word: 0,
    position: [0.0; 3],
};

/// Recorded positions by word address: one slot per word of main RAM and
/// scratchpad (8 MiB), and a bit per slot that holds one (64 KiB, read by
/// every tracked load and store).
pub struct PrecisionTable {
    recorded: Box<[u64]>,
    slots: Box<[Slot]>,
    options: Options,
    stats: Stats,
}

impl Default for PrecisionTable {
    fn default() -> Self {
        Self::new(Options::default())
    }
}

impl PrecisionTable {
    pub fn new(options: Options) -> Self {
        Self {
            recorded: vec![0; WORDS.div_ceil(64)].into_boxed_slice(),
            slots: vec![EMPTY; WORDS].into_boxed_slice(),
            options,
            stats: Stats::default(),
        }
    }
    /// A new table with default options, ready for
    /// [`Machine::set_precision`](crate::Machine::set_precision).
    pub fn shared() -> SharedPrecision {
        Rc::new(RefCell::new(Self::default()))
    }
    pub fn options(&self) -> Options {
        self.options
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    /// Forget every recorded position (a loaded save state replaced RAM).
    pub fn clear(&mut self) {
        self.recorded.fill(0);
    }
    /// The table read against `ram` (the machine's 2 MiB main RAM, e.g. the
    /// slice a [`Device`](crate::Device) receives).
    pub fn view<'a>(&'a self, ram: &'a [u8]) -> PrecisionView<'a> {
        PrecisionView { table: self, ram }
    }
    /// Every recorded main-RAM position: (KSEG0 address, word, position).
    pub fn entries(&self) -> impl Iterator<Item = (u32, u32, [f32; 3])> + '_ {
        (0..RAM_WORDS).filter_map(|index| {
            let slot = self.get(index)?;
            Some((0x8000_0000 | (index as u32) << 2, slot.word, slot.position))
        })
    }

    /// The position recorded at `address` for exactly `word` (the GP0 vertex
    /// word a renderer is drawing from there), else `None`: a word that
    /// changed since, by any means, never gets a stale position.
    pub fn position(&self, address: u32, word: u32) -> Option<[f32; 3]> {
        let slot = self.get(ram_word(address)?)?;
        (slot.word == word).then_some(slot.position)
    }

    fn get(&self, index: usize) -> Option<Slot> {
        (self.recorded[index / 64] >> (index % 64) & 1 != 0).then(|| self.slots[index])
    }
    /// A word store of `word` at slot `index`: record `position` (`None`
    /// clears the slot).
    pub(crate) fn store(&mut self, index: usize, word: u32, position: Option<[f32; 3]>) {
        let (bits, bit) = (&mut self.recorded[index / 64], 1 << (index % 64));
        match position {
            Some(position) => {
                *bits |= bit;
                self.slots[index] = Slot { word, position };
                self.stats.recorded += 1;
            }
            None if *bits & bit != 0 => {
                *bits &= !bit;
                self.stats.cleared += 1;
            }
            None => {}
        }
    }
    /// A word load of `word` from slot `index`: its position when recorded
    /// for that word and loads are followed.
    pub(crate) fn load(&mut self, index: usize, word: u32) -> Option<[f32; 3]> {
        if !self.options.follow_loads {
            return None;
        }
        let slot = self.get(index).filter(|slot| slot.word == word)?;
        self.stats.reloaded += 1;
        Some(slot.position)
    }
    fn lookup(&self, ram: &[u8], address: u32) -> Option<[f32; 3]> {
        let index = ram_word(address)?;
        let slot = self.get(index)?;
        let at = index * 4;
        let word = u32::from_le_bytes(ram.get(at..at + 4)?.try_into().ok()?);
        (word == slot.word).then_some(slot.position)
    }
}

/// Slot of the aligned word at `address` in main RAM (KUSEG/KSEG0/KSEG1,
/// the 2 MiB mirrors, and 24-bit DMA addresses).
fn ram_word(address: u32) -> Option<usize> {
    let physical = address & 0x1fff_ffff;
    (address & 3 == 0 && matches!(address >> 29, 0 | 4 | 5) && physical < 0x80_0000)
        .then_some((physical as usize & (RAM_BYTES - 1)) >> 2)
}

/// Slot of the word holding `address` in main RAM or the scratchpad
/// (the machine's address map, without faults).
pub(crate) fn word_slot(address: u32) -> Option<usize> {
    let physical = address & 0x1fff_ffff;
    let segment = address >> 29;
    if matches!(segment, 0 | 4 | 5) && physical < 0x80_0000 {
        return Some((physical as usize & (RAM_BYTES - 1)) >> 2);
    }
    let offset = physical.wrapping_sub(SCRATCH_BASE);
    (matches!(segment, 0 | 4) && offset < SCRATCH_BYTES as u32)
        .then(|| RAM_WORDS + (offset as usize >> 2))
}

/// A [`PrecisionTable`] checked against main RAM.
#[derive(Clone, Copy)]
pub struct PrecisionView<'a> {
    table: &'a PrecisionTable,
    ram: &'a [u8],
}

impl VertexPrecision for PrecisionView<'_> {
    fn precise_vertex(&self, address: u32) -> Option<[f32; 3]> {
        self.table.lookup(self.ram, address)
    }
}

impl VertexPrecision for crate::Machine {
    /// The machine's own table against its RAM (`None` while tracking is off).
    fn precise_vertex(&self, address: u32) -> Option<[f32; 3]> {
        self.precision()?.borrow().lookup(self.ram(), address)
    }
}
