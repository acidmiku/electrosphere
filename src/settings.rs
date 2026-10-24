//! The settings file, `ac3.toml` next to the executable.
//!
//! Every key is optional. A missing file or key takes the default; a key
//! this build does not know is ignored with a warning on stderr, so a newer
//! file still works with an older build. A known key with a bad value is an
//! error (with the file's line and column). Command-line options override
//! the file.
//!
//! Adding a section: give it a `Default` type with `#[serde(default)]`, add a
//! field to [`Settings`] and a commented block to [`DEFAULT_FILE`] (the
//! `default_file_matches_the_defaults` test keeps the two in step).
//!
//! A settings editor (the launcher) saves through [`Document`], which
//! changes values in place: the file's comments, layout and unknown keys
//! stay.
use crate::machine_card::CardSource;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{InlineTable, Item, Table, TableLike, Value};

/// The settings file's name; it is looked for next to the executable.
pub const FILE_NAME: &str = "ac3.toml";
/// The player's folder in a macOS app bundle:
/// `~/Library/Application Support/Ace Combat 3`.
pub const APP_FOLDER: &str = "Ace Combat 3";
/// The player's folder and the shipped files' folder of a Nix package:
/// `$XDG_DATA_HOME/ace-combat-3` (`~/.local/share/ace-combat-3`), and
/// `share/ace-combat-3` in the package.
pub const DATA_FOLDER: &str = "ace-combat-3";
/// Where Nix packages live, read-only.
const NIX_STORE: &str = "/nix/store";

/// The folder of the player's files (the settings, memory cards, quick
/// saves and, unless named elsewhere, the discs): beside the executable,
/// except where that folder is read-only by design:
/// - a macOS app bundle (signed; may run from a read-only copy):
///   `~/Library/Application Support/Ace Combat 3`;
/// - a Nix package (in the store): `$XDG_DATA_HOME/ace-combat-3`, else
///   `~/.local/share/ace-combat-3`.
///
/// Empty (the working directory) when the executable's path is unknown.
pub fn player_folder() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    player_folder_for(
        std::env::current_exe().ok().as_deref(),
        home.as_deref(),
        data_home.as_deref(),
    )
}

fn player_folder_for(exe: Option<&Path>, home: Option<&Path>, data_home: Option<&Path>) -> PathBuf {
    let Some(folder) = exe.and_then(Path::parent) else {
        return PathBuf::new();
    };
    if bundle_contents(folder).is_some()
        && let Some(home) = home
    {
        return home.join("Library/Application Support").join(APP_FOLDER);
    }
    if folder.starts_with(NIX_STORE) {
        if let Some(data_home) = data_home {
            return data_home.join(DATA_FOLDER);
        }
        if let Some(home) = home {
            return home.join(".local/share").join(DATA_FOLDER);
        }
    }
    folder.to_owned()
}

/// The folder of the files shipped with the game (the bundled CRT looks):
/// beside the executable, a macOS app bundle's `Contents/Resources`, or a
/// Nix package's `share/ace-combat-3` (the executable in its `bin`).
pub fn resources_folder() -> PathBuf {
    resources_folder_for(std::env::current_exe().ok().as_deref())
}

fn resources_folder_for(exe: Option<&Path>) -> PathBuf {
    let Some(folder) = exe.and_then(Path::parent) else {
        return PathBuf::new();
    };
    if let Some(contents) = bundle_contents(folder) {
        return contents.join("Resources");
    }
    match folder.parent() {
        Some(package)
            if folder.starts_with(NIX_STORE)
                && folder.file_name().is_some_and(|name| name == "bin") =>
        {
            package.join("share").join(DATA_FOLDER)
        }
        _ => folder.to_owned(),
    }
}

/// `Name.app/Contents` when `folder` (the executable's) is a macOS app
/// bundle's `Contents/MacOS`.
fn bundle_contents(folder: &Path) -> Option<&Path> {
    let contents = folder.parent()?;
    let app = contents.parent()?;
    (folder.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then_some(contents)
}

/// Everything `ac3.toml` holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `[display]`: the window and how the picture is scaled.
    pub display: Display,
    /// `[effects]`: flight visual effects (off by default).
    pub effects: Effects,
    /// `[paths]`: the disc images, memory cards and the quick-save slot.
    pub paths: Paths,
    /// `[controls]`: the gamepad (port 1 type, dead zone, triggers, rumble,
    /// button map).
    pub controls: crate::gamepad::Bindings,
}

/// `[display]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// Windowed-mode size in physical pixels.
    pub window_width: u32,
    pub window_height: u32,
    pub mode: WindowMode,
    pub scaling: Scaling,
    pub filter: Filter,
    /// Present in step with the monitor's refresh. Game time never depends
    /// on it.
    pub vsync: bool,
    /// Show the game's frame rate in the corner (F3 toggles while playing).
    pub show_fps: bool,
    /// The GPU renderer's internal resolution, 1..=8 times the PlayStation's.
    pub internal_resolution: u32,
    /// Which renderer draws the game.
    pub renderer: Renderer,
    /// Dithering above 1x (always on at 1x, as the game asks).
    pub upscaled_dither: bool,
    /// Above 1x, draw 3D geometry from the GTE's unrounded screen positions
    /// (no vertex wobble). The game itself is unchanged.
    pub subpixel: bool,
    /// Above 1x with `subpixel`, map textures (and shade them) with
    /// perspective, from the GTE's depths, instead of the PlayStation's
    /// affine mapping. The game itself is unchanged.
    pub perspective: bool,
    /// The texture pack folder, relative to the settings file's folder;
    /// empty for none. Its PNGs, named by replacement key, are drawn
    /// instead of the game's textures (with the GPU renderer).
    pub texture_pack: String,
    /// Use the texture pack.
    pub replace_textures: bool,
    /// Anamorphic widescreen in flight (off by default): a wider view, shown
    /// at this aspect; menus, the hub and movies stay 4:3.
    pub widescreen: Widescreen,
    /// A RetroArch slang shader preset (`.slangp`) drawn over the picture,
    /// relative to the settings file's folder; empty for none
    /// (`crate::present::shader`). Set, it takes the bundled CRT's place.
    pub shader_preset: String,
    /// The bundled CRT shader (`crate::present::crt`): on, its look (a
    /// name of `crt::LOOKS`), and curved or flat glass.
    pub crt: bool,
    pub crt_preset: String,
    pub crt_curved: bool,
    /// The bundle's folder for this run ([`Display::crt_folder`]: beside the
    /// settings file, else with the game's files), set by whoever resolves
    /// the file's paths; not a key.
    #[serde(skip)]
    pub crt_folder: Option<PathBuf>,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            window_width: 1280,
            window_height: 960,
            mode: WindowMode::Windowed,
            scaling: Scaling::Aspect,
            filter: Filter::Nearest,
            vsync: true,
            show_fps: false,
            internal_resolution: 1,
            renderer: Renderer::Auto,
            upscaled_dither: false,
            subpixel: true,
            perspective: true,
            texture_pack: DEFAULT_TEXTURE_PACK.to_owned(),
            replace_textures: true,
            widescreen: Widescreen::Off,
            shader_preset: String::new(),
            crt: true,
            crt_preset: crate::present::crt::DEFAULT.to_owned(),
            crt_curved: true,
            crt_folder: None,
        }
    }
}

/// The texture pack folder's default name, next to the settings file.
pub const DEFAULT_TEXTURE_PACK: &str = "textures";
/// Memory card 1's file as the launcher makes it, next to the settings file.
pub const CARD: &str = "card1.mcr";

impl Display {
    /// The texture pack folder (relative paths start at `base`, the
    /// settings file's folder), or `None` when replacement is off or no
    /// folder is set. Whether it exists is not checked.
    pub fn texture_pack_dir(&self, base: &Path) -> Option<PathBuf> {
        let text = self.texture_pack.trim();
        (self.replace_textures && !text.is_empty()).then(|| base.join(text))
    }

    /// The bundled CRT looks' folder: beside the settings file at `base`
    /// when there, else with the game's files ([`resources_folder`]).
    pub fn crt_folder(base: &Path) -> PathBuf {
        let beside = base.join(crate::present::crt::FOLDER);
        let shipped = resources_folder().join(crate::present::crt::FOLDER);
        if !beside.is_dir() && shipped.is_dir() {
            shipped
        } else {
            beside
        }
    }

    /// The shader preset file (relative paths start at `base`, the settings
    /// file's folder), or `None` when none is set. Whether it exists is not
    /// checked.
    pub fn shader_preset_path(&self, base: &Path) -> Option<PathBuf> {
        let text = self.shader_preset.trim();
        (!text.is_empty()).then(|| base.join(text))
    }

    /// The GPU renderer's options, or `None` for the software renderer:
    /// `auto` uses the GPU renderer above 1x, `gpu` at every resolution.
    pub fn gpu_renderer(&self) -> Option<crate::gpu_renderer::Options> {
        let scale = self.internal_resolution;
        let gpu = match self.renderer {
            Renderer::Software => false,
            Renderer::Auto => scale > 1,
            Renderer::Gpu => true,
        };
        gpu.then_some(crate::gpu_renderer::Options {
            scale: scale.clamp(1, crate::gpu_renderer::MAX_SCALE),
            dither: self.upscaled_dither,
        })
    }

    /// Whether textures are mapped with perspective: the setting, with the
    /// GPU renderer above 1x and sub-pixel geometry (which gives the depths).
    pub fn perspective_textures(&self) -> bool {
        self.perspective && self.subpixel && self.gpu_renderer().is_some_and(|gpu| gpu.scale > 1)
    }
}

/// Widescreen in flight: the aspect the picture is shown at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Widescreen {
    /// The original 4:3 everywhere.
    #[default]
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "16:9")]
    Wide16x9,
    #[serde(rename = "16:10")]
    Wide16x10,
    /// The 64:27 of 2560 x 1080 monitors.
    #[serde(rename = "21:9")]
    Wide21x9,
}

