//! Replay of the JP reference captures (`tools/jp/capture_runs.py --pass
//! snap`; `~/ac3-jp/analysis/redux/<run>/snapshots-module{0,1,2}.jsonl`):
//! every captured call of a routine this crate claims (the generated
//! modules or the patch layer) runs natively from its entry snapshot and
//! must reproduce the return snapshot, as `snapshot::check_call` does: all
//! RAM outside the stack, the scratchpad, the GTE, and `v0` when the
//! translation models one.
//!
//! This is how a patch is validated (PATCHES.md): a routine of the patch
//! layer must reproduce every capture of it, or pass as device work: from
//! the same entry snapshot its translation equals the original
//! instructions (state and `v0`), it reached a device and
//! `proof::DEVICE_WORK` or `proof::OWNED` explains the difference, so the
//! capture's difference is the device's (`support::proof`; reported as
//! "pass (device work)"). A call `proof::BLOCKED` by a callee that is not
//! yet the JP routine is reported as "blocked" when its translation equals
//! the original instructions. The replays (`proof::replay_machine`) answer
//! the BIOS event calls from the kernel's event table and the CD drive and
//! VRAM reads from the capture pair. Generated routines are reported by
//! class: an exact routine reproduces its captures unless it reaches a
//! device (host calls the test does not answer) or a review site; a near
//! or changed routine shows the patch work still to do.
//! Per-call results: `$CARGO_TARGET_TMPDIR/jp-captures.tsv`.
mod support;

use ac3_flight_jp::snapshot::{self, Snapshot, SnapshotInfo};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use support::{proof, scan};

/// The capture root (`AC3_JP_CAPTURES`, else `AC3_JP_DIR/analysis/redux`).
fn root() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
}

/// Every `snapshots-module<N>.jsonl` under the root, with N.
fn indexes() -> Vec<(PathBuf, u32)> {
    let mut out = Vec::new();
    let Ok(runs) = std::fs::read_dir(root()) else {
        return out;
    };
    for run in runs.flatten() {
        let Ok(files) = std::fs::read_dir(run.path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if let Some(n) = name
                .strip_prefix("snapshots-module")
                .and_then(|r| r.strip_suffix(".jsonl"))
                .and_then(|n| n.parse().ok())
            {
                out.push((f.path(), n));
            }
        }
    }
    out.sort();
    out
}

/// The return snapshot matching an entry (the first later return of the
/// same callee with the same `ra` and `sp`, as `snapshot::check_call`).
fn exit_of<'a>(index: &'a [SnapshotInfo], entry: &SnapshotInfo) -> Option<&'a SnapshotInfo> {
    index.iter().find(|s| {
        s.callee == entry.callee
            && s.stage == "return"
            && s.sequence > entry.sequence
            && s.ra == entry.ra
            && s.registers.gpr[29] == entry.registers.gpr[29]
    })
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Replay one captured call of a routine in `module` (0 resident): Ok(bytes
/// the original changed) or Err(cause). The replay machine and device
/// (`proof::replay_machine`: the BIOS events, the capture's CD drive and
/// VRAM reads, the MDEC; interrupt-stack captures at `proof::HOST_IRQ_SP`)
/// and the RAM left out of the comparison are shared with the part tests:
/// `proof::OWNED` (the BIOS kernel, the interrupt handlers, the libgpu
/// queue and DMA polling, ClearImage's GPU-status words; found by `jp-ov2a`,
/// `jp-ov2e`, `jp-res3`) and the device transfers' destinations
/// (`proof::device_ranges`: StoreImage, found by `jp-ov2f`; the MDEC
/// output, by `jp-ov2e`).
fn replay(
    directory: &Path,
    entry: &SnapshotInfo,
    exit: &SnapshotInfo,
    module: u32,
    void: bool,
) -> Result<usize, String> {
    let entry = Snapshot::load(directory, entry).map_err(|e| format!("load: {e}"))?;
    let exit = Snapshot::load(directory, exit).map_err(|e| format!("load: {e}"))?;
    let mut m = proof::replay_machine(&entry, &exit, module);
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let callee = entry.info.callee;
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| {
        ac3_flight_jp::dispatch::call(&mut m, callee, args)
    }))
    .map_err(|e| {
        let text = panic_text(&*e);
        if text.contains("JP-REVIEW") {
            format!("unmapped site: {text}")
        } else {
            format!("panic: {text}")
        }
    })?;
    let devices = !m.host_calls.is_empty();
    if let Err(fault) = m.take_fault() {
        return Err(format!("fault: {fault}"));
    }
    let mut d = snapshot::compare(&m, &exit);
    let stored = proof::device_ranges(&m);
    d.ram
        .retain(|&a| !proof::owned(a) && !stored.iter().any(|r| r.contains(&(a & 0x1f_ffff))));
    let tag = if devices { " (with host calls)" } else { "" };
    if !d.is_empty() {
        return Err(format!("state differs{tag}: {}", d.summary()));
    }
    if !void && v0 != exit.gpr(2) {
        return Err(format!(
            "v0 differs{tag}: {v0:#010x}, original {:#010x}",
            exit.gpr(2)
        ));
    }
    Ok(snapshot::changed_bytes(&entry, &exit))
}

