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

pub mod ai;
pub mod arena;
pub mod bios;
pub mod boot;
pub mod cdstream;
pub mod commands;
pub mod common;
pub mod dispatch;
pub mod effects;
pub mod entities;
#[cfg(feature = "dev-fallback")]
pub mod fallback;
pub mod frame;
pub mod game;
pub mod hud;
pub mod init;
pub mod init_38818;
pub mod init_39a68;
pub mod init_objects;
pub mod init_tail;
pub mod loading;
pub mod machine;
pub mod mainloop;
pub mod ov1;
pub mod ov1_card;
pub mod ov1_load;
pub mod ov1_menu;
pub mod ov1_script;
pub mod ov1_text;
pub mod ov2_aircraft;
pub mod ov2_aircraft_draw;
pub mod ov2_briefing;
pub mod ov2_briefing_scene;
pub mod ov2_core;
pub mod ov2_hub;
pub mod ov2_misc;
pub mod ov2_misc_records;
pub mod ov2_misc_settings;
pub mod ov2_options;
pub mod ov3;
pub mod ov4;
pub mod ov5;
pub mod player;
pub mod render;
pub mod replay;
pub mod script;
pub mod sdk;
pub mod session;
pub mod shared;
pub mod states;
pub mod terrain;
pub mod widescreen;

pub use ac3_machine::{host_ids, precision, snapshot};
pub use machine::{Device, Fault, GP, HostCall, Machine, SmallData};

/// Whether this build contains the development interpreter fallback
/// (feature `dev-fallback`). The shipped `ac3` binary asserts at compile
/// time that it does not.
pub const DEV_FALLBACK: bool = cfg!(feature = "dev-fallback");