impl Widescreen {
    /// The machine's setting: `None` for off.
    pub fn aspect(self) -> Option<ac3_flight::widescreen::Aspect> {
        use ac3_flight::widescreen::Aspect;
        match self {
            Self::Off => None,
            Self::Wide16x9 => Some(Aspect::Wide16x9),
            Self::Wide16x10 => Some(Aspect::Wide16x10),
            Self::Wide21x9 => Some(Aspect::Wide21x9),
        }
    }
}

/// `[effects]`: flight visual effects (Milestone 1 Phase 4, `docs/EFFECTS.md`),
/// drawn by the GPU renderer above 1x in Japanese flight. Display only;
/// all off by default, and F6 turns the master switch on and off at run
/// time. With `enabled` off the pictures are exactly those without effects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effects {
    /// The master switch.
    pub enabled: bool,
    /// A debug view of the effects' depth buffer, drawn over the flight
    /// world (`--fx-debug`).
    pub debug: crate::gpu_renderer::fx::DebugView,
    /// The atmosphere (`docs/EFFECTS_ATMOSPHERE.md` §8): the game's
    /// distance fog removed, a height haze in its place.
    pub atmosphere: bool,
    /// The atmosphere's haze density, a multiple of the default (1.0); 0
    /// removes the fog without a haze. 0 to 10.
    pub haze_density: crate::effects::atmosphere_fx::HazeDensity,
    /// Far terrain (`docs/EFFECTS_TERRAIN.md`): the mission's terrain
    /// beyond the game's draw distance, to the horizon. Needs `atmosphere`.
    pub far_terrain: bool,
    /// Far terrain's ring of tile meshes around the camera, in tiles (16
    /// to 96, default 44); beyond it the heightmap is ray-marched.
    pub far_terrain_tiles: crate::effects::far_terrain::Ring,
    /// Far terrain's ray-march step: low, medium (default) or high.
    pub far_terrain_quality: crate::effects::far_terrain::Quality,
    /// The sun (`docs/EFFECTS_SUN.md`): an HDR sun disc, a lens flare in
    /// the game's style and bloom, in place of the game's sun and flare.
    pub sun: bool,
    /// The sun's bloom strength: 0 (none) to 2; 0.5 by default.
    pub bloom: crate::effects::sun::Bloom,
    /// The sun's veiling glare, a multiple of the calibrated one (at least
    /// the game's glare): 0 (none) to 3; 1 by default.
    pub sun_glare: crate::effects::sun::Glare,
    /// The sun's lens flare draws a four-ray star (on by default).
    pub sun_star: crate::effects::sun::Star,
    /// The other effects, each under `enabled`: reserved, not implemented
    /// yet ([`Self::UNIMPLEMENTED`]).
    pub ocean: bool,
    pub clouds: bool,
    pub forest: bool,
    pub gi: bool,
}

impl Effects {
    /// The keys of effects that are not implemented yet.
    pub const UNIMPLEMENTED: [&'static str; 4] = ["ocean", "clouds", "forest", "gi"];

    /// The reserved effects set to true (they draw nothing yet).
    pub fn unimplemented_set(&self) -> Vec<&'static str> {
        let set = [self.ocean, self.clouds, self.forest, self.gi];
        Self::UNIMPLEMENTED
            .into_iter()
            .zip(set)
            .filter_map(|(name, on)| on.then_some(name))
            .collect()
    }

    /// The renderer's configuration of these settings, with widescreen's
    /// aspect in flight (`None`: 4:3).
    pub fn config(
        &self,
        widescreen: Option<ac3_flight::widescreen::Aspect>,
    ) -> crate::gpu_renderer::fx::Config {
        crate::gpu_renderer::fx::Config {
            enabled: self.enabled,
            debug: self.debug,
            x_scale: widescreen.map(|aspect| aspect.squeeze()),
        }
    }
}

/// Which renderer draws the game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Renderer {
    /// The software renderer at 1x (exactly the original), the GPU renderer
    /// above.
    #[default]
    Auto,
    /// Always the software renderer (`internal_resolution` unused).
    Software,
    /// The GPU renderer at every resolution, 1x too.
    Gpu,
}

/// Window or borderless fullscreen; Alt+Enter toggles at run time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    #[default]
    Windowed,
    /// Borderless fullscreen on the window's monitor.
    Borderless,
}

/// How the picture fills the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scaling {
    /// 4:3, letterboxed, as on a TV.
    #[default]
    Aspect,
    /// The whole window.
    Stretch,
    /// Whole-pixel multiples, as close to 4:3 as they allow, centered.
    Integer,
}

/// Texture filtering when scaling the picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Filter {
    /// Sharp pixels.
    #[default]
    Nearest,
    /// Bilinear (smooth).
    Linear,
}

/// The lowercase names of a settings enum, for the command line.
macro_rules! names {
    ($type:ty, $what:literal, $($variant:ident = $name:literal),+) => {
        impl $type {
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
            pub fn parse(text: &str) -> crate::Result<Self> {
                match text.to_ascii_lowercase().as_str() {
                    $($name => Ok(Self::$variant),)+
                    _ => Err(format!(
                        concat!("unknown ", $what, " {}; use one of:", $(" ", $name),+),
                        text
                    )
                    .into()),
                }
            }
        }
    };
}
names!(
    WindowMode,
    "window mode",
    Windowed = "windowed",
    Borderless = "borderless"
);
names!(
    Scaling,
    "scaling",
    Aspect = "aspect",
    Stretch = "stretch",
    Integer = "integer"
);
names!(Filter, "filter", Nearest = "nearest", Linear = "linear");
names!(
    Renderer,
    "renderer",
    Auto = "auto",
    Software = "software",
    Gpu = "gpu"
);
names!(
    Widescreen,
    "widescreen setting",
    Off = "off",
    Wide16x9 = "16:9",
    Wide16x10 = "16:10",
    Wide21x9 = "21:9"
);

/// `[paths]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Paths {
    /// Memory card 1, as `--card1`: a raw .mcr path (relative to the
    /// settings file's folder) or `none`. Unset or empty: a blank card in
    /// memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card1: Option<String>,
    /// Memory card 2, likewise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card2: Option<String>,
    /// The quick-save slot (F5 saves, F8 loads), relative to the settings
    /// file's folder. Unset: next to memory card 1's file
    /// (`card1.ac3state`), else `quick.ac3state` next to the executable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quick_save: Option<String>,
    /// The disc image (or cue sheet) to boot, as `--disc`, relative to the
    /// settings file's folder. Unset: the PAL `.bin` in the working
    /// directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc: Option<String>,
    /// The Japanese release's other disc, as `--disc2`. Unset: the image
    /// next to `disc` named with `(Disc 2)` / `(Disc 1)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc2: Option<String>,
    /// The movie pack, as `--movie-pack`: a folder of replacement movie
    /// frames (`docs/MOVIES.md`), relative to the settings file's folder.
    /// Unset or empty: the game's own movie frames.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub movie_pack: Option<String>,
}

impl Paths {
    /// A configured path, resolved against `base` (the settings file's
    /// folder); `None` when unset or blank.
    fn resolved(text: &Option<String>, base: &Path) -> Option<PathBuf> {
        let text = text.as_deref()?.trim();
        (!text.is_empty()).then(|| base.join(text))
    }
    /// The disc to boot, as configured (the `disc` key).
    pub fn disc(&self, base: &Path) -> Option<PathBuf> {
        Self::resolved(&self.disc, base)
    }
    /// The Japanese release's other disc, as configured.
    pub fn disc2(&self, base: &Path) -> Option<PathBuf> {
        Self::resolved(&self.disc2, base)
    }
    /// The movie pack folder, as configured.
    pub fn movie_pack(&self, base: &Path) -> Option<PathBuf> {
        Self::resolved(&self.movie_pack, base)
    }
    /// The configured quick-save slot, a relative path resolved against
    /// `base` (the settings file's folder); `None` when unset.
    pub fn quick_save(&self, base: &Path) -> Option<PathBuf> {
        let text = self.quick_save.as_deref()?.trim();
        if text.is_empty() {
            return None;
        }
        Some(base.join(text))
    }
    /// Memory card `slot` (0 or 1) as configured, relative paths resolved
    /// against `base` (the settings file's folder); `None` when unset.
    pub fn card(&self, slot: usize, base: &Path) -> Option<CardSource> {
        let text = [&self.card1, &self.card2][slot].as_deref()?.trim();
        if text.is_empty() {
            return None;
        }
        Some(match CardSource::parse(text) {
            CardSource::File(path) if path.is_relative() => CardSource::File(base.join(path)),
            source => source,
        })
    }
}

/// The commented file `ac3 settings --write-default` writes. It parses to
/// [`Settings::default`].
pub const DEFAULT_FILE: &str = r#"# Ace Combat 3 (native port) settings.
#
# This file lives next to the executable as ac3.toml. Every key is optional:
# a missing key keeps the default shown here, and a missing file means all
# defaults. Options on the command line override this file.

