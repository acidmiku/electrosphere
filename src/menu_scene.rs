//! Translation of the language page's asset/layout path, not a CPU interpreter.
//! Input files supply tables and textures; generated draw commands are native.
use crate::{Result, assets::GameData};
use ac3_formats::dot1::{self, Container};
use ac3_graphics::{Frame, Vram, tim, transform::SinCosTable};
use ac3_menu::{Effect, LanguageRow};

pub struct LanguageScene<'a> {
    overlay: &'a [u8],
    trigonometry: SinCosTable,
    scripts: Container<'a>,
    builtin_scripts: Option<Container<'a>>,
    builtin_active: bool,
    strings: Container<'a>,
    layouts: &'a [u8],
    tables: DrawTables,
    pub vram: Vram,
}

struct DrawTables {
    font_info: u32,
    character_map: u32,
    opcode_lengths: u32,
    opcode_conditions: u32,
    top_mask: i16,
    animate_menu_rotation: bool,
}
const FRONT_END_TABLES: DrawTables = DrawTables {
    font_info: 0x800d7afc,
    character_map: 0x800d7c8c,
    opcode_lengths: 0x800d884c,
    opcode_conditions: 0x800d88bc,
    top_mask: -1,
    animate_menu_rotation: true,
};
const MISSION_TABLES: DrawTables = DrawTables {
    font_info: 0x800f5104,
    character_map: 0x800f5294,
    opcode_lengths: 0x800f68d0,
    opcode_conditions: 0x800f6940,
    top_mask: -2,
    animate_menu_rotation: false,
};

pub struct MenuFrame {
    pub frame: Frame,
    /// GP0 words in final ordering-table order, without address-dependent tags.
    pub packets: Vec<Vec<u32>>,
}

/// Native draw lists before address-free ordering-table composition. Both bins
/// and text packets retain insertion order, matching the original prepends.
pub struct SceneCommands {
    pub bins: [Vec<Vec<u32>>; 16],
    pub fonts: Vec<Vec<u32>>,
}
impl SceneCommands {
    pub fn into_packets(self) -> Vec<Vec<u32>> {
        self.bins
            .into_iter()
            .rev()
            .flat_map(|bin| bin.into_iter().rev())
            .chain(self.fonts.into_iter().rev())
            .collect()
    }
}

fn word(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or("menu table word outside input")?;
    Ok(u32::from_le_bytes(bytes.try_into()?))
}
fn half(word: u32, high: bool) -> i16 {
    (word >> if high { 16 } else { 0 }) as i16
}
fn xy(x: i16, y: i16) -> u32 {
    u32::from(x as u16) | (u32::from(y as u16) << 16)
}

impl<'a> LanguageScene<'a> {
    pub fn load(data: &'a GameData) -> Result<Self> {
        let startup = dot1::parse(data.resource(0)?)?;
        let menu = dot1::parse(data.resource(0x45)?)?;
        if startup.len() != 5 || menu.len() != 5 {
            return Err("unexpected PAL menu resource tables".into());
        }
        let mut vram = Vram::new();
        // 0x800259F8: resource-zero children0/3 contain TIM lists;1/2 are TIMs.
        for index in 0..4 {
            let child = startup.child(index)?;
            if index == 0 || index == 3 {
                let list = dot1::parse(child)?;
                for id in 0..list.len() {
                    vram.load_tim(&tim::parse(list.child(id)?)?)?;
                }
            } else {
                vram.load_tim(&tim::parse(child)?)?;
            }
        }
        // 0x800CB964 -> texture worker: all107 textures, in stored order.
        let textures = dot1::parse(menu.child(3)?)?;
        for id in 0..textures.len() {
            vram.load_tim(&tim::parse(textures.child(id)?)?)?;
        }
        Ok(Self {
            overlay: data.overlay(1)?,
            trigonometry: SinCosTable::from_le_bytes(
                &data.executable.payload[0xa4f84..0xa4f84 + 4096 * 4],
            )?,
            scripts: dot1::parse(menu.child(0)?)?,
            builtin_scripts: None,
            builtin_active: false,
            strings: dot1::parse(menu.child(1)?)?,
            layouts: menu.child(2)?,
            tables: FRONT_END_TABLES,
            vram,
        })
    }

    /// Shared asset-script/text layer of overlay2. This does not generate the
    /// separate map, backdrop, briefing or aircraft renderer commands.
    pub fn from_mission(
        data: &'a GameData,
        assets: crate::mission_assets::MissionAssets<'a>,
    ) -> Result<Self> {
        Ok(Self {
            overlay: data.overlay(2)?,
            trigonometry: SinCosTable::from_le_bytes(
                &data.executable.payload[0xa4f84..0xa4f84 + 4096 * 4],
            )?,
            scripts: assets.scripts,
            builtin_scripts: Some(dot1::parse(
                &data.overlay(2)?[(0x800f1e50 - crate::assets::OVERLAY_BASE) as usize..],
            )?),
            builtin_active: false,
            strings: assets.strings,
            layouts: assets.layouts,
            vram: assets.vram,
            tables: MISSION_TABLES,
        })
    }

    pub fn visual_state(&self) -> Result<MenuVisualState> {
        let mut visual = self.blank_visual_state();
        self.script(1, &mut visual.state, 0, &mut 10000)?;
        visual.state.reset_frame();
        Ok(visual)
    }

