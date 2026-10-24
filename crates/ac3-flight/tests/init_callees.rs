//! Resident gameplay initialization 80032510 on original snapshots: its
//! direct callees and the inline spans between them (`init-callees-001`,
//! entry/return snapshots of all 21 direct callees) and the whole function
//! (`initialization-002` stage snapshots, RAM only).
//!
//! Blocked tests carry the missing routine in their name and fail until it
//! is delivered: `_needs_script` (80053704), `_needs_loading` (800354B8),
//! `_needs_sdk` (`sdk::divdi3` 8009CE80, `sdk::rot_matrix_yxz` 800A5BE8) and
//! `_needs_subsystems` (80038818, and 80039A68 claimed in `dispatch`).
use ac3_flight::{
    Machine, dispatch, init,
    snapshot::{self, STACK_START, Snapshot},
};
use std::path::{Path, PathBuf};

fn data(relative: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR")).join(relative)
}
fn index() -> PathBuf {
    data("analysis/flight-runtime/init-callees-001/callees.jsonl")
}
fn stages() -> PathBuf {
    data("analysis/menu-runtime/initialization-002")
}

/// The `occurrence`th `stage` snapshot of `callee`.
fn snap(index: &Path, callee: u32, stage: &str) -> Snapshot {
    let records = snapshot::read_index(index).unwrap();
    let info = records
        .iter()
        .find(|r| r.callee == callee && r.stage == stage)
        .unwrap_or_else(|| panic!("{callee:08x} {stage}"));
    Snapshot::load(index.parent().unwrap(), info).unwrap()
}

/// Run `native` from the `from` snapshot and require the `to` snapshot
/// exactly (RAM outside the stack, scratchpad, GTE, and `v0` when returned).
/// Returns the bytes the original changed.
fn check_span(
    from: (u32, &str),
    to: (u32, &str),
    native: impl FnOnce(&mut Machine, &Snapshot) -> Option<u32>,
) -> usize {
    let entry = snap(&index(), from.0, from.1);
    let exit = snap(&index(), to.0, to.1);
    let mut m = entry.machine();
    let v0 = native(&mut m, &entry);
    if let Err(fault) = m.take_fault() {
        panic!("{fault}");
    }
    let differences = snapshot::compare(&m, &exit);
    assert!(differences.is_empty(), "{}", differences.summary());
    if let Some(v0) = v0 {
        assert_eq!(v0, exit.gpr(2), "v0 {v0:#010x}");
    }
    snapshot::changed_bytes(&entry, &exit)
}

fn stage_ram(name: &str) -> Vec<u8> {
    std::fs::read(stages().join(format!("initialization-{name}.ram.bin"))).unwrap()
}

/// `result` (v0) recorded with stage snapshot `name`.
fn stage_result(name: &str) -> u32 {
    let file = format!("initialization-{name}.ram.bin");
    let text = std::fs::read_to_string(stages().join("initialization.jsonl")).unwrap();
    let record = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|v| v["ram_file"] == file.as_str())
        .unwrap();
    record["result"].as_u64().unwrap() as u32
}

/// RAM-only stage check: run `native` on stage `from` and require stage `to`
/// outside the stack. Returns the bytes the original changed.
fn check_stages(from: &str, to: &str, native: impl FnOnce(&mut Machine) -> Option<u32>) -> usize {
    let entry = stage_ram(from);
    let exit = stage_ram(to);
    let mut m = Machine::from_images(&entry, &[0; 0x400]).unwrap();
    let v0 = native(&mut m);
    if let Err(fault) = m.take_fault() {
        panic!("{fault}");
    }
    let differing: Vec<String> = (0..STACK_START)
        .filter(|&i| m.ram()[i] != exit[i])
        .map(|i| format!("{:08x}", 0x8000_0000 + i))
        .collect();
    assert!(
        differing.is_empty(),
        "ram {} [{}]",
        differing.len(),
        differing[..differing.len().min(16)].join(" ")
    );
    if let Some(v0) = v0 {
        assert_eq!(v0, stage_result(to), "v0");
    }
    (0..STACK_START).filter(|&i| entry[i] != exit[i]).count()
}

// ---- 80032510 piece by piece (init-callees-001) ----

