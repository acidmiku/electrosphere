//! Native hub reveal and map overlay: 800D3C50, 800D3908 and 800D3DDC.
use crate::{Result, assets::GameData};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubMinimap {
    pub revealed: [u32; 32],
    pub pending: [u32; 32],
    pub seed: [i16; 2],
    mask: [u32; 32],
}

/// Packets retain emission order. Each original OT bucket prepends packets,
/// so the scene compositor executes each array in reverse order.
pub struct MinimapPackets {
    /// The grid is attached to the outer scene OT's bucket14.
    pub grid: Vec<Vec<u32>>,
    /// Marker and revealed cells are attached to outer scene OT bucket0.
    pub markers: Vec<Vec<u32>>,
}

fn xy(x: i32, y: i32) -> u32 {
    u32::from(x as u16) | (u32::from(y as u16) << 16)
}
fn word(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or("minimap table outside overlay")?
            .try_into()?,
    ))
}

impl HubMinimap {
    /// 800D483C seeds the map from the selected mission's original table entry.
    pub fn load(data: &GameData, mission: u16, placeholder: bool) -> Result<Self> {
        let overlay = data.overlay(2)?;
        let base = crate::assets::OVERLAY_BASE;
        let id = if placeholder {
            0x3b
        } else {
            *overlay
                .get((0x800f5959 - base) as usize + usize::from(mission) * 12)
                .ok_or("mission metadata outside overlay")? as usize
        };
        let packed = word(overlay, (0x800f5c04 - base) as usize + id * 4)?;
        let seed = [packed as i16, (packed >> 16) as i16];
        if !(0..32).contains(&seed[0]) || !(0..32).contains(&seed[1]) {
            return Err("mission minimap seed outside 32 by 32 grid".into());
        }
        let mut mask = [0; 32];
        for (i, value) in mask.iter_mut().enumerate() {
            *value = word(overlay, (0x800f5b84 - base) as usize + i * 4)?;
        }
        let mut revealed = [0; 32];
        revealed[seed[0] as usize] = 1 << seed[1];
        Ok(Self {
            revealed,
            pending: [0; 32],
            seed,
            mask,
        })
    }

    /// One original propagation tick. The staggered columns have alternating
    /// three-neighbor and single-neighbor connections to adjacent columns.
    pub fn advance(&mut self) {
        for (column, &bits) in self.revealed.iter().enumerate() {
            let adjacent = (bits << 1) | (bits >> 1);
            self.pending[column] |= adjacent;
            if column > 0 {
                self.pending[column - 1] |= if column & 1 == 0 {
                    bits
                } else {
                    bits | adjacent
                };
            }
            if column < 31 {
                self.pending[column + 1] |= if column & 1 == 0 {
                    bits | adjacent
                } else {
                    bits
                };
            }
        }
        for (revealed, pending) in self.revealed.iter_mut().zip(&mut self.pending) {
            *revealed |= *pending;
            *pending = 0;
        }
    }

