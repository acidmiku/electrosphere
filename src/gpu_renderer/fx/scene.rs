//! The scene constants of a Japanese flight frame, read from machine RAM at
//! the frame's DrawOTag kick ([`Scene::read`]).
//!
//! Every address is the JP release's (`crates/ac3-flight-jp`), named after
//! the routine that writes it:
//! - gp `0x800BCB74` ([`ac3_flight_jp::GP`]); the game context `[gp+0x2EC]`
//!   ([`ac3_flight_jp::common::CONTEXT`]), whose `+0x2C` is the resident
//!   state, 2 in flight (`jp::game::GAME.in_flight`); the gameplay block
//!   `[gp+0x2E0]` ([`ac3_flight_jp::common::GAMEPLAY`], "world" below).
//! - The flight frame `800339E0` (`jp::res_p1_frame::frame`): the two
//!   4096-entry ordering tables at world `+0x18`/`+0x1C` (the one being
//!   built at `+0x10`); the world's drawing offset (the screen centre plus
//!   the camera shake) at world `+0x298`/`+0x29A`, linked as the E5 packet
//!   of entry 4095; the GTE offset is 0 while the world is drawn.
//! - The camera record `[world+0x28]` (`render::update_camera` 8007B438):
//!   the eye at `+4..+0xC` (three words, 20.12), the projection H at the
//!   half `+0x24` and near/far at `+0x28`/`+0x2A` (`camera_set_projection`
//!   80077768, `init::reset_view_offsets`).
//! - The view matrix `800332E4` (`render::view_matrix`): the camera's
//!   rotation at world `+0x2C` and its transpose, the view matrix, at
//!   `+0x4C` (a MATRIX: 3 x 3 halves, 1.3.12).
//! - The lights (`init::set_light_vectors` 80032ABC): the light matrix at
//!   world `+0x6C` (row 0 the sun's direction, row 1 a second light, 1.3.12),
//!   the light colour matrix at `+0x8C` (light 1's colour in column 0:
//!   `+0x8C`, `+0x92`, `+0x98`; light 2's in column 1) and the back colour
//!   at the words `+0xAC`/`+0xB0`/`+0xB4`.
//! - The mission parameters `[world+0x244]` (section 0): the background
//!   (sky) type in the low three bits of byte `+2`, the fog colour in the
//!   word `+8` (the colour the JP fog `800943A8` fills its last strip
//!   with); the terrain grid `[world+0x248]` and tile directory
//!   `[world+0x24C]` (`terrain::select_cells`, `draw_cells`).
//!
//! Reads never fault: a pointer outside RAM makes the scene `None` (no
//! effects that frame).
use serde::Serialize;

/// Main RAM: 2 MiB.
const RAM: u32 = 0x20_0000;

/// Offsets from the gameplay (world) block.
mod world {
    pub const CURRENT_OT: u32 = 0x10;
    pub const OTS: [u32; 2] = [0x18, 0x1c];
    pub const CAMERA: u32 = 0x28;
    pub const ROTATION: u32 = 0x2c;
    pub const VIEW: u32 = 0x4c;
    pub const LIGHTS: u32 = 0x6c;
    pub const COLOURS: u32 = 0x8c;
    pub const BACK: u32 = 0xac;
    pub const MISSION: u32 = 0x244;
    pub const GRID: u32 = 0x248;
    pub const TILES: u32 = 0x24c;
    pub const CENTRE: u32 = 0x298;
}

/// The resident state of flight (`[[gp+0x2EC]+0x2C]`).
pub const FLIGHT_STATE: u32 = 2;
/// Entries of each flight ordering table.
pub const OT_ENTRIES: u32 = 4096;

