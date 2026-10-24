//! Translated resident flight engine (state 2) over an address-preserving
//! machine model: the PAL game's code at PAL addresses. See
//! `docs/FLIGHT_MACHINE.md` for the design and the per-function snapshot
//! validation rules.
//!
//! The machine model and everything version-independent (the snapshot
//! framework, the precision table, host-call ids, device models, the
//! widescreen machinery, the [`Game`](ac3_machine::Game) interface) are
//! `ac3_machine`, re-exported here under the paths the translated modules
//! use (`crate::machine`, `crate::precision`, `crate::snapshot`, ...);
//! [`game::GAME`] is this crate's [`Game`](ac3_machine::Game).
#![forbid(unsafe_code)]

// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod ai;
pub mod arena;
pub mod bios;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod boot;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod cdstream;
pub mod commands;
pub mod common;
pub mod dispatch;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod effects;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod entities;
#[cfg(feature = "dev-fallback")]
pub mod fallback;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod frame;
pub mod game;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod hud;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod init;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod init_38818;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod init_39a68;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod init_objects;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod init_tail;
pub mod jp;
pub mod jp_review;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod loading;
pub mod machine;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod mainloop;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod player;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod render;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod replay;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod script;
pub mod sdk;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod session;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod shared;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod states;
// JP-PATCH: helpers of functions the patch layer replaces (src/jp/mod.rs REPLACES)
#[allow(dead_code, unused_imports)]
pub mod terrain;
pub mod widescreen;

pub use ac3_machine::{host_ids, precision, snapshot};
pub use machine::{Device, Fault, GP, HostCall, Machine, SmallData};

/// Whether this build contains the development interpreter fallback
/// (feature `dev-fallback`). The shipped `ac3` binary asserts at compile
/// time that it does not.
pub const DEV_FALLBACK: bool = cfg!(feature = "dev-fallback");