    /// 800F197C's zero/identity/color initialization before the loader binds a
    /// script table. No script is executed and no captured state is consumed.
    pub fn blank_visual_state(&self) -> MenuVisualState {
        let mut state = ScriptState::new(ac3_menu::language_rows(0, 0));
        state.flags.fill(0);
        state.offsets.fill((0, 0));
        MenuVisualState {
            state,
            page: 0,
            visible: [0; 256],
            fonts: std::array::from_fn(|i| {
                u16::from_le_bytes([self.layouts[i * 24], self.layouts[i * 24 + 1]])
            }),
        }
    }

    /// 800F1A9C switches between overlay-local loading scripts and the common
    /// resource's scripts. Both tables are parsed without relocating guest RAM.
    pub fn use_mission_loading_scripts(&mut self, enabled: bool) -> Result<()> {
        if self.builtin_scripts.is_none() {
            return Err("loading scripts require the mission scene".into());
        }
        self.builtin_active = enabled;
        Ok(())
    }

    /// Persistent original visual/text state with frame-local draw effects.
    pub fn render_menu(
        &self,
        visual: &mut MenuVisualState,
        language: usize,
        frame_counter: i32,
        effects: &[Effect],
    ) -> Result<MenuFrame> {
        visual.apply(effects, frame_counter)?;
        if effects
            .iter()
            .any(|e| matches!(e, Effect::DrawLanguageRows { .. }))
        {
            return self.render(effects);
        }
        let mut fonts = Vec::new();
        for effect in effects {
            if let Effect::DrawScreenFade { shade } = effect {
                fade_packets(*shade, &mut fonts);
            }
        }
        let result = self.render_visual_layer(visual, language, fonts)?;
        // 0x800CB4C8 writes rotation angles after submitting this frame.
        if self.tables.animate_menu_rotation {
            visual.state.scales[1][1] = (visual.state.scales[1][1] & 0xffff)
                | ((frame_counter.wrapping_mul(64) as u16 as u32) << 16);
            visual.state.scales[2][1] = (visual.state.scales[2][1] & 0xffff)
                | ((frame_counter.wrapping_mul(-64) as u16 as u32) << 16);
        }
        Ok(result)
    }

    /// Generate the mission's shared asset-script and text layer. The host
    /// separately handles map/backdrop/audio effects and composes their packets.
    pub fn render_mission_layer(
        &self,
        visual: &mut MenuVisualState,
        language: usize,
        effects: &[ac3_mission::Effect],
    ) -> Result<MenuFrame> {
        visual.apply_mission_visuals(effects)?;
        self.render_visual_layer(visual, language, Vec::new())
    }

    fn render_visual_layer(
        &self,
        visual: &mut MenuVisualState,
        language: usize,
        fonts: Vec<Vec<u32>>,
    ) -> Result<MenuFrame> {
        let packets = self
            .visual_commands(visual, language, fonts)?
            .into_packets();
        let mut frame = Frame::new(320, 512)?;
        ac3_graphics::render_packets(&mut frame, &self.vram, &packets)?;
        Ok(MenuFrame { frame, packets })
    }

    pub fn mission_commands(
        &self,
        visual: &mut MenuVisualState,
        language: usize,
    ) -> Result<SceneCommands> {
        self.visual_commands(visual, language, Vec::new())
    }

    fn visual_commands(
        &self,
        visual: &mut MenuVisualState,
        language: usize,
        mut fonts: Vec<Vec<u32>>,
    ) -> Result<SceneCommands> {
        visual.state.reset_frame();
        self.script(visual.page, &mut visual.state, 0, &mut 10000)?;
        let bank = dot1::parse(self.strings.child(language)?)?;
        for index in 0..256 {
            if visual.visible[index] == 0 {
                continue;
            }
            let record = &self.layouts[index * 24..index * 24 + 24];
            if record[3] & 1 != 0 {
                return Err("formatted menu text remains unported".into());
            }
            let at = if record[3] & 2 != 0 {
                4 + language * 4
            } else {
                4
            };
            let position = word(record, at)?;
            let (dx, dy) = visual.state.offsets[record[2] as usize];
            self.font_text(
                visual.fonts[index],
                bank.child(index)?,
                half(position, false).wrapping_add(dx),
                half(position, true).wrapping_add(dy),
                &mut fonts,
            )?;
        }
        Ok(self.finish_commands(&mut visual.state, fonts))
    }

    fn finish_commands(&self, state: &mut ScriptState, fonts: Vec<Vec<u32>>) -> SceneCommands {
        state.bins[15].push(vec![
            0x60000000 | state.frame_color,
            xy(0, self.tables.top_mask),
            xy(320, 4),
        ]);
        state.bins[15].push(vec![0x60000000 | state.frame_color, xy(0, 511), xy(320, 4)]);
        SceneCommands {
            bins: std::mem::take(&mut state.bins),
            fonts,
        }
    }

