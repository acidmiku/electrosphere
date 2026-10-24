//! DEVELOPMENT ONLY (cargo feature `dev-fallback`, never in the shipped
//! `ac3` binary): the interpreter fallback of `ac3-fallback` behind
//! [`dispatch::call`](crate::dispatch::call). See that crate's docs.
//!
//! `dispatch::call` offers every call to [`forced`] first (routines that
//! `--interpret` forces to run interpreted, for validation), then to the
//! modules, then to [`miss`] (the routine nobody translated: interpret it,
//! unless it is below the device boundary). Calls out of interpreted code
//! come back through `dispatch::call`.
//!
//! The guest is [`MachineGuest`], a newtype over the machine: `Machine` is
//! the shared `ac3_machine` type and `Guest` is `ac3-fallback`'s trait, so
//! this crate implements the trait for its own wrapper.
use crate::machine::{Fault, Machine};
use ac3_fallback::Guest;

/// The machine as the fallback's [`Guest`]: this crate's view of it (its
/// `loaded_module`, its stub table, its busy-wait).
pub struct MachineGuest<'m>(pub &'m mut Machine);

impl Guest for MachineGuest<'_> {
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
    fn gte(&mut self) -> &mut ac3_gte::Gte {
        &mut self.0.gte
    }
    fn sp(&self) -> u32 {
        self.0.sp
    }
    fn set_sp(&mut self, sp: u32) {
        self.0.sp = sp;
    }
    fn loaded_module(&mut self) -> u32 {
        crate::common::loaded_module(self.0)
    }
    fn faulted(&self) -> bool {
        self.0.fault().is_some()
    }
    fn fault_text(&self) -> Option<String> {
        self.0.fault().map(|fault| fault.to_string())
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
            code: ac3_fallback::UNSUPPORTED,
        });
    }
    fn raise_runaway(&mut self, address: u32) {
        self.0.raise(Fault::Trap {
            address,
            code: ac3_fallback::RUNAWAY,
        });
    }
    /// This crate's stub translations at `target` (bios.rs; WaitEvent and
    /// TestEvent in sdk.rs). They record the canonical host-call ID `id`:
    /// here the PAL address itself; in `ac3-flight-jp`, whose bios.rs claims
    /// the JP addresses, the PAL counterpart (this file works unchanged
    /// there).
    fn host_stub(&mut self, target: u32, id: u32, args: [u32; 4]) -> Option<u32> {
        let m = &mut *self.0;
        let calls = m.host_calls.len();
        let v0 = crate::bios::dispatch(m, target, args)
            .or_else(|| crate::sdk::dispatch(m, target, args))?;
        debug_assert!(
            m.host_calls[calls..].iter().any(|c| c.function == id),
            "stub {target:08x} did not record host call {id:08x}"
        );
        Some(v0)
    }
    fn busy_wait(&mut self, site: u32, cycles: u32) -> bool {
        crate::states::busy_wait(self.0, site, cycles);
        true
    }
}

/// `dispatch::call` from interpreted code: back to the machine.
fn call_out(g: &mut MachineGuest<'_>, target: u32, args: [u32; 4]) -> u32 {
    crate::dispatch::call(g.0, target, args)
}

/// `dispatch::call`, first: a routine forced to run interpreted.
#[inline]
pub fn forced(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if !ac3_fallback::forcing() {
        return None;
    }
    let fallback = ac3_fallback::active()?;
    fallback.dispatch_forced(&mut MachineGuest(m), target, args, &mut call_out)
}

/// `dispatch::call`, when no module claims `target`.
pub fn miss(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let fallback = ac3_fallback::active()?;
    fallback.dispatch(&mut MachineGuest(m), target, args, &mut call_out)
}
