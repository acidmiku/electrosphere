//! The differential harness, generic over the crate under test (the JP crate
//! against the JP originals, and the PAL crate as a control): every case runs
//! the original instructions (the test-only oracle) and the translation
//! (`dispatch::call`) from clones of one machine state and compares RAM
//! outside the stack window, scratchpad, GTE, `sp`, the first fault and `v0`.
#![allow(dead_code)]
use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// A fault, crate-independent.
#[derive(Clone, Debug, PartialEq)]
pub enum Fault {
    Untranslated(u32),
    Address(u32),
    Misaligned(u32),
    Other(String),
}

/// One machine of a crate under test.
pub trait Target {
    /// The resident `$gp`.
    const GP: u32;
    fn from_ram(ram: &[u8], sp: u32) -> Self;
    fn duplicate(&self) -> Self;
    fn sw(&mut self, address: u32, value: u32);
    /// Attach a device that answers nothing and panics after `limit` calls.
    fn limit_host_calls(&mut self, limit: usize);
    fn oracle(&mut self, start: u32, end: u32, args: [u32; 4]) -> u32;
    fn native(&mut self, start: u32, args: [u32; 4]) -> u32;
    fn take_fault(&mut self) -> Option<Fault>;
    fn ram(&self) -> &[u8];
    fn scratch(&self) -> &[u8];
    fn same_gte(&self, other: &Self) -> bool;
    fn sp(&self) -> u32;
}

/// A function's result: (cases, RAM bytes changed) or (cause, case, detail).
pub type Verdict = Result<(usize, usize), (String, String, String)>;

/// A RAM image to start from: (name, 2 MiB RAM).
pub type State = (String, Vec<u8>);

pub const WORKERS: usize = 8;
pub const WORKER: &str = "differential-worker";
pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_ABANDONED: usize = 96;
/// Randomised variants per RAM image.
pub const VARIANTS: u32 = 6;
/// Guest `sp` at entry.
pub const SP: u32 = 0x801f_ff00;
/// The stack window (not compared: natives keep locals in Rust), as in
/// the snapshot comparisons.
pub const STACK: std::ops::Range<u32> = 0x1f_c000..0x20_0000;
/// Host calls a case may make before it counts as a runaway loop.
pub const HOST_CALLS: usize = 100_000;

/// Deterministic variant generator (the PAL oracle's LCG).
pub struct Rng(pub u32);
impl Rng {
    pub fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
        self.0 >> 8
    }
}

/// Word-aligned RAM pointers held in the gp area of an image.
pub fn pointer_pool(ram: &[u8], gp: u32) -> Vec<u32> {
    let mut pool: Vec<u32> = (gp - 0x8000..gp + 0x8000)
        .step_by(4)
        .map(|a| {
            let at = (a & 0x1f_ffff) as usize;
            u32::from_le_bytes(ram[at..at + 4].try_into().unwrap())
        })
        .filter(|&v| (0x8001_0000..0x8020_0000).contains(&v) && v % 4 == 0)
        .collect();
    pool.sort_unstable();
    pool.dedup();
    pool
}

/// Arguments: pointers from the gp area, zero, small and medium numbers
/// (no full random words: as loop counts they only make cases slow).
pub fn arguments(rng: &mut Rng, pool: &[u32]) -> [u32; 4] {
    std::array::from_fn(|_| match rng.next() % 10 {
        0..=4 if !pool.is_empty() => pool[rng.next() as usize % pool.len()],
        5 | 6 => 0,
        7 | 8 => rng.next() % 16,
        _ => rng.next() % 0x200,
    })
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn is_io(address: u32) -> bool {
    let physical = address & 0x1fff_ffff;
    (0x1f80_1000..0x1f80_4000).contains(&physical) || physical >= 0x1fc0_0000
}

fn fault_cause(fault: &Fault, side: &str) -> (String, String) {
    let detail = format!("{side} {fault:x?}");
    let cause = match *fault {
        Fault::Untranslated(_) => "callee not claimed (the oracle calls by dispatch)",
        Fault::Address(a) | Fault::Misaligned(a) if a >> 16 == 0xdead => {
            "poison value (unmapped const)"
        }
        Fault::Address(a) if is_io(a) => "device access in the original",
        _ => "fault differs",
    };
    (cause.into(), detail)
}

/// One case: Ok(RAM bytes the original changed) or Err((cause, detail)).
pub fn compare<T: Target>(
    base: &T,
    (start, end): (u32, u32),
    args: [u32; 4],
    void: bool,
) -> Result<usize, (String, String)> {
    let mut original = base.duplicate();
    original.limit_host_calls(HOST_CALLS);
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| original.oracle(start, end, args))).map_err(
        |e| {
            let text = panic_text(&*e);
            let cause = if text.contains("JP-REVIEW") {
                "unmapped site (reached by a callee)"
            } else if text.contains("runaway") {
                "original runs away (step or host-call limit)"
            } else {
                "oracle stopped (an instruction it does not model)"
            };
            (cause.to_string(), text)
        },
    )?;
    let mut native = base.duplicate();
    native.limit_host_calls(HOST_CALLS);
    let nv0 =
        panic::catch_unwind(AssertUnwindSafe(|| native.native(start, args))).map_err(|e| {
            let text = panic_text(&*e);
            let cause = if text.contains("JP-REVIEW") {
                "unmapped site"
            } else if text.contains("overflow") {
                "native arithmetic overflow (random input)"
            } else if text.contains("runaway") {
                "native runs away (host-call limit)"
            } else {
                "native panic"
            };
            (cause.to_string(), text)
        })?;
    let (fo, fnat) = (original.take_fault(), native.take_fault());
    if fo != fnat {
        return Err(match (&fo, &fnat) {
            (Some(f), None) => fault_cause(f, "original"),
            (_, Some(f)) => fault_cause(f, "native"),
            _ => unreachable!(),
        });
    }
    let ram: Vec<u32> = (0..0x20_0000u32)
        .filter(|i| !STACK.contains(i) && original.ram()[*i as usize] != native.ram()[*i as usize])
        .map(|i| 0x8000_0000 + i)
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| original.scratch()[i] != native.scratch()[i])
        .count();
    let gte = original.same_gte(&native);
    if !ram.is_empty() || scratch != 0 || !gte || original.sp() != native.sp() {
        let cause = if ram.is_empty() && scratch == 0 && gte {
            // the original left through a jump into another routine's tail,
            // which the oracle runs as a call
            "sp differs (tail jump into another routine)"
        } else if !ram.is_empty() && ram.iter().all(|&a| a < 0x8001_0000) && scratch == 0 {
            "writes below the resident (null pointers from random arguments)"
        } else {
            "state differs"
        };
        return Err((
            cause.into(),
            format!(
                "ram {:08x?} ({} bytes) scratch {scratch} gte equal {gte} sp {:x}/{:x}",
                &ram[..ram.len().min(6)],
                ram.len(),
                original.sp(),
                native.sp()
            ),
        ));
    }
    if nv0 != v0 && !void {
        return Err((
            "v0 differs".into(),
            format!("original {v0:#x}, native {nv0:#x}"),
        ));
    }
    Ok((0..0x20_0000usize)
        .filter(|&i| !STACK.contains(&(i as u32)) && base.ram()[i] != original.ram()[i])
        .count())
}

