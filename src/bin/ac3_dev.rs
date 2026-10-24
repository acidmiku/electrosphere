//! `ac3-dev`: the `ac3` command line built with the development interpreter
//! fallback (cargo feature `dev-fallback`, crate `ac3-fallback`), for the
//! Japanese port. Never shipped:
//! `cargo build --release --features dev-fallback --bin ac3-dev`.
//!
//! `machine` runs as with `ac3`, plus: a routine nobody translated runs
//! interpreted instead of stopping the game with `Fault::Untranslated`;
//! `--interpret LIST` forces translated routines through the interpreter too
//! (see `ac3_fallback::Fallback::force`); the `--report` JSON gains a
//! `fallback` section (the reach counter: the translation queue).
#[path = "../main.rs"]
mod cli;

fn main() -> std::process::ExitCode {
    cli::main()
}