    /// Append original-format glyph packets for mission/UI callers. Packets
    /// have insertion order; the containing ordering table reverses that order.
    pub fn font_text(
        &self,
        font: u16,
        text: &[u8],
        origin_x: i16,
        origin_y: i16,
        packets: &mut Vec<Vec<u32>>,
    ) -> Result<()> {
        if font > 24 {
            return Err(format!("font family {font} not yet connected").into());
        }
        let info = self.rom(self.tables.font_info + u32::from(font) * 16, 16)?;
        let glyph_address = word(info, 12)?;
        let length = text
            .iter()
            .position(|&b| b == 0)
            .ok_or("unterminated menu string")?;
        let mut x = origin_x;
        let mut y = origin_y;
        for at in 0..length {
            let c = text[at];
            if c == b'\n' {
                y = y.wrapping_add(1 + i16::from(info[4] as i8));
                x = origin_x;
                continue;
            }
            if c == b'\r' {
                continue;
            }
            let glyph = self.rom(self.tables.character_map + u32::from(c), 1)?[0];
            let metrics = self.rom(glyph_address + u32::from(glyph) * 4, 4)?;
            let width = if matches!(font, 6 | 8) && (glyph == 0 || (0x10..0x1a).contains(&glyph)) {
                5
            } else if matches!(font, 16 | 17) && c == 7 {
                6
            } else {
                metrics[2]
            };
            let height = if (matches!(font, 16 | 17) && (1..=6).contains(&c))
                || (matches!(font, 18..=20) && glyph >= 0x9c)
            {
                20
            } else {
                info[3]
            };
            if at > 0 {
                x = x.wrapping_add(crate::menu_fonts::pre_adjust(font, text[at - 1], c));
            }
            let uv = (u32::from(info[0]) + u32::from(metrics[0]))
                | ((u32::from(info[1]) + u32::from(metrics[1])) << 8)
                | (u32::from(u16::from_le_bytes([info[8], info[9]])) << 16);
            packets.push(vec![
                0x66808080,
                xy(x, y.wrapping_add(i16::from(metrics[3] as i8))),
                uv,
                u32::from(width) | (u32::from(height) << 16),
            ]);
            x = x
                .wrapping_add(i16::from(width))
                .wrapping_add(crate::menu_fonts::kerning(font, c, text[at + 1]));
        }
        packets.push(vec![
            0xe1000000 | (u32::from(u16::from_le_bytes([info[6], info[7]])) & 0x9ff),
        ]);
        Ok(())
    }