/// A flight frame's constants (see the module documentation). Raw values
/// as the game stores them, plus float conveniences.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Scene {
    /// The gameplay block (`[gp+0x2E0]`).
    pub world: u32,
    /// The two ordering tables (KSEG0 addresses of entry 0), and the one
    /// the game built last (world `+0x10`).
    pub ots: [u32; 2],
    pub current_ot: u32,
    /// The camera record and its eye (20.12 words, as stored).
    pub camera: u32,
    pub eye: [i32; 3],
    /// The projection distance H, near and far (camera halves).
    pub h: i32,
    pub near: i32,
    pub far: i32,
    /// The camera rotation (world `+0x2C`) and the view matrix (its
    /// transpose, world `+0x4C`), rows of 1.3.12 halves.
    pub rotation: [[i16; 3]; 3],
    pub view: [[i16; 3]; 3],
    /// The world's drawing offset (world `+0x298`/`+0x29A`): where view x =
    /// y = 0 projects, in VRAM pixels (the GTE offset is 0 in the world).
    pub centre: [i32; 2],
    /// The sun's direction (light matrix row 0) and the second light's,
    /// 1.3.12.
    pub sun: [i16; 3],
    pub light2: [i16; 3],
    /// Light 1's and light 2's colours (light colour matrix columns, 4.12)
    /// and the back (ambient) colour.
    pub sun_colour: [i16; 3],
    pub light2_colour: [i16; 3],
    pub back: [i32; 3],
    /// The mission parameters block, its sky type (byte `+2`, low three
    /// bits) and its fog colour (R, G, B; the word `+8`).
    pub mission: u32,
    pub sky: u32,
    pub fog: [u8; 3],
    /// The terrain grid and tile directory pointers.
    pub grid: u32,
    pub tiles: u32,
    /// Widescreen's X squeeze k = num / den while the world is drawn
    /// (`[display] widescreen`; 1 / 1 without): screen x = centre.x +
    /// k * H * x / z.
    pub x_scale: (u32, u32),
}

