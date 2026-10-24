//! Four rotating hub wedges: overlay 2 routines 800D169C/800D170C.
//! State and commands are reconstructed natively; RAM snapshots are test inputs only.

use ac3_graphics::transform::SinCosTable;

/// Native fields of the 36-byte record referenced by original pointer 800F539C.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CircleBackdrop {
    pub flags: u32,
    /// Stored as signed halfwords; the selected acceleration comparison is unsigned.
    pub speeds: [i16; 4],
    pub angles: [i16; 4],
    pub centers: [[i16; 2]; 4],
}

/// GP0 command words, without the original RAM-address-dependent linked-list tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackdropPackets {
    pub ot_bucket: usize,
    /// Execution order (wedge 3, 2, 1, 0), after the original prepend operations.
    pub packets: [[u32; 5]; 4],
}

impl Default for CircleBackdrop {
    fn default() -> Self {
        Self::new()
    }
}

impl CircleBackdrop {
    /// Original 800D169C initialization, independent of capture/checkpoint data.
    pub const fn new() -> Self {
        Self {
            flags: 0,
            speeds: [0; 4],
            angles: [0xe00; 4],
            centers: [[0x30, 0x83], [0x56, 0x58], [0x7c, 0x83], [0xa2, 0x58]],
        }
    }

    /// 800D170C's direction flag followed by 800D12EC's update. Only direction
    /// equal to 1 selects subtraction; other values clear bit 0. Selection
    /// outside 0..4 simply decelerates all four wedges, as in the original.
    pub fn update(&mut self, selected: i32, direction: i32) {
        self.flags = (self.flags & !1) | u32::from(direction == 1);
        self.advance(selected);
    }

    fn advance(&mut self, selected: i32) {
        for i in 0..4 {
            if i as i32 == selected {
                // LHU/SLTIU, rather than the signed LH used by deceleration.
                if (self.speeds[i] as u16) < 0x80 {
                    self.speeds[i] = self.speeds[i].wrapping_add(1);
                }
                self.angles[i] = if self.flags & 1 == 0 {
                    self.angles[i].wrapping_add(self.speeds[i])
                } else {
                    self.angles[i].wrapping_sub(self.speeds[i])
                };
            } else {
                if self.speeds[i] > 0 {
                    self.speeds[i] -= 1;
                }
                // Test the speed after decrement. The 6 -> 5 crossing adjusts
                // angle by -32, then still adds 5 (net -27).
                if self.speeds[i] == 5 {
                    self.angles[i] = self.angles[i].wrapping_sub(0x20);
                }
                if self.speeds[i] < 5 {
                    self.speeds[i] = 0;
                }
                self.angles[i] = self.angles[i].wrapping_add(self.speeds[i]);
            }
        }
    }

