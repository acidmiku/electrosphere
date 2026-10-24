//! Address-preserving machine state for the translated game code.
//!
//! Main RAM keeps the original layout, so structures, pointers and function
//! addresses inside them retain their original values. Every routine that
//! operates on it is hand-translated native Rust; nothing here decodes or
//! executes MIPS instructions. Original globals are addressed absolutely.
//!
//! The machine knows no game version: it holds no RAM address of any
//! executable. Each version crate (`ac3-flight` for PAL) adds its own
//! small-data base `$gp` and the `gw`/`set_gw` accessors on top of it.
//!
//! Accessors are infallible so translations read like the original code. An
//! address outside RAM/scratchpad or a misaligned access records the first
//! [`Fault`] and reads as zero/ignores the write; callers check
//! [`Machine::take_fault`] at subsystem boundaries and fail explicitly.
//!
//! Optionally ([`Machine::set_precision`]) the word loads and stores also
//! follow the GTE's screen words into a [`precision`]
//! table for a high-resolution renderer; that never changes RAM or the GTE.
use crate::precision::{self, SharedPrecision};
use crate::widescreen::{Aspect, Widescreen};
use ac3_gte::Gte;
use std::{collections::VecDeque, fmt};

pub const RAM_BYTES: usize = 0x20_0000;
pub const SCRATCH_BYTES: usize = 0x400;
pub const SCRATCH_BASE: u32 = 0x1f80_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Fault {
    Address {
        address: u32,
        width: u8,
    },
    Misaligned {
        address: u32,
        width: u8,
    },
    /// Original allocator overflow (80026F70 prints and returns null).
    ArenaExhausted {
        requested: u32,
        end: u32,
    },
    /// A guest code address with no native translation was dispatched.
    Untranslated {
        address: u32,
    },
    /// The original would execute `break code` at `address` (compiler
    /// divide-by-zero/overflow traps and similar).
    Trap {
        address: u32,
        code: u32,
    },
}
impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Address { address, width } => {
                write!(
                    f,
                    "access outside RAM/scratchpad: {address:#010x} width {width}"
                )
            }
            Self::Misaligned { address, width } => {
                write!(f, "misaligned access: {address:#010x} width {width}")
            }
            Self::ArenaExhausted { requested, end } => {
                write!(f, "arena exhausted: {requested:#x} bytes, end {end:#010x}")
            }
            Self::Untranslated { address } => {
                write!(f, "untranslated original routine {address:#010x}")
            }
            Self::Trap { address, code } => {
                write!(f, "original break {code} at {address:#010x}")
            }
        }
    }
}
impl std::error::Error for Fault {}

/// A hardware-facing SDK routine reached by translated code (GPU, CD, SPU,
/// pads, timers). Translations do not emulate the device: they append the
/// request here, in original call order, and return the value the original
/// routine returns on this path. The host bridge executes the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostCall {
    /// Which routine: its **canonical id**, the routine's entry address in
    /// the PAL executable (for example
    /// [`DMA_LIST`](crate::host_ids::DMA_LIST) = `0x800A8360`), named in
    /// [`host_ids`](crate::host_ids). It is an identifier, not a RAM
    /// address: every version crate logs the PAL id, wherever its own copy
    /// of the routine sits, and the devices match on [`crate::host_ids`].
    /// Address re-mapping (the JP re-addressing tool) never changes it.
    pub function: u32,
    pub args: [u32; 4],
}

/// Host device executing device-facing calls synchronously, at the point in
/// the original call order where the SDK hands work to hardware (so GPU DMA
/// reads packets before the CPU rewrites them). It may read and write RAM
/// and scratchpad (e.g. StoreImage) and may supply the routine's result.
/// Devices recognise a call by its canonical id ([`HostCall::function`],
/// [`crate::host_ids`]).
pub trait Device {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32>;
}