/// The word at `address`; `None` outside main RAM or unaligned.
pub fn word(ram: &[u8], address: u32) -> Option<u32> {
    let physical = address & 0x1fff_ffff;
    if !matches!(address >> 29, 0 | 4 | 5) || physical >= RAM || address & 3 != 0 {
        return None;
    }
    let at = physical as usize;
    let bytes = ram.get(at..at + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// The halfword at `address`, sign-extended; `None` outside main RAM.
pub fn half(ram: &[u8], address: u32) -> Option<i16> {
    let shift = (address & 2) * 8;
    word(ram, address & !3).map(|word| (word >> shift) as u16 as i16)
}

fn byte(ram: &[u8], address: u32) -> Option<u8> {
    let shift = (address & 3) * 8;
    word(ram, address & !3).map(|word| (word >> shift) as u8)
}

/// A pointer word that points into main RAM.
fn pointer(ram: &[u8], address: u32) -> Option<u32> {
    let value = word(ram, address)?;
    word(ram, value & !3).map(|_| value)
}

fn halves<const N: usize>(ram: &[u8], address: u32, stride: u32) -> Option<[i16; N]> {
    let mut out = [0; N];
    for (i, value) in out.iter_mut().enumerate() {
        *value = half(ram, address + i as u32 * stride)?;
    }
    Some(out)
}

fn matrix(ram: &[u8], address: u32) -> Option<[[i16; 3]; 3]> {
    Some([
        halves(ram, address, 2)?,
        halves(ram, address + 6, 2)?,
        halves(ram, address + 12, 2)?,
    ])
}

impl Scene {
    /// The scene of the frame being kicked, or `None` outside flight (the
    /// resident state is not 2) or when a pointer leaves RAM. `x_scale` is
    /// widescreen's squeeze in flight (`None`: 4:3).
    pub fn read(ram: &[u8], x_scale: Option<(u32, u32)>) -> Option<Self> {
        use ac3_flight_jp::{GP, common};
        let context = pointer(ram, GP + common::CONTEXT)?;
        if word(ram, context + 0x2c)? != FLIGHT_STATE {
            return None;
        }
        let world = pointer(ram, GP + common::GAMEPLAY)?;
        let ots = [
            pointer(ram, world + world::OTS[0])?,
            pointer(ram, world + world::OTS[1])?,
        ];
        // Both tables lie in RAM.
        for ot in ots {
            word(ram, ot + (OT_ENTRIES - 1) * 4)?;
        }
        let camera = pointer(ram, world + world::CAMERA)?;
        let mission = pointer(ram, world + world::MISSION)?;
        let fog = word(ram, mission + 8)?;
        let lights: [[i16; 3]; 3] = matrix(ram, world + world::LIGHTS)?;
        let colours: [[i16; 3]; 3] = matrix(ram, world + world::COLOURS)?;
        let column = |c: usize| [colours[0][c], colours[1][c], colours[2][c]];
        Some(Self {
            world,
            ots,
            current_ot: word(ram, world + world::CURRENT_OT)?,
            camera,
            eye: [
                word(ram, camera + 4)? as i32,
                word(ram, camera + 8)? as i32,
                word(ram, camera + 0xc)? as i32,
            ],
            h: i32::from(half(ram, camera + 0x24)?),
            near: i32::from(half(ram, camera + 0x28)?),
            far: i32::from(half(ram, camera + 0x2a)?),
            rotation: matrix(ram, world + world::ROTATION)?,
            view: matrix(ram, world + world::VIEW)?,
            centre: [
                i32::from(half(ram, world + world::CENTRE)?),
                i32::from(half(ram, world + world::CENTRE + 2)?),
            ],
            sun: lights[0],
            light2: lights[1],
            sun_colour: column(0),
            light2_colour: column(1),
            back: [
                word(ram, world + world::BACK)? as i32,
                word(ram, world + world::BACK + 4)? as i32,
                word(ram, world + world::BACK + 8)? as i32,
            ],
            mission,
            sky: u32::from(byte(ram, mission + 2)? & 7),
            fog: [fog as u8, (fog >> 8) as u8, (fog >> 16) as u8],
            grid: word(ram, world + world::GRID)?,
            tiles: word(ram, world + world::TILES)?,
            x_scale: x_scale.unwrap_or((1, 1)),
        })
    }

    /// The eye in world units (the 20.12 words / 4096).
    pub fn eye_units(&self) -> [f32; 3] {
        self.eye.map(|v| v as f32 / 4096.0)
    }

    /// A 1.3.12 matrix as floats, rows.
    pub fn unit(matrix: &[[i16; 3]; 3]) -> [[f32; 3]; 3] {
        matrix.map(|row| row.map(|v| f32::from(v) / 4096.0))
    }

    /// Widescreen's squeeze k as a float.
    pub fn k(&self) -> f32 {
        self.x_scale.0 as f32 / self.x_scale.1.max(1) as f32
    }

    /// The scene as the card's uniform ([`UNIFORM_WORDS`] words; the WGSL
    /// struct `FxScene` in `docs/EFFECTS.md`): all f32 but where noted.
    pub fn uniform(&self, scale: u32, origin: [i32; 2]) -> [f32; UNIFORM_WORDS] {
        let mut u = [0.0; UNIFORM_WORDS];
        let view = Self::unit(&self.view);
        let eye = self.eye_units();
        // 0..12: the view matrix rows, each padded to 4.
        for (row, values) in view.iter().enumerate() {
            u[row * 4..row * 4 + 3].copy_from_slice(values);
        }
        // 12: eye x, y, z, H.
        u[12..15].copy_from_slice(&eye);
        u[15] = self.h as f32;
        // 16: centre x, y (1x VRAM pixels), k, internal scale.
        u[16] = self.centre[0] as f32;
        u[17] = self.centre[1] as f32;
        u[18] = self.k();
        u[19] = scale as f32;
        // 20: sun direction, near; 24: sun colour, far.
        let sun = self.sun.map(|v| f32::from(v) / 4096.0);
        u[20..23].copy_from_slice(&sun);
        u[23] = self.near as f32;
        let colour = self.sun_colour.map(|v| f32::from(v) / 4096.0);
        u[24..27].copy_from_slice(&colour);
        u[27] = self.far as f32;
        // 28: fog colour (0..1), sky type.
        for (i, &c) in self.fog.iter().enumerate() {
            u[28 + i] = f32::from(c) / 255.0;
        }
        u[31] = self.sky as f32;
        // 32: the world area's origin at N x (pixels of the snapshot's 0, 0).
        u[32] = (origin[0] * scale as i32) as f32;
        u[33] = (origin[1] * scale as i32) as f32;
        u
    }
}

/// Words of [`Scene::uniform`] (a multiple of 4).
pub const UNIFORM_WORDS: usize = 36;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A RAM image with a flight scene at plausible JP addresses.
    pub(crate) fn flight_ram() -> Vec<u8> {
        let mut ram = vec![0u8; RAM as usize];
        let mut put = |address: u32, value: u32| {
            let at = (address & 0x1f_ffff) as usize;
            ram[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        use ac3_flight_jp::{GP, common};
        let (context, world, camera, mission) = (
            0x800b_ce60u32,
            0x800f_0000u32,
            0x800f_1000u32,
            0x800f_2000u32,
        );
        put(GP + common::CONTEXT, context);
        put(context + 0x2c, 2);
        put(GP + common::GAMEPLAY, world);
        put(world + 0x10, 0x8010_0000);
        put(world + 0x18, 0x8010_0000);
        put(world + 0x1c, 0x8010_4000);
        put(world + 0x28, camera);
        put(camera + 4, 0x0010_0000);
        put(camera + 8, (-0x0002_0000i32) as u32);
        put(camera + 0xc, 0x0030_0000);
        put(camera + 0x24, 0x0000_00c0);
        put(camera + 0x28, 0x7fff_0010);
        // View matrix: identity (halves m00 +0x4C, m11 +0x54, m22 +0x5C).
        put(world + 0x4c, 0x0000_1000);
        put(world + 0x54, 0x0000_1000);
        put(world + 0x5c, 0x0000_1000);
        // Sun straight down (+y), white.
        put(world + 0x6c, 0x1000_0000);
        put(world + 0x8c, 0x0000_0c00);
        put(world + 0x90, 0x0c00_0000);
        put(world + 0x98, 0x0000_0c00);
        put(world + 0x244, mission);
        put(mission, 0x0002_0000);
        put(mission + 8, 0x00c0_a080);
        put(world + 0x248, 0x8011_0000);
        put(world + 0x24c, 0x8012_0000);
        put(world + 0x298, 0x0078_00a0);
        ram
    }

    #[test]
    fn the_scene_reads_the_jp_flight_words() {
        let ram = flight_ram();
        let scene = Scene::read(&ram, Some((3, 4))).expect("a flight scene");
        assert_eq!(scene.ots, [0x8010_0000, 0x8010_4000]);
        assert_eq!(scene.current_ot, 0x8010_0000);
        assert_eq!(scene.eye, [0x0010_0000, -0x0002_0000, 0x0030_0000]);
        assert_eq!(scene.eye_units(), [256.0, -32.0, 768.0]);
        assert_eq!((scene.h, scene.near, scene.far), (0xc0, 0x10, 0x7fff));
        assert_eq!(scene.view, [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]]);
        assert_eq!(scene.sun, [0, 0x1000, 0]);
        assert_eq!(scene.sun_colour, [0xc00, 0xc00, 0xc00]);
        assert_eq!(scene.fog, [0x80, 0xa0, 0xc0]);
        assert_eq!(scene.sky, 2);
        assert_eq!(scene.centre, [0xa0, 0x78]);
        assert_eq!((scene.grid, scene.tiles), (0x8011_0000, 0x8012_0000));
        assert_eq!(scene.k(), 0.75);
        let uniform = scene.uniform(4, [0, 256]);
        assert_eq!(&uniform[0..3], &[1.0, 0.0, 0.0]);
        assert_eq!(&uniform[12..16], &[256.0, -32.0, 768.0, 192.0]);
        assert_eq!(&uniform[16..20], &[160.0, 120.0, 0.75, 4.0]);
        assert_eq!(uniform[33], 1024.0);
    }

    #[test]
    fn no_scene_outside_flight_or_with_bad_pointers() {
        let mut ram = flight_ram();
        let context = 0x800b_ce60u32;
        let at = ((context + 0x2c) & 0x1f_ffff) as usize;
        ram[at] = 3;
        assert_eq!(Scene::read(&ram, None), None, "the hub");
        ram[at] = 2;
        assert!(Scene::read(&ram, None).is_some());
        // The gameplay block outside RAM.
        let gameplay = ((ac3_flight_jp::GP + ac3_flight_jp::common::GAMEPLAY) & 0x1f_ffff) as usize;
        ram[gameplay..gameplay + 4].copy_from_slice(&0x9000_0000u32.to_le_bytes());
        assert_eq!(Scene::read(&ram, None), None);
        assert_eq!(word(&[0; 8], 0x8000_0004), Some(0));
        assert_eq!(word(&[0; 8], 0x8000_0002), None, "unaligned");
        assert_eq!(word(&[0; 8], 0x1f80_0000), None, "not main RAM");
    }
}