    fn rom(&self, address: u32, length: usize) -> Result<&'a [u8]> {
        let offset = address
            .checked_sub(0x800c9dc0)
            .ok_or("pointer below menu overlay")? as usize;
        self.overlay
            .get(offset..offset + length)
            .ok_or_else(|| "pointer outside menu overlay".into())
    }

    /// Draw the pre-input state requested by the translated callback effects.
    pub fn render(&self, effects: &[Effect]) -> Result<MenuFrame> {
        let mut rows = None;
        let mut font_packets = Vec::new();
        for effect in effects {
            match effect {
                Effect::DrawLanguageRows { rows: requested } => rows = Some(*requested),
                Effect::DrawLanguageFade { shade } => {
                    font_packets.push(vec![0xe1000000, 0]);
                    font_packets.push(vec![
                        0x62000000 | (u32::from(*shade) * 0x10101),
                        0,
                        0x01ff0140,
                    ]);
                    font_packets.push(vec![0xe1000040, 0]);
                }
                _ => (),
            }
        }
        let rows = rows.ok_or("language frame has no draw effect")?;
        for (index, row) in rows.iter().enumerate() {
            self.font_row(index, row, &mut font_packets)?;
        }
        let mut state = ScriptState::new(rows);
        let mut budget = 10000;
        // 0x800CBA6C selects page1 while resources finish loading. Its script
        // initializes the persistent color/transform tables before language page2.
        self.script(1, &mut state, 0, &mut budget)?;
        for bin in &mut state.bins {
            bin.clear();
        }
        self.script(2, &mut state, 0, &mut budget)?;
        // 0x800D5724 masks the top/bottom four scanlines in renderer OT slot15.
        state.bins[15].push(vec![0x60000000, xy(0, -1), xy(320, 4)]);
        state.bins[15].push(vec![0x60000000, xy(0, 511), xy(320, 4)]);
        let mut packets = Vec::new();
        for bin in state.bins.into_iter().rev() {
            packets.extend(bin.into_iter().rev());
        }
        packets.extend(font_packets.into_iter().rev());
        let mut frame = Frame::new(320, 512)?;
        ac3_graphics::render_packets(&mut frame, &self.vram, &packets)?;
        Ok(MenuFrame { frame, packets })
    }

    fn font_row(&self, index: usize, row: &LanguageRow, packets: &mut Vec<Vec<u32>>) -> Result<()> {
        // Font2: 0x800CFAF0. Font metrics, glyph map, and language labels stay in data.
        let info = self.rom(0x800d7afc + 2 * 16, 16)?;
        let glyph_address = word(info, 12)?;
        let text_address = word(self.rom(0x800d8778, 20)?, index * 4)?;
        let text = self.rom(text_address, 64)?;
        let length = text
            .iter()
            .position(|&b| b == 0)
            .ok_or("unterminated language label")?;
        let mut x = row.x as i16;
        for at in 0..length {
            let character = text[at];
            let glyph = self.rom(0x800d7c8c + u32::from(character), 1)?[0];
            let metrics = self.rom(glyph_address + u32::from(glyph) * 4, 4)?;
            let color = 0x66808080;
            let uv = u32::from(info[0].wrapping_add(metrics[0]))
                | (u32::from(info[1].wrapping_add(metrics[1])) << 8)
                | (u32::from(u16::from_le_bytes([info[8], info[9]])) << 16);
            packets.push(vec![
                color,
                xy(x, (row.y as i16).wrapping_add(i16::from(metrics[3] as i8))),
                uv,
                u32::from(metrics[2]) | (u32::from(info[3]) << 16),
            ]);
            x = x
                .wrapping_add(i16::from(metrics[2]))
                .wrapping_add(font2_kerning(character, text[at + 1]));
        }
        packets.push(vec![
            0xe1000000 | (u32::from(u16::from_le_bytes([info[6], info[7]])) & 0x9ff),
        ]);
        Ok(())
    }

    fn script(
        &self,
        index: usize,
        state: &mut ScriptState,
        depth: usize,
        budget: &mut usize,
    ) -> Result<()> {
        if depth >= 32 {
            return Err("menu script recursion exceeded".into());
        }
        let scripts = if self.builtin_active {
            self.builtin_scripts
                .as_ref()
                .ok_or("missing loading scripts")?
        } else {
            &self.scripts
        };
        let bytes = scripts.child(index)?;
        let mut cursor = 0;
        loop {
            if *budget == 0 {
                return Err("menu script instruction limit exceeded".into());
            }
            *budget -= 1;
            let op = word(bytes, cursor)? as usize;
            if op == 0 {
                return Ok(());
            }
            if op >= 28 {
                return Err(format!("invalid menu script opcode {op}").into());
            }
            let mut length =
                self.rom(self.tables.opcode_lengths + op as u32 * 4, 1)?[0] as usize * 4;
            if matches!(op, 26 | 27) {
                length += usize::try_from(half(word(bytes, cursor + 4)?, false))?
                    * if op == 26 { 24 } else { 12 };
            }
            if length < 4 {
                return Err("zero-length menu script command".into());
            }
            let command = bytes
                .get(cursor..cursor + length)
                .ok_or("truncated menu script command")?;
            cursor += length;
            let conditional = self.rom(self.tables.opcode_conditions + op as u32, 1)?[0] != 0;
            if conditional && state.condition != state.value {
                continue;
            }
            let a = word(command, 4)?;
            match op {
                1 => {
                    // 0x800D363C: rectangle, current color and translated offset.
                    let (x, y) = state.position(a)?;
                    let size = word(command, 8)?;
                    let rect = state.transform_rect([x, y, half(size, false), half(size, true)])?;
                    if !rect.draw {
                        continue;
                    }
                    let color = state.color()?;
                    state.emit(vec![
                        (if state.blend == -1 {
                            0x60000000
                        } else {
                            0x62000000
                        }) | color,
                        xy(rect.rect[0], rect.rect[1]),
                        xy(rect.rect[2], rect.rect[3]),
                    ])?;
                }
                2 => state.color_slot = a as usize,
                3 => {
                    *state
                        .colors
                        .get_mut((a & 255) as usize)
                        .ok_or("color index outside table")? = a >> 8;
                }
                4 => state.blend = a as i32,
                5 => state.bin = a as usize,
                6 => {
                    state.value = i32::from(half(a, true));
                    let index = usize::try_from(half(a, false))?;
                    state.condition_slot = index;
                    state.condition = i32::from(
                        *state
                            .flags
                            .get(index)
                            .ok_or("visual flag index outside table")?,
                    );
                }
                7 => {
                    // D3824/EFCB8: SPRT is gated by the transform's final
                    // threshold, but its rectangle is not interpolated/cropped.
                    let transform = state
                        .transforms
                        .get(state.transform_slot)
                        .ok_or("sprite transform outside table")?;
                    if transform[1] != 0 && transform[7] < transform[6] {
                        continue;
                    }
                    // Active tpage is supplied separately by opcode8.
                    let (x, y) = state.position(a)?;
                    let texture = word(command, 12)?;
                    let clut_depth = word(command, 20)?;
                    let depth = half(clut_depth, true);
                    let scale = match depth {
                        0 => 4,
                        1 => 2,
                        2 => 1,
                        _ => return Err("invalid sprite texture depth".into()),
                    };
                    let u = ((i32::from(half(texture, false)) % 64) * scale) as u8;
                    let v = half(texture, true) as u8;
                    state.emit(vec![
                        (if state.blend == -1 {
                            0x64000000
                        } else {
                            0x66000000
                        }) | state.color()?,
                        xy(x, y),
                        u32::from(u) | (u32::from(v) << 8) | ((clut_depth & 0xffff) << 16),
                        word(command, 8)?,
                    ])?;
                }
                8 => state.emit(vec![0xe1000000 | (a & 0x9ff), 0])?,
                9 => {
                    // 0x800D3AE0: textured rectangle, optionally rotated by
                    // 0x800D33C8. Texture dimensions are separate from screen size.
                    let (x, y) = state.position(a)?;
                    let size = word(command, 8)?;
                    let rect = state.transform_rect([x, y, half(size, false), half(size, true)])?;
                    if !rect.draw {
                        continue;
                    }
                    let [x, y, width, height] = rect.rect;
                    let texture = word(command, 12)?;
                    let texture_size = word(command, 16)?;
                    let clut_depth = word(command, 20)?;
                    let depth = half(clut_depth, true);
                    let scale = match depth {
                        0 => 4,
                        1 => 2,
                        2 => 1,
                        _ => return Err("invalid quad texture depth".into()),
                    };
                    let u = ((i32::from(half(texture, false)) % 64) * scale) as u8;
                    let v = half(texture, true) as u8;
                    let ur =
                        u.wrapping_add((i32::from(half(texture_size, false)) * scale - 1) as u8);
                    let vb = v.wrapping_add((half(texture_size, true) - 1) as u8);
                    let coords = self.quad_coordinates(state, x, y, width, height)?;
                    let page = texture_page(
                        texture,
                        depth,
                        if state.blend == -1 { 0 } else { state.blend },
                    );
                    state.emit(vec![
                        (if state.blend == -1 {
                            0x2c000000
                        } else {
                            0x2e000000
                        }) | state.color()?,
                        coords[0],
                        u32::from(u) | (u32::from(v) << 8) | ((clut_depth & 0xffff) << 16),
                        coords[1],
                        u32::from(ur) | (u32::from(v) << 8) | (page << 16),
                        coords[2],
                        u32::from(u) | (u32::from(vb) << 8),
                        coords[3],
                        u32::from(ur) | (u32::from(vb) << 8),
                    ])?;
                }
                11 => {
                    // 0x800D4AB0: nested script plus signed16 local translation.
                    let b = word(command, 8)?;
                    let transform = state
                        .transforms
                        .get(state.transform_slot)
                        .ok_or("nested script transform outside table")?;
                    if half(b, true) != 1
                        || (state.condition == state.value
                            && (state.transform_slot == 0 || transform[4] < transform[7]))
                    {
                        let dx = half(a, true);
                        let dy = half(b, false);
                        state.x = state.x.wrapping_add(dx);
                        state.y = state.y.wrapping_add(dy);
                        // Scratch+8 is the first nonzero nested origin. The
                        // original clears it after every nested return; it
                        // does not restore a parent origin from the stack.
                        if state.transform_origin == (0, 0) {
                            state.transform_origin = (state.x, state.y);
                        }
                        self.script(usize::try_from(half(a, false))?, state, depth + 1, budget)?;
                        state.x = state.x.wrapping_sub(dx);
                        state.y = state.y.wrapping_sub(dy);
                        state.transform_origin = (0, 0);
                    }
                }
                12 => state.offset_slot = a as usize,
                13 => {
                    let slot = usize::try_from(half(a, false))?;
                    let record = state
                        .transforms
                        .get_mut(slot)
                        .ok_or("transform index outside table")?;
                    for (index, value) in record.iter_mut().take(7).enumerate() {
                        *value = half(word(command, 4 + index / 2 * 4)?, index % 2 != 0);
                    }
                }
                14 => state.transform_slot = a as usize,
                15 => {
                    let slot = usize::try_from(half(a, false))?;
                    *state
                        .scales
                        .get_mut(slot)
                        .ok_or("scale index outside table")? = [a, word(command, 8)?];
                }
                16 => state.scale_slot = a as usize,
                20 => {
                    let slot = usize::try_from(half(a, false))?;
                    state.condition_slot = slot;
                    state.condition = i32::from(
                        *state
                            .flags
                            .get(slot)
                            .ok_or("visual variant outside table")?,
                    );
                }
                21 => state.value = i32::from(half(a, true)),
                22 => {
                    // F0DD4: temporarily force the condition to its expected
                    // value, or restore the byte indexed by scratch+0x0C.
                    state.condition = if a == 1 {
                        state.value
                    } else {
                        i32::from(
                            *state
                                .flags
                                .get(state.condition_slot)
                                .ok_or("restored visual condition outside table")?,
                        )
                    };
                }
                23 => {
                    let size = word(command, 8)?;
                    if let Some(packet) = script_draw_area(
                        [
                            half(a, false),
                            half(a, true),
                            half(size, false),
                            half(size, true),
                        ],
                        state.position(0)?,
                    ) {
                        state.emit(packet.to_vec())?;
                    }
                }
                26 => {
                    let count = i32::from(half(a, false));
                    let first = i32::from(half(a, true));
                    if (first..first + count).contains(&state.condition) {
                        let offset = 8 + (state.condition - first) as usize * 24;
                        let (x, y) = state.position(word(command, offset + 4)?)?;
                        let size = word(command, offset + 8)?;
                        let texture = word(command, offset + 12)?;
                        let clut_depth = word(command, offset + 20)?;
                        let depth = half(clut_depth, true);
                        let scale = match depth {
                            0 => 4,
                            1 => 2,
                            2 => 1,
                            _ => return Err("invalid variant texture depth".into()),
                        };
                        let u = ((i32::from(half(texture, false)) % 64) * scale) as u8;
                        let v = half(texture, true) as u8;
                        state.emit(vec![
                            (if state.blend == -1 {
                                0x64000000
                            } else {
                                0x66000000
                            }) | state.color()?,
                            xy(x, y),
                            u32::from(u) | (u32::from(v) << 8) | ((clut_depth & 0xffff) << 16),
                            size,
                        ])?;
                        state.emit(vec![
                            0xe1000000 | texture_page(texture, depth, state.blend),
                            0,
                        ])?;
                    }
                }
                _ => {
                    return Err(
                        format!("unported menu script opcode {op} in script {index}").into(),
                    );
                }
            }
        }
    }

    fn quad_coordinates(
        &self,
        state: &ScriptState,
        x: i16,
        y: i16,
        w: i16,
        h: i16,
    ) -> Result<[u32; 4]> {
        if state.scale_slot == 0 {
            return Ok([
                xy(x, y),
                xy(x.wrapping_add(w), y),
                xy(x, y.wrapping_add(h)),
                xy(x.wrapping_add(w), y.wrapping_add(h)),
            ]);
        }
        let scale = state
            .scales
            .get(state.scale_slot)
            .ok_or("quad rotation slot outside table")?;
        let (dx, dy) = state.offsets[state.offset_slot];
        let px = half(scale[0], true).wrapping_add(state.x).wrapping_add(dx);
        let py = half(scale[1], false).wrapping_add(state.y).wrapping_add(dy);
        let [sin, cos] = self.trigonometry.sin_cos(half(scale[1], true));
        Ok([
            (x, y),
            (x.wrapping_add(w - 2), y),
            (x, y.wrapping_add(h - 2)),
            (x.wrapping_add(w - 2), y.wrapping_add(h - 2)),
        ]
        .map(|(vx, vy)| {
            let rx = i32::from(vx.wrapping_sub(px));
            let ry = i32::from(vy.wrapping_sub(py));
            xy(
                (i32::from(px) + ((cos * rx - sin * ry) >> 12)) as i16,
                (i32::from(py) + ((sin * 2 * rx + cos * 2 * ry) >> 12)) as i16,
            )
        }))
    }
}