pub struct Machine {
    ram: Box<[u8]>,
    scratch: Box<[u8]>,
    pub gte: Gte,
    pub host_calls: Vec<HostCall>,
    /// Values device-facing routines return, in call order (VSync counts,
    /// root counters, SPU key status words, ...). The host bridge supplies
    /// them; snapshot tests supply the original's values. An empty queue
    /// yields each routine's documented default.
    pub host_results: VecDeque<u32>,
    /// Guest stack pointer for locals whose address the original passes to a
    /// callee (`addiu a0, sp, 0x10`). Use `push_frame`/`pop_frame` with the
    /// original frame size; locals never passed by address stay in Rust.
    pub sp: u32,
    /// Synchronous device; when absent, calls are only logged.
    pub device: Option<Box<dyn Device>>,
    fault: Option<Fault>,
    /// Sub-pixel vertex tracking ([`Machine::set_precision`]); host-side,
    /// never game state.
    precision: Option<SharedPrecision>,
    /// Anamorphic widescreen in flight ([`Machine::set_widescreen`]);
    /// host-side, never game state.
    pub(crate) widescreen: Widescreen,
}

impl Clone for Machine {
    /// Copies machine state and the widescreen setting; the attached device
    /// and the precision table (with the GTE's sub-pixel window) are not
    /// cloned.
    fn clone(&self) -> Self {
        let mut gte = self.gte.clone();
        gte.disable_subpixel();
        Self {
            ram: self.ram.clone(),
            scratch: self.scratch.clone(),
            gte,
            host_calls: self.host_calls.clone(),
            host_results: self.host_results.clone(),
            sp: self.sp,
            device: None,
            fault: self.fault,
            precision: None,
            widescreen: self.widescreen.clone(),
        }
    }
}

/// Everything of a [`Machine`] that a save state holds: RAM, scratchpad,
/// GTE, the guest stack pointer, queued host results and the fault. Not
/// part of it: the device hook (the host keeps its own link), the
/// host-call log of the last pass (cleared at the next pass), the
/// precision table with the GTE's sub-pixel window, and the widescreen
/// setting with the GTE's projection X scale.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MachineState {
    pub ram: Vec<u8>,
    pub scratch: Vec<u8>,
    pub gte: Gte,
    pub host_results: VecDeque<u32>,
    pub sp: u32,
    pub fault: Option<Fault>,
}

impl Machine {
    /// The machine's state (see [`MachineState`]).
    pub fn snapshot(&self) -> MachineState {
        // Every field named: a new one must be placed here or excluded.
        let Self {
            ram,
            scratch,
            gte,
            host_calls: _,
            host_results,
            sp,
            device: _,
            fault,
            precision: _,
            widescreen: _,
        } = self;
        let mut gte = gte.clone();
        gte.disable_subpixel();
        MachineState {
            ram: ram.to_vec(),
            scratch: scratch.to_vec(),
            gte,
            host_results: host_results.clone(),
            sp: *sp,
            fault: *fault,
        }
    }
    /// Replace the machine's state with `state`, keeping the device hook,
    /// the widescreen setting (and the GTE's projection X scale) and the
    /// precision tracking (its table is cleared, its window emptied); the
    /// host-call log is cleared. Fails (changing nothing) when the RAM or
    /// scratchpad image has the wrong size.
    pub fn restore(&mut self, state: MachineState) -> Result<(), String> {
        if state.ram.len() != RAM_BYTES || state.scratch.len() != SCRATCH_BYTES {
            return Err(format!(
                "machine state has {} RAM and {} scratchpad bytes, expected {RAM_BYTES} and {SCRATCH_BYTES}",
                state.ram.len(),
                state.scratch.len()
            ));
        }
        let MachineState {
            ram,
            scratch,
            gte,
            host_results,
            sp,
            fault,
        } = state;
        self.ram = ram.into_boxed_slice();
        self.scratch = scratch.into_boxed_slice();
        let x_scale = self.gte.projection_x_scale();
        self.gte = gte;
        self.gte.set_projection_x_scale(x_scale);
        self.host_calls.clear();
        self.host_results = host_results;
        self.sp = sp;
        self.fault = fault;
        let precision = self.precision.take();
        self.set_precision(precision);
        Ok(())
    }

