//! The reach counter: calls and instructions per interpreted routine, in
//! first-reach order, plus the calls the fallback refused. It is the
//! translation queue.
use crate::extents::{Extent, flags};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default)]
pub struct Counter {
    pub calls: u64,
    pub instructions: u64,
    /// First-reach position (1, 2, ...), 0 when never reached.
    pub first: u32,
}

/// A call the fallback did not run: a device routine or no routine at all.
#[derive(Clone, Copy, Debug, Default)]
pub struct Miss {
    pub calls: u64,
    pub first: u32,
    /// Extent id of a refused (device) routine.
    pub id: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct FaultNote {
    pub id: Option<usize>,
    pub address: u32,
    pub what: String,
}

#[derive(Default)]
pub struct Reach {
    pub counters: Vec<Counter>,
    order: u32,
    /// Device routines that were called, by address.
    pub blocked: BTreeMap<u32, Miss>,
    /// Addresses no extent covers, by address.
    pub unknown: BTreeMap<u32, Miss>,
    /// Calls out of interpreted code (native or interpreted callee).
    pub callouts: u64,
    /// Interpreted instructions in total.
    pub instructions: u64,
    pub hazards: u64,
    pub deepest: u32,
    pub faults: Vec<FaultNote>,
    /// Where runs went on inline into another extent (the table split a
    /// routine), with the number of runs.
    pub crossings: BTreeMap<u32, u64>,
    /// Host-call stubs run under their canonical ID: address -> (ID, calls).
    pub stubs: BTreeMap<u32, (u32, u64)>,
    /// Busy-wait iterations of interpreted code polling the VBlank counter.
    pub busy_waits: u64,
    /// Code outside every extent that ran (see `Fallback::gap`), by address.
    pub gaps: BTreeMap<u32, Counter>,
    /// Interpreted runs of explicit ranges (`Fallback::run_range`).
    pub range_runs: u64,
}

impl Reach {
    pub fn new(extents: usize) -> Self {
        Self {
            counters: vec![Counter::default(); extents],
            ..Self::default()
        }
    }
    fn next_order(&mut self) -> u32 {
        self.order += 1;
        self.order
    }
    pub fn enter(&mut self, id: usize) {
        if self.counters[id].first == 0 {
            self.counters[id].first = self.next_order();
        }
        self.counters[id].calls += 1;
    }
    pub fn leave(&mut self, id: usize, steps: u64, hazards: u64) {
        self.counters[id].instructions += steps;
        self.instructions += steps;
        self.hazards += hazards;
    }
    pub fn refuse(&mut self, address: u32, id: Option<usize>) {
        let order = self.order + 1;
        let map = if id.is_some() {
            &mut self.blocked
        } else {
            &mut self.unknown
        };
        let miss = map.entry(address).or_insert(Miss {
            calls: 0,
            first: order,
            id,
        });
        if miss.first == order {
            self.order = order;
        }
        miss.calls += 1;
    }
    pub fn fault(&mut self, id: Option<usize>, address: u32, what: String) {
        if self.faults.len() < 16 {
            self.faults.push(FaultNote { id, address, what });
        }
    }
}

pub fn hex(value: u32) -> String {
    format!("{value:08x}")
}

/// One routine of the report.
pub fn routine(extent: &Extent, module: usize, counter: &Counter, forced: bool) -> Value {
    json!({
        "address": hex(extent.start),
        "module": module_name(module),
        "name": extent.name,
        "bytes": extent.size,
        "class": extent.class.name(),
        "counterpart": (extent.counterpart != 0).then(|| hex(extent.counterpart)),
        "pal_owner": (!extent.owner.is_empty()).then_some(extent.owner),
        "sdk": extent.flags & flags::SDK != 0,
        "wide": extent.flags & flags::WIDE != 0,
        "forced": forced,
        "first": counter.first,
        "calls": counter.calls,
        "instructions": counter.instructions,
    })
}

pub fn module_name(module: usize) -> String {
    if module == 0 {
        "resident".to_owned()
    } else {
        format!("ov{module}")
    }
}
