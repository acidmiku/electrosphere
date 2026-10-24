//! DEVELOPMENT ONLY: the interpreter fallback for untranslated routines
//! (JP_PORT_PLAN §7(b)).
//!
//! The port never interprets MIPS code (FLIGHT_MACHINE.md). This crate is
//! the one exception, for development: it lets the Japanese game boot and
//! run before all of its routines are translated. Translated routines run
//! natively; a routine nobody translated yet is interpreted from its
//! original instructions; every interpreted call is counted, so translation
//! can follow the order in which the game reaches code.
//!
//! # Gating (never in a shipped build)
//!
//! * Translation crates depend on this crate only through their cargo
//!   feature `dev-fallback`, which is off by default; so does the root
//!   package. No crate enables it for another.
//! * Each translation crate exports `DEV_FALLBACK` (`cfg!(feature =
//!   "dev-fallback")`). The shipped `ac3` binary asserts at compile time
//!   that it is false, so whatever feature unification happens, a build of
//!   `ac3` that would link the interpreter fails to compile.
//! * The same command line with the fallback is the separate binary
//!   `ac3-dev` (`required-features = ["dev-fallback"]`):
//!   `cargo build --release --features dev-fallback --bin ac3-dev`.
//!   `tools/build-windows.py` and plain `cargo build` never build it.
//!
//! # How it plugs in
//!
//! `dispatch::call(m, target, args)` of a translation crate asks every
//! module's `dispatch`; when none claims `target` it calls
//! [`Fallback::dispatch`] of the [`active`] fallback. That finds the routine
//! whose extent contains `target` ([`extents`], generated from
//! `jp_pal_map.tsv`; the overlay table is the one of
//! `common::loaded_module`), interprets it, and passes every call it makes
//! back through `dispatch::call`. Translated and interpreted code therefore
//! mix freely in both directions. With `--interpret SPEC` (see
//! [`Fallback::force`]) `dispatch::call` first offers every call to
//! [`Fallback::dispatch_forced`], which interprets translated routines too:
//! that is how the fallback is validated on PAL.
//!
//! # API
//!
//! * [`Fallback::new`]`(Version)`, [`Fallback::force`]`(spec)`; [`install`],
//!   [`active`], [`uninstall`] (one per thread); [`start`]/[`finish`] wrap
//!   them for `ac3-dev`'s `machine` command.
//! * [`Fallback::run`]`(m, address, args, call) -> v0` interprets the
//!   routine owning `address`; [`Fallback::run_wide`] returns `(v0, v1)`;
//!   [`Fallback::run_range`] takes an explicit extent (the oracle's form).
//!   `call: &mut dyn FnMut(&mut G, target, [a0..a3]) -> v0` is
//!   `dispatch::call`: every call out of interpreted code goes through it.
//! * [`Fallback::dispatch`] / [`Fallback::dispatch_forced`]: the two hooks
//!   of `dispatch::call`.
//! * [`Fallback::report`]: the reach counter as JSON (the `fallback`
//!   section of the run report): `queue` (first-reach order), `by_calls`,
//!   `blocked`, `unknown`, `stubs`, `faults`, totals.
//!
//! # The device boundary (host calls)
//!
//! Host-call IDs stay the canonical PAL SDK addresses in the JP crate. The
//! fallback keeps that true by interpreting only **above the device
//! boundary**, and by mapping the host-call stubs to their canonical IDs:
//! * it never interprets a routine marked DEVICE in the extent tables
//!   ([`flags::DEVICE`]): COP0, `syscall`, BIOS vectors, I/O ports or
//!   kernel RAM seen in its instructions, and every SDK routine not on the
//!   reviewed list of pure CPU library code (libgcc, libc, libgte, libgpu
//!   primitive helpers, TIM/TMD parsers);
//! * so an interpreted routine reaches a device only by calling an SDK
//!   routine through `dispatch::call`, where its translation (re-addressed
//!   from PAL) emits the same `HostCall`, with the PAL ID, as when the
//!   caller is translated;
//! * the BIOS and libcard routines the port answers with one host call each
//!   (`bios.rs`) are host-call stubs ([`flags::STUB`]). Their ID is
//!   canonical ([`Extent::host_id`]): a JP stub maps to its PAL counterpart
//!   through `jp_pal_map.tsv`, a JP-only one (`lseek` 800AF3E8,
//!   `Krom2RawAdd` 800AF438) keeps its JP address. JP and PAL stub
//!   addresses collide (JP `_card_format` is 800AD808, PAL `open`), so the
//!   fallback never passes a JP stub address on as an ID: a call to a stub
//!   from interpreted code, or a miss on one, runs [`Guest::host_stub`] with
//!   the canonical ID, which records the same `HostCall` the translated
//!   caller would;
//! * a call to any other device routine without a translation is refused:
//!   it raises `Fault::Untranslated` exactly as without the fallback, and
//!   the report lists it under `blocked` with its PAL counterpart (on JP,
//!   for example, the near `LoadImage2`): it needs a translation first.
//!
//! Interpreted code that spins on libetc's VBlank counter
//! ([`Tables::vblank_counter`], JP 800B6174) without calling anything waits
//! for an interrupt the host only delivers when asked: from the 256th tight
//! poll on, each iteration is a [`Guest::busy_wait`], as in the translated
//! busy-wait loops (`states::busy`).
//!
//! # Semantics and limits
//!
//! See [`interp`]: the test oracle's semantics plus Redux's COP2 pipeline.
//! A routine that runs more than [`DEFAULT_MAX_STEPS`] instructions
//! without returning (a wait for an interrupt the host only delivers in
//! VSync waits) or meets an instruction it cannot run stops with a fault
//! ([`RUNAWAY`], [`UNSUPPORTED`]), and so do the interpreted routines around
//! it. A fifth and later argument stay in the caller's frame at
//! `$sp + 0x10..`, as for translations. Interpreted callees save the
//! callee-saved registers they use with values of their own run, so stack
//! words below the caller's frame differ from the original (comparisons
//! exclude the stack).
#![forbid(unsafe_code)]

