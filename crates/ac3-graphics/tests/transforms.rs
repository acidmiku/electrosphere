use ac3_graphics::transform::{SinCosTable, rotate_translate};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires original isolated RTPS/RTPT oracle"]
fn native_projection_matches_original_cpu_and_cop2() {
    check_projection_file(
        "analysis/transform-runtime/projection-001/projection-vectors.json",
        "projection-validation.json",
        (196, 85),
        true,
    );
}

#[test]
#[ignore = "requires natural mission hub projection observations"]
fn native_projection_matches_running_hub() {
    check_projection_file(
        "analysis/menu-runtime/hub-projection-003/projection-map.json",
        "map-projection-validation.json",
        (71, 115),
        false,
    );
}

fn check_projection_file(source: &str, report: &str, counts: (usize, usize), isolated: bool) {
    use ac3_graphics::projection::{ProjectionControls, project, project_three};
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let path = root.join(source);
    let oracle: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut singles = 0;
    let mut triplets = 0;
    for case in oracle["cases"].as_array().unwrap() {
        if isolated {
            assert_eq!(case["ram_and_registers_restored"], true);
            assert_eq!(case["non_ram_accesses"], 0);
        }
        let matrix = std::array::from_fn(|r| {
            std::array::from_fn(|c| case["matrix"][r * 3 + c].as_i64().unwrap() as i16)
        });
        let translation = std::array::from_fn(|i| case["translation"][i].as_i64().unwrap() as i32);
        let p = &case["projection"];
        let controls = ProjectionControls {
            offset: [
                p["ofx"].as_i64().unwrap() as i32,
                p["ofy"].as_i64().unwrap() as i32,
            ],
            distance: p["h"].as_u64().unwrap() as u16,
            depth_scale: p["dqa"].as_i64().unwrap() as i16,
            depth_offset: p["dqb"].as_i64().unwrap() as i32,
        };
        let (screen, ir0, flags, depth, mac, ir, mac0) = match case["kind"].as_str().unwrap() {
            "rot_trans_pers" | "rtps" => {
                let vector =
                    std::array::from_fn(|i| case["vectors"][0][i].as_i64().unwrap() as i16);
                let result = project(matrix, translation, controls, vector);
                singles += 1;
                (
                    serde_json::json!(result.screen),
                    result.ir0,
                    result.flags,
                    result.depth[0],
                    result.last_vector,
                    result.last_ir,
                    result.mac0,
                )
            }
            "rot_trans_pers3" | "rtpt" => {
                let vectors = std::array::from_fn(|i| {
                    std::array::from_fn(|j| case["vectors"][i][j].as_i64().unwrap() as i16)
                });
                let result = project_three(matrix, translation, controls, vectors);
                triplets += 1;
                (
                    serde_json::json!(result.screen),
                    result.ir0,
                    result.flags,
                    result.depth[2],
                    result.last_vector,
                    result.last_ir,
                    result.mac0,
                )
            }
            other => panic!("unknown case {other}"),
        };
        assert_eq!(screen, case["expected_sxy"], "{} screen", case["name"]);
        assert_eq!(
            serde_json::json!(ir0),
            case["expected_p"],
            "{} depth cue",
            case["name"]
        );
        assert_eq!(
            serde_json::json!(flags),
            case["expected_flag"],
            "{} flags",
            case["name"]
        );
        assert_eq!(
            serde_json::json!(depth >> 2),
            case["expected_return"],
            "{} depth return",
            case["name"]
        );
        if !isolated {
            let data = &case["expected_data"];
            for i in 0..3 {
                assert_eq!(
                    mac[i],
                    data[25 + i].as_u64().unwrap() as i32,
                    "{} MAC{}",
                    case["name"],
                    i + 1
                );
                assert_eq!(
                    ir[i],
                    data[9 + i].as_u64().unwrap() as i16,
                    "{} IR{}",
                    case["name"],
                    i + 1
                );
            }
            assert_eq!(
                mac0,
                data[24].as_u64().unwrap() as i32,
                "{} MAC0",
                case["name"]
            );
        }
    }
    assert_eq!((singles, triplets), counts);
    fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
    fs::write(root.join("analysis/native-mission").join(report),serde_json::to_vec_pretty(&serde_json::json!({"passed":true,"single_cases":singles,"triplet_cases":triplets,"source":source,"isolated_leaf":isolated,"scope":"Original RTPS/RTPT screen, depth, depth cue and flags; natural cases additionally compare MAC/IR. Map command generation remains separate."})).unwrap()).unwrap();
}

#[test]
fn rotation_translation_overflow_and_ir_saturation_are_distinct() {
    let identity = [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
    let result = rotate_translate(identity, [i32::MAX, i32::MIN, 0], [1, -1, 123]);
    assert_eq!(result.vector, [i32::MIN, i32::MAX, 123]);
    assert_eq!(result.saturated, [-32768, 32767, 123]);
    assert_eq!(result.flags, 0xc5800000);
    let z = rotate_translate(identity, [0, 0, 65536], [0; 3]);
    assert_eq!(z.flags, 1 << 22); // IR3 saturation alone does not set ERROR.
    assert_eq!(z.vector, [0, 0, 65536]);
    assert_eq!(z.saturated, [0, 0, 32767]);
}

#[test]
#[ignore = "requires original isolated CPU/COP2 transform oracle"]
fn native_transforms_match_original_cpu_and_cop2() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/transform-runtime/oracle-002");
    let table =
        SinCosTable::from_le_bytes(&fs::read(capture.join("sincos-table.bin")).unwrap()).unwrap();
    let oracle: serde_json::Value =
        serde_json::from_slice(&fs::read(capture.join("transform-vectors.json")).unwrap()).unwrap();
    let mut matrix_cases = 0;
    let mut transform_cases = 0;
    for case in oracle["cases"].as_array().unwrap() {
        assert_eq!(case["ram_and_registers_restored"], true);
        assert_eq!(case["non_ram_accesses"], 0);
        if case["kind"] == "rot_matrix" {
            let angles = std::array::from_fn(|i| case["angles"][i].as_i64().unwrap() as i16);
            let actual: Vec<i16> = table.rotation(angles).into_iter().flatten().collect();
            assert_eq!(
                serde_json::json!(actual),
                case["expected_matrix"],
                "{}",
                case["name"]
            );
            matrix_cases += 1;
        } else {
            let matrix = std::array::from_fn(|r| {
                std::array::from_fn(|c| case["matrix"][r * 3 + c].as_i64().unwrap() as i16)
            });
            let translation =
                std::array::from_fn(|i| case["translation"][i].as_i64().unwrap() as i32);
            let vector = std::array::from_fn(|i| case["vector"][i].as_i64().unwrap() as i16);
            let actual = rotate_translate(matrix, translation, vector);
            assert_eq!(
                serde_json::json!(actual.vector),
                case["expected_vector"],
                "{}",
                case["name"]
            );
            assert_eq!(
                serde_json::json!(actual.flags),
                case["expected_flag"],
                "{}",
                case["name"]
            );
            transform_cases += 1;
        }
    }
    assert_eq!((matrix_cases, transform_cases), (72, 64));
    fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
    fs::write(root.join("analysis/native-mission/transform-validation.json"),serde_json::to_vec_pretty(&serde_json::json!({"passed":true,"matrix_cases":matrix_cases,"transform_cases":transform_cases,"source":"analysis/transform-runtime/oracle-002/transform-vectors.json","scope":"Native fixed point matrix/transform results and flags; perspective/lighting remain separate."})).unwrap()).unwrap();
}