    pub fn packets(&self, timer: i32, exit_offset: i32, frame_counter: i32) -> MinimapPackets {
        let mut grid = Vec::new();
        let tile = |x, y, w, h| vec![0x60808080, xy(x, y), xy(w, h)];
        if timer > 0 {
            let complete = timer >= 120;
            for row in 0..17 {
                if !complete && timer < row {
                    break;
                }
                let width = if complete {
                    140
                } else {
                    timer.wrapping_sub(row).wrapping_mul(7).min(140)
                };
                grid.push(tile(exit_offset.wrapping_add(168), 258 + row * 9, width, 2));
            }
            let width = if complete {
                75
            } else {
                (timer.wrapping_mul(75) / 17).min(75)
            };
            grid.push(tile(exit_offset.wrapping_add(232), 160, width, 2));
            for column in 0..16 {
                if !complete && timer < column {
                    break;
                }
                let height = if complete {
                    242
                } else {
                    (timer.wrapping_sub(column).wrapping_mul(242) / 20).min(242)
                };
                grid.push(tile(
                    exit_offset.wrapping_add(232 + column * 5),
                    160,
                    1,
                    height,
                ));
            }
            let height = if complete {
                144
            } else {
                (timer.wrapping_mul(144) / 17).min(144)
            };
            grid.push(tile(exit_offset.wrapping_add(168), 258, 1, height));
        }
        let [sx, sy] = self.seed.map(i32::from);
        let pulse = (64 - frame_counter % 128).unsigned_abs().wrapping_mul(2) as u8;
        let color = u32::from(pulse) * 0x10101;
        let mut markers = vec![
            vec![
                0x64000000 | color,
                xy(
                    exit_offset.wrapping_add(150).wrapping_add(sx * 5),
                    sy * 10 + ((sx + 1) % 2) * 5 + 190,
                ),
                0x02980cf8,
                0x000c0008,
            ],
            vec![0xe1000007, 0],
        ];
        let shade = if timer >= 61 {
            0
        } else if timer < 31 {
            128
        } else {
            128_i32.wrapping_sub(timer.wrapping_sub(30).wrapping_mul(128) / 30) as u8
        };
        for column in 0..32 {
            let visible = self.revealed[column] & self.mask[column];
            for row in 0..32 {
                if visible & (1 << row) == 0 {
                    continue;
                }
                markers.push(vec![
                    0x66000000 | (u32::from(shade) * 0x10101),
                    xy(
                        exit_offset.wrapping_add(150 + column as i32 * 5),
                        190 + ((column as i32 + 1) % 2) * 5 + row * 10,
                    ),
                    0x025800f8,
                    0x000c0008,
                ]);
            }
        }
        markers.push(vec![0xe1000047, 0]);
        MinimapPackets { grid, markers }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    #[test]
    fn staggered_reveal_does_not_wrap_edges_and_clears_pending() {
        let mut map = HubMinimap {
            revealed: [0; 32],
            pending: [0; 32],
            seed: [0, 0],
            mask: [!0; 32],
        };
        map.revealed[0] = 1;
        map.advance();
        assert_eq!(&map.revealed[..3], &[3, 3, 0]);
        assert_eq!(map.pending, [0; 32]);
        map.revealed = [0; 32];
        map.revealed[31] = 1 << 31;
        map.advance();
        assert_eq!(&map.revealed[29..], &[0, 0xc0000000, 0xc0000000]);
    }

    #[test]
    #[ignore = "requires original hub runtime captures and assets"]
    fn normal_hub_reveal_and_generated_overlay_packets_match() {
        let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
        let data = GameData::load(&root.join("extracted/disc")).unwrap();
        let base = root.join("analysis/menu-runtime/hub-render-001");
        for frame in 0..3 {
            let before =
                fs::read(base.join(format!("render-{frame:05}-callback-entry-hub-state.bin")))
                    .unwrap();
            let after =
                fs::read(base.join(format!("render-{frame:05}-text-entry-hub-state.bin"))).unwrap();
            let meta: serde_json::Value = serde_json::from_slice(
                &fs::read(base.join(format!("render-{frame:05}-callback-entry.json"))).unwrap(),
            )
            .unwrap();
            let mut map = HubMinimap::load(&data, 1, false).unwrap();
            assert_eq!(
                xy(i32::from(map.seed[0]), i32::from(map.seed[1])),
                word(&before, 0x118).unwrap()
            );
            for i in 0..32 {
                map.pending[i] = word(&before, 0x18 + i * 4).unwrap();
                map.revealed[i] = word(&before, 0x98 + i * 4).unwrap();
            }
            map.advance();
            for i in 0..32 {
                assert_eq!(map.pending[i], word(&after, 0x18 + i * 4).unwrap());
                assert_eq!(map.revealed[i], word(&after, 0x98 + i * 4).unwrap());
            }
            let packets = map.packets(
                word(&after, 0x14).unwrap() as i32,
                word(&after, 0x11c).unwrap() as i32,
                meta["context_frame"].as_i64().unwrap() as i32,
            );
            let ot: serde_json::Value = serde_json::from_slice(
                &fs::read(base.join(format!("ot-{:05}-{:03}.json", frame + 1, frame + 2))).unwrap(),
            )
            .unwrap();
            let marker = ot["packets"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["words"][2].as_u64() == Some(0x02980cf8))
                .unwrap();
            let cursor = marker["address"].as_u64().unwrap() as usize & 0x1fffff;
            let mut cursor = cursor
                - packets
                    .grid
                    .iter()
                    .map(|p| (p.len() + 1) * 4)
                    .sum::<usize>();
            let ram = fs::read(base.join(format!("frame-{:05}.ram.bin", frame + 1))).unwrap();
            for (index, packet) in packets.grid.iter().chain(&packets.markers).enumerate() {
                assert_eq!(
                    word(&ram, cursor).unwrap() >> 24,
                    packet.len() as u32,
                    "frame {frame} packet {index} length"
                );
                let original: Vec<_> = (0..packet.len())
                    .map(|i| word(&ram, cursor + 4 + i * 4).unwrap())
                    .collect();
                assert_eq!(
                    *packet, original,
                    "frame {frame} packet {index} at {cursor:x}"
                );
                cursor += (packet.len() + 1) * 4;
            }
        }
    }

    #[test]
    #[ignore = "requires original isolated minimap calls and assets"]
    fn original_minimap_propagation_and_drawing_edges_match() {
        let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
        let data = GameData::load(&root.join("extracted/disc")).unwrap();
        let base = root.join("analysis/minimap-runtime/oracle-001");
        let captured_code = fs::read(base.join("minimap-code.bin")).unwrap();
        let offset = (0x800d3908 - crate::assets::OVERLAY_BASE) as usize;
        assert_eq!(
            captured_code,
            data.overlay(2).unwrap()[offset..offset + captured_code.len()]
        );
        let vectors: serde_json::Value =
            serde_json::from_slice(&fs::read(base.join("minimap-vectors.json")).unwrap()).unwrap();
        let mut map = HubMinimap::load(&data, 1, false).unwrap();
        let convert = |value: &serde_json::Value| -> Vec<u32> {
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|v| u32::try_from(v.as_u64().unwrap()).unwrap())
                .collect()
        };
        let load = |map: &mut HubMinimap, before: &[u32]| {
            map.pending.copy_from_slice(&before[6..38]);
            map.revealed.copy_from_slice(&before[38..70]);
            map.seed = [before[70] as i16, (before[70] >> 16) as i16];
        };
        let propagation = vectors["propagation"].as_array().unwrap();
        for case in propagation {
            let mut actual = convert(&case["before"]);
            load(&mut map, &actual);
            map.advance();
            actual[6..38].copy_from_slice(&map.pending);
            actual[38..70].copy_from_slice(&map.revealed);
            assert_eq!(actual, convert(&case["after"]), "{}", case["name"]);
        }
        let drawing = vectors["drawing"].as_array().unwrap();
        let mut packet_count = 0;
        for case in drawing {
            let before = convert(&case["before"]);
            load(&mut map, &before);
            let native = map.packets(
                case["timer"].as_i64().unwrap() as i32,
                before[71] as i32,
                case["frame_counter"].as_i64().unwrap() as i32,
            );
            let packets = if case["pc"].as_u64() == Some(0x800d3908) {
                native.grid
            } else {
                native.markers
            };
            // The Lua encoder represents an empty array as an empty object.
            let raw = if case["packets_with_tags"].is_object() {
                Vec::new()
            } else {
                convert(&case["packets_with_tags"])
            };
            let mut cursor = 0;
            for (index, packet) in packets.iter().enumerate() {
                assert_eq!(
                    (raw[cursor] >> 24) as usize,
                    packet.len(),
                    "packet {index} length, {case}"
                );
                assert_eq!(
                    &raw[cursor + 1..cursor + 1 + packet.len()],
                    packet,
                    "packet {index}, {case}"
                );
                cursor += packet.len() + 1;
            }
            assert_eq!(cursor, raw.len());
            packet_count += packets.len();
        }
        fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
        fs::write(root.join("analysis/native-mission/minimap-validation.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"passed":true,
                "propagation_cases":propagation.len(),"drawing_cases":drawing.len(),"generated_packets":packet_count,
                "normal_frames":3,"scope":"Native reveal arrays, grid and marker packets from original leaf/state inputs; CPU/RAM isolated, DMA address tags excluded. Full hub integration remains separate."})).unwrap()).unwrap();
    }
}