[display]
# Window size in pixels, for windowed mode.
window_width = 1920
window_height = 1440
# "windowed", or "borderless" (borderless fullscreen on the current monitor).
# Alt+Enter switches between the two while playing.
mode = "borderless"
# How the picture fills the window:
#   "aspect"  - 4:3 with black bars, as on a TV
#   "stretch" - the whole window
#   "integer" - whole-pixel multiples (sharpest), as close to 4:3 as they
#               allow, centered
scaling = "aspect"
# "nearest" (sharp pixels) or "linear" (smooth).
filter = "nearest"
# Present in step with the monitor's refresh (no tearing). The game's speed
# does not depend on it.
vsync = true
# Show the frame rate in the top-left corner: the game's own frames per
# second and how many of them reached the screen. F3 toggles it while playing.
show_fps = true
# Internal resolution: the game is drawn at this multiple of the
# PlayStation's own resolution, from 1 (the original) to 8. Above 1 it is
# drawn on the graphics card (Vulkan, DX12 or Metal).
internal_resolution = 4
# Which renderer draws the game:
#   "auto"     - the software renderer at 1 (exactly the original), the
#                GPU renderer above
#   "software" - always the software renderer (internal_resolution unused)
#   "gpu"      - the GPU renderer at every resolution, 1 too
renderer = "gpu"
# Above 1, dither shaded surfaces as the PlayStation does (its pattern,
# scaled up). At 1 the game's own dithering always applies.
upscaled_dither = true
# Above 1, draw 3D geometry from the GTE's unrounded screen positions
# instead of whole PlayStation pixels: no vertex "wobble". The game itself is
# unchanged; menus and the HUD are drawn as before.
subpixel = true
# Above 1, with subpixel on, map the textures of 3D surfaces with
# perspective, as a modern graphics card does, instead of the PlayStation's
# affine mapping, which bends and swims on large polygons (the runway, the
# ground, aircraft close up). The game itself is unchanged.
perspective = true
# Texture replacement: PNGs in this folder (and its subfolders) whose names
# end with a replacement key (ac3tex-..., as `ac3 machine --dump-textures`
# names them) are drawn instead of the game's textures, at any size. It
# needs the GPU renderer (internal_resolution above 1, or renderer = "gpu").
# Relative to this file's folder; "" for none. F9 reloads it while playing.
# See docs/ASSET_DUMP.md.
texture_pack = "textures"
# Use the texture pack (false: the game's own textures).
replace_textures = false
# Widescreen in flight: "off" (the original 4:3), "16:9", "16:10" or "21:9"
# (2560 x 1080). Flight shows a wider view at that shape, its HUD a centred
# 4:3 block; menus, the hub and movies stay 4:3. Set a window of that shape
# (or fullscreen) to fill it. Off keeps the game exactly the original.
widescreen = "16:9"
# The CRT screen: the picture, movies included, drawn as on a CRT by the
# bundled Lola Retro presets (shaders/lola-retro next to this file). It gets
# the picture at the game's own resolution (the GPU renderer's picture
# averaged back to it); messages stay on top. While playing, F2 turns it off
# and on, Shift+F3 steps to the next look, F4 swaps curved and flat.
crt = true
# The look: "trinitron-s-video", "consumer-tv-composite", "dos-pc",
# "gamecube" or "playstation".
crt_preset = "trinitron-s-video"
# Curved glass (false: flat).
crt_curved = true
# A RetroArch slang shader preset (.slangp) of your own instead of the
# bundled looks: libretro's slang-shaders (github.com/libretro/slang-shaders)
# or RetroArch's shaders_slang folder, e.g.
# "slang-shaders/crt/crt-lottes.slangp". Relative to this file's folder; ""
# for none. F2 turns it off and on, Shift+F2 loads it again. See
# docs/LIBRASHADER.md.
shader_preset = ""

[effects]
# Flight visual effects of the Japanese release, drawn by the graphics card
# above internal resolution 1. Only the picture changes; the game itself is
# unchanged. Off by default: then the picture is exactly as without them.
# F6 turns them on and off while playing.
enabled = true
# A debug view over the flight world: "off", "depth" (the depth buffer as
# colours: near white to far dark red, the sky blue, no depth magenta) or
# "coverage" (green: the pixel has a depth, red: it has none, blue: sky).
debug = "off"
# The effects, each also needs enabled.
# The atmosphere: the game's distance fog (terrain turning grey from about
# 3 km) is removed and a height haze drawn in its place, by the depth of
# every pixel; terrain fades into the sky where the game stops drawing it.
# Smoke, explosions, the HUD and menus are untouched.
atmosphere = true
# Its haze density, times the default: 1 is light, 3 about the game's own
# depth; 0 removes the fog without a haze. 0 to 10.
haze_density = 1.0
# Far terrain: the mission's terrain beyond the game's own draw distance, to
# the horizon, beneath the game's terrain. Needs atmosphere (without it the
# game's fogged terrain would end in a wall of fog).
far_terrain = true
# Far terrain's ring of tile meshes around the camera, in tiles of about
# 410 m (16 to 96); beyond it the heightmap is ray-marched to the horizon.
far_terrain_tiles = 44
# Far terrain's ray-march step: "low" (faster), "medium" or "high" (finer).
far_terrain_quality = "high"
# The sun: an HDR sun disc, a lens flare in the game's style and bloom on
# bright light (the sun, explosions), in place of the game's own sun and
# flare. The HUD is untouched.
sun = true
# The bloom's strength with sun: 0 (none) to 2.
bloom = 0.7
# The sun's glare: looking into the sun veils the picture (not the HUD) at
# least as much as the game's own sun does, by how visible the sun is and
# how close to the centre. A multiple of that: 0 (none) to 3.
sun_glare = 2.0
# The lens flare's cross-shaped (four-ray) star around the sun.
sun_star = true
# Not implemented yet: these keys are reserved and draw nothing so far.
ocean = false
clouds = false
forest = false
gi = false

[paths]
# Memory card 1, as --card1: a raw 128 KiB .mcr file (relative paths start in
# this file's folder; created when missing), or "none" for no card. Unset: a
# blank card kept in memory, whose saves are lost when the game closes. The
# launcher keeps it in card1.mcr.
card1 = "card1.mcr"
# Memory card 2, as --card2: the same, unset a blank card in memory.
# card2 = "none"
# The quick-save slot: F5 saves the running game there, F8 loads it. Unset:
# next to card 1's file (card1.mcr -> card1.ac3state), else quick.ac3state
# next to the executable.
# quick_save = "quick.ac3state"
# The disc image (or .cue sheet) to boot, as --disc: the PAL disc, or a
# Japanese disc (relative paths start in this file's folder). Unset: the
# launcher finds the Japanese discs next to it (else the PAL .bin in the
# working directory).
# disc = "Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 1) (Rev 1).cue"
# The Japanese release's other disc, as --disc2. Both discs are mounted and
# changed for you when the game asks (F7 changes them by hand). Unset: the
# image next to disc with (Disc 2) / (Disc 1) in its name.
# disc2 = "Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 2) (Rev 1).cue"
# The movie pack, as --movie-pack: a folder of replacement movie frames, one
# folder per movie key as `ac3 export-movies` names them (relative paths
# start in this file's folder; F9 reloads it). The pictures change, never
# the game. Unset: the game's own movie frames.
# movie_pack = "movies"

[controls]
# The controller the game sees on port 1:
#   "auto"      - a DualShock when a gamepad is connected at start, else a
#                 digital pad. The game switches a DualShock to analog: the
#                 left stick flies, and the keyboard arrows move that stick.
#   "digital"   - a digital pad (a gamepad's left stick acts as the d-pad)
#   "dualshock" - always a DualShock
pad_type = "auto"
# Stick dead zone (0.0 .. 0.9 of full deflection).
deadzone = 0.15
# How far LT/RT must be pulled to press L2/R2 (0.05 .. 1.0).
trigger_threshold = 0.3
# Play the game's vibration on the gamepad.
rumble = true
# Carry the sticks past the game's own dead zone. The game ignores each
# stick axis until it passes 60 % of its travel (KEY CONFIG's response
# setting), which drops the smaller axis of a diagonal; on, every movement
# outside the dead zone above reaches the game. Off: as a DualShock sends.
compensate_deadzone = true
# Swap the triggers and the bumpers: LT / RT press what LB / RB are bound to,
# and back. With the game's default key config: throttle on the triggers,
# yaw on the bumpers.
swap_triggers = true

[controls.buttons]
# Gamepad button (Xbox names) = PlayStation button: cross, circle, square,
# triangle, l1, r1, l2, r2, l3, r3, select, start, up, down, left, right,
# or "none".
a = "cross"
b = "circle"
x = "square"
y = "triangle"
lb = "l1"
rb = "r1"
lt = "l2"
rt = "r2"
back = "select"
start = "start"
dpad_up = "up"
dpad_down = "down"
dpad_left = "left"
dpad_right = "right"
left_stick = "l3"
right_stick = "r3"
guide = "none"

[controls.keys]
# The keyboard: each PlayStation button's keys, by physical position (KeyW,
# ArrowUp, Space, ShiftLeft, Numpad0 ...). Escape and the function keys are
# the game window's. With the game's default key config: the arrows pitch
# and roll, W / S speed up and slow down, A / D yaw.
up = ["ArrowUp"]
down = ["ArrowDown"]
left = ["ArrowLeft"]
right = ["ArrowRight"]
cross = ["KeyK"]
circle = ["KeyL"]
square = ["Space", "KeyJ"]
triangle = ["KeyI"]
l1 = ["KeyS"]
r1 = ["KeyW"]
l2 = ["KeyA"]
r2 = ["KeyD"]
l3 = []
r3 = []
select = ["Backspace"]
start = ["Enter"]
"#;

impl Settings {
    /// The settings a player starts with: the launcher's Defaults and the
    /// file it writes first ([`DEFAULT_FILE`]). Borderless 1920x1440, the
    /// graphics card at 4x (dithered), widescreen 16:9, the CRT, the frame
    /// rate and the flight effects on (far terrain high, bloom 0.7, glare
    /// 2x), memory card 1 in `card1.mcr`, the game's own movies (the CRT
    /// suits them), the triggers swapped with the bumpers. [`Default`] is the plain engine's, for tools,
    /// tests and `ac3 machine` without a settings file.
    pub fn recommended() -> Self {
        let mut settings = Self::default();
        let display = &mut settings.display;
        display.window_width = 1920;
        display.window_height = 1440;
        display.mode = WindowMode::Borderless;
        display.show_fps = true;
        display.internal_resolution = 4;
        display.renderer = Renderer::Gpu;
        display.upscaled_dither = true;
        display.replace_textures = false;
        display.widescreen = Widescreen::Wide16x9;
        let effects = &mut settings.effects;
        effects.enabled = true;
        effects.atmosphere = true;
        effects.far_terrain = true;
        effects.far_terrain_quality = crate::effects::far_terrain::Quality::High;
        effects.sun = true;
        effects.bloom = crate::effects::sun::Bloom(0.7);
        effects.sun_glare = crate::effects::sun::Glare(2.0);
        settings.paths.card1 = Some(CARD.into());
        settings.controls.swap_triggers = true;
        settings
    }

