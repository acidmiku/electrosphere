//! Deliberately opt-in: the source repository does not include game assets.
use ac3_port::{assets::GameData, validation};
use std::path::PathBuf;

fn root() -> PathBuf {
    std::env::var_os("AC3_DATA_DIR")
        .map(PathBuf::from)
        .expect("set AC3_DATA_DIR to the recovery workspace")
}

#[test]
#[ignore = "requires the user's extracted PAL disc and Redux captures"]
fn rust_loader_and_native_bootstrap_match_all_original_captures() {
    let root = root();
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let report = validation::verify_captures(&data, &root.join("analysis/runtime")).unwrap();
    assert_eq!(report["passed"], true, "{report:#}");
}

#[test]
#[ignore = "requires original MIPS oracle vectors captured by Redux"]
fn native_dispatcher_matches_original_mips_cases() {
    let root = root();
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let report =
        validation::verify_oracle(&data, &root.join("analysis/rust-oracle/state-vectors.json"))
            .unwrap();
    assert_eq!(report["passed"], true, "{report:#}");
}
