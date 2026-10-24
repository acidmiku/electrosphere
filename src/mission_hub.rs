//! Ordered hub visual effects and the outer scene's 2D draw lists.
//! The separate map renderer supplies depth-sorted packets; this module never
//! loads a captured command list or interprets guest instructions.
use crate::{
    Result,
    assets::GameData,
    menu_scene::{LanguageScene, MenuFrame, MenuVisualState},
    mission_assets::MissionAssets,
    mission_backdrop::CircleBackdrop,
    mission_background::SceneColor,
    mission_map::{MapRenderer, Random},
    mission_minimap::{HubMinimap, MinimapPackets},
};
use ac3_graphics::{Frame, transform::SinCosTable};
use ac3_mission::Effect;

pub struct HubScene<'a> {
    data: &'a GameData,
    pub scene: LanguageScene<'a>,
    pub visual: MenuVisualState,
    pub circle: CircleBackdrop,
    pub minimap: Option<HubMinimap>,
    pub color: SceneColor,
    pub map: MapRenderer,
    clear_color: [u8; 3],
    trigonometry: SinCosTable,
}

/// Foreground execution order, plus effects that require another subsystem.
/// A caller must handle `remaining_effects` before claiming full scene support.
pub struct HubLayers {
    pub packets: Vec<Vec<u32>>,
    pub clear_color: [u8; 3],
    pub remaining_effects: Vec<Effect>,
}

impl<'a> HubScene<'a> {
    pub fn new(data: &'a GameData, assets: MissionAssets<'a>, random: &mut Random) -> Result<Self> {
        let map = MapRenderer::new(
            &data.executable.payload,
            data.overlay(2)?,
            &assets.vram,
            random,
        )?;
        let scene = LanguageScene::from_mission(data, assets)?;
        let visual = scene.visual_state()?;
        Ok(Self {
            data,
            scene,
            visual,
            circle: CircleBackdrop::new(),
            minimap: None,
            color: SceneColor::default(),
            map,
            clear_color: [0; 3],
            trigonometry: SinCosTable::from_le_bytes(
                &data.executable.payload[0xa4f84..0xa4f84 + 4096 * 4],
            )?,
        })
    }

    /// The visual writes in loader callback 800D328C. Loading itself is owned
    /// by the host; these are resource byte counts, not TIM upload counts or CD timings.
    pub fn set_load_progress(&mut self, total: i32, loaded: i32) -> Result<()> {
        let variant = if total == 0 {
            0
        } else {
            loaded.wrapping_sub(1).wrapping_mul(7).wrapping_div(total)
        };
        let mut effects: Vec<_> = (0..6)
            .map(|index| Effect::SetVisualStyle {
                slot: index + 1,
                style: u32::from(index < variant),
            })
            .collect();
        effects.push(Effect::SetVisualStyle {
            slot: 7,
            style: variant as u32,
        });
        self.visual.apply_mission_visuals(&effects)
    }