fn texture_page(texture: u32, depth: i16, blend: i32) -> u32 {
    let x = texture & 0xffff;
    let y = texture >> 16;
    ((depth as u32 & 3) << 7)
        | ((blend as u32 & 3) << 5)
        | ((y & 0x100) >> 4)
        | ((x & 0x3ff) >> 6)
        | ((y & 0x200) << 2)
}
fn fade_packets(shade: u8, packets: &mut Vec<Vec<u32>>) {
    packets.push(vec![0xe1000000, 0]);
    packets.push(vec![
        0x62000000 | (u32::from(shade) * 0x10101),
        0,
        0x01ff0140,
    ]);
    packets.push(vec![0xe1000040, 0]);
}

pub struct MenuVisualState {
    state: ScriptState,
    pub page: usize,
    visible: [u8; 256],
    fonts: [u16; 256],
}
impl MenuVisualState {
    /// F1788: replace one script group's RGB modulation color.
    pub fn set_color(&mut self, slot: usize, rgb: [u8; 3]) -> Result<()> {
        *self
            .state
            .colors
            .get_mut(slot)
            .ok_or("visual color outside table")? =
            u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16);
        Ok(())
    }
    pub fn offset(&self, slot: usize) -> Result<[i16; 2]> {
        let &(x, y) = self
            .state
            .offsets
            .get(slot)
            .ok_or("visual offset outside table")?;
        Ok([x, y])
    }
    /// Apply only this layer's effects. The scene host owns the remaining
    /// effects, including sound, background color animation and map drawing.
    pub fn apply_mission_visuals(&mut self, effects: &[ac3_mission::Effect]) -> Result<()> {
        use ac3_mission::Effect as M;
        for effect in effects {
            match *effect {
                M::SelectVisualPage { page } => self.page = usize::try_from(page)?,
                M::ClearTextVisibility => self.visible.fill(0),
                M::SetTextVisibility { entry, value } => {
                    *self
                        .visible
                        .get_mut(usize::try_from(entry)?)
                        .ok_or("mission text entry outside table")? = value;
                }
                M::SetVisualStyle { slot, style } => {
                    *self
                        .state
                        .flags
                        .get_mut(usize::try_from(slot)?)
                        .ok_or("mission visual style outside table")? = style as u8;
                }
                M::SetVisualOffset { slot, x, y } => {
                    *self
                        .state
                        .offsets
                        .get_mut(usize::try_from(slot)?)
                        .ok_or("mission visual offset outside table")? = (x as i16, y as i16);
                }
                M::SetVisualParameter { slot, value } => {
                    self.state
                        .transforms
                        .get_mut(usize::try_from(slot)?)
                        .ok_or("mission visual transform outside table")?[7] = value as i16;
                }
                _ => (),
            }
        }
        Ok(())
    }
    /// Color currently submitted through the display environment. Mission
    /// background animation supplies it independently of script group colors.
    pub fn set_frame_color(&mut self, rgb: [u8; 3]) {
        self.state.frame_color =
            u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16);
    }
    pub fn apply(&mut self, effects: &[Effect], frame_counter: i32) -> Result<()> {
        for effect in effects {
            match *effect {
                Effect::SelectVisualPage { page } => self.page = usize::try_from(page)?,
                Effect::ClearTextVisibility => self.visible.fill(0),
                Effect::SetTextVisibility { entry, value } => {
                    *self
                        .visible
                        .get_mut(usize::try_from(entry)?)
                        .ok_or("text entry outside table")? = value
                }
                Effect::SetTextFont { entry, font } => {
                    *self
                        .fonts
                        .get_mut(usize::try_from(entry)?)
                        .ok_or("text entry outside table")? = font
                }
                Effect::SetVisualStyle { slot, style } => {
                    *self
                        .state
                        .flags
                        .get_mut(usize::try_from(slot)?)
                        .ok_or("visual slot outside table")? = style
                }
                Effect::SetVisualOffset { slot, x, y } => {
                    *self
                        .state
                        .offsets
                        .get_mut(usize::try_from(slot)?)
                        .ok_or("visual offset outside table")? = (x as i16, y as i16)
                }
                Effect::PulseVisualColor { slot, from, to } => {
                    let rgb = ac3_menu::pulse_color(frame_counter, from, to);
                    *self
                        .state
                        .colors
                        .get_mut(usize::try_from(slot)?)
                        .ok_or("color slot outside table")? =
                        u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 0]);
                }
                Effect::DrawLanguageRows { rows } => {
                    for (i, row) in rows.iter().enumerate() {
                        self.state.offsets[i + 1] = (row.visual_x as i16, 0);
                        self.state.flags[i + 1] = row.style;
                    }
                }
                _ => (),
            }
        }
        Ok(())
    }
}