/// The proof for a patch-layer call its replay does not reproduce (`cause`):
/// Ok((verdict, why)) for a `proof::BLOCKED` call (`proof::blocked`:
/// "blocked") or one whose state or `v0` differs as device work
/// (`proof::device_work`: "pass (device work)"), from the replay's machine
/// and device; Err(why not).
fn prove(
    directory: &Path,
    (entry, exit): (&SnapshotInfo, &SnapshotInfo),
    module: u32,
    range: (u32, u32),
    void: bool,
    cause: &str,
) -> Result<(&'static str, String), String> {
    let entry = Snapshot::load(directory, entry).map_err(|e| format!("load: {e}"))?;
    let exit = Snapshot::load(directory, exit).map_err(|e| format!("load: {e}"))?;
    let base = proof::replay_machine(&entry, &exit, module);
    let call = proof::Call {
        entry: &entry,
        exit: &exit,
        base: &base,
        module,
        range,
        void,
    };
    let device = || -> Box<dyn ac3_flight_jp::Device> {
        Box::new(proof::Replay::new(&entry, &exit, module))
    };
    if proof::blocked_by(module, entry.info.callee).is_some() {
        return proof::blocked(&call, &device)
            .map(|why| ("blocked", why))
            .map_err(|e| format!("not blocked: {e}"));
    }
    if !(cause.starts_with("state differs") || cause.starts_with("v0 differs")) {
        return Err("no proof applies".into());
    }
    proof::device_work(&call, &device)
        .map(|why| ("pass (device work)", why))
        .map_err(|e| format!("not device work: {e}"))
}

