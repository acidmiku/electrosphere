use ac3_graphics::scalar::ScalarMath;
use std::{fs, path::PathBuf};

fn fixture() -> (PathBuf, ScalarMath) {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let ram = fs::read(root.join("analysis/menu-runtime/mission-002/frame-00600.ram.bin")).unwrap();
    let math = ScalarMath::from_resident(&ram, 0x80000000).unwrap();
    (root, math)
}

#[test]
fn malformed_tables_are_rejected() {
    assert!(ScalarMath::from_le_bytes(&[], &[], &[]).is_err());
    assert!(ScalarMath::from_resident(&[], 0x80010000).is_err());
    assert!(ScalarMath::from_resident(&[], 0x90000000).is_err());
}

#[test]
#[ignore = "requires original resident tables and isolated scalar oracle"]
fn scalar_math_matches_original_valid_domain() {
    let (root, math) = fixture();
    let oracle: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("analysis/transform-runtime/scalar-math-001/scalar-math-vectors.json"))
            .unwrap(),
    )
    .unwrap();
    let mut counts = [0; 3];
    for case in oracle["cases"].as_array().unwrap() {
        assert_eq!(case["ram_and_registers_restored"], true);
        assert_eq!(case["non_ram_accesses"], 0);
        let args = case["args"].as_array().unwrap();
        let result = match case["kind"].as_str().unwrap() {
            "ratan2" => {
                if case["domain"] == "valid" {
                    counts[0] += 1;
                }
                math.atan2(
                    args[0].as_i64().unwrap() as i32,
                    args[1].as_i64().unwrap() as i32,
                )
            }
            "square_root0" => {
                if case["domain"] == "valid" {
                    counts[1] += 1;
                }
                math.square_root(args[0].as_i64().unwrap() as u32)
            }
            other => panic!("unknown {other}"),
        };
        if case["domain"] == "valid" {
            assert_eq!(
                result.unwrap(),
                case["expected_return"].as_i64().unwrap() as i32,
                "{}",
                case["name"]
            );
        } else {
            assert!(result.is_err(), "{} domain", case["name"]);
            counts[2] += 1;
        }
    }
    assert_eq!(counts, [124, 139, 20]);
    let camera: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("analysis/menu-runtime/briefing-camera-001/camera-math.json")).unwrap(),
    )
    .unwrap();
    for case in camera["cases"].as_array().unwrap() {
        let args = &case["args"];
        let actual = match case["kind"].as_str().unwrap() {
            "square_root0" => math.square_root(args[0].as_u64().unwrap() as u32),
            "ratan2" => math.atan2(
                args[0].as_i64().unwrap() as i32,
                args[1].as_i64().unwrap() as i32,
            ),
            _ => unreachable!(),
        };
        assert_eq!(
            actual.unwrap(),
            case["expected_return"].as_i64().unwrap() as i32
        );
    }
    let report = serde_json::json!({
        "passed":true,"atan_cases":counts[0],"square_root_cases":counts[1],
        "domain_rejections":counts[2],"normal_camera_cases":camera["cases"].as_array().unwrap().len(),
        "scope":"Original resident integer results. INT_MIN atan and negative signed32 sqrt are explicit API errors."
    });
    fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
    fs::write(
        root.join("analysis/native-mission/scalar-validation.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "requires original resident trigonometry tables"]
fn scalar_sine_matches_existing_rotation_table_for_whole_period() {
    let (root, math) = fixture();
    let bytes =
        fs::read(root.join("analysis/transform-runtime/oracle-002/sincos-table.bin")).unwrap();
    let table = ac3_graphics::transform::SinCosTable::from_le_bytes(&bytes).unwrap();
    for angle in -4096..=4096 {
        let expected = table.sin_cos(angle as i16);
        assert_eq!([math.sin(angle), math.cos(angle)], expected, "{angle}");
    }
    assert_eq!([math.sin(i32::MIN), math.cos(i32::MIN)], [0, 4096]);
}

#[test]
#[ignore = "requires original isolated signed32 rsin/rcos observations"]
fn scalar_trigonometry_matches_original_signed32_calls() {
    let (root, math) = fixture();
    let oracle: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("analysis/transform-runtime/scalar-math-002/scalar-math-vectors.json"))
            .unwrap(),
    )
    .unwrap();
    let mut counts = [0; 2];
    for case in oracle["cases"].as_array().unwrap() {
        let angle = case["args"][0].as_i64().unwrap() as i32;
        let actual = match case["kind"].as_str().unwrap() {
            "sin" => {
                counts[0] += 1;
                math.sin(angle)
            }
            "cos" => {
                counts[1] += 1;
                math.cos(angle)
            }
            _ => continue,
        };
        assert_eq!(case["ram_and_registers_restored"], true);
        assert_eq!(case["non_ram_accesses"], 0);
        assert_eq!(
            actual,
            case["expected_return"].as_i64().unwrap() as i32,
            "{}",
            case["name"]
        );
    }
    assert_eq!(counts, [62, 62]);
    fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
    fs::write(
        root.join("analysis/native-mission/scalar-trig-validation.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "passed":true,"sin_cases":counts[0],"cos_cases":counts[1],
            "scope":"Original signed32 rsin/rcos calls, including INT_MIN and quadrant/wrap boundaries."
        })).unwrap(),
    ).unwrap();
}