/// Every case of one function.
pub fn run_function<T: Target>(
    start: u32,
    end: u32,
    void: bool,
    states: &[State],
) -> (Verdict, usize) {
    let (mut passed, mut changed) = (0usize, 0usize);
    for (name, ram) in states {
        let pool = pointer_pool(ram, T::GP);
        for variant in 0..VARIANTS {
            let mut rng = Rng(start ^ (variant << 24) ^ (name.len() as u32 * 0x9e37));
            let mut base = T::from_ram(ram, SP);
            if variant >= VARIANTS / 2 {
                // randomise a few gp-area words (mode words, flags, counters)
                for _ in 0..6 {
                    let offset = (rng.next() % 0x600) & !3;
                    let value = match rng.next() % 3 {
                        0 => 0,
                        1 => rng.next() % 4,
                        _ => rng.next() % 0x100,
                    };
                    base.sw(T::GP + offset, value);
                }
            }
            let args = arguments(&mut rng, &pool);
            match compare(&base, (start, end), args, void) {
                Ok(n) => {
                    passed += 1;
                    changed += n;
                }
                Err((cause, detail)) => {
                    return (
                        Err((cause, format!("{name} v{variant} args {args:08x?}"), detail)),
                        passed + 1,
                    );
                }
            }
        }
    }
    (Ok((passed, changed)), passed)
}

/// Run every function `(start, end, void)` on worker threads; a function
/// that does not finish in TIMEOUT (a loop that does not end on its random
/// state) is recorded as such and its thread abandoned. Returns the
/// verdicts (None: not run) and the number of cases.
pub fn run_all<T: Target + 'static>(
    functions: &[(u32, u32, bool)],
    states: Vec<State>,
) -> (Vec<Option<Verdict>>, usize) {
    let states = Arc::new(states);
    let hook = Arc::new(panic::take_hook());
    {
        let hook = hook.clone();
        panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() != Some(WORKER) {
                hook(info);
            }
        }));
    }
    let (tx, rx) = mpsc::channel::<(usize, Verdict, usize)>();
    let mut verdicts: Vec<Option<Verdict>> = vec![None; functions.len()];
    let mut active: BTreeMap<usize, Instant> = BTreeMap::new();
    let (mut next, mut done, mut cases, mut abandoned) = (0, 0, 0usize, 0);
    while done < functions.len() {
        while active.len() < WORKERS && next < functions.len() && abandoned < MAX_ABANDONED {
            let (start, end, void) = functions[next];
            let (states, tx) = (states.clone(), tx.clone());
            let index = next;
            std::thread::Builder::new()
                .name(WORKER.into())
                .stack_size(64 << 20)
                .spawn(move || {
                    let (verdict, n) = run_function::<T>(start, end, void, &states);
                    let _ = tx.send((index, verdict, n));
                })
                .expect("worker");
            active.insert(next, Instant::now());
            next += 1;
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok((index, verdict, n)) => {
                if active.remove(&index).is_some() {
                    verdicts[index] = Some(verdict);
                    cases += n;
                    done += 1;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let late: Vec<usize> = active
                    .iter()
                    .filter(|(_, t)| t.elapsed() > TIMEOUT)
                    .map(|(i, _)| *i)
                    .collect();
                for index in late {
                    active.remove(&index);
                    verdicts[index] = Some(Err((
                        "does not finish in 10 s (a loop on a random state)".into(),
                        String::new(),
                        String::new(),
                    )));
                    done += 1;
                    abandoned += 1;
                }
                if abandoned >= MAX_ABANDONED && active.is_empty() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = panic::take_hook();
    (verdicts, cases)
}