/// F0560 and resident SetDrawArea: retain its width-for-height clipping test
/// and signed-halfword endpoint stores before the GPU-coordinate clamp.
fn script_draw_area([x, y, w, h]: [i16; 4], origin: (i16, i16)) -> Option<[u32; 2]> {
    let rect = if x == -1 {
        [0, 0, 320, 511]
    } else {
        let xx = i32::from(x) + i32::from(origin.0);
        let yy = i32::from(y) + i32::from(origin.1);
        let right = xx + i32::from(w);
        let bottom = yy + i32::from(h);
        if xx >= 320 || yy >= 512 {
            return None;
        }
        let (rx, rw) = if right >= 320 {
            (xx as i16, (320 - xx) as i16)
        } else if xx < 0 {
            if right < 0 {
                return None;
            }
            (0, right as i16)
        } else {
            (xx as i16, w)
        };
        let (ry, rh) = if yy + i32::from(w) >= 512 {
            (yy as i16, (511 - yy) as i16)
        } else if yy < 0 {
            if bottom < 0 {
                return None;
            }
            (0, bottom as i16)
        } else {
            (yy as i16, h)
        };
        [rx, ry, rw, rh]
    };
    let encode = |x: i16, y: i16| {
        u32::from(x.clamp(0, 1023) as u16) | (u32::from(y.clamp(0, 511) as u16) << 10)
    };
    Some([
        0xe3000000 | encode(rect[0], rect[1]),
        0xe4000000
            | encode(
                rect[0].wrapping_add(rect[2]).wrapping_sub(1),
                rect[1].wrapping_add(rect[3]).wrapping_sub(1),
            ),
    ])
}

