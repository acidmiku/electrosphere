//! Original overlay-2 aircraft resources and 0x800D8CB4 statistics renderer.
//! Input is typed native selection state and the user's verified game data.
use crate::{
    Result,
    assets::{GameData, OVERLAY_BASE},
    menu_scene::LanguageScene,
    mission_assets::TextureUpload,
};
use ac3_formats::{
    dot1::{self, Container},
    ulz,
};
use ac3_graphics::{Vram, tim};
use ac3_mission::{AircraftSelectionState, Effect};

pub struct AircraftScene<'a> {
    image_tables: [Container<'a>; 2],
    axes: [[i16; 2]; 6],
}
pub struct AircraftCommands {
    /// Original allocation/insertion order into the outer OT's bucket0.
    /// Execute in reverse order, after shared UI/text packets inserted later.
    pub packets: Vec<Vec<u32>>,
    /// D87F8 clears visibility and selects the aircraft-label entry. Apply at
    /// the draw boundary before the reducer's later weapon-visibility effects.
    pub effects: Vec<Effect>,
}
fn half(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn word(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn xy(x: i16, y: i16) -> u32 {
    u32::from(x as u16) | (u32::from(y as u16) << 16)
}
fn hundredths(value: i32) -> Vec<u8> {
    // Original "%2d.%2d", then replace byte3 when it is a space. The fixed
    // byte index matters for values wider than the minimum printf field width.
    let mut text = format!("{:>2}.{:>2}", value / 100, value % 100).into_bytes();
    if text.get(3) == Some(&b' ') {
        text[3] = b'0';
    }
    text.push(0);
    text
}
impl<'a> AircraftScene<'a> {
    pub fn load(data: &'a GameData) -> Result<Self> {
        let common = dot1::parse(data.resource(72)?)?;
        let images = [
            dot1::parse(common.child(1)?)?,
            dot1::parse(common.child(2)?)?,
        ];
        if images.iter().any(|table| table.len() != 21) {
            return Err("unexpected aircraft TIM table count".into());
        }
        let overlay = data.overlay(2)?;
        let base = (0x800f5fd8 - OVERLAY_BASE) as usize;
        let bytes = overlay
            .get(base..base + 24)
            .ok_or("aircraft radar axes outside overlay")?;
        Ok(Self {
            image_tables: images,
            axes: std::array::from_fn(|i| [half(bytes, i * 4), half(bytes, i * 4 + 2)]),
        })
    }

    /// 800CE978 called twice: common child1 image then child2 label TIM.
    /// Clear the first rectangle before either upload; preserve pixel/CLUT order.
    /// Capacity arguments 0x3820/0x828 bound decompressed data, not table stride.
    pub fn load_resources(
        &self,
        index: i32,
        rectangle: [i16; 4],
        vram: &mut Vram,
    ) -> Result<Vec<TextureUpload>> {
        let index = usize::try_from(index).map_err(|_| "negative aircraft image index")?;
        let [x, y, width, height] = rectangle;
        let (x, y, width, height) = (
            u16::try_from(x)?,
            u16::try_from(y)?,
            u16::try_from(width)?,
            u16::try_from(height)?,
        );
        if usize::from(x) + usize::from(width) > ac3_graphics::VRAM_WIDTH
            || usize::from(y) + usize::from(height) > ac3_graphics::VRAM_HEIGHT
        {
            return Err("aircraft clear rectangle outside VRAM".into());
        }
        for table in &self.image_tables {
            table.child(index)?;
        }
        let clear = vec![0; usize::from(width) * usize::from(height) * 2];
        vram.load_block(&tim::Block {
            x,
            y,
            width,
            height,
            data: &clear,
        })?;
        let mut uploads = Vec::new();
        for (table, capacity) in self.image_tables.iter().zip([0x3820, 0x828]) {
            let input = table.child(index)?;
            let decoded;
            let bytes = if input.starts_with(b"Ulz\x1a") {
                decoded = ulz::decompress(input)?.payload;
                if decoded.len() > capacity {
                    return Err("aircraft TIM exceeds original decompression capacity".into());
                }
                &decoded[..]
            } else {
                input
            };
            let image = tim::parse(bytes)?;
            vram.load_tim(&image)?;
            for block in [Some(image.pixels), image.clut].into_iter().flatten() {
                uploads.push(TextureUpload {
                    x: block.x,
                    y: block.y,
                    width: block.width,
                    height: block.height,
                    data: block.data.to_vec(),
                });
            }
        }
        Ok(uploads)
    }

    /// 800D83FC/85B8/87F8/8ABC, at AircraftUpdate.draw_state's exact boundary.
    pub fn commands(
        &self,
        scene: &LanguageScene<'_>,
        state: &AircraftSelectionState,
    ) -> Result<AircraftCommands> {
        let mut packets = Vec::new();
        let origin = [state.panel_offsets[0].wrapping_add(0xec), 0x134_i16];
        let point = |at: usize| {
            xy(
                self.axes[at][0].wrapping_add(origin[0]),
                self.axes[at][1].wrapping_add(origin[1]),
            )
        };
        for i in 1..6 {
            packets.push(vec![0x40404040, point(0), point(i)]);
        }
        for i in 0..5 {
            packets.push(vec![0x40404040, point(i + 1), point((i + 1) % 5 + 1)]);
        }
        let statistics = [2, 4, 10, 6, 8].map(|offset| i32::from(half(&state.current.0, offset)));
        let statistic_point = |at: usize| {
            let value = statistics[at];
            let axis = self.axes[at + 1];
            xy(
                ((i32::from(axis[0]).wrapping_mul(value) / 240) as i16).wrapping_add(origin[0]),
                ((i32::from(axis[1]).wrapping_mul(value) / 240) as i16).wrapping_add(origin[1]),
            )
        };
        for i in 0..5 {
            packets.push(vec![
                0x20178bcf,
                point(0),
                statistic_point(i),
                statistic_point((i + 1) % 5),
            ]);
        }
        let font = if state.flags & 0x80000 != 0 { 24 } else { 23 };
        let x = state.panel_offsets[1].wrapping_add(0x4e);
        for (offset, y) in [(16, 0xf9), (12, 0x105), (20, 0x111)] {
            scene.font_text(
                font,
                &hundredths(word(&state.current.0, offset)),
                x,
                y,
                &mut packets,
            )?;
        }
        for (offset, y, dx) in [(24, 0x11d, 0x4e), (28, 0x129, 0x44)] {
            let text = format!("{:>5}\0", word(&state.current.0, offset));
            scene.font_text(
                font,
                text.as_bytes(),
                state.panel_offsets[1].wrapping_add(dx),
                y,
                &mut packets,
            )?;
        }
        let mut effects = vec![Effect::ClearTextVisibility];
        let label = state.current.0[0x21];
        if label != 0 {
            effects.push(Effect::SetTextVisibility {
                entry: i32::from(label) + 0xa3,
                value: 1,
            });
        }
        let weapon = state
            .weapon_current
            .get(usize::try_from(state.weapon_row).map_err(|_| "negative selected weapon row")?)
            .ok_or("selected weapon row outside two-row state")?;
        for (value, y) in weapon.0[4..7].iter().zip([0x13a, 0x14d, 0x160]) {
            let width = u32::from(*value) * 0x26 / 0xf0;
            packets.push(vec![
                0x60d5e5e5,
                xy(state.panel_offsets[3].wrapping_add(0x10a), y),
                width | (8 << 16),
            ]);
        }
        Ok(AircraftCommands { packets, effects })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_fixed_hundredths_padding_is_not_generic_decimal_formatting() {
        assert_eq!(hundredths(103), b" 1.03\0");
        assert_eq!(hundredths(-103), b"-1.-3\0");
        assert_eq!(hundredths(12303), b"123. 3\0");
    }
}