    /// Process callback effects in their original order. In particular the
    /// rotating wedges use group 2's offset before this tick changes it.
    pub fn layers(
        &mut self,
        effects: &[Effect],
        frame_counter: i32,
        language: usize,
        held_input: u32,
    ) -> Result<HubLayers> {
        self.begin_frame(held_input)?;
        let mut circles = Vec::new();
        let mut minimap = MinimapPackets {
            grid: Vec::new(),
            markers: Vec::new(),
        };
        let mut remaining_effects = Vec::new();
        for effect in effects {
            match *effect {
                Effect::DrawHubBackdrop { selection } => {
                    self.circle.update(selection, 0);
                    circles.extend(
                        self.circle
                            .packets(&self.trigonometry, self.visual.offset(2)?)
                            .packets
                            .into_iter()
                            .rev()
                            .map(Vec::from),
                    );
                }
                Effect::InitializeHubMap {
                    mission,
                    placeholder,
                } => {
                    self.minimap = Some(HubMinimap::load(self.data, mission, placeholder)?);
                }
                Effect::PendingOriginalCall {
                    address: 0x800d3c50,
                } => {
                    self.minimap
                        .as_mut()
                        .ok_or("hub minimap not initialized")?
                        .advance();
                }
                Effect::DrawHubMap {
                    reveal_timer,
                    exit_offset,
                } => {
                    minimap = self
                        .minimap
                        .as_ref()
                        .ok_or("hub minimap not initialized")?
                        .packets(reveal_timer, exit_offset, frame_counter);
                }
                Effect::SetSceneColor { red, green, blue } => self.color.set([red, green, blue]),
                Effect::BeginHubBackgroundFade => {
                    self.map.set_tube_phase(1)?;
                    remaining_effects.push(effect.clone()); // Five sound-voice fades remain external.
                }
                Effect::BeginLaunchBackgroundFade => {
                    self.map.set_tube_phase(3)?;
                    remaining_effects.push(effect.clone());
                }
                Effect::PendingOriginalCall {
                    address: 0x800e9824,
                } => self.map.set_object_phase(1)?,
                Effect::PendingOriginalCall {
                    address: 0x800e983c,
                } => self.map.set_object_phase(3)?,
                Effect::SelectVisualPage { .. }
                | Effect::ClearTextVisibility
                | Effect::SetTextVisibility { .. }
                | Effect::SetVisualStyle { .. }
                | Effect::SetVisualOffset { .. }
                | Effect::SetVisualParameter { .. } => {
                    self.visual
                        .apply_mission_visuals(std::slice::from_ref(effect))?;
                }
                _ => remaining_effects.push(effect.clone()),
            }
        }
        self.visual.set_frame_color(self.color.rgb);
        let mut commands = self.scene.mission_commands(&mut self.visual, language)?;
        circles.append(&mut commands.bins[15]);
        commands.bins[15] = circles;
        let packets = minimap
            .grid
            .into_iter()
            .rev()
            .chain(commands.into_packets())
            .chain(minimap.markers.into_iter().rev())
            .collect();
        Ok(HubLayers {
            packets,
            clear_color: self.clear_color,
            remaining_effects,
        })
    }

    /// Outer mission wrapper actions shared by hub and briefing callbacks.
    pub fn begin_frame(&mut self, held_input: u32) -> Result<[u8; 3]> {
        if let Some(rgb) = self.color.take_draw_color() {
            self.clear_color = rgb;
        }
        self.visual
            .apply_mission_visuals(&[Effect::SetVisualStyle {
                slot: 0x78,
                style: u32::from(held_input & 0x10 != 0),
            }])?;
        Ok(self.clear_color)
    }

    /// The map depth OT executes first (outer bucket 15), then grid (14),
    /// shared UI (13), and text followed by marker cells (0).
    pub fn render(&mut self, layers: &HubLayers, depth_packets: &[Vec<u32>]) -> Result<MenuFrame> {
        let packets: Vec<_> = depth_packets
            .iter()
            .chain(&layers.packets)
            .cloned()
            .collect();
        let mut frame = Frame::new(320, 512)?;
        // PutDrawEnv's direct fill is outside the original ordering table.
        frame.draw_tile(0, 0, 320, 511, layers.clear_color, None);
        ac3_graphics::render_packets_with_transfers(&mut frame, &mut self.scene.vram, &packets)?;
        Ok(MenuFrame { frame, packets })
    }