pub mod extents;
pub mod interp;
mod reach;

pub use extents::{Class, Extent, Tables, Version, flags};
pub use interp::{RETURN, Stop};

use ac3_gte::Gte;
use reach::Reach;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Trap code of an instruction the interpreter does not run (COP0,
/// `syscall`, an unknown opcode or GTE command), for `Fault::Trap`.
pub const UNSUPPORTED: u32 = 0xffff_ffff;
/// Trap code of a run stopped by the step limit, for `Fault::Trap`.
pub const RUNAWAY: u32 = 0xffff_fffe;
/// Instructions one run may execute without returning.
pub const DEFAULT_MAX_STEPS: u64 = 100_000_000;

/// The machine the interpreter runs on: the address-preserving `Machine`
/// of a translation crate (it implements this next to `dispatch.rs`).
pub trait Guest {
    /// The instruction word at `pc` (no side effects).
    fn fetch(&self, pc: u32) -> u32;
    fn lb(&mut self, address: u32) -> i8;
    fn lbu(&mut self, address: u32) -> u8;
    fn lh(&mut self, address: u32) -> i16;
    fn lhu(&mut self, address: u32) -> u16;
    fn lw(&mut self, address: u32) -> u32;
    fn sb(&mut self, address: u32, value: u8);
    fn sh(&mut self, address: u32, value: u16);
    fn sw(&mut self, address: u32, value: u32);
    fn gte(&mut self) -> &mut Gte;
    fn sp(&self) -> u32;
    fn set_sp(&mut self, sp: u32);
    /// `common::loaded_module`: the overlay module loaded now.
    fn loaded_module(&mut self) -> u32;
    /// Whether a fault is recorded.
    fn faulted(&self) -> bool;
    /// The recorded fault, for the report.
    fn fault_text(&self) -> Option<String> {
        None
    }
    /// `break code` at `address` (`Fault::Trap`).
    fn raise_trap(&mut self, address: u32, code: u32);
    /// A call to `address` nobody runs (`Fault::Untranslated`).
    fn raise_untranslated(&mut self, address: u32);
    /// An instruction the interpreter does not run ([`UNSUPPORTED`]).
    fn raise_unsupported(&mut self, address: u32, word: u32);
    /// The step limit ([`RUNAWAY`]).
    fn raise_runaway(&mut self, address: u32);
    /// Run the host-call stub at `target` (this version's address) whose
    /// canonical ID is `id` ([`Extent::host_id`]: the PAL SDK address, or a
    /// JP-only stub's JP address) as `bios.rs` does: record
    /// `HostCall { function: id, args }` and return the host's result or the
    /// documented default. The translation crate's own stub table does that
    /// (`bios::dispatch(target)`: the JP crate claims the JP addresses and
    /// records the PAL IDs), or the stub function for `id`. Never hand the
    /// address to another version's table: JP `_card_format` is 0x800AD808,
    /// PAL `open`. `None`: no such stub here; the call then goes to
    /// `dispatch::call` as any other.
    fn host_stub(&mut self, _target: u32, _id: u32, _args: [u32; 4]) -> Option<u32> {
        None
    }
    /// One iteration of interpreted code spinning on the VBlank counter
    /// ([`Tables::vblank_counter`]) at `site`, without calling anything: on
    /// the console the interrupt advances it meanwhile, here the host only
    /// delivers VBlanks when asked. Let `cycles` of guest time pass, as the
    /// native busy-wait loops do (`states::busy_wait`: the `BUSY_WAIT` host
    /// call delivers the VBlanks that fall due, then the CD interrupt work
    /// runs), and return true. The default does nothing (the loop then ends
    /// at the step limit).
    fn busy_wait(&mut self, _site: u32, _cycles: u32) -> bool {
        false
    }
}

/// How interpreted code calls out: `dispatch::call` of the translation
/// crate (a native translation, or another interpreted routine). Returns
/// `v0`.
pub type Call<'a, G> = dyn FnMut(&mut G, u32, [u32; 4]) -> u32 + 'a;

/// The routine that owns an address.
#[derive(Clone, Copy, Debug)]
pub struct Owner {
    /// Index into the counters: resident routines first, then each overlay.
    pub id: usize,
    /// 0 resident, n overlay module n.
    pub module: usize,
    pub extent: &'static Extent,
}

/// The fallback: extent tables, the routines forced to run interpreted, and
/// the reach counter.
pub struct Fallback {
    tables: Tables,
    /// First id of each table (resident, overlay 1, ...).
    offsets: Vec<usize>,
    forced: Vec<bool>,
    forced_spec: String,
    forced_count: usize,
    /// Starts of the WIDE routines (resident).
    wide: Vec<u32>,
    /// Starts of the KEEP_V0 routines (resident).
    keeps_v0: Vec<u32>,
    /// Host-call stubs (resident): start and canonical ID.
    stubs: Vec<(u32, u32)>,
    max_steps: u64,
    reach: RefCell<Reach>,
    depth: Cell<u32>,
    abort: Cell<bool>,
    busy: Cell<Duration>,
    /// The registers of an interpreted caller for the run it calls (see
    /// [`interp`]).
    handoff: Cell<Option<interp::Registers>>,
    /// Lines of call trace still to print on stderr (`set_trace`).
    trace: Cell<u32>,
}

impl Fallback {
    pub fn new(version: Version) -> Self {
        let tables = Tables::of(version);
        let mut offsets = vec![0];
        let mut total = tables.resident.len();
        for table in tables.overlays {
            offsets.push(total);
            total += table.len();
        }
        let starts = |flag: u16| -> Vec<u32> {
            tables
                .resident
                .iter()
                .filter(|e| e.flags & flag != 0)
                .map(|e| e.start)
                .collect()
        };
        let (wide, keeps_v0) = (starts(flags::WIDE), starts(flags::KEEP_V0));
        let stubs = tables
            .resident
            .iter()
            .filter_map(|e| Some((e.start, e.host_id(version)?)))
            .collect();
        Self {
            tables,
            offsets,
            forced: vec![false; total],
            forced_spec: String::new(),
            forced_count: 0,
            wide,
            keeps_v0,
            stubs,
            max_steps: DEFAULT_MAX_STEPS,
            reach: RefCell::new(Reach::new(total)),
            depth: Cell::new(0),
            abort: Cell::new(false),
            busy: Cell::new(Duration::ZERO),
            trace: Cell::new(0),
            handoff: Cell::new(None),
        }
    }
    pub fn version(&self) -> Version {
        self.tables.version
    }
    pub fn tables(&self) -> &Tables {
        &self.tables
    }
    /// Print the next `lines` entries, calls out and crossings on stderr.
    pub fn set_trace(&self, lines: u32) {
        self.trace.set(lines);
    }
    pub(crate) fn trace(&self, what: impl FnOnce() -> String) {
        let left = self.trace.get();
        if left > 0 {
            self.trace.set(left - 1);
            eprintln!(
                "fallback: {}{}",
                "  ".repeat(self.depth.get() as usize),
                what()
            );
        }
    }
    pub fn set_max_steps(&mut self, steps: u64) {
        self.max_steps = steps.max(1);
    }
    /// Whether any routine is forced to run interpreted.
    pub fn is_forcing(&self) -> bool {
        self.forced_count > 0
    }
    /// Every routine: (id, module, extent).
    pub fn routines(&self) -> impl Iterator<Item = (usize, usize, &'static Extent)> + '_ {
        let tables =
            std::iter::once(self.tables.resident).chain(self.tables.overlays.iter().copied());
        tables.enumerate().flat_map(move |(module, table)| {
            let offset = self.offsets[module];
            table
                .iter()
                .enumerate()
                .map(move |(index, extent)| (offset + index, module, extent))
        })
    }

    /// Force routines to run interpreted although translated (validation).
    /// `spec` is a comma-separated list; an item with a leading `-` removes
    /// routines again:
    /// * `all`; `resident`; `game` (all game code); `sdk` (the pure SDK);
    ///   `ov1`, `ov2`, ...: one overlay module;
    /// * `owner:NAME`: routines whose PAL counterpart the Rust module NAME
    ///   translates (e.g. `owner:hud`);
    /// * `ADDR` or `ADDR-ADDR` (hex): the routine containing ADDR, or every
    ///   routine starting in the range; `ovN:` before them picks an overlay
    ///   (overlay addresses overlap), otherwise the resident below the
    ///   overlay base and every overlay above it.
    ///
    /// Device routines and host-coupled translations ([`flags::HOST`]) are
    /// never forced. Returns the number forced.
    pub fn force(&mut self, spec: &str) -> Result<usize, String> {
        let total = self.forced.len();
        let (mut include, mut exclude) = (vec![false; total], vec![false; total]);
        for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (set, what) = match item.strip_prefix('-') {
                Some(rest) => (&mut exclude, rest),
                None => (&mut include, item),
            };
            let chosen = self.select(what)?;
            if chosen.is_empty() {
                return Err(format!("interpret {item}: no routine matches"));
            }
            for id in chosen {
                set[id] = true;
            }
        }
        let interpretable: Vec<bool> = {
            let mut v = vec![false; total];
            for (id, _, extent) in self.routines() {
                v[id] = extent.forceable();
            }
            v
        };
        for id in 0..total {
            self.forced[id] = include[id] && !exclude[id] && interpretable[id];
        }
        self.forced_count = self.forced.iter().filter(|&&f| f).count();
        self.forced_spec = spec.to_owned();
        Ok(self.forced_count)
    }

    fn select(&self, item: &str) -> Result<Vec<usize>, String> {
        let pick = |keep: &dyn Fn(usize, &Extent) -> bool| -> Vec<usize> {
            self.routines()
                .filter(|&(_, module, extent)| keep(module, extent))
                .map(|(id, _, _)| id)
                .collect()
        };
        let module_of = |name: &str| -> Option<usize> {
            let n: usize = name.strip_prefix("ov")?.parse().ok()?;
            (1..=self.tables.overlays.len()).contains(&n).then_some(n)
        };
        match item {
            "all" => return Ok(pick(&|_, _| true)),
            "resident" => return Ok(pick(&|module, _| module == 0)),
            "game" => return Ok(pick(&|_, e| e.flags & flags::SDK == 0)),
            "sdk" => return Ok(pick(&|_, e| e.flags & flags::SDK != 0)),
            _ => {}
        }
        if let Some(n) = module_of(item) {
            return Ok(pick(&|module, _| module == n));
        }
        if let Some(name) = item.strip_prefix("owner:") {
            return Ok(pick(&|_, e| {
                e.owner
                    .split(',')
                    .map(|o| o.trim().trim_end_matches(" (doc)"))
                    .any(|o| o == name)
            }));
        }
        let (module, range) = match item.split_once(':') {
            Some((m, rest)) => (
                Some(module_of(m).ok_or_else(|| format!("interpret {item}: no module {m}"))?),
                rest,
            ),
            None => (None, item),
        };
        let parse = |text: &str| {
            u32::from_str_radix(
                text.trim()
                    .trim_start_matches("0x")
                    .replace('_', "")
                    .as_str(),
                16,
            )
            .map_err(|_| format!("interpret {item}: not an address or a set"))
        };
        let base = self.tables.overlay_base;
        let in_space = move |m: usize, address: u32| match module {
            Some(n) => m == n,
            None => (m == 0) == (address < base),
        };
        Ok(match range.split_once('-') {
            Some((low, high)) => {
                let (low, high) = (parse(low)?, parse(high)?);
                pick(&|m, e| in_space(m, e.start) && (low..=high).contains(&e.start))
            }
            None => {
                let address = parse(range)?;
                pick(&|m, e| in_space(m, address) && e.contains(address))
            }
        })
    }

    /// The routine whose extent contains `address`, with the overlay module
    /// the guest has loaded.
    pub fn owner<G: Guest>(&self, g: &mut G, address: u32) -> Option<Owner> {
        let (module, table) = self.tables.table(address, || g.loaded_module())?;
        let index = Tables::find(table, address)?;
        Some(Owner {
            id: self.offsets[module] + index,
            module,
            extent: &table[index],
        })
    }

    /// `dispatch::call` found no translation for `target`: interpret the
    /// routine that owns it, or return `None` (a device routine, or no
    /// routine: the caller raises `Fault::Untranslated`) and count the miss.
    pub fn dispatch<G: Guest>(
        &self,
        g: &mut G,
        target: u32,
        args: [u32; 4],
        call: &mut Call<'_, G>,
    ) -> Option<u32> {
        match self.owner(g, target) {
            Some(owner) if owner.extent.interpretable() => {
                Some(self.enter(g, &owner, target, args, call).0)
            }
            Some(owner) => {
                if let Some(v0) = self.stub(g, target, args) {
                    return Some(v0);
                }
                self.reach.borrow_mut().refuse(target, Some(owner.id));
                None
            }
            None => match self.gap(g, target) {
                Some(end) => Some(self.run_gap(g, (target, end), args, call)),
                None => {
                    self.reach.borrow_mut().refuse(target, None);
                    None
                }
            },
        }
    }

    /// `dispatch::call`, before any translation: interpret `target` when
    /// its routine is forced (see [`Fallback::force`]).
    pub fn dispatch_forced<G: Guest>(
        &self,
        g: &mut G,
        target: u32,
        args: [u32; 4],
        call: &mut Call<'_, G>,
    ) -> Option<u32> {
        if self.forced_count == 0 {
            return None;
        }
        let owner = self.owner(g, target)?;
        if !self.forced[owner.id] {
            return None;
        }
        Some(self.enter(g, &owner, target, args, call).0)
    }

    /// Interpret the routine that owns `address` (from `address`) and
    /// return `v0`; with no interpretable owner, raise
    /// `Fault::Untranslated` and return 0.
    pub fn run<G: Guest>(
        &self,
        g: &mut G,
        address: u32,
        args: [u32; 4],
        call: &mut Call<'_, G>,
    ) -> u32 {
        self.run_wide(g, address, args, call).0
    }

    /// [`Fallback::run`] returning `(v0, v1)`, for 64-bit results.
    pub fn run_wide<G: Guest>(
        &self,
        g: &mut G,
        address: u32,
        args: [u32; 4],
        call: &mut Call<'_, G>,
    ) -> (u32, u32) {
        match self.owner(g, address) {
            Some(owner) if owner.extent.interpretable() => {
                self.enter(g, &owner, address, args, call)
            }
            other => {
                self.reach
                    .borrow_mut()
                    .refuse(address, other.map(|owner| owner.id));
                g.raise_untranslated(address);
                (0, 0)
            }
        }
    }

    /// Interpret `start..end` from `start`, like the test oracle's `run`
    /// (no extent lookup, not counted per routine). Returns `(v0, v1)`.
    pub fn run_range<G: Guest>(
        &self,
        g: &mut G,
        (start, end): (u32, u32),
        args: [u32; 4],
        call: &mut Call<'_, G>,
    ) -> (u32, u32) {
        self.reach.borrow_mut().range_runs += 1;
        let out = self.guarded(g, |fallback, g| {
            interp::exec(fallback, g, (start, end), start, args, call)
        });
        let mut reach = self.reach.borrow_mut();
        reach.instructions += out.steps;
        reach.hazards += out.hazards;
        (out.v0, out.v1)
    }

    /// Code no extent covers, called or jumped to (the analysis left 8-byte
    /// stubs outside functions, and some extents end early, e.g. before
    /// the second entry at 800A6050 that PAL's libgte code reaches): inside
    /// the code of the resident or the loaded overlay, it runs up to the next
    /// extent (at most 16 KiB). Returns that end.
    fn gap<G: Guest>(&self, g: &mut G, target: u32) -> Option<u32> {
        let (module, table) = self.tables.table(target, || g.loaded_module())?;
        let (first, last) = (table.first()?, table.last()?);
        let low = if module == 0 {
            first.start
        } else {
            self.tables.overlay_base
        };
        if target & 3 != 0 || target < low || target >= last.end() {
            return None;
        }
        let next_start = table
            .get(table.partition_point(|e| e.start <= target))
            .map_or(last.end(), |e| e.start);
        Some(next_start.min(target.saturating_add(0x4000)))
    }

    fn run_gap<G: Guest>(
        &self,
        g: &mut G,
        (start, end): (u32, u32),
        args: [u32; 4],
        call: &mut Call<'_, G>,
    ) -> u32 {
        self.trace(|| {
            format!(
                "enter {start:08x} (no extent, up to {end:08x}) sp {:08x}",
                g.sp()
            )
        });
        let out = self.guarded(g, |fallback, g| {
            interp::exec(fallback, g, (start, end), start, args, call)
        });
        let mut reach = self.reach.borrow_mut();
        let gap = reach.gaps.entry(start).or_default();
        gap.calls += 1;
        gap.instructions += out.steps;
        reach.instructions += out.steps;
        out.v0
    }

    /// Run `body` one level deeper: abort and time bookkeeping.
    fn guarded<G: Guest>(
        &self,
        g: &mut G,
        body: impl FnOnce(&Self, &mut G) -> interp::Outcome,
    ) -> interp::Outcome {
        let depth = self.depth.get();
        if depth == 0 {
            self.abort.set(false);
        }
        self.depth.set(depth + 1);
        let started = (depth == 0).then(Instant::now);
        let out = body(self, g);
        self.depth.set(depth);
        if let Some(started) = started {
            self.busy.set(self.busy.get() + started.elapsed());
        }
        if matches!(
            out.stop,
            Some(Stop::Unsupported | Stop::Runaway | Stop::Aborted)
        ) {
            self.abort.set(true);
        }
        {
            let mut reach = self.reach.borrow_mut();
            reach.deepest = reach.deepest.max(depth + 1);
        }
        out
    }

    fn enter<G: Guest>(
        &self,
        g: &mut G,
        owner: &Owner,
        entry: u32,
        args: [u32; 4],
        call: &mut Call<'_, G>,
    ) -> (u32, u32) {
        self.reach.borrow_mut().enter(owner.id);
        self.trace(|| {
            format!(
                "enter {entry:08x} ({} {:08x}) sp {:08x}",
                owner.extent.name,
                owner.extent.start,
                g.sp()
            )
        });
        let faulted = g.faulted();
        let extent = owner.extent;
        let out = self.guarded(g, |fallback, g| {
            interp::exec(fallback, g, (extent.start, extent.end()), entry, args, call)
        });
        let mut reach = self.reach.borrow_mut();
        reach.leave(owner.id, out.steps, out.hazards);
        if let Some(address) = out.crossed {
            *reach.crossings.entry(address).or_default() += 1;
        }
        if !faulted && g.faulted() {
            let what = match out.stop {
                Some(Stop::Runaway) => format!("step limit ({} instructions)", self.max_steps),
                Some(Stop::Unsupported) => "unsupported instruction".to_owned(),
                _ => g.fault_text().unwrap_or_else(|| "fault".to_owned()),
            };
            reach.fault(Some(owner.id), entry, what);
        }
        (out.v0, out.v1)
    }

    /// A call out of interpreted code: `(v0, v1)`, `None` where the
    /// caller's register stays. libgcc's 64-bit helpers are interpreted here
    /// even when translated, so the caller gets `v1` too; anything else goes
    /// through `call` (`v1` then stays, as in the oracle). A KEEP_V0 device
    /// routine (ExitCriticalSection) leaves `v0` too.
    pub(crate) fn callout<G: Guest>(
        &self,
        g: &mut G,
        target: u32,
        args: [u32; 4],
        call: &mut Call<'_, G>,
    ) -> (Option<u32>, Option<u32>) {
        self.reach.borrow_mut().callouts += 1;
        self.trace(|| format!("call {target:08x} {args:08x?}"));
        if self.wide.contains(&target)
            && let Some(owner) = self.owner(g, target)
        {
            let (v0, v1) = self.enter(g, &owner, target, args, call);
            return (Some(v0), Some(v1));
        }
        let keep = self.keeps_v0.contains(&target);
        if let Some(v0) = self.stub(g, target, args) {
            return ((!keep).then_some(v0), None);
        }
        let v0 = call(g, target, args);
        ((!keep).then_some(v0), None)
    }

    /// The host-call stub at `target` run under its canonical ID
    /// ([`Guest::host_stub`]), when it is one and the guest has it.
    fn stub<G: Guest>(&self, g: &mut G, target: u32, args: [u32; 4]) -> Option<u32> {
        let &(_, id) = self.stubs.iter().find(|(start, _)| *start == target)?;
        let v0 = g.host_stub(target, id, args)?;
        self.trace(|| format!("stub {target:08x} as host call {id:08x}"));
        self.reach
            .borrow_mut()
            .stubs
            .entry(target)
            .or_insert((id, 0))
            .1 += 1;
        Some(v0)
    }

    pub(crate) fn busy_waited(&self) {
        self.reach.borrow_mut().busy_waits += 1;
        self.trace(|| "busy wait (VBlank counter poll)".to_owned());
    }

    pub(crate) fn aborted(&self) -> bool {
        self.abort.get()
    }

    /// The routines interpreted so far in first-reach order, with their
    /// calls and instructions.
    pub fn reached_routines(&self) -> Vec<(Owner, u64, u64)> {
        let reach = self.reach.borrow();
        let mut list: Vec<_> = self
            .routines()
            .filter(|&(id, _, _)| reach.counters[id].calls > 0)
            .map(|(id, module, extent)| {
                let c = reach.counters[id];
                (
                    c.first,
                    (Owner { id, module, extent }, c.calls, c.instructions),
                )
            })
            .collect();
        list.sort_by_key(|(first, _)| *first);
        list.into_iter().map(|(_, entry)| entry).collect()
    }

    /// Calls and instructions of the routine starting at `address` (any
    /// table), for tests and tools.
    pub fn reached(&self, address: u32) -> Option<(u64, u64)> {
        let reach = self.reach.borrow();
        self.routines()
            .filter(|(_, _, e)| e.start == address)
            .map(|(id, _, _)| reach.counters[id])
            .find(|c| c.calls > 0)
            .map(|c| (c.calls, c.instructions))
    }
    /// Interpreted instructions in total.
    pub fn instructions(&self) -> u64 {
        self.reach.borrow().instructions
    }
    /// Runs that went on inline into another extent, in total.
    pub fn crossings(&self) -> u64 {
        self.reach.borrow().crossings.values().sum()
    }

    /// The run report (JSON): routines in first-reach order (the
    /// translation queue) and by calls, refused calls, faults, totals.
    pub fn report(&self) -> Value {
        let reach = self.reach.borrow();
        let mut reached: Vec<(usize, usize, &Extent)> = self
            .routines()
            .filter(|&(id, _, _)| reach.counters[id].calls > 0)
            .collect();
        reached.sort_by_key(|&(id, _, _)| reach.counters[id].first);
        let entry = |&(id, module, extent): &(usize, usize, &Extent)| {
            reach::routine(extent, module, &reach.counters[id], self.forced[id])
        };
        let (wide, queue): (Vec<_>, Vec<_>) = reached
            .iter()
            .copied()
            .partition(|(_, _, e)| e.flags & flags::WIDE != 0);
        let mut by_calls = reached.clone();
        by_calls.sort_by_key(|&(id, _, _)| {
            let c = &reach.counters[id];
            (
                std::cmp::Reverse(c.calls),
                std::cmp::Reverse(c.instructions),
                c.first,
            )
        });
        let by_id = |id: usize| self.routines().find(|&(i, _, _)| i == id);
        let refused = |misses: &std::collections::BTreeMap<u32, reach::Miss>| {
            let mut list: Vec<_> = misses.iter().collect();
            list.sort_by_key(|(_, miss)| miss.first);
            list.into_iter()
                .map(|(&address, miss)| {
                    let mut v = json!({
                        "address": reach::hex(address),
                        "calls": miss.calls,
                        "first": miss.first,
                    });
                    if let Some((_, module, extent)) = miss.id.and_then(by_id) {
                        v["routine"] = json!(reach::hex(extent.start));
                        v["module"] = json!(reach::module_name(module));
                        v["name"] = json!(extent.name);
                        v["reasons"] = json!(flags::reasons(extent.flags));
                        v["class"] = json!(extent.class.name());
                        v["counterpart"] = json!(
                            (extent.counterpart != 0).then(|| reach::hex(extent.counterpart))
                        );
                    }
                    v
                })
                .collect::<Vec<_>>()
        };
        json!({
            "version": match self.tables.version { Version::Pal => "pal", Version::Jp => "jp" },
            "forced": self.forced_spec,
            "forced_routines": self.forced_count,
            "routines": queue.len(),
            "bytes": queue.iter().map(|(_, _, e)| u64::from(e.size)).sum::<u64>(),
            "calls": queue.iter().map(|&(id, _, _)| reach.counters[id].calls).sum::<u64>(),
            "instructions": reach.instructions,
            "callouts": reach.callouts,
            "deepest": reach.deepest,
            "gte_hazards": reach.hazards,
            "range_runs": reach.range_runs,
            // Wall time of interpreted call trees, native callees included.
            "seconds": self.busy.get().as_secs_f64(),
            "queue": queue.iter().map(&entry).collect::<Vec<_>>(),
            "by_calls": by_calls.iter().map(|&(id, _, e)| json!([
                reach::hex(e.start), reach.counters[id].calls, reach.counters[id].instructions
            ])).collect::<Vec<_>>(),
            "wide_helpers": wide.iter().map(&entry).collect::<Vec<_>>(),
            "blocked": refused(&reach.blocked),
            "unknown": refused(&reach.unknown),
            "crossings": reach.crossings.iter().map(|(&a, n)| json!([reach::hex(a), n])).collect::<Vec<_>>(),
            "stubs": reach.stubs.iter().map(|(&a, &(id, n))| json!({
                "address": reach::hex(a), "host_call": reach::hex(id), "calls": n
            })).collect::<Vec<_>>(),
            "busy_waits": reach.busy_waits,
            "gaps": reach.gaps.iter().map(|(&a, c)| json!({
                "address": reach::hex(a), "calls": c.calls, "instructions": c.instructions
            })).collect::<Vec<_>>(),
            "faults": reach.faults.iter().map(|f| json!({
                "routine": f.id.and_then(by_id).map(|(_, _, e)| reach::hex(e.start)),
                "entry": reach::hex(f.address),
                "fault": f.what,
            })).collect::<Vec<_>>(),
        })
    }

    /// One line for the console.
    pub fn summary(&self) -> String {
        let report = self.report();
        format!(
            "Fallback ({}): {} routines interpreted ({} bytes, {} calls, {} instructions, {:.2} s), \
             {} forced, {} blocked, {} unknown, {} faults",
            report["version"].as_str().unwrap_or("?"),
            report["routines"],
            report["bytes"],
            report["calls"],
            report["instructions"],
            report["seconds"].as_f64().unwrap_or(0.0),
            report["forced_routines"],
            report["blocked"].as_array().map_or(0, Vec::len),
            report["unknown"].as_array().map_or(0, Vec::len),
            report["faults"].as_array().map_or(0, Vec::len),
        )
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Rc<Fallback>>> = const { RefCell::new(None) };
    static FORCING: Cell<bool> = const { Cell::new(false) };
}

/// Make `fallback` the one `dispatch::call` uses on this thread.
pub fn install(fallback: Fallback) -> Rc<Fallback> {
    let fallback = Rc::new(fallback);
    FORCING.set(fallback.is_forcing());
    ACTIVE.with_borrow_mut(|active| *active = Some(fallback.clone()));
    fallback
}
/// The installed fallback.
pub fn active() -> Option<Rc<Fallback>> {
    ACTIVE.with_borrow(Clone::clone)
}
/// Remove the installed fallback.
pub fn uninstall() -> Option<Rc<Fallback>> {
    FORCING.set(false);
    ACTIVE.with_borrow_mut(Option::take)
}
/// Whether the installed fallback forces any routine (the cheap test
/// `dispatch::call` makes on every call).
#[inline]
pub fn forcing() -> bool {
    FORCING.get()
}

/// `ac3-dev`: install the fallback for `version`, with `--interpret SPEC`
/// when given (and `AC3_FALLBACK_MAX_STEPS` from the environment), and say
/// so on stderr.
pub fn start(version: Version, interpret: Option<&str>) -> Result<Rc<Fallback>, String> {
    let mut fallback = Fallback::new(version);
    if let Some(steps) = std::env::var("AC3_FALLBACK_MAX_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        fallback.set_max_steps(steps);
    }
    if let Some(spec) = interpret {
        fallback.force(spec)?;
    }
    eprintln!(
        "ac3-dev: DEVELOPMENT BUILD with the interpreter fallback ({:?} tables, {} routines \
         forced to run interpreted)",
        version, fallback.forced_count
    );
    Ok(install(fallback))
}

/// `ac3-dev`: remove the fallback, add its report to `result["fallback"]`
/// and print the summary line.
pub fn finish(fallback: Rc<Fallback>, mut result: Value) -> Value {
    uninstall();
    println!("{}", fallback.summary());
    result["fallback"] = fallback.report();
    result
}