struct ScriptState {
    frame_color: u32,
    bins: [Vec<Vec<u32>>; 16],
    bin: usize,
    colors: [u32; 128],
    color_slot: usize,
    offsets: [(i16, i16); 32],
    transforms: [[i16; 8]; 16],
    scales: [[u32; 2]; 16],
    transform_slot: usize,
    scale_slot: usize,
    offset_slot: usize,
    flags: [u8; 128],
    condition: i32,
    condition_slot: usize,
    value: i32,
    blend: i32,
    x: i16,
    y: i16,
    transform_origin: (i16, i16),
}
impl ScriptState {
    fn new(rows: [LanguageRow; 5]) -> Self {
        let mut state = Self {
            frame_color: 0,
            bins: std::array::from_fn(|_| Vec::new()),
            bin: 1,
            colors: [0x808080; 128],
            color_slot: 0,
            offsets: [(0, 0); 32],
            transforms: std::array::from_fn(|index| [index as i16, 0, 0, 0, 0, 0, 0, 0]),
            scales: std::array::from_fn(|index| [index as u32, 0]),
            transform_slot: 0,
            scale_slot: 0,
            offset_slot: 0,
            flags: [0; 128],
            condition: 0,
            condition_slot: 0,
            value: 0,
            blend: -1,
            x: 0,
            y: 0,
            transform_origin: (0, 0),
        };
        for (index, row) in rows.iter().enumerate() {
            state.offsets[index + 1] = (row.visual_x as i16, 0);
            state.flags[index + 1] = row.style;
        }
        state
    }
    fn reset_frame(&mut self) {
        for bin in &mut self.bins {
            bin.clear();
        }
        self.bin = 1;
        self.color_slot = 0;
        self.offset_slot = 0;
        self.condition = 0;
        self.condition_slot = 0;
        self.value = 0;
        self.blend = -1;
        self.x = 0;
        self.y = 0;
    }
    fn color(&self) -> Result<u32> {
        self.colors
            .get(self.color_slot)
            .copied()
            .ok_or_else(|| "color index outside table".into())
    }
    fn transform_rect(&self, rect: [i16; 4]) -> Result<crate::menu_transform::TransformedRect> {
        let mut record = *self
            .transforms
            .get(self.transform_slot)
            .ok_or("rectangle transform slot outside table")?;
        record[2] = record[2].wrapping_add(self.transform_origin.0);
        record[3] = record[3].wrapping_add(self.transform_origin.1);
        Ok(crate::menu_transform::transform_rect(rect, record)?)
    }
    fn position(&self, position: u32) -> Result<(i16, i16)> {
        let (dx, dy) = self
            .offsets
            .get(self.offset_slot)
            .ok_or("visual offset outside table")?;
        Ok((
            half(position, false).wrapping_add(self.x).wrapping_add(*dx),
            half(position, true).wrapping_add(self.y).wrapping_add(*dy),
        ))
    }
    fn emit(&mut self, packet: Vec<u32>) -> Result<()> {
        self.bins
            .get_mut(self.bin)
            .ok_or("ordering-table index outside table")?
            .push(packet);
        Ok(())
    }
}