/// 80032548..800327DC: from the 800354B8 return to the 80076210 entry,
/// including the camera arguments (the fifth on the stack).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn flight_state_80032548() {
    let camera = snap(&index(), 0x8007_6210, "entry");
    let changed = check_span((0x8003_54b8, "return"), (0x8007_6210, "entry"), |m, _| {
        let arguments = init::begin_flight_state(m);
        let stacked = u32::from_le_bytes(
            camera.ram[(camera.gpr(29) + 0x10 - 0x8000_0000) as usize..][..4]
                .try_into()
                .unwrap(),
        );
        let original = [
            camera.gpr(4),
            camera.gpr(5),
            camera.gpr(6),
            camera.gpr(7),
            stacked,
        ];
        assert_eq!(arguments, original, "80076210 arguments");
        None
    });
    assert!(changed > 100, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn camera_80076210() {
    let changed = snapshot::check_call(&index(), 0x8007_6210, 0, |m, entry| {
        let aircraft = m.lw(entry.gpr(29) + 0x10);
        Some(init::initialize_camera(
            m,
            entry.gpr(4),
            entry.gpr(5),
            entry.gpr(6),
            entry.gpr(7),
            aircraft,
        ))
    })
    .unwrap();
    assert!(changed > 100, "{changed}");
}

/// The same through the dispatcher (fifth argument from the guest stack).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn camera_80076210_by_address() {
    snapshot::check_call(&index(), 0x8007_6210, 0, |m, entry| {
        let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
        init::dispatch(m, 0x8007_6210, args)
    })
    .unwrap();
}

/// 800327E8..80032848: from the 80076210 return to the 8005D0C4 entry
/// (offsets, SetGeomOffset/SetGeomScreen; GTE compared), including the
/// 8005D0C4 arguments.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn view_offsets_800327e8() {
    let cue = snap(&index(), 0x8005_d0c4, "entry");
    let changed = check_span((0x8007_6210, "return"), (0x8005_d0c4, "entry"), |m, _| {
        init::reset_view_offsets(m);
        let arguments = init::depth_cue_arguments(m);
        assert_eq!(arguments, [cue.gpr(4), cue.gpr(5), cue.gpr(6)]);
        None
    });
    assert!(changed > 0, "{changed}");
}

