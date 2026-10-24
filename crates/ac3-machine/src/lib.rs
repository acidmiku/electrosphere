//! The address-preserving machine model shared by every game version: RAM,
//! scratchpad and GTE ([`Machine`]), faults, host calls and their canonical
//! ids ([`host_ids`]), the version-independent device models (the CD drive,
//! the boot's disc, the controller ports), the precision side table, the
//! widescreen machinery, the snapshot test framework, and the game-version
//! interface the host drives ([`Game`]).
//!
//! Nothing here holds an address of a game executable. The translated
//! routines, at a version's addresses, live in that version's crate
//! (`ac3-flight` for PAL), which re-exports what it uses from here. See
//! `docs/FLIGHT_MACHINE.md`.
#![forbid(unsafe_code)]

pub mod cd;
pub mod disc;
pub mod game;
pub mod host_ids;
pub mod machine;
pub mod precision;
pub mod sio;
pub mod snapshot;
pub mod widescreen;

pub use game::Game;
pub use machine::{Device, Fault, HostCall, Machine, MachineState};
