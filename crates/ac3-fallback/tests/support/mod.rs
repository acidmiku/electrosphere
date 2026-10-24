//! Test glue: the PAL machine as a fallback [`Guest`], as the
//! `dev-fallback` integration of `ac3-flight` provides it (a newtype here:
//! test crates may not implement a foreign trait for a foreign type), and
//! `dispatch::call` with the fallback behind it.
#![allow(dead_code)]
use ac3_fallback::{Fallback, Guest, RUNAWAY, UNSUPPORTED};
use ac3_flight::{Fault, Machine};
use ac3_gte::Gte;
use std::path::PathBuf;

pub struct Pal<'a>(pub &'a mut Machine);

impl Guest for Pal<'_> {
    fn fetch(&self, pc: u32) -> u32 {
        self.0.peek(pc)
    }
    fn lb(&mut self, address: u32) -> i8 {
        self.0.lb(address)
    }
    fn lbu(&mut self, address: u32) -> u8 {
        self.0.lbu(address)
    }
    fn lh(&mut self, address: u32) -> i16 {
        self.0.lh(address)
    }
    fn lhu(&mut self, address: u32) -> u16 {
        self.0.lhu(address)
    }
    fn lw(&mut self, address: u32) -> u32 {
        self.0.lw(address)
    }
    fn sb(&mut self, address: u32, value: u8) {
        self.0.sb(address, value);
    }
    fn sh(&mut self, address: u32, value: u16) {
        self.0.sh(address, value);
    }
    fn sw(&mut self, address: u32, value: u32) {
        self.0.sw(address, value);
    }
    fn gte(&mut self) -> &mut Gte {
        &mut self.0.gte
    }
    fn sp(&self) -> u32 {
        self.0.sp
    }
    fn set_sp(&mut self, sp: u32) {
        self.0.sp = sp;
    }
    fn loaded_module(&mut self) -> u32 {
        ac3_flight::common::loaded_module(self.0)
    }
    fn faulted(&self) -> bool {
        self.0.fault().is_some()
    }
    fn fault_text(&self) -> Option<String> {
        self.0.fault().map(|f| f.to_string())
    }
    fn raise_trap(&mut self, address: u32, code: u32) {
        self.0.raise(Fault::Trap { address, code });
    }
    fn raise_untranslated(&mut self, address: u32) {
        self.0.raise(Fault::Untranslated { address });
    }
    fn raise_unsupported(&mut self, address: u32, _word: u32) {
        self.0.raise(Fault::Trap {
            address,
            code: UNSUPPORTED,
        });
    }
    fn raise_runaway(&mut self, address: u32) {
        self.0.raise(Fault::Trap {
            address,
            code: RUNAWAY,
        });
    }
    fn host_stub(&mut self, target: u32, id: u32, args: [u32; 4]) -> Option<u32> {
        let calls = self.0.host_calls.len();
        let v0 = ac3_flight::bios::dispatch(self.0, target, args)
            .or_else(|| ac3_flight::sdk::dispatch(self.0, target, args))?;
        assert!(
            self.0.host_calls[calls..].iter().any(|c| c.function == id),
            "stub {target:08x} did not record host call {id:08x}"
        );
        Some(v0)
    }
    fn busy_wait(&mut self, site: u32, cycles: u32) -> bool {
        ac3_flight::states::busy_wait(self.0, site, cycles);
        true
    }
}

/// `dispatch::call` as the integration patch makes it: a translation, and
/// the fallback when no module claims `target` (`dispatch::call` itself
/// raises `Fault::Untranslated` for it and does nothing else, so the test
/// takes that fault back and asks the fallback, as `dispatch::call` would
/// before raising it). With `deep`, routines the fallback forces run
/// interpreted first.
pub fn call(fallback: &Fallback, deep: bool, g: &mut Pal<'_>, target: u32, args: [u32; 4]) -> u32 {
    let mut inner = |g: &mut Pal<'_>, t: u32, a: [u32; 4]| call(fallback, deep, g, t, a);
    if deep && let Some(v0) = fallback.dispatch_forced(g, target, args, &mut inner) {
        return v0;
    }
    let had = g.0.fault().is_some();
    let v0 = ac3_flight::dispatch::call(g.0, target, args);
    if !had && g.0.fault() == Some(Fault::Untranslated { address: target }) {
        let _ = g.0.take_fault();
        if let Some(v0) = fallback.dispatch(g, target, args, &mut inner) {
            return v0;
        }
        g.0.raise(Fault::Untranslated { address: target });
    }
    v0
}

pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
}

/// The guest stack: everything from `machine::STACK_LIMIT` up (overlay 4's
/// 0x8180-byte frame goes below `snapshot::STACK_START`). Interpreted
/// routines spill registers into their frames, translations keep them in
/// Rust; the stack is dead after a return (FLIGHT_MACHINE.md).
pub fn in_stack(i: usize) -> bool {
    (ac3_flight::machine::STACK_LIMIT as usize & 0x1f_ffff..0x20_0000).contains(&i)
}

/// The scratchpad stack 8008589C (flight model drawing) switches to, from
/// 0x1F8003F8 down: its frames hold spills of the caller's registers.
pub const SCRATCH_STACK: std::ops::Range<usize> = 0x3c0..0x400;

/// RAM addresses that differ outside the stack, the differing scratchpad
/// addresses (outside the 0x100 bytes below `sp` when the routine runs on a
/// scratchpad stack, like render's 8008589C callees), and whether `sp`
/// agrees.
pub fn differences(a: &Machine, b: &Machine, sp: u32) -> (Vec<u32>, Vec<u32>, bool) {
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !in_stack(i) && a.ram()[i] != b.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let frame = if (0x1f80_0000..0x1f80_0400).contains(&sp) {
        let top = (sp - 0x1f80_0000) as usize;
        top.saturating_sub(0x100)..top
    } else {
        0..0
    };
    let scratch = (0..0x400)
        .filter(|&i| {
            !frame.contains(&i) && !SCRATCH_STACK.contains(&i) && a.scratch()[i] != b.scratch()[i]
        })
        .map(|i| 0x1f80_0000 + i as u32)
        .collect();
    (ram, scratch, a.sp == b.sp)
}