    /// 800D13E8 emits four black flat quads into bucket 15 (OT + 0x3C).
    /// The supplied offset is script group 2's signed XY pair, read by 800F1908.
    /// This is screen-space trigonometry; perspective projection is not involved.
    pub fn packets(&self, trigonometry: &SinCosTable, group_offset: [i16; 2]) -> BackdropPackets {
        let packets = std::array::from_fn(|execution_index| {
            let i = 3 - execution_index;
            let angle = self.angles[i];
            // Signed16 wrapping differs by a multiple of 4096 angle units,
            // preserving the resident table's exact full-turn symmetry.
            let left = trigonometry.sin_cos(angle.wrapping_sub(0x80));
            let right = trigonometry.sin_cos(angle.wrapping_add(0x80));
            let vertex = |trig: [i32; 2], radii: [i32; 2]| {
                // Original adds 0xFFF to negative products before >>12: DIV
                // toward zero, not an arithmetic shift of a negative product.
                let x = group_offset[0]
                    .wrapping_add(self.centers[i][0])
                    .wrapping_add((trig[1] * radii[0] / 4096) as i16);
                let y = group_offset[1]
                    .wrapping_add(self.centers[i][1])
                    .wrapping_add((trig[0] * radii[1] / 4096) as i16);
                u32::from(x as u16) | (u32::from(y as u16) << 16)
            };
            [
                0x28000000,
                vertex(left, [22, 40]),
                vertex(right, [22, 40]),
                vertex(left, [18, 29]),
                vertex(right, [18, 29]),
            ]
        });
        BackdropPackets {
            ot_bucket: 15,
            packets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_and_acceleration_follow_original_constants() {
        let mut state = CircleBackdrop::new();
        assert_eq!(state.angles, [3584; 4]);
        assert_eq!(state.centers, [[48, 131], [86, 88], [124, 131], [162, 88]]);
        state.update(1, 0);
        assert_eq!(state.speeds, [0, 1, 0, 0]);
        assert_eq!(state.angles, [3584, 3585, 3584, 3584]);
        for _ in 0..127 {
            state.update(1, 0);
        }
        assert_eq!(state.speeds, [0, 128, 0, 0]);
        let before = state.angles[1];
        state.update(1, 0);
        assert_eq!(state.speeds[1], 128);
        assert_eq!(state.angles[1], before.wrapping_add(128));
    }

    #[test]
    fn deceleration_crossing_has_single_backstep_then_stops() {
        let mut state = CircleBackdrop {
            speeds: [7, 6, 5, 4],
            angles: [100; 4],
            ..CircleBackdrop::new()
        };
        state.update(-1, 1);
        assert_eq!(state.speeds, [6, 5, 0, 0]);
        assert_eq!(state.angles, [106, 73, 100, 100]);
        state.update(4, 1);
        assert_eq!(state.speeds, [5, 0, 0, 0]);
        assert_eq!(state.angles, [79, 73, 100, 100]);
    }

    #[test]
    fn selected_unsigned_threshold_differs_from_other_signed_speeds() {
        let mut state = CircleBackdrop {
            speeds: [-1, -1, i16::MIN, i16::MAX],
            angles: [i16::MIN; 4],
            ..CircleBackdrop::new()
        };
        state.update(0, 0);
        assert_eq!(state.speeds, [-1, 0, 0, 32766]);
        assert_eq!(state.angles, [i16::MAX, i16::MIN, i16::MIN, -2]);
        state.speeds[0] = 129;
        state.update(0, 0);
        assert_eq!(state.speeds[0], 129);
    }

    #[test]
    fn direction_changes_only_bit_zero_and_only_exact_one_reverses() {
        let mut state = CircleBackdrop {
            flags: 0xa5a50001,
            speeds: [128; 4],
            angles: [i16::MIN; 4],
            ..CircleBackdrop::new()
        };
        state.update(2, 1);
        assert_eq!(state.flags, 0xa5a50001);
        assert_eq!(state.angles[2], 32640);
        assert_eq!(state.angles[0], -32641);
        state.update(2, -1);
        assert_eq!(state.flags, 0xa5a50000);
        assert_eq!(state.angles[2], i16::MIN);
        state.update(2, 2);
        assert_eq!(state.flags, 0xa5a50000);
    }

    #[test]
    fn packet_order_signed_rounding_and_halfword_wrap() {
        // A deliberately uniform fixture isolates geometry/rounding from table lookup.
        let mut bytes = Vec::with_capacity(16384);
        for _ in 0..4096 {
            bytes.extend_from_slice(&(-2047_i16).to_le_bytes());
            bytes.extend_from_slice(&2047_i16.to_le_bytes());
        }
        let trig = SinCosTable::from_le_bytes(&bytes).unwrap();
        let mut state = CircleBackdrop::new();
        state.centers[3] = [32760, -32760];
        let result = state.packets(&trig, [8, -8]);
        assert_eq!(result.ot_bucket, 15);
        let xy = |x: i16, y: i16| u32::from(x as u16) | (u32::from(y as u16) << 16);
        assert_eq!(
            result.packets[0],
            [
                0x28000000,
                xy(-32758, 32749),
                xy(-32758, 32749),
                xy(-32760, 32754),
                xy(-32760, 32754)
            ]
        );
        assert_eq!(
            result.packets[3],
            [
                0x28000000,
                xy(66, 104),
                xy(66, 104),
                xy(64, 109),
                xy(64, 109)
            ]
        );
    }

    fn captured_state(bytes: &[u8]) -> CircleBackdrop {
        assert_eq!(bytes.len(), 36);
        let half = |at: usize| i16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
        CircleBackdrop {
            flags: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            speeds: std::array::from_fn(|i| half(4 + i * 2)),
            angles: std::array::from_fn(|i| half(12 + i * 2)),
            centers: std::array::from_fn(|i| [half(20 + i * 2), half(28 + i * 2)]),
        }
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and original circle-001 isolated-call vectors"]
    fn original_state_edge_cases_match() {
        let root =
            std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("analysis/transform-runtime/circle-001/circle-vectors.json"))
                .unwrap(),
        )
        .unwrap();
        let cases = value["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 29);
        let state = |v: &serde_json::Value| {
            let words = v.as_array().unwrap();
            assert_eq!(words.len(), 18);
            let bytes: Vec<u8> = words
                .iter()
                .flat_map(|n| u16::try_from(n.as_u64().unwrap()).unwrap().to_le_bytes())
                .collect();
            captured_state(&bytes)
        };
        for case in cases {
            let mut input = state(&case["input_state"]);
            input.advance(i32::try_from(case["selected"].as_i64().unwrap()).unwrap());
            assert_eq!(input, state(&case["expected_state"]), "{}", case["name"]);
        }
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and normal hub-render-001 original captures"]
    fn original_hub_states_and_twelve_submitted_quad_packets_match() {
        let root =
            std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
        let data = crate::assets::GameData::load(&root.join("extracted/disc")).unwrap();
        let trig =
            SinCosTable::from_le_bytes(&data.executable.payload[0xa4f84..0xa4f84 + 16384]).unwrap();
        let base = root.join("analysis/menu-runtime/hub-render-001");
        for frame in 0..4 {
            let read = |suffix: &str| {
                std::fs::read(base.join(format!("render-{frame:05}-{suffix}.bin"))).unwrap()
            };
            let mut state = captured_state(&read("callback-entry-circle-state"));
            let hub = read("callback-entry-hub-state");
            let selected = i32::from_le_bytes(hub[4..8].try_into().unwrap());
            state.update(selected, 0);
            assert_eq!(
                state,
                captured_state(&read("text-entry-circle-state")),
                "frame {frame} state"
            );
            // DrawOTag occurs before the callback: this update is submitted
            // on the next local frame. No frame-4 OT was captured.
            if frame == 3 {
                continue;
            }
            let script = read("text-entry-script-state");
            let group = std::array::from_fn(|i| {
                i16::from_le_bytes(script[0x2a0 + i * 2..0x2a2 + i * 2].try_into().unwrap())
            });
            let generated = state.packets(&trig, group);
            let ot: serde_json::Value = serde_json::from_slice(
                &std::fs::read(base.join(format!("ot-{:05}-{:03}.json", frame + 1, frame + 2)))
                    .unwrap(),
            )
            .unwrap();
            let originals: Vec<[u32; 5]> = ot["packets"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["words"][0].as_u64() == Some(0x28000000))
                .map(|p| {
                    assert_eq!(p["words"].as_array().unwrap().len(), 5);
                    std::array::from_fn(|i| p["words"][i].as_u64().unwrap() as u32)
                })
                .collect();
            assert_eq!(originals, generated.packets, "frame {frame} packets");
        }
    }
}