    pub fn draw(
        &mut self,
        effects: &[Effect],
        frame_counter: i32,
        language: usize,
        held_input: u32,
    ) -> Result<(MenuFrame, Vec<Effect>)> {
        let layers = self.layers(effects, frame_counter, language, held_input)?;
        let depth = self.map.tick(self.color.rgb)?;
        let packets: Vec<_> = depth
            .execution_order()
            .iter()
            .map(|p| p.words.clone())
            .collect();
        let image = self.render(&layers, &packets)?;
        Ok((image, layers.remaining_effects))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac3_mission::{HubState, MissionContext, MissionState, initialize_hub, tick_hub};
    use std::{fs, path::PathBuf};

    fn word(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }
    fn normalize(packet: &mut [u32]) {
        if matches!(packet[0] >> 24, 0x2c..=0x2f) {
            packet[6] &= 0xffff;
            packet[8] &= 0xffff;
        }
    }

    #[test]
    #[ignore = "requires original assets and normal hub frame captures"]
    fn continuous_native_hub_matches_original_packets_and_pixels() {
        let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
        let data = GameData::load(&root.join("extracted/disc")).unwrap();
        let capture = root.join("analysis/menu-runtime/hub-render-001");
        let mut scene = HubScene::new(
            &data,
            MissionAssets::load(&data, 74).unwrap(),
            &mut Random { state: 0 },
        )
        .unwrap();
        // Match the fixture's observed elapsed callback counts, without loading
        // any captured geometry, visual state, VRAM, or commands into the scene.
        // The native host supplies its own loading/frame timeline.
        for _ in 0..187 {
            assert!(scene.map.tick([0; 3]).unwrap().emitted.is_empty());
        }
        scene.set_load_progress(712, 712).unwrap();
        let mut update = initialize_hub(
            MissionState::default(),
            MissionContext {
                options: 0x42,
                current_pair: [1, 0],
                ..Default::default()
            },
        );
        scene.draw(&update.effects, 0, 0, 0).unwrap();
        for elapsed in 1..305 {
            update = tick_hub(update.state, update.mission, update.context, 0).unwrap();
            scene.draw(&update.effects, elapsed, 0, 0).unwrap();
        }
        assert_eq!(scene.map.state.frame, 492);
        let mut packet_count = 0;
        let mut hashes = Vec::new();
        for frame in 0..3 {
            let meta: serde_json::Value = serde_json::from_slice(
                &fs::read(capture.join(format!("render-{frame:05}-callback-entry.json"))).unwrap(),
            )
            .unwrap();
            update = tick_hub(update.state, update.mission, update.context, 0).unwrap();
            let (image, remaining) = scene
                .draw(
                    &update.effects,
                    meta["context_frame"].as_i64().unwrap() as i32,
                    0,
                    0,
                )
                .unwrap();
            assert!(remaining.is_empty());
            let ot: serde_json::Value = serde_json::from_slice(
                &fs::read(capture.join(format!("ot-{:05}-{:03}.json", frame + 1, frame + 2)))
                    .unwrap(),
            )
            .unwrap();
            let mut original: Vec<Vec<u32>> = ot["packets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    p["words"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap() as u32)
                        .collect()
                })
                .collect();
            for packet in &mut original {
                normalize(packet);
            }
            for (index, native) in image.packets.iter().enumerate() {
                assert_eq!(
                    *native, original[index],
                    "native frame {frame}, packet {index}"
                );
            }
            let actual: Vec<_> = image
                .frame
                .words()
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect();
            let expected =
                fs::read(capture.join(format!("frame-{:05}.screen.raw", frame + 2))).unwrap();
            let differences = actual
                .chunks_exact(2)
                .zip(expected.chunks_exact(2))
                .filter(|(a, b)| a != b)
                .count();
            crate::frontend::save_frame(
                &image.frame,
                &root.join(format!("analysis/native-mission/hub-native-{frame}")),
            )
            .unwrap();
            assert_eq!(actual.len(), expected.len());
            assert_eq!(differences, 0, "frame {frame} pixel differences");
            packet_count += image.packets.len();
            hashes.push(crate::sha256(&actual));
        }
        fs::write(root.join("analysis/native-mission/hub-native-validation.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"passed":true,
                "frames":3,"pixels":3*320*512,"different_pixels":0,"packets":packet_count,
                "frame_sha256":hashes,"frame_binding":"callback N -> OT N+1 -> screenshot N+2", "inactive_ticks":187,"active_ticks_before_capture":305,
                "scope":"All native hub layers and evolving state initialized from original assets and PRNG seed 0. Only elapsed frame counts, controller inputs and frame clock supplied; no captured state, VRAM or GPU commands used for generation. Audio and CD timing excluded.",
                "normalization":"DMA addresses excluded; unused FT4 UV2/UV3 upper halfwords masked. Resident post-scene housekeeping excluded from packet comparison."})).unwrap()).unwrap();
    }

    #[test]
    #[ignore = "requires original assets and normal hub capture"]
    fn initialized_visuals_and_composed_hub_layers_match_original() {
        let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
        let data = GameData::load(&root.join("extracted/disc")).unwrap();
        let capture = root.join("analysis/menu-runtime/hub-render-001");
        let mut scene = HubScene::new(
            &data,
            MissionAssets::load(&data, 74).unwrap(),
            &mut Random { state: 0 },
        )
        .unwrap();
        scene.set_load_progress(712, 712).unwrap();
        let mut update = initialize_hub(
            MissionState::default(),
            MissionContext {
                options: 0x42,
                current_pair: [1, 0],
                ..Default::default()
            },
        );
        let init = scene.layers(&update.effects, 0, 0, 0).unwrap();
        assert!(
            init.remaining_effects
                .iter()
                .any(|e| matches!(e, Effect::BeginHubBackgroundFade))
        );
        let mut elapsed = 0;
        while update.state.phase != 2 {
            update = tick_hub(update.state, update.mission, update.context, 0).unwrap();
            scene.layers(&update.effects, elapsed, 0, 0).unwrap();
            elapsed += 1;
            assert!(elapsed < 100);
        }
        assert_eq!(elapsed, 87);
        assert_eq!(scene.color.rgb, [137, 129, 79]);
        assert_eq!(scene.visual.offset(1).unwrap(), [0, 1]);
        assert_eq!(scene.visual.offset(2).unwrap(), [-1, 0]);
        let mut checked_packets = 0;
        for frame in 0..3 {
            // These observed moving-state inputs isolate compositor wiring.
            // UI state, textures, scripts and text remain initialized natively.
            let before =
                fs::read(capture.join(format!("render-{frame:05}-callback-entry-hub-state.bin")))
                    .unwrap();
            let circle = fs::read(
                capture.join(format!("render-{frame:05}-callback-entry-circle-state.bin")),
            )
            .unwrap();
            scene.circle.flags = word(&circle, 0);
            for i in 0..4 {
                scene.circle.speeds[i] = (word(&circle, 4 + i / 2 * 4) >> (i % 2 * 16)) as i16;
                scene.circle.angles[i] = (word(&circle, 12 + i / 2 * 4) >> (i % 2 * 16)) as i16;
                scene.circle.centers[i] = [
                    (word(&circle, 20 + i / 2 * 4) >> (i % 2 * 16)) as i16,
                    (word(&circle, 28 + i / 2 * 4) >> (i % 2 * 16)) as i16,
                ];
            }
            let map = scene.minimap.as_mut().unwrap();
            for i in 0..32 {
                map.pending[i] = word(&before, 0x18 + i * 4);
                map.revealed[i] = word(&before, 0x98 + i * 4);
            }
            update = tick_hub(
                HubState::from_bytes(&before).unwrap(),
                update.mission,
                update.context,
                0,
            )
            .unwrap();
            let meta: serde_json::Value = serde_json::from_slice(
                &fs::read(capture.join(format!("render-{frame:05}-callback-entry.json"))).unwrap(),
            )
            .unwrap();
            let layers = scene
                .layers(
                    &update.effects,
                    meta["context_frame"].as_i64().unwrap() as i32,
                    0,
                    0,
                )
                .unwrap();
            assert!(layers.remaining_effects.is_empty());
            let ot: serde_json::Value = serde_json::from_slice(
                &fs::read(capture.join(format!("ot-{:05}-{:03}.json", frame + 1, frame + 2)))
                    .unwrap(),
            )
            .unwrap();
            let mut original: Vec<Vec<u32>> = ot["packets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    p["words"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap() as u32)
                        .collect()
                })
                .collect();
            for packet in &mut original {
                normalize(packet);
            }
            let start = original
                .iter()
                .position(|p| p == &layers.packets[0])
                .unwrap();
            for (index, native) in layers.packets.iter().enumerate() {
                assert_eq!(
                    *native,
                    original[start + index],
                    "frame {frame}, packet {index}, OT index {}",
                    start + index
                );
            }
            checked_packets += layers.packets.len();
        }
        fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
        fs::write(root.join("analysis/native-mission/hub-layer-validation.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"passed": true,
                "normal_frames": 3, "generated_packets": checked_packets,
                "native_entry_ticks": elapsed,
                "scope": "Native initialized visual state, assets, scripts and text; original moving hub/circle inputs for callback/compositor comparison. All 2D packets and order match; 3D background and audio excluded.",
                "normalization": "DMA addresses excluded; FT4 UV2/UV3 unused upper halfwords masked."})).unwrap()).unwrap();
    }
}