    /// `ac3.toml` in the [`player_folder`]: next to the running executable
    /// (the working directory if its path is unknown), or Application
    /// Support for a macOS app.
    pub fn default_path() -> PathBuf {
        player_folder().join(FILE_NAME)
    }

    /// Parse settings text. Returns the settings and the dotted paths of the
    /// keys that were ignored as unknown.
    pub fn parse(text: &str) -> Result<(Self, Vec<String>), String> {
        let mut unknown = Vec::new();
        let deserializer = toml::Deserializer::parse(text).map_err(|e| e.to_string())?;
        let settings: Self = serde_ignored::deserialize(deserializer, |path| {
            unknown.push(path.to_string());
        })
        .map_err(|e| e.to_string())?;
        settings.validate()?;
        Ok((settings, unknown))
    }

    /// What loading checks beyond the types: the window size and the
    /// internal resolution.
    pub fn validate(&self) -> Result<(), String> {
        for (key, value) in [
            ("window_width", self.display.window_width),
            ("window_height", self.display.window_height),
        ] {
            if !(64..=16384).contains(&value) {
                return Err(format!("[display] {key} = {value}: use 64..16384"));
            }
        }
        if crate::present::crt::index(&self.display.crt_preset).is_none() {
            let names: Vec<_> = crate::present::crt::LOOKS.iter().map(|l| l.name).collect();
            return Err(format!(
                "[display] crt_preset = {:?}: use one of {}",
                self.display.crt_preset,
                names.join(", ")
            ));
        }
        let scale = self.display.internal_resolution;
        if !(1..=crate::gpu_renderer::MAX_SCALE).contains(&scale) {
            return Err(format!(
                "[display] internal_resolution = {scale}: use 1..{}",
                crate::gpu_renderer::MAX_SCALE
            ));
        }
        let density = self.effects.haze_density;
        if !density.valid() {
            return Err(format!(
                "[effects] haze_density = {}: use 0 .. {}",
                density.0,
                crate::effects::atmosphere_fx::HazeDensity::MAX
            ));
        }
        let bloom = self.effects.bloom;
        if !bloom.valid() {
            return Err(format!(
                "[effects] bloom = {}: use 0 .. {}",
                bloom.0,
                crate::effects::sun::Bloom::MAX
            ));
        }
        let glare = self.effects.sun_glare;
        if !glare.valid() {
            return Err(format!(
                "[effects] sun_glare = {}: use 0 .. {}",
                glare.0,
                crate::effects::sun::Glare::MAX
            ));
        }
        Ok(())
    }

    /// The checks before saving: [`Self::validate`], and the `[controls]`
    /// ranges (which loading leaves to `Bindings::sanitized` to clamp).
    pub fn check(&self) -> Result<(), String> {
        self.validate()?;
        let controls = &self.controls;
        if let Some(name) = controls.keys.unknown().first() {
            return Err(format!(
                "[controls.keys] {name:?}: not a key the game can use (Escape and the function keys are the window's)"
            ));
        }
        for (key, value, low, high) in [
            ("deadzone", controls.deadzone, 0.0, 0.9),
            ("trigger_threshold", controls.trigger_threshold, 0.05, 1.0),
        ] {
            if !(low..=high).contains(&value) {
                return Err(format!("[controls] {key} = {value}: use {low} .. {high}"));
            }
        }
        Ok(())
    }

    /// Save to `path` as [`Document::set`] edits a file: the file's comments
    /// and unknown keys stay; a file that does not exist starts as
    /// [`DEFAULT_FILE`]. The values are checked first ([`Self::check`]).
    pub fn save(&self, path: &Path) -> crate::Result<()> {
        let mut document = Document::open(path)?;
        document
            .set(self)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        document.write(path)
    }

    /// Load `path`: defaults when it does not exist; unknown keys are
    /// reported on stderr and ignored.
    pub fn load(path: &Path) -> crate::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("{}: {error}", path.display()).into()),
        };
        let (settings, unknown) =
            Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for key in unknown {
            eprintln!(
                "ac3: warning: {}: unknown setting {key} ignored",
                path.display()
            );
        }
        Ok(settings)
    }

    /// The settings as TOML, without comments (for a settings editor).
    pub fn to_toml(&self) -> crate::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Write [`DEFAULT_FILE`] to `path`; an existing file is kept unless
    /// `force`.
    pub fn write_default(path: &Path, force: bool) -> crate::Result<()> {
        if path.exists() && !force {
            return Err(format!(
                "{} exists; delete it, give another path, or add --force",
                path.display()
            )
            .into());
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, DEFAULT_FILE)?;
        Ok(())
    }
}

// ---- editing ---------------------------------------------------------------------------

/// Keys a table also accepts under another name (the serde aliases of
/// `gamepad::ButtonMap`): `(table, key, alias)`. An edit keeps the name the
/// file uses.
const ALIASES: &[(&str, &str, &str)] = &[
    ("controls.buttons", "back", "view"),
    ("controls.buttons", "start", "menu"),
    ("controls.buttons", "left_stick", "ls"),
    ("controls.buttons", "right_stick", "rs"),
];

/// A settings file open for editing. Values change in place, so the file's
/// comments, layout and unknown keys stay. [`Document::set`] writes
/// [`Settings`] into it:
/// - a key whose value is unchanged keeps its text;
/// - a changed value keeps its comments (the lines above it and the end of
///   its line);
/// - a missing key is added only when its value is not the default (a
///   missing key already means the default). It goes where a commented-out
///   `# key = ...` line at the end of its table stands and replaces that
///   line, as `[paths]` in [`DEFAULT_FILE`] has them; else after the
///   table's last key;
/// - a memory card or quick-save slot that becomes unset is written as `""`
///   (which also means unset), so its comments stay;
/// - unknown keys and tables are not touched.
///
/// A file with Windows line ends (CRLF) keeps them on every line.
#[derive(Clone, Debug)]
pub struct Document {
    doc: toml_edit::DocumentMut,
    /// The text had CRLF line ends.
    crlf: bool,
}

/// What [`Document::read`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contents {
    /// The settings. A key whose value does not load takes its default.
    pub settings: Settings,
    /// Unknown keys (ignored by the game, kept by [`Document::set`]).
    pub unknown: Vec<String>,
    /// Why the file does not load as it is ([`Settings::parse`]'s error).
    pub error: Option<String>,
    /// The keys that took their default because their value does not load.
    pub reset: Vec<String>,
}

impl Document {
    /// [`DEFAULT_FILE`], for a settings file that does not exist yet.
    pub fn default_file() -> Self {
        Self::parse(DEFAULT_FILE).expect("DEFAULT_FILE is valid TOML")
    }

    /// Settings text for editing. Only the TOML syntax is checked here; the
    /// values are read by [`Self::read`].
    pub fn parse(text: &str) -> Result<Self, String> {
        let doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            doc,
            crlf: text.contains("\r\n"),
        })
    }

    /// `path` for editing, or [`Self::default_file`] when it does not exist.
    pub fn open(path: &Path) -> crate::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default_file()),
            Err(error) => Err(format!("{}: {error}", path.display()).into()),
        }
    }

    /// The settings the document holds, as loading reads them. When the file
    /// does not load as it is, every value that loads on its own is still
    /// used, and the others are listed in [`Contents::reset`].
    pub fn read(&self) -> Contents {
        let error = match Settings::parse(&self.doc.to_string()) {
            Ok((settings, unknown)) => {
                return Contents {
                    settings,
                    unknown,
                    ..Contents::default()
                };
            }
            Err(error) => error,
        };
        // Key by key: keep each value that loads.
        let mut good = toml_edit::DocumentMut::new();
        let mut reset = Vec::new();
        for (path, value) in leaves(self.doc.as_table()) {
            let mut trial = good.clone();
            put(trial.as_table_mut(), &path, value);
            if Settings::parse(&trial.to_string()).is_ok() {
                good = trial;
            } else {
                reset.push(path.join("."));
            }
        }
        let (settings, unknown) = Settings::parse(&good.to_string()).unwrap_or_default();
        Contents {
            settings,
            unknown,
            error: Some(error),
            reset,
        }
    }

    /// Write `settings` into the document (see [`Document`]). The values are
    /// checked first ([`Settings::check`]), and the edited text must load as
    /// `settings`; on an error the document is unchanged.
    pub fn set(&mut self, settings: &Settings) -> Result<(), String> {
        settings.check()?;
        let new = plain(settings)?;
        let defaults = plain(&Settings::default())?;
        let mut doc = self.doc.clone();
        let mut added = Vec::new();
        merge(
            doc.as_table_mut(),
            new.as_table(),
            Some(defaults.as_table()),
            &mut Vec::new(),
            false,
            &mut added,
        );
        for (path, key) in &added {
            uncomment(&mut doc, path, key);
        }
        let (read, _) = Settings::parse(&doc.to_string())
            .map_err(|e| format!("the edited file does not load ({e})"))?;
        if unset_blank(read) != unset_blank(settings.clone()) {
            return Err("the edited file does not load as the settings given".into());
        }
        self.doc = doc;
        Ok(())
    }

    /// Write the document to `path`: a temporary file next to it, then a
    /// rename over it. Creates the folder.
    pub fn write(&self, path: &Path) -> crate::Result<()> {
        if let Some(folder) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        }
        let mut name = path.as_os_str().to_owned();
        name.push(".tmp");
        let temporary = PathBuf::from(name);
        let written = std::fs::write(&temporary, self.to_string())
            .and_then(|()| std::fs::rename(&temporary, path));
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("{}: {error}", path.display()).into());
        }
        Ok(())
    }
}

