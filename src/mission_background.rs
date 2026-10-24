//! Mission scene RGB and the six inline palette uploads from 800E8574.
//! Map projection supplies depth inputs; this layer does not infer them from frames.

use ac3_graphics::{Error, Result, VRAM_HEIGHT, VRAM_WIDTH, Vram};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneColor {
    pub rgb: [u8; 3],
    /// Native counterpart of display-state flag mask 0x08, not other display flags.
    pub dirty: bool,
}

impl SceneColor {
    /// 800E79D4 stores only the low byte of each argument and sets flag 0x08.
    pub fn set(&mut self, rgb: [i32; 3]) {
        self.rgb = rgb.map(|v| v as u8);
        self.dirty = true;
    }

    /// The RGB-update part of resident 80025720 consumes flag 0x08 and sets
    /// the draw environments' clear color. Other display flags are out of scope.
    pub fn take_draw_color(&mut self) -> Option<[u8; 3]> {
        if self.dirty {
            self.dirty = false;
            Some(self.rgb)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingPaletteDepth {
    /// RTPT's IR0 from the current ring's first three vertices, stored at scratch+104.
    pub depth_cue: u16,
    /// The previous ring's first vertex SZ, copied to scratch+C4.
    pub previous_ring_first_depth: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GradientUpload {
    /// Bucket in the map's separate 256-entry depth OT, not the menu's 16-entry OT.
    pub ot_bucket: u8,
    /// A0 command, destination, 16x1 extent, eight packed color words, cache clear.
    pub words: [u32; 12],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaletteGradient {
    destination: [u16; 2],
    colors: [u16; 16],
}

impl PaletteGradient {
    /// Original 800E7E20 snapshots this palette once after texture loading. Do
    /// this before rendering mutates the same VRAM location with gradient uploads.
    pub fn from_vram(vram: &Vram, destination: [u16; 2]) -> Result<Self> {
        let [x, y] = destination.map(usize::from);
        if x + 16 > VRAM_WIDTH || y >= VRAM_HEIGHT {
            return Err(Error::Invalid("mission gradient palette rectangle"));
        }
        let start = y * VRAM_WIDTH + x;
        let colors = std::array::from_fn(|i| vram.words()[start + i]);
        Ok(Self {
            destination,
            colors,
        })
    }

    pub fn colors(&self) -> &[u16; 16] {
        &self.colors
    }

    /// Emit one ring's inline CLUT update. Insert it after that ring's quads
    /// using original OT prepend semantics; its execution position is depth-dependent.
    pub fn upload(&self, rgb: [u8; 3], depth: RingPaletteDepth) -> Result<GradientUpload> {
        if depth.depth_cue > 4096 {
            return Err(Error::Invalid(
                "mission palette IR0 outside projected 0..4096",
            ));
        }
        let mut words = [0_u32; 12];
        words[0] = 0xa0000000;
        words[1] = u32::from(self.destination[0]) | (u32::from(self.destination[1]) << 16);
        words[2] = 0x00010010;
        for (i, pair) in self.colors.chunks_exact(2).enumerate() {
            words[3 + i] = u32::from(interpolate_rgb555(pair[0], rgb, depth.depth_cue))
                | (u32::from(interpolate_rgb555(pair[1], rgb, depth.depth_cue)) << 16);
        }
        words[11] = 0x01000000;
        Ok(GradientUpload {
            ot_bucket: ((u32::from(depth.previous_ring_first_depth) >> 8) + 5).min(255) as u8,
            words,
        })
    }

    /// Six ring uploads in source emission order (rings 1..6). They must remain
    /// interleaved with map geometry; this array is not a complete GP0 draw list.
    pub fn uploads(
        &self,
        rgb: [u8; 3],
        depths: [RingPaletteDepth; 6],
    ) -> Result<[GradientUpload; 6]> {
        let mut output = [GradientUpload {
            ot_bucket: 0,
            words: [0; 12],
        }; 6];
        for (out, depth) in output.iter_mut().zip(depths) {
            *out = self.upload(rgb, depth)?;
        }
        Ok(output)
    }
}

// Exact restricted IRGB -> INTPL(sf=12,lm=0) -> ORGB path at 800E8F64..800E8F88.
// RGB8*16 and RGB5*128 fit 0..4080; their difference cannot saturate signed IR.
// With projected IR0 0..4096 all interpolated IRs stay nonnegative and bounded,
// so the two arithmetic shifts compose into >>19. STP is discarded by IRGB.
fn interpolate_rgb555(color: u16, far: [u8; 3], ir0: u16) -> u16 {
    let mut output = 0;
    for (axis, far) in far.into_iter().enumerate() {
        let source = i32::from((color >> (axis * 5)) & 31) * 128;
        let difference = i32::from(far) * 16 - source;
        let value = ((source << 12) + i32::from(ir0) * difference) >> 19;
        output |= (value.clamp(0, 31) as u16) << (axis * 5);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_stores_low_bytes_and_dirty_update_is_consumed_once() {
        let mut color = SceneColor::default();
        assert_eq!(color.take_draw_color(), None);
        color.set([-1, 256, 257]);
        assert_eq!(color.rgb, [255, 0, 1]);
        assert_eq!(color.take_draw_color(), Some([255, 0, 1]));
        assert_eq!(color.take_draw_color(), None);
        color.set([-1, 256, 257]);
        assert!(color.dirty); // Writing equal RGB still marks the original dirty bit.
    }

    #[test]
    fn interpolation_preserves_endpoints_and_discards_stp() {
        assert_eq!(interpolate_rgb555(0xffff, [0, 0, 0], 0), 0x7fff);
        assert_eq!(interpolate_rgb555(0, [255, 255, 255], 4096), 0x7fff);
        assert_eq!(interpolate_rgb555(0x7fff, [0, 0, 0], 4096), 0);
        assert_eq!(interpolate_rgb555(1, [0, 0, 0], 2048), 0);
        assert_eq!(interpolate_rgb555(0, [8, 0, 0], 2048), 0);
        assert_eq!(interpolate_rgb555(0, [16, 0, 0], 2048), 1);
        assert_eq!(interpolate_rgb555(0, [0, 0, 255], 4096), 31 << 10);
    }

    #[test]
    fn palette_bounds_packet_layout_and_depth_bucket_saturation() {
        let mut vram = Vram::new();
        let bytes: Vec<u8> = (0_u16..16).flat_map(u16::to_le_bytes).collect();
        vram.load_block(&ac3_graphics::tim::Block {
            x: 1008,
            y: 511,
            width: 16,
            height: 1,
            data: &bytes,
        })
        .unwrap();
        assert!(PaletteGradient::from_vram(&vram, [1009, 511]).is_err());
        assert!(PaletteGradient::from_vram(&vram, [0, 512]).is_err());
        let gradient = PaletteGradient::from_vram(&vram, [1008, 511]).unwrap();
        let depth = RingPaletteDepth {
            depth_cue: 0,
            previous_ring_first_depth: 65535,
        };
        let packet = gradient.upload([0; 3], depth).unwrap();
        assert_eq!(packet.ot_bucket, 255);
        assert_eq!(packet.words[0..3], [0xa0000000, 0x01ff03f0, 0x00010010]);
        assert_eq!(packet.words[3], 0x00010000);
        assert_eq!(packet.words[10], 0x000f000e);
        assert_eq!(packet.words[11], 0x01000000);
        assert!(
            gradient
                .upload(
                    [0; 3],
                    RingPaletteDepth {
                        depth_cue: 4097,
                        ..depth
                    }
                )
                .is_err()
        );
        let zero = gradient
            .upload(
                [0; 3],
                RingPaletteDepth {
                    previous_ring_first_depth: 0,
                    ..depth
                },
            )
            .unwrap();
        assert_eq!(zero.ot_bucket, 5);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR, local assets and original hub projection/render captures"]
    fn original_six_palettes_match_using_captured_projection_inputs() {
        let root =
            std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
        let data = crate::assets::GameData::load(&root.join("extracted/disc")).unwrap();
        let assets = crate::mission_assets::MissionAssets::load(&data, 74).unwrap();
        // Resident overlay RECT at 800F6808; source TIMs populate this location.
        let overlay = data.overlay(2).unwrap();
        let at = (0x800f6808 - crate::assets::OVERLAY_BASE) as usize;
        let destination = std::array::from_fn(|i| {
            u16::from_le_bytes(overlay[at + i * 2..at + i * 2 + 2].try_into().unwrap())
        });
        let gradient = PaletteGradient::from_vram(&assets.vram, destination).unwrap();
        let render = root.join("analysis/menu-runtime/hub-render-001");
        let map = std::fs::read(render.join("reference-map-state.bin")).unwrap();
        let original_base: [u16; 16] = std::array::from_fn(|i| {
            u16::from_le_bytes(map[0xbc + i * 2..0xbe + i * 2].try_into().unwrap())
        });
        assert_eq!(*gradient.colors(), original_base);
        let ram = std::fs::read(render.join("checkpoint.ram.bin")).unwrap();
        let display =
            u32::from_le_bytes(ram[0xbacb8..0xbacbc].try_into().unwrap()) as usize & 0x1fffff;
        let rgb = ram[display + 16..display + 19].try_into().unwrap();
        let projection: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                root.join("analysis/menu-runtime/hub-projection-003/projection-map.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let rings: Vec<&serde_json::Value> = projection["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["pc"].as_u64() == Some(0x800e8af0))
            .step_by(5)
            .collect();
        assert_eq!(rings.len(), 7);
        let depths = std::array::from_fn(|i| RingPaletteDepth {
            depth_cue: u16::try_from(rings[i + 1]["expected_p"].as_u64().unwrap()).unwrap(),
            previous_ring_first_depth: u16::try_from(
                rings[i]["expected_data"][17].as_u64().unwrap(),
            )
            .unwrap(),
        });
        let generated = gradient.uploads(rgb, depths).unwrap();
        // Same checkpoint/callback frame 0; DrawOTag frame 1 submits its output.
        let ot: serde_json::Value =
            serde_json::from_slice(&std::fs::read(render.join("ot-00001-002.json")).unwrap())
                .unwrap();
        let original: Vec<[u32; 12]> = ot["packets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["words"][0].as_u64() == Some(0xa0000000))
            .map(|p| {
                assert_eq!(p["words"].as_array().unwrap().len(), 12);
                std::array::from_fn(|i| p["words"][i].as_u64().unwrap() as u32)
            })
            .collect();
        assert_eq!(original, generated.map(|p| p.words));
        assert_eq!(generated.map(|p| p.ot_bucket), [96, 85, 73, 61, 49, 37]);
    }
}