/// Font2/3/4 pair spacing, original0x800CEAAC. Other font families are separate.
fn font2_kerning(left: u8, right: u8) -> i16 {
    match (left, right) {
        (b'N', b'e')
        | (b'S', b'c')
        | (b'c', b'h' | b'i')
        | (b'd', b'e')
        | (b'e', b'u' | b'w' | b'n')
        | (b'f', 0xe1)
        | (b'g', b'e')
        | (b'j', b'e')
        | (b'l', b'i' | b'o' | b'e')
        | (b'n', 0xe7 | b'f' | b'e' | b'u')
        | (b'o', b'c')
        | (b'p', b'e')
        | (b'r', b'e')
        | (b's', b'c' | b'p' | b'y' | 0xed)
        | (b'u', b'e')
        | (b'w', b'i')
        | (b'y', b'e')
        | (0xed, b'c') => -1,
        (b'e', b's') => -2,
        (b'i', b'd') => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod mission_script_tests {
    use super::*;
    use std::{fs, path::PathBuf};

    #[test]
    #[ignore = "requires normal-game mission script capture and original assets"]
    fn native_hub_asset_script_matches_original_commands() {
        let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
        let capture = root.join("analysis/menu-runtime/hub-render-001");
        let data = GameData::load(&root.join("extracted/disc")).unwrap();
        let assets = crate::mission_assets::MissionAssets::load(&data, 74).unwrap();
        let scene = LanguageScene::from_mission(&data, assets).unwrap();
        // The captured renderer state is an input fixture for this isolated
        // asset interpreter test. The native player never loads this fixture.
        let before = fs::read(capture.join("render-00001-script-entry-script-state.bin")).unwrap();
        let mut state = ScriptState::new(ac3_menu::language_rows(0, 0));
        state.flags.copy_from_slice(&before[0x4a4..0x524]);
        for index in 0..128 {
            state.colors[index] = word(&before, 0x98 + 4 * index).unwrap() & 0xffffff;
        }
        for index in 0..32 {
            let value = word(&before, 0x298 + 4 * index).unwrap();
            state.offsets[index] = (half(value, false), half(value, true));
        }
        for index in 0..16 {
            for part in 0..8 {
                state.transforms[index][part] = half(
                    word(&before, 0x318 + 16 * index + part / 2 * 4).unwrap(),
                    part % 2 != 0,
                );
            }
            state.scales[index] = [
                word(&before, 0x418 + 8 * index).unwrap(),
                word(&before, 0x41c + 8 * index).unwrap(),
            ];
        }
        state.transform_slot = word(&before, 0x498).unwrap() as usize;
        state.scale_slot = word(&before, 0x49c).unwrap() as usize;
        let trace: serde_json::Value =
            serde_json::from_slice(&fs::read(capture.join("script-frame-00001.json")).unwrap())
                .unwrap();
        let origin = trace[0]["scratch_header"][2].as_u64().unwrap() as u32;
        state.transform_origin = (half(origin, false), half(origin, true));
        scene.script(2, &mut state, 0, &mut 10000).unwrap();
        assert!(
            state
                .bins
                .iter()
                .enumerate()
                .all(|(i, p)| i == 1 || p.is_empty())
        );
        let mut packets = std::mem::take(&mut state.bins[1]);
        let ram = fs::read(capture.join("frame-00001.ram.bin")).unwrap();
        let frame_state = word(&ram, 0xbacb8).unwrap() as usize & 0x1fffff;
        let frame_color = word(&ram, frame_state + 0x10).unwrap() & 0xffffff;
        packets.push(vec![0x60000000 | frame_color, xy(0, -2), xy(320, 4)]);
        packets.push(vec![0x60000000 | frame_color, xy(0, 511), xy(320, 4)]);
        let meta: serde_json::Value = serde_json::from_slice(
            &fs::read(capture.join("render-00001-script-entry.json")).unwrap(),
        )
        .unwrap();
        let address = meta["script_state"].as_u64().unwrap() as usize & 0x1fffff;
        let end = word(&ram, address + 4).unwrap() as usize & 0x1fffff;
        let bytes: usize = packets.iter().map(|p| 4 + p.len() * 4).sum();
        let mut cursor = end - bytes;
        for (index, native) in packets.iter().enumerate() {
            let count = (word(&ram, cursor).unwrap() >> 24) as usize;
            assert_eq!(count, native.len(), "packet {index} at {cursor:x} length");
            let mut original: Vec<u32> = (0..count)
                .map(|i| word(&ram, cursor + 4 + i * 4).unwrap())
                .collect();
            // POLY_FT4 leaves two GPU-ignored UV padding halfwords in reused
            // packet memory. Native commands explicitly zero those bits.
            if matches!(original[0] >> 24, 0x2c..=0x2f) {
                original[6] &= 0xffff;
                original[8] &= 0xffff;
            }
            assert_eq!(*native, original, "packet {index} at {cursor:x}");
            cursor += 4 + count * 4;
        }
        assert_eq!(cursor, end);
        fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
        fs::write(root.join("analysis/native-mission/script-validation.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"passed":true,
                "original_script_commands":trace.as_array().unwrap().len(),
                "generated_packets":packets.len(),"packet_bytes":bytes,
                "normalization":"POLY_FT4 unused upper halfwords of UV2/UV3 are masked; address-dependent DMA tags excluded",
                "scope":"Native asset-script interpreter and rectangle animation from captured visual-state input; separate map/backdrop/text layers excluded."})).unwrap()).unwrap();
    }
}