impl std::fmt::Display for Document {
    /// The file's text. toml_edit ends key lines with LF; a CRLF file gets
    /// CRLF everywhere again.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = self.doc.to_string();
        if self.crlf {
            f.write_str(&text.replace("\r\n", "\n").replace('\n', "\r\n"))
        } else {
            f.write_str(&text)
        }
    }
}

/// `settings` as a plain document, with unset paths as `""`.
fn plain(settings: &Settings) -> Result<toml_edit::DocumentMut, String> {
    let mut settings = settings.clone();
    let paths = &mut settings.paths;
    for path in [
        &mut paths.card1,
        &mut paths.card2,
        &mut paths.quick_save,
        &mut paths.disc,
        &mut paths.disc2,
        &mut paths.movie_pack,
    ] {
        path.get_or_insert_with(String::new);
    }
    let text = toml::to_string(&settings).map_err(|e| e.to_string())?;
    text.parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())
}

/// Blank paths as unset (they mean the same).
fn unset_blank(mut settings: Settings) -> Settings {
    let paths = &mut settings.paths;
    for path in [
        &mut paths.card1,
        &mut paths.card2,
        &mut paths.quick_save,
        &mut paths.disc,
        &mut paths.disc2,
        &mut paths.movie_pack,
    ] {
        if path.as_deref().is_some_and(|text| text.trim().is_empty()) {
            *path = None;
        }
    }
    settings
}

/// Whether two values are the same TOML value (whatever their spelling).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Float(a), Value::Float(b)) => a.value() == b.value(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same(a, b))
        }
        _ => false,
    }
}

/// The name `key` has in `table` (at `path`): an alias the file uses
/// instead of it, else `key`.
fn spelling(table: &dyn TableLike, path: &[String], key: &str) -> String {
    let at = path.join(".");
    if !table.contains_key(key) {
        for &(table_path, name, alias) in ALIASES {
            if table_path == at && name == key && table.contains_key(alias) {
                return alias.to_owned();
            }
        }
    }
    key.to_owned()
}

/// Write the values of `new` into `target` (see [`Document`]). `defaults`
/// holds the default values at the same place, `path` is where `target` is,
/// and `inline` says whether it is (inside) an inline table. The keys added
/// to standard tables are collected in `added` as (table path, key).
fn merge(
    target: &mut dyn TableLike,
    new: &dyn TableLike,
    defaults: Option<&dyn TableLike>,
    path: &mut Vec<String>,
    inline: bool,
    added: &mut Vec<(Vec<String>, String)>,
) {
    for (key, item) in new.iter() {
        let name = spelling(target, path, key);
        let default = defaults.and_then(|table| table.get(key));
        if let Some(new_table) = item.as_table_like() {
            path.push(key.to_owned());
            let defaults = default.and_then(Item::as_table_like);
            if target.get(&name).is_some_and(Item::is_table_like) {
                let existing = target.get_mut(&name).expect("checked above");
                let inline = inline || existing.is_inline_table();
                let existing = existing.as_table_like_mut().expect("checked above");
                merge(existing, new_table, defaults, path, inline, added);
            } else {
                // Missing (or not a table, which does not load anyway). A new
                // standard table shows its header only when it gets keys.
                let mut fresh = if inline {
                    Item::Value(Value::InlineTable(InlineTable::new()))
                } else {
                    let mut table = Table::new();
                    table.set_implicit(true);
                    Item::Table(table)
                };
                let table = fresh.as_table_like_mut().expect("a new table");
                merge(table, new_table, defaults, path, inline, added);
                if !table.is_empty() || target.contains_key(&name) {
                    target.insert(&name, fresh);
                }
            }
            path.pop();
            continue;
        }
        let Some(value) = item.as_value() else {
            continue;
        };
        match target.get_mut(&name) {
            Some(Item::Value(old)) => {
                if !same(old, value) {
                    let decor = old.decor().clone();
                    *old = value.clone();
                    *old.decor_mut() = decor;
                }
            }
            Some(other) => *other = Item::Value(value.clone()),
            None => {
                if default
                    .and_then(Item::as_value)
                    .is_some_and(|default| same(default, value))
                {
                    continue;
                }
                let mut value = value.clone();
                value.decor_mut().clear();
                target.insert(&name, Item::Value(value));
                if !inline && !target.is_dotted() {
                    added.push((path.clone(), name));
                }
            }
        }
    }
}

/// Every value in `table` (inline tables opened up) with its key path.
fn leaves(table: &dyn TableLike) -> Vec<(Vec<String>, Value)> {
    let mut found = Vec::new();
    for (key, item) in table.iter() {
        if let Some(inner) = item.as_table_like() {
            for (mut path, value) in leaves(inner) {
                path.insert(0, key.to_owned());
                found.push((path, value));
            }
        } else if let Some(value) = item.as_value() {
            let mut value = value.clone();
            value.decor_mut().clear();
            found.push((vec![key.to_owned()], value));
        }
    }
    found
}

/// Put `value` at the key path `path` under `table`, making tables as needed.
fn put(table: &mut Table, path: &[String], value: Value) {
    let Some((key, parents)) = path.split_last() else {
        return;
    };
    let mut table = table;
    for parent in parents {
        let item = table
            .entry(parent)
            .or_insert_with(|| Item::Table(Table::new()));
        match item.as_table_mut() {
            Some(inner) => table = inner,
            None => return,
        }
    }
    table.insert(key, Item::Value(value));
}

/// The standard table at `path` under `root`.
fn table_at<'a>(root: &'a Table, path: &[String]) -> Option<&'a Table> {
    path.iter()
        .try_fold(root, |table, key| table.get(key).and_then(Item::as_table))
}

fn table_at_mut<'a>(root: &'a mut Table, path: &[String]) -> Option<&'a mut Table> {
    path.iter().try_fold(root, |table, key| {
        table.get_mut(key).and_then(Item::as_table_mut)
    })
}

/// Where the comments after the table at document position `after` are
/// kept: the prefix of the next table header (by its key path), or the
/// document's trailing text (`Some(None)`). `None` when the next header is
/// an array of tables.
fn next_header(root: &Table, after: isize) -> Option<Option<Vec<String>>> {
    /// The nearest header after `after`: (position, its path; None for an
    /// array of tables).
    type Best = Option<(isize, Option<Vec<String>>)>;
    fn consider(best: &mut Best, after: isize, position: Option<isize>, path: Option<Vec<String>>) {
        if let Some(position) = position.filter(|&position| position > after)
            && best.as_ref().is_none_or(|(best, _)| position < *best)
        {
            *best = Some((position, path));
        }
    }
    fn visit(table: &Table, path: &mut Vec<String>, after: isize, best: &mut Best) {
        for (key, item) in table.iter() {
            if let Some(inner) = item.as_table() {
                path.push(key.to_owned());
                if !inner.is_dotted() && !inner.is_implicit() {
                    consider(best, after, inner.position(), Some(path.clone()));
                }
                visit(inner, path, after, best);
                path.pop();
            } else if let Some(array) = item.as_array_of_tables() {
                for inner in array.iter() {
                    consider(best, after, inner.position(), None);
                }
            }
        }
    }
    let mut best = None;
    visit(root, &mut Vec::new(), after, &mut best);
    match best {
        Some((_, Some(path))) => Some(Some(path)),
        Some((_, None)) => None,
        None => Some(None),
    }
}

/// `text` split around its first line that comments out `key` (`# key =
/// ...`): the lines before it and the lines after it.
fn split_at_commented(text: &str, key: &str) -> Option<(String, String)> {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let commented = line
            .trim_start()
            .strip_prefix('#')
            .and_then(|rest| rest.trim_start().strip_prefix(key))
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        if commented {
            return Some((text[..at].to_owned(), text[at + line.len()..].to_owned()));
        }
        at += line.len();
    }
    None
}

