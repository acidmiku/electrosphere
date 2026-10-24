use ac3_port::{
    assets::GameData, flight_assets::FlightMissionAssets, flight_camera::depth_buckets,
    flight_world::*,
};
use serde_json::{Value, json};
use std::path::PathBuf;

fn bytes(value: &Value) -> Vec<u8> {
    let text = value.as_str().unwrap();
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}
fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn half(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn point(bytes: &[u8], offset: usize) -> ScreenPoint {
    ScreenPoint([half(bytes, offset), half(bytes, offset + 2)])
}
fn packets(bytes: &[u8]) -> Vec<Vec<u32>> {
    let mut offset = 0;
    let mut packets = vec![];
    while offset < bytes.len() {
        let count = (word(bytes, offset) >> 24) as usize;
        assert!(count > 0);
        packets.push(
            (0..count)
                .map(|i| word(bytes, offset + 4 + i * 4))
                .collect(),
        );
        offset += (count + 1) * 4;
    }
    assert_eq!(offset, bytes.len());
    packets
}

#[test]
fn terrain_border_stitch_sign_extension_and_coordinate_wrap() {
    let mut bytes = vec![0; 131072];
    for (i, pair) in bytes.chunks_exact_mut(2).enumerate() {
        pair.copy_from_slice(&((i % 511 + 1) as i16).to_le_bytes());
    }
    let grid = TerrainGrid::from_bytes(&bytes).unwrap();
    assert_eq!(grid.cell(-1, -1), grid.cell(7, 7));
    assert_eq!(grid.cell(256, 257), grid.cell(248, 249));
    assert_eq!(grid.cell(65536, 65536), grid.cell(0, 0));
    let mut view = TerrainView {
        height: 32768,
        tile_origin: [0, 0],
        fraction: [32767, -32768],
        scan_origin: [31, 31],
        width: 2,
        row_masks: vec![3, 3],
    };
    let cells = grid.select(&view).unwrap();
    assert_eq!(cells.len(), 4);
    assert_eq!(cells[0].position, [31745, -32768, 31744]);
    assert_eq!(cells[0].descriptor & 0x0e40_0000, 0x0040_0000);
    assert_eq!(cells[1].descriptor & 0x0e40_0000, 0x0800_0000);
    assert_eq!(cells[3].descriptor & 0x0e40_0000, 0x0e00_0000);
    view.width = 33;
    assert!(grid.select(&view).is_err());
    bytes[0..2].copy_from_slice(&0x8001_u16.to_le_bytes());
    let grid = TerrainGrid::from_bytes(&bytes).unwrap();
    view = TerrainView {
        height: 0,
        tile_origin: [0, 0],
        fraction: [0, 0],
        scan_origin: [0, 0],
        width: 1,
        row_masks: vec![1],
    };
    assert_eq!(grid.select(&view).unwrap()[0].descriptor, 0xffff8001);
    assert!(TerrainGrid::from_bytes(&bytes[..131070]).is_err());
}

#[test]
fn fog_chains_preserve_strip_order_and_restore_environment() {
    let material = FogMaterial { rgb: [1, 2, 3] };
    let output = material
        .packets([0; 17], [-1, -2, 320, 256], [-160, -128], 4)
        .unwrap();
    assert_eq!(output.len(), 17);
    assert_eq!(
        output[0].packets[1],
        [
            0xe6000000, 0x01000000, 0x80000000, 0x01800140, 0x01000140, 0x00400010, 0xe6000001
        ]
    );
    assert_eq!(output[1].packets[1][4], 0x8000);
    assert_eq!(
        output[16].packets[1],
        [0xe6000001, 0x60030201, 0, 0x00400010, 0xe6000001]
    );
    assert_eq!(output[0].packets[2][0], 0xe30ffbff);
    assert_eq!(output[0].packets[2][2], 0xe53c0760);
    let mut invalid = [0; 17];
    invalid[16] = -1;
    assert!(material.packets(invalid, [0; 4], [0; 2], 0).is_err());
    assert!(FogMaterial::from_mission_parameters(&[0; 10]).is_err());
}

#[test]
#[ignore = "requires original assets and passive world-001 capture"]
fn original_first_flight_terrain_fog_and_sky_packets() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let assets = FlightMissionAssets::parse(data.resource(7).unwrap()).unwrap();
    let directory = root.join("analysis/flight-runtime/world-001");
    let cases: Vec<Value> = std::fs::read_to_string(directory.join("world.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let grid = TerrainGrid::from_bytes(assets.sections()[1].bytes()).unwrap();
    let material = FogMaterial::from_mission_parameters(assets.sections()[0].bytes()).unwrap();
    let parameters = assets.sections()[0].bytes();
    let native_depths = depth_buckets(
        i32::from(half(parameters, 12)),
        i32::from(half(parameters, 14)),
    );
    let mut terrain = 0;
    let mut fog = 0;
    let mut flat = 0;
    let mut gradient = 0;
    let mut cells = 0;
    let mut packet_count = 0;
    let mut word_count = 0;
    for case in &cases {
        let kind = case["kind"].as_str().unwrap();
        let expected_bytes = bytes(&case["output_hex"]);
        if kind == "terrain_cells" {
            let original_grid =
                std::fs::read(directory.join(case["grid_file"].as_str().unwrap())).unwrap();
            assert_eq!(assets.sections()[1].bytes(), original_grid);
            let b = bytes(&case["view_hex"]);
            let view = TerrainView {
                height: word(&b, 4) as i32,
                tile_origin: [word(&b, 0x38) as i32, word(&b, 0x3c) as i32],
                fraction: [word(&b, 0x40) as i32, word(&b, 0x44) as i32],
                scan_origin: [word(&b, 0x48) as i32, word(&b, 0x4c) as i32],
                width: word(&b, 0x58).try_into().unwrap(),
                row_masks: (0..word(&b, 0x5c) as usize)
                    .map(|i| word(&b, 0x60 + i * 4))
                    .collect(),
            };
            let actual = grid.select(&view).unwrap();
            assert_eq!(expected_bytes.len(), (actual.len() + 1) * 12);
            for (i, cell) in actual.iter().enumerate() {
                let start = i * 12;
                assert_eq!(
                    cell.position,
                    [
                        half(&expected_bytes, start),
                        half(&expected_bytes, start + 2),
                        half(&expected_bytes, start + 4)
                    ],
                    "terrain coordinates case {}",
                    case["id"]
                );
                assert_eq!(
                    cell.descriptor,
                    word(&expected_bytes, start + 8),
                    "terrain descriptor case {}",
                    case["id"]
                );
            }
            assert_eq!(
                word(&expected_bytes, actual.len() * 12 + 8),
                TERRAIN_LIST_END
            );
            terrain += 1;
            cells += actual.len();
            continue;
        }
        let actual = if kind == "fog" {
            let b = bytes(&case["depth_hex"]);
            let depth: [i16; 17] = std::array::from_fn(|i| half(&b, i * 2));
            assert_eq!(native_depths, depth);
            let b = bytes(&case["draw_rect_hex"]);
            let rect = std::array::from_fn(|i| half(&b, i * 2));
            let offset = case["draw_offset"].as_u64().unwrap() as u32;
            let chains = material
                .packets(
                    native_depths,
                    rect,
                    [offset as i16, (offset >> 16) as i16],
                    case["mask_flags"].as_u64().unwrap() as u32,
                )
                .unwrap();
            // Verify real packet linkage and OT prepend destinations, not only payloads.
            let before = bytes(&case["ot_before_hex"]);
            let after = bytes(&case["ot_after_hex"]);
            let mut heads: Vec<u32> = (0..4096).map(|i| word(&before, i * 4)).collect();
            let mut byte_offset = 0;
            let address = case["output_address"].as_u64().unwrap() as u32;
            for chain in &chains {
                let start = address + byte_offset as u32;
                let old_head = heads[chain.bucket as usize];
                for (index, packet) in chain.packets.iter().enumerate() {
                    let next_offset = byte_offset + (packet.len() + 1) * 4;
                    let link = if index + 1 == chain.packets.len() {
                        old_head
                    } else {
                        address + next_offset as u32
                    };
                    assert_eq!(
                        word(&expected_bytes, byte_offset),
                        ((packet.len() as u32) << 24) | (link & 0xffffff)
                    );
                    byte_offset = next_offset;
                }
                heads[chain.bucket as usize] = start & 0xffffff;
            }
            for (i, head) in heads.iter().enumerate() {
                assert_eq!(*head, word(&after, i * 4));
            }
            fog += 1;
            chains.into_iter().flat_map(|c| c.packets).collect()
        } else {
            let b = bytes(&case["scratch_hex"]);
            let band_index = case["args"][2].as_u64().unwrap() as usize;
            let band = SkyBand {
                upper_codes: word(&b, 0x7c + band_index * 4),
                lower_codes: word(&b, 0x80 + band_index * 4),
                upper_edge: [
                    point(&b, 0x4c + band_index * 8),
                    point(&b, 0x50 + band_index * 8),
                ],
                lower_edge: [
                    point(&b, 0x54 + band_index * 8),
                    point(&b, 0x58 + band_index * 8),
                ],
                corners: std::array::from_fn(|i| point(&b, 0xe8 + i * 4)),
            };
            let rgb: [u8; 3] = b[0xf8 + band_index * 4..0xfb + band_index * 4]
                .try_into()
                .unwrap();
            if kind == "sky_flat" {
                flat += 1;
                band.flat_packets(rgb)
            } else {
                gradient += 1;
                let upper_rgb = b[0xf4 + band_index * 4..0xf7 + band_index * 4]
                    .try_into()
                    .unwrap();
                let triangle = std::array::from_fn(|i| point(&b, 0x24 + band_index * 8 + i * 4));
                let corner_areas =
                    std::array::from_fn(|i| word(&b, 0x98 + (band_index - 1) * 16 + i * 4) as i32);
                band.gradient_packets(upper_rgb, rgb, triangle, corner_areas)
                    .unwrap()
            }
        };
        let expected = packets(&expected_bytes);
        assert_eq!(
            actual, expected,
            "{} case {} frame {}",
            kind, case["id"], case["frame"]
        );
        packet_count += actual.len();
        word_count += actual.iter().map(Vec::len).sum::<usize>();
    }
    assert_eq!((terrain, fog, flat, gradient), (20, 20, 40, 60));
    let report = json!({"status":"pass","terrain_cases":terrain,"terrain_cells":cells,
        "fog_cases":fog,"sky_flat_cases":flat,"sky_gradient_cases":gradient,
        "packets":packet_count,"packet_words":word_count,"differences":0,
        "scope":"Asset grid/material plus captured typed visibility/projected sky input; no full world renderer or pixels claimed"});
    println!("{report}");
    std::fs::write(
        directory.join(format!("native-world-{}.json", std::env::consts::OS)),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