    /// Follow the GTE's screen words into `table` (see
    /// [`precision`]), or stop with `None`. The GTE's
    /// sub-pixel window is enabled with the table's capacity (or disabled),
    /// and the table is cleared. RAM, the GTE registers and every result are
    /// the same either way.
    pub fn set_precision(&mut self, table: Option<SharedPrecision>) {
        match &table {
            Some(table) => {
                let mut table = table.borrow_mut();
                table.clear();
                self.gte.enable_subpixel(table.options().window);
            }
            None => self.gte.disable_subpixel(),
        }
        self.precision = table;
    }
    #[inline]
    pub fn precision(&self) -> Option<&SharedPrecision> {
        self.precision.as_ref()
    }
    /// Show flight at `aspect` (anamorphic widescreen, see [`crate::widescreen`]),
    /// or the original 4:3 with `None` (the default). Host configuration:
    /// with `None` nothing changes the game; it is not saved in states.
    pub fn set_widescreen(&mut self, aspect: Option<Aspect>) {
        self.widescreen.set_aspect(aspect);
        if aspect.is_none() {
            self.gte.set_projection_x_scale(None);
        }
    }
    /// The widescreen setting and the state of the current pass.
    #[inline]
    pub fn widescreen(&self) -> &Widescreen {
        &self.widescreen
    }
    /// The RAM word at `address` (any mirror, word-aligned), read without
    /// precision tracking or faults: for host-side readers.
    #[inline]
    pub fn peek(&self, address: u32) -> u32 {
        let at = (address & 0x1f_fffc) as usize;
        u32::from_le_bytes(self.ram[at..at + 4].try_into().unwrap())
    }
    /// The RAM halfword at `address` (any mirror), like [`Self::peek`].
    #[inline]
    pub fn peek_half(&self, address: u32) -> u16 {
        let at = (address & 0x1f_fffe) as usize;
        u16::from_le_bytes(self.ram[at..at + 2].try_into().unwrap())
    }
    /// Write the RAM word at `address` without precision tracking or
    /// faults: for host-side display transforms that put the word back.
    pub(crate) fn poke(&mut self, address: u32, value: u32) {
        let at = (address & 0x1f_fffc) as usize;
        self.ram[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    /// A word store of `value` (`width` 4) or a narrower store at `address`
    /// (already written): record the position when `value` is a recent
    /// screen word, else clear the slot.
    #[cold]
    fn track_store(&mut self, address: u32, width: usize, value: u32) {
        let (Some(table), Some(slot)) = (&self.precision, precision::word_slot(address)) else {
            return;
        };
        let position = match self.gte.subpixel() {
            Some(window) if width == 4 => window.find(value),
            _ => None,
        };
        table.borrow_mut().store(slot, value, position);
    }
    /// A word load of `value` from `address`: a recorded word goes back into
    /// the GTE's window as its newest entry.
    #[cold]
    fn track_load(&mut self, address: u32, value: u32) {
        let (Some(table), Some(slot)) = (&self.precision, precision::word_slot(address)) else {
            return;
        };
        let Some(position) = table.borrow_mut().load(slot, value) else {
            return;
        };
        if let Some(window) = self.gte.subpixel_mut()
            && window.newest() != Some((value, Some(position)))
        {
            window.push(value, Some(position));
        }
    }
}

/// Initial `$sp` of the resident main loop (snapshot tests use the captured one).
pub const INITIAL_SP: u32 = 0x801f_fff0;
/// Lowest guest stack address. Flight reaches 0x801FCEE8 (8006AE1C's
/// 0x3098-byte frame); overlay 4's 800CD1B4 (0x8180 bytes) and its callees
/// reach about 0x801F7A50. Snapshot comparisons still exclude only
/// `snapshot::STACK_START` up.
pub const STACK_LIMIT: u32 = 0x801f_7000;

impl Default for Machine {
    fn default() -> Self {
        Self {
            ram: vec![0; RAM_BYTES].into_boxed_slice(),
            scratch: vec![0; SCRATCH_BYTES].into_boxed_slice(),
            gte: Gte::default(),
            host_calls: Vec::new(),
            host_results: VecDeque::new(),
            sp: INITIAL_SP,
            device: None,
            fault: None,
            precision: None,
            widescreen: Widescreen::default(),
        }
    }
}

enum Region {
    Ram(usize),
    Scratch(usize),
}

// The accessors below are `#[inline]`: they are the translated code's
// hottest calls, and the code calling them lives in other crates.
impl Machine {
    /// Construct from complete RAM and scratchpad images (tests, host bridge).
    pub fn from_images(ram: &[u8], scratch: &[u8]) -> Option<Self> {
        (ram.len() == RAM_BYTES && scratch.len() == SCRATCH_BYTES).then(|| Self {
            ram: ram.into(),
            scratch: scratch.into(),
            gte: Gte::default(),
            host_calls: Vec::new(),
            host_results: VecDeque::new(),
            sp: INITIAL_SP,
            device: None,
            fault: None,
            precision: None,
            widescreen: Widescreen::default(),
        })
    }
    #[inline]
    pub fn ram(&self) -> &[u8] {
        &self.ram
    }
    #[inline]
    pub fn scratch(&self) -> &[u8] {
        &self.scratch
    }
    #[inline]
    pub fn fault(&self) -> Option<Fault> {
        self.fault
    }
    #[inline]
    pub fn take_fault(&mut self) -> Result<(), Fault> {
        self.fault.take().map_or(Ok(()), Err)
    }
    #[inline]
    pub fn raise(&mut self, fault: Fault) {
        self.fault.get_or_insert(fault);
    }
    #[inline]
    pub fn host_call(&mut self, function: u32, args: [u32; 4]) {
        self.dispatch_host_call(function, args);
    }
    #[inline]
    fn dispatch_host_call(&mut self, function: u32, args: [u32; 4]) -> Option<u32> {
        let call = HostCall { function, args };
        self.host_calls.push(call);
        let mut device = self.device.take()?;
        let result = device.call(&mut self.ram, &mut self.scratch, call);
        self.device = Some(device);
        result
    }
    /// Allocate a stack frame of `size` bytes (original `addiu sp, sp, -size`)
    /// and return its base; overflowing the stack region records a fault.
    #[inline]
    pub fn push_frame(&mut self, size: u32) -> u32 {
        self.sp = self.sp.wrapping_sub(size);
        if self.sp < STACK_LIMIT {
            self.raise(Fault::Address {
                address: self.sp,
                width: 4,
            });
        }
        self.sp
    }
    #[inline]
    pub fn pop_frame(&mut self, size: u32) {
        self.sp = self.sp.wrapping_add(size);
    }
    /// Record a device-facing call and return the next host-supplied result,
    /// or `default` when none was supplied.
    #[inline]
    pub fn host_call_result(&mut self, function: u32, args: [u32; 4], default: u32) -> u32 {
        match self.dispatch_host_call(function, args) {
            Some(result) => result,
            None => self.host_results.pop_front().unwrap_or(default),
        }
    }

    #[inline]
    fn region(&mut self, address: u32, width: u8) -> Option<Region> {
        if address & (u32::from(width) - 1) != 0 {
            self.raise(Fault::Misaligned { address, width });
            return None;
        }
        let physical = address & 0x1fff_ffff;
        if matches!(address >> 29, 0 | 4 | 5) && physical < 0x80_0000 {
            // KUSEG/KSEG0/KSEG1 and the four 2 MiB mirrors.
            return Some(Region::Ram((physical as usize) & (RAM_BYTES - 1)));
        }
        if address >> 29 == 0 || address >> 29 == 4 {
            let offset = physical.wrapping_sub(SCRATCH_BASE);
            if offset < SCRATCH_BYTES as u32 {
                return Some(Region::Scratch(offset as usize));
            }
        }
        self.raise(Fault::Address { address, width });
        None
    }
    #[inline]
    fn bytes<const N: usize>(&mut self, address: u32) -> [u8; N] {
        match self.region(address, N as u8) {
            Some(Region::Ram(at)) => self.ram[at..at + N].try_into().unwrap(),
            Some(Region::Scratch(at)) => self.scratch[at..at + N].try_into().unwrap(),
            None => [0; N],
        }
    }
    #[inline]
    fn store<const N: usize>(&mut self, address: u32, value: [u8; N]) {
        match self.region(address, N as u8) {
            Some(Region::Ram(at)) => self.ram[at..at + N].copy_from_slice(&value),
            Some(Region::Scratch(at)) => self.scratch[at..at + N].copy_from_slice(&value),
            None => return,
        }
        if self.precision.is_some() {
            let mut word = [0; 4];
            word[..N].copy_from_slice(&value);
            self.track_store(address, N, u32::from_le_bytes(word));
        }
    }

    #[inline]
    pub fn lw(&mut self, address: u32) -> u32 {
        let value = u32::from_le_bytes(self.bytes(address));
        if self.precision.is_some() {
            self.track_load(address, value);
        }
        value
    }
    #[inline]
    pub fn lh(&mut self, address: u32) -> i16 {
        i16::from_le_bytes(self.bytes(address))
    }
    #[inline]
    pub fn lhu(&mut self, address: u32) -> u16 {
        u16::from_le_bytes(self.bytes(address))
    }
    #[inline]
    pub fn lb(&mut self, address: u32) -> i8 {
        i8::from_le_bytes(self.bytes(address))
    }
    #[inline]
    pub fn lbu(&mut self, address: u32) -> u8 {
        self.bytes::<1>(address)[0]
    }
    #[inline]
    pub fn sw(&mut self, address: u32, value: u32) {
        self.store(address, value.to_le_bytes());
    }
    #[inline]
    pub fn sh(&mut self, address: u32, value: u16) {
        self.store(address, value.to_le_bytes());
    }
    #[inline]
    pub fn sb(&mut self, address: u32, value: u8) {
        self.store(address, [value]);
    }
    /// Unchecked bulk copy into RAM for host-side image construction.
    #[inline]
    pub fn write_bytes(&mut self, address: u32, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            self.sb(address.wrapping_add(i as u32), b);
        }
    }
    #[inline]
    pub fn read_bytes(&mut self, address: u32, length: usize) -> Vec<u8> {
        (0..length)
            .map(|i| self.lbu(address.wrapping_add(i as u32)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mirrors_scratchpad_and_faults() {
        let mut m = Machine::default();
        m.sw(0x8001_0000, 0x1234_5678);
        assert_eq!(m.lw(0x0001_0000), 0x1234_5678);
        assert_eq!(m.lw(0xa001_0000), 0x1234_5678);
        assert_eq!(m.lw(0x8061_0000), 0x1234_5678, "2 MiB mirror");
        assert_eq!(m.lh(0x8001_0002), 0x1234);
        assert_eq!(m.lb(0x8001_0003), 0x12);
        m.sh(0x1f80_0010, 0xbeef);
        assert_eq!(m.lhu(0x1f80_0010), 0xbeef);
        assert_eq!(m.scratch()[0x10], 0xef);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.lw(0x8001_0002), 0);
        assert_eq!(
            m.take_fault(),
            Err(Fault::Misaligned {
                address: 0x8001_0002,
                width: 4
            })
        );
        m.sw(0x1f80_1810, 1);
        m.sw(0xbfc0_0000, 1);
        assert_eq!(
            m.take_fault(),
            Err(Fault::Address {
                address: 0x1f80_1810,
                width: 4
            }),
            "first fault is retained"
        );
        assert!(m.take_fault().is_ok());
    }
}