/// Move `key`, just added to the standard table at `path`, to where a
/// commented-out `# key = ...` line among the comments that end the table
/// stands, replacing that line (see [`Document`]).
fn uncomment(doc: &mut toml_edit::DocumentMut, path: &[String], key: &str) {
    let Some(position) = table_at(doc.as_table(), path).and_then(Table::position) else {
        return;
    };
    let Some(next) = next_header(doc.as_table(), position) else {
        return;
    };
    let trailing = match &next {
        Some(next) => table_at(doc.as_table(), next)
            .and_then(|table| table.decor().prefix())
            .and_then(|prefix| prefix.as_str()),
        None => doc.trailing().as_str(),
    };
    let Some((above, rest)) = trailing.and_then(|text| split_at_commented(text, key)) else {
        return;
    };
    match &next {
        Some(next) => match table_at_mut(doc.as_table_mut(), next) {
            Some(table) => table.decor_mut().set_prefix(rest),
            None => return,
        },
        None => doc.set_trailing(rest),
    }
    if let Some(mut key) = table_at_mut(doc.as_table_mut(), path).and_then(|t| t.key_mut(key)) {
        key.leaf_decor_mut().set_prefix(above);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_matches_the_defaults() {
        let (settings, unknown) = Settings::parse(DEFAULT_FILE).unwrap();
        assert_eq!(settings, Settings::recommended());
        assert!(unknown.is_empty(), "{unknown:?}");
        assert!(Settings::recommended().check().is_ok());
    }

    #[test]
    fn missing_keys_take_the_defaults() {
        let (settings, unknown) = Settings::parse("").unwrap();
        assert_eq!(settings, Settings::default());
        assert!(unknown.is_empty());
        let (settings, _) = Settings::parse("[display]\nscaling = \"integer\"\n").unwrap();
        assert_eq!(settings.display.scaling, Scaling::Integer);
        assert_eq!(
            Display {
                scaling: Scaling::Aspect,
                ..settings.display
            },
            Display::default()
        );
        assert_eq!(settings.paths, Paths::default());
    }

    #[test]
    fn every_key_parses() {
        let text = r#"
            [display]
            window_width = 1920
            window_height = 1080
            mode = "borderless"
            scaling = "stretch"
            filter = "linear"
            vsync = false
            internal_resolution = 4
            renderer = "gpu"
            upscaled_dither = true
            perspective = false
            texture_pack = "packs/hd"
            replace_textures = false
            widescreen = "16:9"
            shader_preset = "shaders/crt.slangp"
            crt = false
            crt_preset = "dos-pc"
            crt_curved = false
            [paths]
            card1 = "cards/one.mcr"
            card2 = "none"
        "#;
        let (settings, unknown) = Settings::parse(text).unwrap();
        assert!(unknown.is_empty());
        assert_eq!(
            settings.display,
            Display {
                window_width: 1920,
                window_height: 1080,
                mode: WindowMode::Borderless,
                scaling: Scaling::Stretch,
                filter: Filter::Linear,
                vsync: false,
                show_fps: false,
                internal_resolution: 4,
                renderer: Renderer::Gpu,
                upscaled_dither: true,
                subpixel: true,
                perspective: false,
                texture_pack: "packs/hd".into(),
                replace_textures: false,
                widescreen: Widescreen::Wide16x9,
                shader_preset: "shaders/crt.slangp".into(),
                crt: false,
                crt_preset: "dos-pc".into(),
                crt_curved: false,
                crt_folder: None,
            }
        );
        let base = Path::new("game");
        assert_eq!(
            settings.paths.card(0, base),
            Some(CardSource::File(base.join("cards/one.mcr")))
        );
        assert_eq!(settings.paths.card(1, base), Some(CardSource::Absent));
        // Serialized and parsed again: the same settings.
        let again = Settings::parse(&settings.to_toml().unwrap()).unwrap().0;
        assert_eq!(again, settings);
    }

    #[test]
    fn unknown_keys_are_reported_and_ignored() {
        let text = "top = 1\n[display]\nvsync = false\nshader = \"crt\"\n[later]\nx = 2\n";
        let (settings, mut unknown) = Settings::parse(text).unwrap();
        assert!(!settings.display.vsync);
        unknown.sort();
        assert_eq!(unknown, ["display.shader", "later", "top"]);
    }

    #[test]
    fn bad_values_are_errors() {
        let error = Settings::parse("[display]\nscaling = \"zoom\"\n").unwrap_err();
        assert!(error.contains("zoom"), "{error}");
        assert!(Settings::parse("[display]\nvsync = 1\n").is_err());
        assert!(Settings::parse("[display]\nwindow_width = 0\n").is_err());
        assert!(Settings::parse("[display\n").is_err());
        for scale in [0, 9] {
            let text = format!("[display]\ninternal_resolution = {scale}\n");
            let error = Settings::parse(&text).unwrap_err();
            assert!(error.contains("internal_resolution"), "{error}");
        }
        assert!(Settings::parse("[display]\nrenderer = \"vulkan\"\n").is_err());
    }

    #[test]
    fn renderer_choice() {
        let display = |internal_resolution, renderer| Display {
            internal_resolution,
            renderer,
            ..Display::default()
        };
        assert_eq!(
            Display::default().gpu_renderer(),
            None,
            "software by default"
        );
        assert_eq!(display(1, Renderer::Auto).gpu_renderer(), None);
        let options = display(4, Renderer::Auto).gpu_renderer().unwrap();
        assert_eq!((options.scale, options.dither), (4, false));
        assert_eq!(display(1, Renderer::Gpu).gpu_renderer().unwrap().scale, 1);
        assert_eq!(display(8, Renderer::Software).gpu_renderer(), None);
        let dithered = Display {
            upscaled_dither: true,
            ..display(2, Renderer::Auto)
        };
        assert!(dithered.gpu_renderer().unwrap().dither);
        assert_eq!(Renderer::parse("GPU").unwrap(), Renderer::Gpu);
        assert!(Renderer::parse("vulkan").is_err());
    }

    #[test]
    fn perspective_needs_the_gpu_renderer_above_1x_and_subpixel() {
        let display = |internal_resolution, renderer, subpixel, perspective| Display {
            internal_resolution,
            renderer,
            subpixel,
            perspective,
            ..Display::default()
        };
        // On by default, but the default 1x software renderer never uses it.
        assert!(Display::default().perspective);
        assert!(!Display::default().perspective_textures());
        assert!(display(4, Renderer::Auto, true, true).perspective_textures());
        assert!(display(2, Renderer::Gpu, true, true).perspective_textures());
        assert!(
            !display(1, Renderer::Gpu, true, true).perspective_textures(),
            "1x is exact"
        );
        assert!(!display(4, Renderer::Software, true, true).perspective_textures());
        assert!(
            !display(4, Renderer::Auto, false, true).perspective_textures(),
            "no depths"
        );
        assert!(!display(4, Renderer::Auto, true, false).perspective_textures());
        for value in [true, false] {
            let text = format!("[display]\nperspective = {value}\n");
            assert_eq!(Settings::parse(&text).unwrap().0.display.perspective, value);
        }
        assert!(Settings::parse("[display]\nperspective = \"on\"\n").is_err());
    }

    #[test]
    fn texture_pack_folder() {
        let base = Path::new("game");
        let display = Display::default();
        assert_eq!(
            display.texture_pack_dir(base),
            Some(base.join("textures")),
            "a textures folder next to the settings file by default"
        );
        let off = Display {
            replace_textures: false,
            ..Display::default()
        };
        assert_eq!(off.texture_pack_dir(base), None);
        let blank = Display {
            texture_pack: "  ".into(),
            ..Display::default()
        };
        assert_eq!(blank.texture_pack_dir(base), None);
        let absolute = std::env::temp_dir().join("pack");
        let elsewhere = Display {
            texture_pack: absolute.display().to_string(),
            ..Display::default()
        };
        assert_eq!(elsewhere.texture_pack_dir(base), Some(absolute));
    }

    #[test]
    fn widescreen_choice() {
        assert_eq!(Display::default().widescreen, Widescreen::Off);
        assert_eq!(Widescreen::Off.aspect(), None);
        for (name, setting) in [
            ("16:9", Widescreen::Wide16x9),
            ("16:10", Widescreen::Wide16x10),
            ("21:9", Widescreen::Wide21x9),
        ] {
            assert_eq!(Widescreen::parse(name).unwrap(), setting);
            assert_eq!(setting.name(), name);
            assert_eq!(setting.aspect().unwrap().name(), name);
            let text = format!("[display]\nwidescreen = \"{name}\"\n");
            assert_eq!(
                Settings::parse(&text).unwrap().0.display.widescreen,
                setting
            );
        }
        assert!(Widescreen::parse("4:3").is_err());
        assert!(Settings::parse("[display]\nwidescreen = \"32:9\"\n").is_err());
    }

    #[test]
    fn card_paths() {
        let base = Path::new("base");
        let paths = Paths {
            card1: Some(String::new()),
            card2: None,
            quick_save: Some(" ".into()),
            disc: Some("  ".into()),
            disc2: None,
            movie_pack: Some(" ".into()),
        };
        assert_eq!(paths.card(0, base), None, "empty means unset");
        assert_eq!(paths.card(1, base), None);
        assert_eq!(paths.quick_save(base), None, "blank means unset");
        assert_eq!((paths.disc(base), paths.disc2(base)), (None, None));
        assert_eq!(paths.movie_pack(base), None, "blank means unset");
        let absolute = std::env::temp_dir().join("card.mcr");
        let paths = Paths {
            card1: Some(absolute.display().to_string()),
            card2: Some("NONE".into()),
            quick_save: Some("saves/quick.ac3state".into()),
            disc: Some("discs/one.cue".into()),
            disc2: Some(absolute.display().to_string()),
            movie_pack: Some("packs/movies".into()),
        };
        assert_eq!(
            paths.card(0, base),
            Some(CardSource::File(absolute.clone()))
        );
        assert_eq!(paths.card(1, base), Some(CardSource::Absent));
        assert_eq!(
            paths.quick_save(base),
            Some(base.join("saves/quick.ac3state"))
        );
        assert_eq!(paths.disc(base), Some(base.join("discs/one.cue")));
        assert_eq!(paths.disc2(base), Some(absolute));
        assert_eq!(paths.movie_pack(base), Some(base.join("packs/movies")));
    }

    #[test]
    fn command_line_names() {
        assert_eq!(Scaling::parse("Integer").unwrap(), Scaling::Integer);
        assert_eq!(
            WindowMode::parse("borderless").unwrap().name(),
            "borderless"
        );
        assert_eq!(Filter::parse("linear").unwrap(), Filter::Linear);
        let error = Filter::parse("cubic").unwrap_err().to_string();
        assert!(error.contains("nearest linear"), "{error}");
    }

    #[test]
    fn load_and_write_default() {
        let dir = std::env::temp_dir().join(format!("ac3-settings-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        Settings::write_default(&path, false).unwrap();
        assert!(Settings::write_default(&path, false).is_err(), "kept");
        Settings::write_default(&path, true).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), Settings::recommended());
        std::fs::write(&path, "[display]\nfilter = \"linear\"\n").unwrap();
        assert_eq!(
            Settings::load(&path).unwrap().display.filter,
            Filter::Linear
        );
        std::fs::write(&path, "[display]\nfilter = 3\n").unwrap();
        let error = Settings::load(&path).unwrap_err().to_string();
        assert!(error.contains(FILE_NAME), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---- editing ----

    /// `text` with `edit` applied to its settings through [`Document::set`].
    fn edited(text: &str, edit: impl FnOnce(&mut Settings)) -> String {
        let mut document = Document::parse(text).unwrap();
        let mut settings = document.read().settings;
        edit(&mut settings);
        document.set(&settings).unwrap();
        let text = document.to_string();
        assert_eq!(
            unset_blank(Settings::parse(&text).unwrap().0),
            unset_blank(settings),
            "{text}"
        );
        text
    }

    const USER_FILE: &str = "# My own settings\n\
        [display]\n\
        # wide window\n\
        window_width = 1600  # wide\n\
        window_height = 1200\n\
        scaling = 'stretch'\n\
        shader = \"crt\"  # not a setting yet\n\
        \n\
        [paths]\n\
        card1 = \"cards/one.mcr\"  # my card\n\
        \n\
        [later]\n\
        x = 2\n";

    #[test]
    fn editing_keeps_comments_layout_and_unknown_keys() {
        // Nothing changed: the text stays byte for byte.
        assert_eq!(edited(USER_FILE, |_| {}), USER_FILE);
        let text = edited(USER_FILE, |settings| {
            settings.display.window_width = 1920;
            settings.display.vsync = false;
            settings.controls.deadzone = 0.2;
        });
        assert_eq!(
            text,
            "# My own settings\n\
            [display]\n\
            # wide window\n\
            window_width = 1920  # wide\n\
            window_height = 1200\n\
            scaling = 'stretch'\n\
            shader = \"crt\"  # not a setting yet\n\
            vsync = false\n\
            \n\
            [paths]\n\
            card1 = \"cards/one.mcr\"  # my card\n\
            \n\
            [later]\n\
            x = 2\n\
            \n\
            [controls]\n\
            deadzone = 0.2\n"
        );
        let unknown = Document::parse(&text).unwrap().read().unknown;
        assert_eq!(unknown, ["display.shader", "later"]);
    }

    #[test]
    fn unset_paths_become_blank_and_keep_their_comments() {
        let text = edited(USER_FILE, |settings| settings.paths.card1 = None);
        assert!(text.contains("card1 = \"\"  # my card\n"), "{text}");
        let settings = Settings::parse(&text).unwrap().0;
        assert_eq!(settings.paths.card(0, Path::new("base")), None);
        // A blank path that stays unset is not touched.
        assert_eq!(edited(&text, |_| {}), text);
    }

    #[test]
    fn the_default_file_is_edited_in_place() {
        // Unchanged settings: DEFAULT_FILE as it is.
        assert_eq!(edited(DEFAULT_FILE, |_| {}), DEFAULT_FILE);
        // Every value changed: still every comment of DEFAULT_FILE, except
        // the commented-out [paths] keys, which became keys.
        let text = edited(DEFAULT_FILE, |settings| {
            *settings = Settings {
                display: Display {
                    window_width: 1920,
                    window_height: 1440,
                    mode: WindowMode::Borderless,
                    scaling: Scaling::Integer,
                    filter: Filter::Linear,
                    vsync: false,
                    show_fps: false,
                    internal_resolution: 3,
                    renderer: Renderer::Software,
                    upscaled_dither: true,
                    subpixel: true,
                    perspective: false,
                    texture_pack: "packs/hd".into(),
                    replace_textures: false,
                    widescreen: Widescreen::Wide21x9,
                    shader_preset: "shaders/crt.slangp".into(),
                    crt: false,
                    crt_preset: "gamecube".into(),
                    crt_curved: false,
                    crt_folder: None,
                },
                effects: Effects {
                    enabled: true,
                    debug: crate::gpu_renderer::fx::DebugView::Coverage,
                    atmosphere: true,
                    gi: true,
                    ..Effects::default()
                },
                paths: Paths {
                    card1: Some("cards/one.mcr".into()),
                    card2: Some("none".into()),
                    quick_save: Some("saves/quick.ac3state".into()),
                    disc: Some("discs/one.cue".into()),
                    disc2: Some("discs/two.cue".into()),
                    movie_pack: Some("packs/movies".into()),
                },
                controls: crate::gamepad::Bindings {
                    pad_type: crate::gamepad::PadChoice::DualShock,
                    deadzone: 0.25,
                    trigger_threshold: 0.5,
                    rumble: false,
                    compensate_deadzone: false,
                    swap_triggers: false,
                    keys: crate::keyboard::KeyMap {
                        r1: vec!["KeyE".into(), "ShiftLeft".into()],
                        ..Default::default()
                    },
                    buttons: crate::gamepad::ButtonMap {
                        a: crate::gamepad::PsButton::Circle,
                        guide: crate::gamepad::PsButton::Start,
                        ..Default::default()
                    },
                },
            };
        });
        for line in DEFAULT_FILE.lines().filter(|line| line.starts_with('#')) {
            let commented_key = [
                "# card1 =",
                "# card2 =",
                "# quick_save =",
                "# disc =",
                "# disc2 =",
                "# movie_pack =",
            ]
            .iter()
            .any(|key| line.starts_with(key));
            assert_eq!(text.contains(line), !commented_key, "{line}\n{text}");
        }
        assert!(
            text.contains("# launcher keeps it in card1.mcr.\ncard1 = \"cards/one.mcr\"\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "# Memory card 2, as --card2: the same, unset a blank card in memory.\n\
                card2 = \"none\"\n\
                # The quick-save slot"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "# next to the executable.\nquick_save = \"saves/quick.ac3state\"\n# The disc image"
            ),
            "{text}"
        );
        assert!(
            text.contains("# working directory).\ndisc = \"discs/one.cue\"\n# The Japanese"),
            "{text}"
        );
        assert!(
            text.contains(
                "# image next to disc with (Disc 2) / (Disc 1) in its name.\n\
                disc2 = \"discs/two.cue\"\n# The movie pack"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "# the game. Unset: the game's own movie frames.\n\
                movie_pack = \"packs/movies\"\n\n[controls]"
            ),
            "{text}"
        );
        assert!(
            text.contains("\nmode = \"borderless\"\n# How the picture"),
            "{text}"
        );
        assert!(
            text.contains("\ninternal_resolution = 3\n# Which renderer"),
            "{text}"
        );
        assert!(text.contains("\nrenderer = \"software\"\n"), "{text}");
        assert!(
            text.contains("\ntexture_pack = \"packs/hd\"\n# Use the texture pack"),
            "{text}"
        );
        assert!(text.contains("\nreplace_textures = false\n"), "{text}");
        assert!(text.contains("\nwidescreen = \"21:9\"\n"), "{text}");
        assert!(
            text.contains("\nperspective = false\n# Texture replacement"),
            "{text}"
        );
        assert!(text.contains("\na = \"circle\"\n"), "{text}");
        assert!(text.contains("\nguide = \"start\"\n"), "{text}");
        assert!(
            text.contains("# F6 turns them on and off while playing.\nenabled = true\n"),
            "{text}"
        );
        assert!(text.contains("\ndebug = \"coverage\"\n"), "{text}");
        assert!(text.contains("\natmosphere = true\n"), "{text}");
        assert!(text.contains("\ngi = true\n"), "{text}");
    }

    #[test]
    fn effects_are_off_by_default_and_every_key_parses() {
        use crate::gpu_renderer::fx::DebugView;
        let effects = Effects::default();
        assert!(!effects.enabled);
        assert_eq!(effects.debug, DebugView::Off);
        assert!(effects.unimplemented_set().is_empty());
        let config = effects.config(None);
        assert!(!config.enabled);
        assert_eq!(config.x_scale, None);
        let text = "[effects]\nenabled = true\ndebug = \"depth\"\natmosphere = true\n\
                    far_terrain = true\nocean = true\nclouds = true\nforest = true\n\
                    sun = true\ngi = true\n";
        let (settings, unknown) = Settings::parse(text).unwrap();
        assert!(unknown.is_empty(), "{unknown:?}");
        let effects = settings.effects;
        assert!(effects.enabled);
        assert_eq!(effects.debug, DebugView::Depth);
        assert_eq!(effects.unimplemented_set(), Effects::UNIMPLEMENTED);
        let config = effects.config(Some(ac3_flight::widescreen::Aspect::Wide16x9));
        assert_eq!(config.x_scale, Some((3, 4)));
        assert!(Settings::parse("[effects]\ndebug = \"normals\"\n").is_err());
        assert!(Settings::parse("[effects]\nenabled = \"yes\"\n").is_err());
        // The atmosphere's haze density: 1 by default, a number 0 to 10.
        use crate::effects::atmosphere_fx::HazeDensity;
        assert_eq!(effects.haze_density, HazeDensity(1.0));
        assert!(effects.atmosphere, "implemented: not in the reserved list");
        // The sun and its bloom strength: 0.5 by default, a number 0 to 2.
        use crate::effects::sun::Bloom;
        assert!(effects.sun, "implemented: not in the reserved list");
        assert_eq!(effects.bloom, Bloom(0.5));
        for (text, value) in [("1", 1.0), ("0.35", 0.35), ("0", 0.0), ("2.0", 2.0)] {
            let (settings, unknown) =
                Settings::parse(&format!("[effects]\nbloom = {text}\n")).unwrap();
            assert!(unknown.is_empty());
            assert_eq!(settings.effects.bloom, Bloom(value), "{text}");
        }
        for bad in ["-0.5", "2.5", "nan", "\"soft\""] {
            assert!(
                Settings::parse(&format!("[effects]\nbloom = {bad}\n")).is_err(),
                "{bad}"
            );
        }
        // The sun's glare (1 by default, 0 to 3) and star (on by default).
        use crate::effects::sun::{Glare, Star};
        assert_eq!(Effects::default().sun_glare, Glare(1.0));
        assert_eq!(Effects::default().sun_star, Star(true));
        let (parsed, unknown) =
            Settings::parse("[effects]\nsun_glare = 2.5\nsun_star = false\n").unwrap();
        assert!(unknown.is_empty());
        assert_eq!(parsed.effects.sun_glare, Glare(2.5));
        assert_eq!(parsed.effects.sun_star, Star(false));
        for bad in ["-1", "3.5", "nan"] {
            assert!(
                Settings::parse(&format!("[effects]\nsun_glare = {bad}\n")).is_err(),
                "{bad}"
            );
        }
        assert!(Settings::parse("[effects]\nsun_star = 1\n").is_err());
        for (text, value) in [("3", 3.0), ("0.5", 0.5), ("0", 0.0), ("10.0", 10.0)] {
            let (settings, unknown) =
                Settings::parse(&format!("[effects]\nhaze_density = {text}\n")).unwrap();
            assert!(unknown.is_empty());
            assert_eq!(settings.effects.haze_density, HazeDensity(value), "{text}");
        }
        for bad in ["-1", "10.5", "nan", "\"thick\""] {
            assert!(
                Settings::parse(&format!("[effects]\nhaze_density = {bad}\n")).is_err(),
                "{bad}"
            );
        }
        // Every reserved key is a known key.
        for key in Effects::UNIMPLEMENTED {
            let (_, unknown) = Settings::parse(&format!("[effects]\n{key} = true\n")).unwrap();
            assert!(unknown.is_empty(), "{key}");
        }
        let again = Settings::parse(&settings.to_toml().unwrap()).unwrap().0;
        assert_eq!(again, settings);
    }

    #[test]
    fn minimal_files_stay_minimal() {
        assert_eq!(
            edited("[display]\nvsync = false\n", |settings| {
                settings.display.vsync = true;
                settings.controls.buttons.b = crate::gamepad::PsButton::Cross;
            }),
            "[display]\nvsync = true\n\n[controls.buttons]\nb = \"cross\"\n"
        );
        // Back to the defaults: values change, nothing is added.
        assert_eq!(edited("", |_| {}), "");
        assert_eq!(
            edited("[display]\nvsync = false\n", |s| *s = Settings::default()),
            "[display]\nvsync = true\n"
        );
    }

    #[test]
    fn aliases_keep_their_spelling() {
        let text = edited(
            "[controls.buttons]\nview = \"start\"\nls = \"none\"\n",
            |settings| {
                let buttons = &mut settings.controls.buttons;
                buttons.back = crate::gamepad::PsButton::Select;
                buttons.left_stick = crate::gamepad::PsButton::L1;
                buttons.start = crate::gamepad::PsButton::Select;
            },
        );
        assert_eq!(
            text,
            "[controls.buttons]\nview = \"select\"\nls = \"l1\"\nstart = \"select\"\n"
        );
        // Each alias names its key (gamepad.rs keeps the serde aliases).
        for &(table, key, alias) in ALIASES {
            assert_eq!(table, "controls.buttons");
            let with = |name: &str| format!("[controls.buttons]\n{name} = \"triangle\"\n");
            let by_alias = Settings::parse(&with(alias)).unwrap();
            let by_key = Settings::parse(&with(key)).unwrap();
            assert_eq!(by_alias.0, by_key.0, "{alias}");
            assert!(by_alias.1.is_empty(), "{alias}");
        }
    }

    #[test]
    fn inline_dotted_and_implicit_tables() {
        let text = edited("controls = { rumble = false }\n", |settings| {
            settings.controls.rumble = true;
            settings.controls.deadzone = 0.5;
            settings.controls.buttons.x = crate::gamepad::PsButton::Cross;
        });
        assert!(text.starts_with("controls = { rumble = true"), "{text}");
        assert_eq!(text.lines().count(), 1, "still one inline table: {text}");
        let text = edited("[controls]\nbuttons.a = \"circle\"\n", |settings| {
            settings.controls.buttons.b = crate::gamepad::PsButton::Square;
        });
        assert_eq!(
            text,
            "[controls]\nbuttons.a = \"circle\"\nbuttons.b = \"square\"\n"
        );
        edited("[controls.buttons]\na = \"circle\"\n", |settings| {
            settings.controls.deadzone = 0.4;
            settings.display.filter = Filter::Linear;
        });
        // Windows line ends stay.
        let text = edited(
            "[display]\r\n# sharp\r\nfilter = \"nearest\"\r\n",
            |settings| {
                settings.display.filter = Filter::Linear;
            },
        );
        assert_eq!(text, "[display]\r\n# sharp\r\nfilter = \"linear\"\r\n");
    }

    #[test]
    fn bad_values_take_their_defaults_key_by_key() {
        let text = "[display]\nscaling = \"zoom\"\nvsync = false\nwindow_width = 5\n\
                    [controls]\ndeadzone = \"wide\"\nrumble = false\n";
        let contents = Document::parse(text).unwrap().read();
        assert!(contents.error.is_some(), "{contents:?}");
        assert_eq!(
            contents.reset,
            [
                "display.scaling",
                "display.window_width",
                "controls.deadzone"
            ]
        );
        assert!(!contents.settings.display.vsync);
        assert!(!contents.settings.controls.rumble);
        assert_eq!(contents.settings.display.scaling, Scaling::Aspect);
        // Saving these settings writes the defaults over the bad values.
        let fixed = edited(text, |_| {});
        assert_eq!(
            fixed,
            "[display]\nscaling = \"aspect\"\nvsync = false\nwindow_width = 1280\n\
             [controls]\ndeadzone = 0.15\nrumble = false\n"
        );
        assert!(Document::parse("[display\n").is_err());
    }

    #[test]
    fn values_are_checked_before_saving() {
        let mut document = Document::default_file();
        for edit in [
            (|s: &mut Settings| s.display.window_width = 0) as fn(&mut Settings),
            |s| s.display.window_height = 20000,
            |s| s.controls.deadzone = 0.95,
            |s| s.display.internal_resolution = 0,
            |s| s.display.internal_resolution = 9,
            |s| s.controls.deadzone = f64::NAN,
            |s| s.controls.trigger_threshold = 0.0,
        ] {
            let mut settings = Settings::default();
            edit(&mut settings);
            assert!(settings.check().is_err(), "{settings:?}");
            assert!(document.set(&settings).is_err(), "{settings:?}");
            assert_eq!(document.to_string(), DEFAULT_FILE, "unchanged");
        }
        // The edges of the ranges are fine; loading stays lenient.
        let mut settings = Settings::default();
        settings.controls.deadzone = 0.9;
        settings.controls.trigger_threshold = 0.05;
        settings.check().unwrap();
        let (loaded, _) = Settings::parse("[controls]\ndeadzone = 2.0\n").unwrap();
        assert!(loaded.check().is_err(), "the game clamps it instead");
    }

    #[test]
    fn a_macos_app_keeps_the_player_files_in_application_support() {
        let home = Path::new("/Users/pilot");
        let app = Path::new("/Applications/Ace Combat 3.app/Contents/MacOS/ac3");
        assert_eq!(
            player_folder_for(Some(app), Some(home), None),
            Path::new("/Users/pilot/Library/Application Support/Ace Combat 3")
        );
        assert_eq!(
            resources_folder_for(Some(app)),
            Path::new("/Applications/Ace Combat 3.app/Contents/Resources")
        );
        // Anywhere else, beside the executable.
        let loose = Path::new("/games/ac3/ac3");
        assert_eq!(
            player_folder_for(Some(loose), Some(home), None),
            Path::new("/games/ac3")
        );
        assert_eq!(resources_folder_for(Some(loose)), Path::new("/games/ac3"));
        let fake = Path::new("/games/Contents/MacOS/ac3");
        assert_eq!(
            player_folder_for(Some(fake), Some(home), None),
            Path::new("/games/Contents/MacOS")
        );
        assert_eq!(player_folder_for(None, Some(home), None), Path::new(""));
    }

    #[test]
    fn a_nix_package_keeps_the_player_files_in_the_data_home() {
        let home = Path::new("/home/pilot");
        let exe = Path::new("/nix/store/abc123-ace-combat-3-0.1.0/bin/ac3");
        assert_eq!(
            player_folder_for(Some(exe), Some(home), None),
            Path::new("/home/pilot/.local/share/ace-combat-3")
        );
        assert_eq!(
            player_folder_for(Some(exe), Some(home), Some(Path::new("/data"))),
            Path::new("/data/ace-combat-3")
        );
        assert_eq!(
            resources_folder_for(Some(exe)),
            Path::new("/nix/store/abc123-ace-combat-3-0.1.0/share/ace-combat-3")
        );
        // A copy run from a home folder keeps everything beside it.
        let loose = Path::new("/home/pilot/games/ac3/ac3");
        assert_eq!(
            player_folder_for(Some(loose), Some(home), Some(Path::new("/data"))),
            Path::new("/home/pilot/games/ac3")
        );
        assert_eq!(
            resources_folder_for(Some(loose)),
            Path::new("/home/pilot/games/ac3")
        );
    }

    #[test]
    fn save_creates_the_default_file_and_edits_it() {
        let dir = std::env::temp_dir().join(format!("ac3-settings-save-{}", std::process::id()));
        let path = dir.join("sub").join(FILE_NAME);
        let _ = std::fs::remove_dir_all(&dir);
        let mut settings = Settings::recommended();
        settings.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_FILE);
        settings.display.scaling = Scaling::Integer;
        settings.paths.card1 = Some("card1.mcr".into());
        settings.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Ace Combat 3 (native port) settings."));
        assert!(text.contains("\nscaling = \"integer\"\n"));
        assert!(text.contains("\ncard1 = \"card1.mcr\"\n"));
        assert_eq!(Settings::load(&path).unwrap(), settings);
        assert!(!dir.join("sub").join("ac3.toml.tmp").exists());
        // A file that is not TOML is not overwritten.
        std::fs::write(&path, "[display\n").unwrap();
        assert!(settings.save(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[display\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