fn cause_class(cause: &str) -> String {
    let head = cause.split(':').next().unwrap_or(cause);
    head.split(" (").next().unwrap_or(head).to_string()
        + if cause.contains("(with host calls)") {
            " (with host calls)"
        } else {
            ""
        }
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls_replay() {
    // Untranslated callees of a patch-layer routine run interpreted.
    #[cfg(feature = "dev-fallback")]
    let _fallback = ac3_fallback::install(ac3_fallback::Fallback::new(ac3_fallback::Version::Jp));
    let rows = scan::jp_map(&scan::jp_dir());
    let (resident, void) = scan::jp_resident_claims();
    let overlay = scan::jp_overlay_claims();
    // Overlay routines whose arm returns no value (`{ f(m); 0 }`), per
    // module (found by `jp-ov1a`): their `v0` is not compared either.
    let overlay_void: BTreeMap<u32, BTreeSet<u32>> = (1..=4)
        .map(|n| {
            let other = |name: &str| scan::overlay_of(name) != Some(n);
            (n, scan::claims_and_void(&scan::crate_dir(), &other).1)
        })
        .collect();
    let patched: BTreeSet<u32> = resident
        .iter()
        .filter(|(_, sites)| sites.iter().any(|(f, _)| f.starts_with("jp/")))
        .map(|(a, _)| *a)
        .collect();
    let replaced: BTreeSet<u32> = scan::patch_table("REPLACES").into_iter().collect();
    let class_of = |region: &str, address: u32| {
        rows.iter()
            .find(|r| {
                r.jp == address
                    && (r.region == region
                        || (region == "resident" && r.region.starts_with("resident")))
            })
            .map_or("unknown".to_string(), |r| r.class.clone())
    };
    let extent_of = |region: &str, address: u32| {
        rows.iter()
            .find(|r| {
                r.jp == address
                    && (r.region == region
                        || (region == "resident" && r.region.starts_with("resident")))
            })
            .map(|r| (r.jp, r.jp + r.size))
    };
    let indexes = indexes();
    assert!(
        !indexes.is_empty(),
        "no captures under {}",
        root().display()
    );
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let mut table: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut report = String::from("capture\tsequence\tcallee\tclass\tresult\tdetail\n");
    let (mut unclaimed, mut patch_failures) = (BTreeMap::<String, usize>::new(), Vec::new());
    let (mut blocked, mut stale) = (Vec::new(), BTreeSet::new());
    for (path, module) in &indexes {
        let directory = path.parent().unwrap();
        let run = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let index =
            snapshot::read_index(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for entry in index.iter().filter(|s| s.stage == "entry") {
            let callee = entry.callee;
            let in_overlay = callee >= ac3_flight_jp::common::OVERLAY_BASE && *module != 0;
            let region = if in_overlay {
                format!("ov{module}")
            } else {
                "resident".to_string()
            };
            let claimed = if in_overlay {
                overlay.contains_key(&(*module, callee))
            } else {
                resident.contains_key(&callee)
            };
            let mut class = class_of(&region, callee);
            if !claimed {
                *unclaimed.entry(format!("{region} {class}")).or_default() += 1;
                continue;
            }
            let is_patch = patched.contains(&callee) || replaced.contains(&callee) || in_overlay;
            if is_patch {
                class = format!("patch ({class})");
            }
            let Some(exit) = exit_of(&index, entry) else {
                continue;
            };
            let is_void = if in_overlay {
                overlay_void
                    .get(module)
                    .is_some_and(|v| v.contains(&callee))
            } else {
                void.contains(&callee)
            };
            let module_key = if in_overlay { *module } else { 0 };
            let result = replay(directory, entry, exit, module_key, is_void);
            // A patch-layer call that misses its capture passes as device
            // work, or counts as blocked by a callee, only with the proofs
            // of `support::proof` (found by `jp-ov1a`): from the same entry,
            // the translation equals the original instructions (state and
            // `v0`); for device work it reached a device and
            // `proof::DEVICE_WORK` or `proof::OWNED` explains the difference.
            let (verdict, detail, failed) = match &result {
                Ok(n) => {
                    let mut detail = format!("{n} bytes changed");
                    if let Some(why) = proof::blocked_by(module_key, callee) {
                        stale.insert(format!("{callee:08x} ({why})"));
                        detail += "; passes, listed in proof::BLOCKED";
                    }
                    ("pass".to_string(), detail, false)
                }
                Err(e) if is_patch => {
                    let proof = extent_of(&region, callee)
                        .ok_or_else(|| "no extent in the map".to_string())
                        .and_then(|range| {
                            prove(directory, (entry, exit), module_key, range, is_void, e)
                        });
                    match proof {
                        Ok((verdict, why)) => (verdict.to_string(), format!("{e}; {why}"), false),
                        Err(not) => (cause_class(e), format!("{e}; {not}"), true),
                    }
                }
                Err(e) => (cause_class(e), e.clone(), false),
            };
            if failed {
                patch_failures.push(format!("{run} #{} {callee:08x}: {detail}", entry.sequence));
            }
            if verdict == "blocked" {
                blocked.push(format!("{run} #{} {callee:08x}: {detail}", entry.sequence));
            }
            *table.entry((class.clone(), verdict.clone())).or_default() += 1;
            let _ = writeln!(
                report,
                "{run}/module{module}\t{}\t{callee:08x}\t{class}\t{verdict}\t{}",
                entry.sequence,
                detail.replace(['\t', '\n'], " ")
            );
        }
    }
    panic::set_hook(hook);
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("jp-captures.tsv");
    std::fs::write(&out, &report).expect("report");
    println!(
        "captures: {} index files under {}",
        indexes.len(),
        root().display()
    );
    for ((class, verdict), n) in &table {
        println!("  {n:5}  {class:16} {verdict}");
    }
    for (what, n) in &unclaimed {
        println!("  {n:5}  not claimed: {what}");
    }
    for b in &blocked {
        println!("captures: blocked: {b}");
    }
    for s in &stale {
        println!("captures: proof::BLOCKED lists a routine whose captures pass: {s}");
    }
    println!("captures: per-call results in {}", out.display());
    let passes: usize = table
        .iter()
        .filter(|((_, v), _)| v == "pass")
        .map(|(_, n)| n)
        .sum();
    assert!(passes > 0, "no captured call reproduced");
    assert!(
        patch_failures.is_empty(),
        "patch-layer routines that miss their captures: {patch_failures:#?}"
    );
}
