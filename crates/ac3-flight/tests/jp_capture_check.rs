//! JP reference captures load into `ac3_flight::snapshot` like PAL ones:
//! address-independent PsyQ routines (identical JP and PAL instructions,
//! translated in `sdk.rs`) reproduce the JP original's return snapshot
//! (all RAM outside the stack, scratchpad, GTE, v0) from its entry snapshot.
//! Captures: `~/ac3-jp/analysis/redux/<run>-002/snapshots-module0.jsonl`
//! (tools/jp/capture_runs.py --pass snap), root overridable with
//! `AC3_JP_CAPTURES`.
use ac3_flight::{Machine, sdk, snapshot};
use std::path::PathBuf;

type Native = fn(&mut Machine, &snapshot::Snapshot) -> Option<u32>;

fn root() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME")).join("ac3-jp/analysis/redux")
        })
}

#[test]
#[ignore = "requires the JP Redux captures (AC3_JP_DIR)"]
fn jp_snapshots_reproduce_address_independent_sdk_routines() {
    let cases: &[(&str, u32, Native)] = &[
        ("memcpy", 0x800a_dcd8, |m, e| {
            Some(sdk::memcpy(m, e.gpr(4), e.gpr(5), e.gpr(6)))
        }),
        ("memset", 0x800a_dd18, |m, e| {
            Some(sdk::memset(m, e.gpr(4), e.gpr(5), e.gpr(6)))
        }),
        ("CatPrim", 0x800a_c728, |m, e| {
            sdk::cat_prim(m, e.gpr(4), e.gpr(5));
            None
        }),
        ("SetDrawTPage", 0x800a_c758, |m, e| {
            sdk::set_draw_tpage(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7));
            None
        }),
        ("MulMatrix", 0x800a_6c58, |m, e| {
            Some(sdk::mul_matrix(m, e.gpr(4), e.gpr(5)))
        }),
        ("MulMatrix2", 0x800a_6d68, |m, e| {
            Some(sdk::mul_matrix2(m, e.gpr(4), e.gpr(5)))
        }),
        ("TransposeMatrix", 0x800a_7448, |m, e| {
            Some(sdk::transpose_matrix(m, e.gpr(4), e.gpr(5)))
        }),
        ("ApplyMatrixSV", 0x800a_6e78, |m, e| {
            Some(sdk::apply_matrix_sv(m, e.gpr(4), e.gpr(5), e.gpr(6)))
        }),
        ("ApplyMatrixLV", 0x800a_68a8, |m, e| {
            Some(sdk::apply_matrix_lv(m, e.gpr(4), e.gpr(5), e.gpr(6)))
        }),
        ("RotTransPers", 0x800a_7358, |m, e| {
            Some(sdk::rot_trans_pers(
                m,
                e.gpr(4),
                e.gpr(5),
                e.gpr(6),
                e.gpr(7),
            ))
        }),
        ("RotTrans", 0x800a_73e8, |m, e| {
            Some(sdk::rot_trans(m, e.gpr(4), e.gpr(5), e.gpr(6)))
        }),
        ("SetRotMatrix", 0x800a_7048, |m, e| {
            sdk::set_rot_matrix(m, e.gpr(4));
            None
        }),
        ("SetTransMatrix", 0x800a_70a8, |m, e| {
            sdk::set_trans_matrix(m, e.gpr(4));
            None
        }),
        ("SetColorMatrix", 0x800a_7078, |m, e| {
            sdk::set_color_matrix(m, e.gpr(4));
            None
        }),
    ];
    let mut passed = Vec::new();
    for run in ["boot-002", "story-002", "flight-002", "save-002"] {
        let index = root().join(run).join("snapshots-module0.jsonl");
        if !index.exists() {
            continue;
        }
        for &(name, address, native) in cases {
            for occurrence in 0..2 {
                match snapshot::check_call(&index, address, occurrence, native) {
                    Ok(bytes) => passed.push(format!("{run} {name}#{occurrence} ({bytes} B)")),
                    Err(e) if e.contains("no entry snapshot") => break,
                    Err(e) => panic!("{run} {name}#{occurrence}: {e}"),
                }
            }
        }
    }
    eprintln!("{} JP snapshot pairs reproduced: {passed:?}", passed.len());
    assert!(
        !passed.is_empty(),
        "no JP snapshot pairs found under {:?}",
        root()
    );
}
