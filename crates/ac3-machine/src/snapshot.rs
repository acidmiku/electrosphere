//! Original-machine snapshots for validating translations (test fixtures only;
//! the player never loads them). A snapshot pairs 2 MiB RAM, the scratchpad
//! and CPU/GTE registers, as written by `tools/redux-init-callees.lua` and
//! later function-snapshot observers.
use crate::machine::{Machine, RAM_BYTES, SCRATCH_BYTES};
use serde_json::Value;
use std::{fs, io, path::Path};

/// RAM excluded from comparisons: the guest stack (`machine::STACK_LIMIT` up).
pub const STACK_START: usize = 0x1f_c000;

#[derive(Clone, Debug)]
pub struct Registers {
    pub gpr: [u32; 32],
    pub cp2c: [u32; 32],
    pub cp2d: [u32; 32],
}

#[derive(Clone, Debug)]
pub struct SnapshotInfo {
    pub sequence: u64,
    pub stage: String,
    pub callee: u32,
    pub ra: u32,
    pub frame: u64,
    pub stem: String,
    pub registers: Registers,
}

fn words(v: &Value) -> io::Result<[u32; 32]> {
    let list = v
        .as_array()
        .filter(|a| a.len() == 32)
        .ok_or_else(|| io::Error::other("register list"))?;
    let mut out = [0; 32];
    for (slot, item) in out.iter_mut().zip(list) {
        *slot = match item {
            Value::String(s) => s.parse::<u64>().map_err(io::Error::other)? as u32,
            other => other.as_u64().ok_or_else(|| io::Error::other("register"))? as u32,
        };
    }
    Ok(out)
}

pub fn read_index(path: &Path) -> io::Result<Vec<SnapshotInfo>> {
    let text = fs::read_to_string(path)?;
    text.lines()
        .map(|line| {
            let v: Value = serde_json::from_str(line).map_err(io::Error::other)?;
            let int = |name: &str| {
                v[name]
                    .as_u64()
                    .ok_or_else(|| io::Error::other(name.to_owned()))
            };
            Ok(SnapshotInfo {
                sequence: int("sequence")?,
                stage: v["stage"].as_str().unwrap_or_default().to_owned(),
                callee: int("callee")? as u32,
                ra: int("ra")? as u32,
                frame: int("frame")?,
                stem: v["stem"].as_str().unwrap_or_default().to_owned(),
                registers: Registers {
                    gpr: words(&v["gpr"])?,
                    cp2c: words(&v["cp2c"])?,
                    cp2d: words(&v["cp2d"])?,
                },
            })
        })
        .collect()
}

#[derive(Clone)]
pub struct Snapshot {
    pub info: SnapshotInfo,
    pub ram: Vec<u8>,
    pub scratch: Vec<u8>,
}

impl Snapshot {
    pub fn load(directory: &Path, info: &SnapshotInfo) -> io::Result<Self> {
        let ram = fs::read(directory.join(format!("{}.ram.bin", info.stem)))?;
        let scratch = fs::read(directory.join(format!("{}.scratch.bin", info.stem)))?;
        if ram.len() != RAM_BYTES || scratch.len() != SCRATCH_BYTES {
            return Err(io::Error::other("snapshot image size"));
        }
        Ok(Self {
            info: info.clone(),
            ram,
            scratch,
        })
    }
    pub fn machine(&self) -> Machine {
        let mut m = Machine::from_images(&self.ram, &self.scratch).expect("checked sizes");
        m.gte.data = self.info.registers.cp2d;
        m.gte.control = self.info.registers.cp2c;
        m.sp = self.info.registers.gpr[29];
        m
    }
    pub fn gpr(&self, index: usize) -> u32 {
        self.info.registers.gpr[index]
    }
}

#[derive(Debug, Default)]
pub struct Differences {
    pub ram: Vec<u32>,
    pub scratch: Vec<u32>,
    pub gte_data: Vec<usize>,
    pub gte_control: Vec<usize>,
}
impl Differences {
    pub fn is_empty(&self) -> bool {
        self.ram.is_empty()
            && self.scratch.is_empty()
            && self.gte_data.is_empty()
            && self.gte_control.is_empty()
    }
    pub fn summary(&self) -> String {
        let hex = |v: &[u32]| {
            v.iter()
                .take(16)
                .map(|a| format!("{a:08x}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!(
            "ram {} [{}] scratch {} [{}] gte data {:?} control {:?}",
            self.ram.len(),
            hex(&self.ram),
            self.scratch.len(),
            hex(&self.scratch),
            self.gte_data,
            self.gte_control
        )
    }
}

/// Compare complete machine-visible state outside the stack.
pub fn compare(actual: &Machine, expected: &Snapshot) -> Differences {
    Differences {
        ram: (0..STACK_START)
            .filter(|&i| actual.ram()[i] != expected.ram[i])
            .map(|i| 0x8000_0000 + i as u32)
            .collect(),
        scratch: (0..SCRATCH_BYTES)
            .filter(|&i| actual.scratch()[i] != expected.scratch[i])
            .map(|i| 0x1f80_0000 + i as u32)
            .collect(),
        gte_data: (0..32)
            .filter(|&i| actual.gte.data[i] != expected.info.registers.cp2d[i])
            .collect(),
        gte_control: (0..32)
            .filter(|&i| actual.gte.control[i] != expected.info.registers.cp2c[i])
            .collect(),
    }
}

/// Bytes the original changed outside the stack (to show a test is not vacuous).
pub fn changed_bytes(entry: &Snapshot, exit: &Snapshot) -> usize {
    (0..STACK_START)
        .filter(|&i| entry.ram[i] != exit.ram[i])
        .count()
        + (0..SCRATCH_BYTES)
            .filter(|&i| entry.scratch[i] != exit.scratch[i])
            .count()
}

/// Run `native` on the `occurrence`th (0-based) entry snapshot of `callee` in
/// `index_file`'s directory and require the matching return snapshot: all RAM
/// outside the stack, scratchpad, GTE registers, and `v0` when `native`
/// returns one. Returns the number of bytes the original changed.
pub fn check_call(
    index_file: &Path,
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &Snapshot) -> Option<u32>,
) -> Result<usize, String> {
    let directory = index_file.parent().ok_or("index directory")?;
    let index = read_index(index_file).map_err(|e| e.to_string())?;
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .ok_or_else(|| format!("{callee:08x}: no entry snapshot #{occurrence}"))?;
    // The matching return is the first later return of this callee with the
    // same return address and stack pointer.
    let exit = index
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .ok_or_else(|| format!("{callee:08x}: no return snapshot"))?;
    let entry = Snapshot::load(directory, entry).map_err(|e| e.to_string())?;
    let exit = Snapshot::load(directory, exit).map_err(|e| e.to_string())?;
    let mut machine = entry.machine();
    let result = native(&mut machine, &entry);
    machine
        .take_fault()
        .map_err(|f| format!("{callee:08x}: {f}"))?;
    let differences = compare(&machine, &exit);
    if !differences.is_empty() {
        return Err(format!("{callee:08x}: {}", differences.summary()));
    }
    if let Some(v0) = result
        && v0 != exit.gpr(2)
    {
        return Err(format!(
            "{callee:08x}: v0 {v0:#010x}, original {:#010x}",
            exit.gpr(2)
        ));
    }
    Ok(changed_bytes(&entry, &exit))
}