/// 80032548..80032848 composed: prelude state, camera and view offsets.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn flight_state_camera_view_composed() {
    let changed = check_span((0x8003_54b8, "return"), (0x8005_d0c4, "entry"), |m, _| {
        let [camera, projection, near, far, aircraft] = init::begin_flight_state(m);
        init::initialize_camera(m, camera, projection, near, far, aircraft);
        init::reset_view_offsets(m);
        None
    });
    assert!(changed > 100, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001; blocked on sdk::divdi3"]
fn depth_cue_8005d0c4_needs_sdk() {
    let changed = snapshot::check_call(&index(), 0x8005_d0c4, 0, |m, entry| {
        Some(init::set_depth_cue(
            m,
            entry.gpr(4),
            entry.gpr(5),
            entry.gpr(6),
        ))
    })
    .unwrap();
    assert_eq!(changed, 0, "GTE only");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn depth_buckets_8005d390() {
    let changed = snapshot::check_call(&index(), 0x8005_d390, 0, |m, entry| {
        Some(init::depth_buckets(
            m,
            entry.gpr(4),
            entry.gpr(5),
            entry.gpr(6),
        ))
    })
    .unwrap();
    assert!(changed > 20, "{changed}");
}

/// 80032850..80032874: allocation and 8005D390, from the 8005D0C4 return.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn depth_table_80032850() {
    let changed = check_span((0x8005_d0c4, "return"), (0x8005_d390, "return"), |m, _| {
        init::allocate_depth_buckets(m);
        None
    });
    assert!(changed > 20, "{changed}");
}

/// 80032878..8003298C: lighting, from the 8005D390 return to the 8004AF9C
/// entry; the frame is the captured one.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001; blocked on sdk::rot_matrix_yxz"]
fn light_vectors_80032878_needs_sdk() {
    let changed = check_span(
        (0x8005_d390, "return"),
        (0x8004_af9c, "entry"),
        |m, entry| {
            init::set_light_vectors(m, entry.gpr(29));
            None
        },
    );
    assert!(changed > 20, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn primitive_buffers_8004af9c() {
    let changed = snapshot::check_call(&index(), 0x8004af9c, 0, |m, _| {
        init::primitive_buffers(m);
        None
    })
    .unwrap();
    assert!(changed > 300);
}

/// 800329A0..80032A34: the frame table copy and entity 0 bytes, from the
/// 80038818 return to the 80039A68 entry.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn player_aircraft_800329a0() {
    let changed = check_span(
        (0x8003_8818, "return"),
        (0x8003_9a68, "entry"),
        |m, entry| {
            init::select_player_aircraft(m, entry.gpr(29));
            None
        },
    );
    assert!(changed > 0, "{changed}");
}

/// 80032A38..80032A68 by address: 8007C41C, 80083414, 8006ADB8, 8006B6A0
/// (entity 0) and 8004C1A0(0), from the 80039A68 return to the 8004C1A0
/// return (routing to `init_objects`).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn object_initializers_80032a38() {
    let changed = check_span((0x8003_9a68, "return"), (0x8004_c1a0, "return"), |m, _| {
        for subsystem in [0x8007_c41c, 0x8008_3414, 0x8006_adb8] {
            dispatch::call(m, subsystem, [0; 4]);
        }
        let table = m.lw(ac3_flight::GP + 0x2d8);
        let player = m.lw(table);
        dispatch::call(m, 0x8006_b6a0, [player, 0, 0, 0]);
        dispatch::call(m, 0x8004_c1a0, [0; 4]);
        None
    });
    assert!(changed > 100, "{changed}");
}

// ---- Whole function (initialization-002, RAM only) ----

/// 80032548..800327E8: prefix and camera, stage 02 to stage 05.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/menu-runtime/initialization-002"]
fn flight_state_camera_stages_02_05() {
    let changed = check_stages("02-after_loading_poll", "05-camera_return", |m| {
        let [camera, projection, near, far, aircraft] = init::begin_flight_state(m);
        Some(init::initialize_camera(
            m, camera, projection, near, far, aircraft,
        ))
    });
    assert!(changed > 100, "{changed}");
}

/// 80032510 from its entry (stage 01) to its return (stage 09).
/// The whole 80032510 on the complete original entry snapshot (RAM,
/// scratchpad, all CPU/GTE registers) of `whole-init-001`, against its return
/// snapshot: every byte outside the stack, scratchpad, GTE and v0.
/// (The RAM-only stage-01..09 test below differs in 4 bytes only because
/// those stage snapshots carry no GTE registers for the inherited state.)
#[test]
#[ignore = "requires AC3_DATA_DIR and whole-init-001"]
fn gameplay_initialization_whole_80032510() {
    let changed = snapshot::check_call(
        &data("analysis/flight-runtime/whole-init-001/snapshots.jsonl"),
        0x8003_2510,
        0,
        |m, _| Some(init::gameplay_initialization(m)),
    )
    .unwrap();
    assert!(changed > 20_000, "{changed}");
}

/// Bytes the RAM-only stage images cannot reproduce: stage 01 has no GTE
/// registers, and 80032510 stores these two halfwords from GTE state set
/// before it ran. The full-snapshot run above (`whole-init-001`, GTE
/// included) matches them exactly.
const GTE_DEPENDENT: [usize; 4] = [0x1614ca, 0x1614cb, 0x1614f2, 0x1614f3];

/// [`check_stages`] for 01..09, allowing exactly [`GTE_DEPENDENT`].
fn check_whole_stages(native: impl FnOnce(&mut Machine) -> Option<u32>) -> usize {
    let entry = stage_ram("01-entry");
    let mut exit = stage_ram("09-return");
    let mut m = Machine::from_images(&entry, &[0; 0x400]).unwrap();
    let v0 = native(&mut m);
    m.take_fault().unwrap();
    for i in GTE_DEPENDENT {
        exit[i] = m.ram()[i];
    }
    let differing: Vec<String> = (0..STACK_START)
        .filter(|&i| m.ram()[i] != exit[i])
        .map(|i| format!("{:08x}", 0x8000_0000 + i))
        .collect();
    assert!(
        differing.is_empty(),
        "ram {} [{}]",
        differing.len(),
        differing[..differing.len().min(16)].join(" ")
    );
    if let Some(v0) = v0 {
        assert_eq!(v0, stage_result("09-return"), "v0");
    }
    (0..STACK_START).filter(|&i| entry[i] != exit[i]).count()
}

#[test]
#[ignore = "requires AC3_DATA_DIR and initialization-002"]
fn gameplay_initialization_stages_01_09() {
    let changed = check_whole_stages(|m| Some(init::gameplay_initialization(m)));
    assert!(changed > 10_000, "{changed}");
}

/// The same through `init::dispatch` (as the state-2 callback).
#[test]
#[ignore = "requires AC3_DATA_DIR and initialization-002"]
fn gameplay_initialization_stages_01_09_by_address() {
    check_whole_stages(|m| init::dispatch(m, 0x8003_2510, [0; 4]));
}

// ---- Untraced paths ----

/// 80076210 in mode 2 (context byte +0x3A3 set) and 80077D54's non-follow
/// branch, on synthetic records.
#[test]
fn camera_modes() {
    let mut m = Machine::default();
    let (context, table, entity, camera, aircraft) = (
        0x8010_0000,
        0x8010_1000,
        0x8010_2000,
        0x8010_3000,
        0x8010_4000,
    );
    m.sw(ac3_flight::GP + 0x2dc, context);
    m.sw(ac3_flight::GP + 0x2d8, table);
    m.sw(table, entity);
    m.sw(entity, 0x1234_56ab);
    for (i, value) in [0x7fffu16, 0x8000, 1025].into_iter().enumerate() {
        m.sh(aircraft + 2 * i as u32, value);
    }
    for i in 0..0x160 {
        m.sb(camera + i, 0x55);
    }
    let v0 = init::initialize_camera(&mut m, camera, 0x128, 0x400b, 0x6d60, aircraft);
    assert!(m.take_fault().is_ok());
    assert_eq!(v0, 0x1234_56ab);
    assert_eq!(m.lw(camera), 0x52b);
    assert_eq!(m.lw(camera + 0x50), 1);
    assert_eq!((m.lw(camera + 0xc4), m.lw(camera + 0xc8)), (0x2b, 0x2b));
    assert_eq!(m.lw(camera + 0x34), entity + 8);
    assert_eq!(m.read_bytes(camera + 0x2c, 6), [0xc0, 0xff, 0, 0, 0x40, 0]);
    assert_eq!(m.read_bytes(camera + 0x10, 4), [0x55; 4], "hole kept");

    m.sb(context + 0x3a3, 1);
    init::initialize_camera(&mut m, camera, 0x128, 0x400b, 0x6d60, aircraft);
    assert_eq!(m.lw(camera + 0x50), 2);
    assert_eq!(m.lw(camera + 0xc4), 0xffff_ffff);
    assert_eq!(m.lw(camera + 0xc8), 0x2b);

    // Mode without 0xDFF bits: no follow, bit 0x100 cleared.
    m.sw(camera, 0x1ff);
    m.sw(camera + 0x34, 7);
    let v0 = ac3_flight::shared::set_camera_target(&mut m, camera, 0x1000, entity);
    assert_eq!(v0, 0x4ff);
    assert_eq!(m.lw(camera), 0x4ff);
    assert_eq!(m.lw(camera + 0xc4), 0xffff_ffff);
    assert_eq!(m.lw(camera + 0x34), 7);
    assert_eq!(m.lw(camera + 0x50), 0x1000);
    assert!(m.take_fault().is_ok());
}

#[test]
fn depth_buckets_edges() {
    let mut m = Machine::default();
    let out = 0x8010_0000;
    let mut run = |near: i32, far: i32| {
        init::depth_buckets(&mut m, near as u32, far as u32, out);
        (0..17)
            .map(|i| m.lh(out + 2 * i) as i32)
            .collect::<Vec<_>>()
    };
    assert_eq!(run(8, -8), [&[0; 16][..], &[4094]].concat());
    assert_eq!(run(32760, 32767), [&[4093; 16][..], &[4094]].concat());
    assert_eq!(run(100, 84)[0], 12);
    assert_eq!(run(100, 84)[15], 10);
    assert_eq!(run(i32::MAX, i32::MIN)[0], 4093);
    assert_eq!(run(i32::MAX, i32::MIN)[15], 0);
}

#[test]
fn dispatch_claims_translations() {
    for &address in &init::TRANSLATED {
        if address == 0x8003_2510 {
            continue; // the whole initializer; covered by the stage tests
        }
        let mut m = Machine::default();
        assert!(
            init::dispatch(&mut m, address, [0x8010_0000; 4]).is_some(),
            "{address:08x}"
        );
    }
    let mut m = Machine::default();
    assert_eq!(
        init::dispatch(&mut m, 0x8005_d390, [0, 0, 0x8010_0000, 0]),
        Some(0xffe)
    );
    assert_eq!(init::dispatch(&mut m, 0x8005_3704, [0; 4]), None);
}
