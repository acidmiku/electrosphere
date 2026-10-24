//! Anamorphic widescreen (`docs/MILESTONE_1.md` Phase 4 items 2 and 3; the
//! plan is `docs/RECOMP_ENHANCEMENTS.md` §4.2): the version-independent
//! machinery. The version crate decides *when* (which passes are flight,
//! where the frame's world section begins and ends, where the frame's
//! primitive buffer has room) and calls in here; `ac3_flight::widescreen`
//! documents the PAL routines and what each squeezes.
//!
//! The frame keeps its 320 columns but holds a wider view: the GTE squeezes
//! the X of every projected vertex by k = (4/3) / aspect
//! ([`ac3_gte::Gte::set_projection_x_scale`]), and the presenter shows the
//! picture stretched to the aspect. Off by default: with the setting off
//! (the [`Machine`] default) nothing here runs, and the game is
//! bit-identical.
//!
//! - [`begin_pass`] (each main-loop pass) notes whether the pass runs in
//!   flight with the setting on;
//! - [`world_begin`] / [`world_end`] bracket the world section of a flight
//!   frame: the GTE projects squeezed in between, and the packets linked by
//!   its end are the world;
//! - the screen-space drawers of the world section scale their X extents by
//!   k ([`squeeze_x`], [`unsqueeze_x`], [`squeeze_about`], [`x_scale`]);
//! - [`before_draw`] / [`after_draw`] bracket the frame's DrawOTag: every
//!   packet linked after the world section is squeezed toward the screen
//!   centre (sprites become textured quads in the free tail of the frame's
//!   primitive buffer), and the list is put back as the game wrote it once
//!   the GPU has read it, so RAM after the pass is untouched by this step.
//!
//! [`shown_aspect`] tells the presenter at which aspect the picture at a
//! display origin was drawn.
use crate::machine::Machine;

/// The widescreen setting: the aspect the flight picture is shown at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Aspect {
    /// 16:9, k = 3/4.
    Wide16x9,
    /// 16:10, k = 5/6.
    Wide16x10,
    /// "21:9": the 64:27 of 2560 x 1080 monitors, k = 9/16 (a 3440 x 1440
    /// monitor, 43:18, shows thin side bars).
    Wide21x9,
}

impl Aspect {
    pub const ALL: [Self; 3] = [Self::Wide16x9, Self::Wide16x10, Self::Wide21x9];

    /// The setting's name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Wide16x9 => "16:9",
            Self::Wide16x10 => "16:10",
            Self::Wide21x9 => "21:9",
        }
    }
    /// The aspect named `text` (as [`Self::name`]).
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|aspect| aspect.name() == text.trim())
    }
    /// Width and height of the shown picture's shape.
    pub fn ratio(self) -> (u32, u32) {
        match self {
            Self::Wide16x9 => (16, 9),
            Self::Wide16x10 => (16, 10),
            Self::Wide21x9 => (64, 27),
        }
    }
    /// k = (4/3) / aspect as (numerator, denominator): the projected X is
    /// multiplied by it.
    pub fn squeeze(self) -> (u32, u32) {
        match self {
            Self::Wide16x9 => (3, 4),
            Self::Wide16x10 => (5, 6),
            Self::Wide21x9 => (9, 16),
        }
    }
}

/// A picture drawn by a main-loop pass: its drawing area and the aspect it
/// was drawn for (`None`: 4:3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Picture {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub aspect: Option<Aspect>,
}

/// What the last [`before_draw`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Packets walked, and those linked after the world section.
    pub packets: u32,
    pub overlay: u32,
    /// Commands squeezed in place, rebuilt in new packets (sprites to
    /// quads), kept (full width) and left (no room for a rebuilt packet).
    pub squeezed: u32,
    pub rebuilt: u32,
    pub full_width: u32,
    pub no_room: u32,
}

/// Bits of the RAM words: one per word (2 MiB / 4).
const WORDS: usize = 0x20_0000 / 4;
/// The end of a packet list.
const LIST_END: u32 = 0x00ff_ffff;
/// Most packets one list walk follows (a broken list never hangs).
const MAX_PACKETS: u32 = 0x4_0000;

/// The widescreen part of a [`Machine`]: the setting and the state of the
/// current pass. Host-side, never game state (not in save states).
#[derive(Clone, Default)]
pub struct Widescreen {
    aspect: Option<Aspect>,
    /// This pass runs in flight with the setting on.
    flight: bool,
    /// This pass drew the flight world squeezed.
    drew: bool,
    /// One bit per RAM word: the packets linked when the world section
    /// ended (empty while unused).
    world: Vec<u64>,
    /// The pictures of the two drawing buffers.
    pictures: [Option<Picture>; 2],
    /// The last transform.
    stats: Stats,
}

impl std::fmt::Debug for Widescreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Widescreen")
            .field("aspect", &self.aspect)
            .field("flight", &self.flight)
            .field("drew", &self.drew)
            .field("pictures", &self.pictures)
            .finish_non_exhaustive()
    }
}

impl Widescreen {
    /// The setting.
    #[inline]
    pub fn aspect(&self) -> Option<Aspect> {
        self.aspect
    }
    pub fn set_aspect(&mut self, aspect: Option<Aspect>) {
        self.aspect = aspect;
        if aspect.is_none() {
            self.flight = false;
            self.world = Vec::new();
        }
    }
    /// Whether the current pass runs in flight with the setting on.
    #[inline]
    pub fn in_flight(&self) -> bool {
        self.flight
    }
    /// Whether the current pass drew its world squeezed.
    #[inline]
    pub fn drew(&self) -> bool {
        self.drew
    }
    /// The pictures of the two drawing buffers, as the last passes drew them.
    pub fn pictures(&self) -> [Option<Picture>; 2] {
        self.pictures
    }
    /// What the last DrawOTag transform did.
    pub fn stats(&self) -> Stats {
        self.stats
    }
    fn is_world(&self, address: u32) -> bool {
        let word = ((address & 0x1f_ffff) >> 2) as usize;
        self.world
            .get(word / 64)
            .is_some_and(|bits| bits >> (word % 64) & 1 != 0)
    }
    fn mark_world(&mut self, address: u32) {
        let word = ((address & 0x1f_ffff) >> 2) as usize;
        if let Some(bits) = self.world.get_mut(word / 64) {
            *bits |= 1 << (word % 64);
        }
    }
}

/// The GTE's current X squeeze k (`None`: the original projection). Set
/// only in the world section of a widescreen flight frame.
#[inline]
pub fn x_scale(m: &Machine) -> Option<(u32, u32)> {
    m.gte.projection_x_scale()
}

/// An X extent of a screen-space shape sized from H (a sprite's width, a
/// half width), times k while the world is drawn squeezed, rounded to the
/// nearest pixel; `value` unchanged otherwise. Signed, as the game's
/// registers hold it.
#[inline]
pub fn squeeze_x(m: &Machine, value: u32) -> u32 {
    match x_scale(m) {
        Some((num, den)) => scale_round(value as i32, num, den) as u32,
        None => value,
    }
}

/// The inverse of [`squeeze_x`]: `value / k`, rounded. For the routines that
/// use RTPS as a 2D transform (the translation a screen point, the depth H):
/// a squeezed screen x put into TRX comes out of the squeezed projection
/// where it was. `value` unchanged when not squeezed.
#[inline]
pub fn unsqueeze_x(m: &Machine, value: u32) -> u32 {
    match x_scale(m) {
        Some((num, den)) => scale_round(value as i32, den, num) as u32,
        None => value,
    }
}

/// A screen x offset `offset` from `centre` squeezed: `centre + offset * k`
/// (the low halfwords, as packet coordinates hold them); `centre + offset`
/// when not squeezed.
#[inline]
pub fn squeeze_about(m: &Machine, centre: u32, offset: u32) -> u32 {
    centre.wrapping_add(squeeze_x(m, offset))
}

/// `value * num / den` rounded to the nearest integer (halves away from 0).
#[inline]
fn scale_round(value: i32, num: u32, den: u32) -> i32 {
    let (num, den) = (i64::from(num), i64::from(den));
    let scaled = i64::from(value) * num;
    let rounded = if scaled >= 0 {
        (2 * scaled + den) / (2 * den)
    } else {
        -((-2 * scaled + den) / (2 * den))
    };
    rounded as i32
}

/// Main-loop pass start: note whether this pass is a widescreen flight
/// pass (`in_flight`: the version's flight state, with the setting on). The
/// GTE projects normally until [`world_begin`].
#[inline]
pub fn begin_pass(m: &mut Machine, in_flight: bool) {
    if m.widescreen.aspect.is_none() {
        return;
    }
    m.widescreen.flight = in_flight;
    m.widescreen.drew = false;
    m.gte.set_projection_x_scale(None);
}

/// Before the world drawers of the flight frame: squeeze the projection
/// when this is a widescreen flight pass.
#[inline]
pub fn world_begin(m: &mut Machine) {
    let Some(aspect) = m.widescreen.aspect.filter(|_| m.widescreen.flight) else {
        return;
    };
    m.gte.set_projection_x_scale(Some(aspect.squeeze()));
    m.widescreen.drew = true;
}

/// After the world drawers of the flight frame: the original projection
/// again, and note every packet linked so far from `list` (the frame's
/// ordering table; the world); what is linked later is screen-space overlay.
pub fn world_end(m: &mut Machine, list: u32) {
    if x_scale(m).is_none() {
        return;
    }
    m.gte.set_projection_x_scale(None);
    let mut world = std::mem::take(&mut m.widescreen.world);
    world.clear();
    world.resize(WORDS / 64, 0);
    m.widescreen.world = world;
    let mut address = list & LIST_END;
    for _ in 0..MAX_PACKETS {
        if address == LIST_END {
            break;
        }
        m.widescreen.mark_world(address);
        address = m.peek(address) & LIST_END;
    }
}

/// The words [`before_draw`] changed, to put back after the GPU read them.
#[derive(Debug, Default)]
pub struct Undo(Vec<(u32, u32)>);

/// Before DrawOTag(`ot`) of the frame drawn with drawing environment `draw`
/// (buffer `index`): remember the picture's aspect, and in a pass that drew
/// its world squeezed, squeeze the overlay packets (see the module
/// documentation). `room` gives the free tail of the frame's primitive
/// buffer as `[start, end)` (start 0: none), where sprites rebuilt as quads
/// go; it is asked only in such a pass. Returns what [`after_draw`] puts
/// back.
pub fn before_draw(
    m: &mut Machine,
    ot: u32,
    draw: u32,
    index: u32,
    room: impl FnOnce(&Machine) -> (u32, u32),
) -> Undo {
    let Some(aspect) = m.widescreen.aspect else {
        return Undo::default();
    };
    let drew = m.widescreen.drew;
    let picture = Picture {
        x: m.peek_half(draw),
        y: m.peek_half(draw + 2),
        width: m.peek_half(draw + 4),
        height: m.peek_half(draw + 6),
        aspect: drew.then_some(aspect),
    };
    if let Some(slot) = m.widescreen.pictures.get_mut(index as usize & 1) {
        *slot = Some(picture);
    }
    if !drew {
        return Undo::default();
    }
    let room = room(m);
    let mut walk = Walk::new(m, draw, aspect.squeeze(), room);
    walk.run(m, ot);
    m.widescreen.stats = walk.stats;
    m.widescreen.world.clear();
    Undo(walk.undo)
}

/// After DrawOTag: put back every word [`before_draw`] changed.
pub fn after_draw(m: &mut Machine, undo: Undo) {
    for (address, word) in undo.0.into_iter().rev() {
        m.poke(address, word);
    }
}

/// The aspect of the picture whose drawing area holds the display origin
/// (`x`, `y`), as the pass that drew it set it up; `None` for 4:3.
pub fn shown_aspect(m: &Machine, x: u16, y: u16) -> Option<Aspect> {
    m.widescreen
        .pictures
        .iter()
        .flatten()
        .find(|p| {
            (p.x..p.x.saturating_add(p.width)).contains(&x)
                && (p.y..p.y.saturating_add(p.height)).contains(&y)
        })
        .and_then(|p| p.aspect)
}

// ---- the DrawOTag transform -------------------------------------------------------

/// Sign-extend the 11-bit field at bit `shift` (GP0 E5 offsets).
fn sext11(word: u32, shift: u32) -> i32 {
    (((word >> shift) << 21) as i32) >> 21
}
/// The signed x and y halves of a vertex word.
fn xy(word: u32) -> (i32, i32) {
    (
        i32::from(word as u16 as i16),
        i32::from((word >> 16) as u16 as i16),
    )
}
fn with_x(word: u32, x: i32) -> u32 {
    (word & 0xffff_0000) | (x as u32 & 0xffff)
}

/// Words of the GP0 command starting at `words[at]` (0 when it does not
/// fit, ending the packet).
fn command_length(words: &[u32], at: usize) -> usize {
    let word = words[at];
    let code = word >> 24;
    let length = match code {
        0x02 => 3,
        0x20..=0x3f => {
            let (gouraud, quad, textured) = (code & 0x10 != 0, code & 8 != 0, code & 4 != 0);
            let n = if quad { 4 } else { 3 };
            1 + n * (1 + usize::from(textured)) + if gouraud { n - 1 } else { 0 }
        }
        0x40..=0x5f => {
            let (gouraud, poly) = (code & 0x10 != 0, code & 8 != 0);
            if !poly {
                if gouraud { 4 } else { 3 }
            } else {
                // Vertices (and colours) until the 0x5xxx5xxx terminator.
                let step = if gouraud { 2 } else { 1 };
                let mut j = at + 2;
                loop {
                    let Some(&next) = words.get(j) else {
                        return 0;
                    };
                    if next & 0xf000_f000 == 0x5000_5000 {
                        break j + 1 - at;
                    }
                    j += step;
                }
            }
        }
        0x60..=0x7f => 2 + usize::from(code & 4 != 0) + usize::from(code >> 3 & 3 == 0),
        0x80..=0x9f => 4,
        0xa0..=0xbf => {
            let Some(&size) = words.get(at + 2) else {
                return 0;
            };
            let pixels = (size & 0xffff) as usize * (size >> 16) as usize;
            3 + pixels.div_ceil(2)
        }
        0xc0..=0xdf => 3,
        _ => 1,
    };
    if at + length <= words.len() {
        length
    } else {
        0
    }
}

/// The GPU state the walk follows, and the squeeze.
struct Walk {
    /// Drawing offset (E5).
    offset: (i32, i32),
    /// Texture page bits of the draw mode (E1, textured polygons).
    texpage: u32,
    /// The frame's screen: left edge and width in VRAM x, and its centre.
    left: i32,
    width: i32,
    centre: i32,
    squeeze: (u32, u32),
    /// Free bytes for rebuilt packets: [next, end).
    room: (u32, u32),
    undo: Vec<(u32, u32)>,
    stats: Stats,
}

impl Walk {
    fn new(m: &Machine, draw: u32, squeeze: (u32, u32), room: (u32, u32)) -> Self {
        let left = i32::from(m.peek_half(draw) as i16);
        let width = i32::from(m.peek_half(draw + 4) as i16).max(1);
        // The DR_ENV PutDrawEnv just sent: E1 at +0x10, E5 at +0xC.
        let mode = m.peek(draw + 0x1c + 0x10);
        let offset = m.peek(draw + 0x1c + 0xc);
        Self {
            offset: (sext11(offset, 0), sext11(offset, 11)),
            texpage: mode & 0x3fff,
            left,
            width,
            centre: left + width / 2,
            squeeze,
            room,
            undo: Vec::new(),
            stats: Stats::default(),
        }
    }

    /// A screen x (VRAM, offset applied) squeezed toward the centre.
    fn squeeze(&self, x: i32) -> i32 {
        self.centre + scale_round(x - self.centre, self.squeeze.0, self.squeeze.1)
    }
    /// A vertex x (relative to the drawing offset) squeezed.
    fn vertex(&self, x: i32) -> i32 {
        self.squeeze(x + self.offset.0) - self.offset.0
    }
    /// Whether screen x range [`low`, `high`) covers the whole width.
    fn full_width(&self, low: i32, high: i32) -> bool {
        low <= self.left && high >= self.left + self.width
    }

    fn write(&mut self, m: &mut Machine, address: u32, word: u32) {
        let old = m.peek(address);
        if old != word {
            self.undo.push((address, old));
            m.poke(address, word);
        }
    }

    fn run(&mut self, m: &mut Machine, ot: u32) {
        let mut address = ot & LIST_END;
        let mut words = Vec::new();
        for _ in 0..MAX_PACKETS {
            if address == LIST_END {
                break;
            }
            let tag = m.peek(address);
            let length = (tag >> 24) as usize;
            let next = tag & LIST_END;
            self.stats.packets += 1;
            if length > 0 {
                words.clear();
                words.extend((1..=length as u32).map(|i| m.peek(address + 4 * i)));
                let overlay = !m.widescreen.is_world(address);
                self.stats.overlay += u32::from(overlay);
                self.packet(m, address, tag, &words, overlay);
            }
            address = next;
        }
    }

    /// One packet: follow its state commands; squeeze it when `overlay`.
    fn packet(&mut self, m: &mut Machine, address: u32, tag: u32, words: &[u32], overlay: bool) {
        // Each command: where it starts, its length and its squeezed words.
        let mut commands: Vec<(usize, usize, Vec<u32>)> = Vec::new();
        let mut at = 0;
        while at < words.len() {
            let length = command_length(words, at);
            if length == 0 {
                break;
            }
            let command = &words[at..at + length];
            if overlay {
                let mut squeezed = Vec::with_capacity(length + 6);
                self.squeeze_command(command, &mut squeezed);
                commands.push((at, length, squeezed));
            }
            self.follow(command);
            at += length;
        }
        if !overlay {
            return;
        }
        let tail = &words[at..];
        let grown = commands.iter().any(|(_, length, out)| out.len() != *length);
        if !grown {
            for (at, _, out) in &commands {
                for (i, &word) in out.iter().enumerate() {
                    self.write(m, address + 4 * ((at + i) as u32 + 1), word);
                }
            }
            return;
        }
        // A longer packet (sprites as quads): built in the free tail of the
        // primitive buffer and linked in place of this one, which becomes
        // empty.
        let mut rebuilt: Vec<u32> = commands
            .iter()
            .flat_map(|(_, _, out)| out.iter().copied())
            .collect();
        rebuilt.extend_from_slice(tail);
        let bytes = 4 * (rebuilt.len() as u32 + 1);
        let (next, end) = self.room;
        if rebuilt.len() > 0xff || next == 0 || next.wrapping_add(bytes) > end {
            // No room: the commands that keep their length squeeze in
            // place; sprites and fixed tiles keep their width, centred on
            // their squeezed centre.
            self.stats.no_room += 1;
            for (at, length, out) in &commands {
                if out.len() == *length {
                    for (i, &word) in out.iter().enumerate() {
                        self.write(m, address + 4 * ((at + i) as u32 + 1), word);
                    }
                } else {
                    let command = &words[*at..at + length];
                    let (x, _) = xy(command[1]);
                    let width = rect_size(command).0;
                    let x = self.vertex(x + width / 2) - width / 2;
                    self.write(m, address + 4 * (*at as u32 + 2), with_x(command[1], x));
                }
            }
            return;
        }
        self.room.0 = next + bytes;
        let link = tag & LIST_END;
        self.write(m, next, ((rebuilt.len() as u32) << 24) | link);
        for (i, &word) in rebuilt.iter().enumerate() {
            self.write(m, next + 4 * (i as u32 + 1), word);
        }
        self.write(m, address, next & LIST_END);
    }

    /// Follow the GPU state a command sets (after it ran).
    fn follow(&mut self, command: &[u32]) {
        let word = command[0];
        match word >> 24 {
            0xe1 => self.texpage = word & 0x3fff,
            0xe5 => self.offset = (sext11(word, 0), sext11(word, 11)),
            code @ 0x20..=0x3f if code & 4 != 0 => {
                let gouraud = usize::from(code & 0x10 != 0);
                if let Some(&uv) = command.get(4 + gouraud) {
                    self.texpage = (self.texpage & !0x1ff) | (uv >> 16 & 0x1ff);
                }
            }
            _ => {}
        }
    }

    /// Append `command` squeezed (or as it is) to `out`.
    fn squeeze_command(&mut self, command: &[u32], out: &mut Vec<u32>) {
        let code = command[0] >> 24;
        match code {
            0x20..=0x3f => {
                let (gouraud, quad, textured) = (code & 0x10 != 0, code & 8 != 0, code & 4 != 0);
                let n = if quad { 4 } else { 3 };
                let stride = 1 + usize::from(textured) + usize::from(gouraud);
                let at = |k: usize| if k == 0 { 1 } else { 1 + k * stride };
                self.squeeze_vertices(command, (0..n).map(at).collect(), out);
            }
            0x40..=0x5f => {
                let gouraud = code & 0x10 != 0;
                let mut vertices = vec![1];
                let mut j = if gouraud { 3 } else { 2 };
                while j < command.len() && command[j] & 0xf000_f000 != 0x5000_5000 {
                    vertices.push(j);
                    j += if gouraud { 2 } else { 1 };
                }
                if code & 8 == 0 {
                    vertices.truncate(2);
                }
                self.squeeze_vertices(command, vertices, out);
            }
            0x60..=0x7f => self.squeeze_rect(command, out),
            0x80..=0x9f => {
                // A VRAM copy onto the screen (a text line, 8002CF08):
                // pixels cannot be scaled, so the block keeps its size,
                // centred where its squeezed centre falls. Destinations are
                // absolute VRAM, without the drawing offset.
                out.extend_from_slice(command);
                let (x, _) = xy(command[2]);
                let width = (command[3] & 0x3ff) as i32;
                if width > 0 && !self.full_width(x, x + width) {
                    let x = self.squeeze(x + width / 2) - width / 2;
                    let at = out.len() - command.len() + 2;
                    out[at] = with_x(command[2], x.clamp(0, 1023));
                    self.stats.squeezed += 1;
                }
            }
            0xe3 | 0xe4 => {
                // The drawing area (absolute VRAM): each edge inside the
                // screen moves; an edge at or beyond the screen's stays.
                let word = command[0];
                let x = (word & 0x3ff) as i32;
                let x = if code == 0xe3 {
                    if x <= self.left { x } else { self.squeeze(x) }
                } else if x >= self.left + self.width - 1 {
                    x
                } else {
                    self.squeeze(x + 1) - 1
                };
                out.push((word & !0x3ff) | (x as u32 & 0x3ff));
                self.stats.squeezed += 1;
            }
            _ => out.extend_from_slice(command),
        }
    }

    /// A polygon or line whose vertex words are at `vertices`.
    fn squeeze_vertices(&mut self, command: &[u32], vertices: Vec<usize>, out: &mut Vec<u32>) {
        let xs: Vec<i32> = vertices
            .iter()
            .map(|&i| xy(command[i]).0 + self.offset.0)
            .collect();
        let low = xs.iter().copied().min().unwrap_or(0);
        let high = xs.iter().copied().max().unwrap_or(0);
        let start = out.len();
        out.extend_from_slice(command);
        if self.full_width(low, high + 1) {
            self.stats.full_width += 1;
            return;
        }
        for &i in &vertices {
            let (x, _) = xy(command[i]);
            out[start + i] = with_x(command[i], self.vertex(x));
        }
        self.stats.squeezed += 1;
    }

    /// A rectangle: tiles squeeze in place (fixed sizes become variable),
    /// sprites become textured quads.
    fn squeeze_rect(&mut self, command: &[u32], out: &mut Vec<u32>) {
        let code = command[0] >> 24;
        let textured = code & 4 != 0;
        let (x, y) = xy(command[1]);
        let (width, height) = rect_size(command);
        let low = x + self.offset.0;
        if width <= 0 || height <= 0 || self.full_width(low, low + width) {
            self.stats.full_width += u32::from(width > 0);
            out.extend_from_slice(command);
            return;
        }
        let left = self.vertex(x);
        let right = self.vertex(x + width);
        self.stats.squeezed += 1;
        if code >> 3 & 3 == 1 {
            // A single pixel moves.
            out.extend_from_slice(command);
            let at = out.len() - command.len() + 1;
            out[at] = with_x(command[1], self.vertex(x));
            return;
        }
        let squeezed = (right - left).max(1);
        let xy_word = |x: i32| (command[1] & 0xffff_0000) | (x as u32 & 0xffff);
        if !textured {
            // TILE: the variable-size form.
            out.push((command[0] & !0x1800_0000) | 0x6000_0000);
            out.push(xy_word(left));
            out.push(((height as u32) << 16) | squeezed as u32);
            if code >> 3 & 3 != 0 {
                self.stats.rebuilt += 1;
            }
            return;
        }
        // SPRT: a textured quad over the same texels with the current
        // texture page (raw texture for the neutral colour, so it is not
        // dithered).
        let uv = command[2];
        let (u, v) = ((uv & 0xff) as i32, (uv >> 8 & 0xff) as i32);
        let clut = uv >> 16;
        let colour = command[0] & 0x00ff_ffff;
        let raw = code & 1 != 0 || colour == 0x0080_8080;
        let quad = 0x2c | u32::from(raw) | (code & 2);
        // The far texture coordinates are the sprite's own edge (u + width,
        // v + height): no rasterizer reaches them, so each pixel maps its
        // own texel and the last row and column are drawn. That holds at
        // every scale: the GPU renderer above 1x maps texture coordinates at
        // 1x pixel centres (`a_quad_over_a_sprites_texels_draws_the_sprite`
        // in the port's executor tests). An edge one texel in, as this once
        // had above 1x, dropped every HUD glyph's last row.
        let u1 = (u + width).min(255) as u32;
        let v1 = (v + height).min(255) as u32;
        let (u0, v0) = (u as u32, v as u32);
        let bottom = ((y + height) as u32 & 0xffff) << 16;
        let top = command[1] & 0xffff_0000;
        let tpage = self.texpage & 0x1ff;
        out.extend_from_slice(&[
            (quad << 24) | colour,
            top | (left as u32 & 0xffff),
            (clut << 16) | (v0 << 8) | u0,
            top | (right as u32 & 0xffff),
            (tpage << 16) | (v0 << 8) | u1,
            bottom | (left as u32 & 0xffff),
            (v1 << 8) | u0,
            bottom | (right as u32 & 0xffff),
            (v1 << 8) | u1,
        ]);
        self.stats.rebuilt += 1;
    }
}

/// Width and height of a GP0 rectangle command.
fn rect_size(command: &[u32]) -> (i32, i32) {
    let code = command[0] >> 24;
    match code >> 3 & 3 {
        0 => {
            let size = command[command.len() - 1];
            ((size & 0x3ff) as i32, (size >> 16 & 0x1ff) as i32)
        }
        1 => (1, 1),
        2 => (8, 8),
        _ => (16, 16),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspects_and_squeezes() {
        for aspect in Aspect::ALL {
            assert_eq!(Aspect::parse(aspect.name()), Some(aspect));
            // k = (4/3) / aspect exactly.
            let (w, h) = aspect.ratio();
            let (num, den) = aspect.squeeze();
            assert_eq!(
                u64::from(num) * 3 * u64::from(w),
                u64::from(den) * 4 * u64::from(h)
            );
        }
        assert_eq!(Aspect::parse("4:3"), None);
        assert_eq!(scale_round(100, 3, 4), 75);
        assert_eq!(scale_round(-100, 3, 4), -75);
        assert_eq!(scale_round(2, 3, 4), 2, "1.5 rounds away from zero");
        assert_eq!(scale_round(-2, 3, 4), -2);
        assert_eq!(scale_round(7, 9, 16), 4);
    }

    #[test]
    fn command_lengths() {
        let words = [0x2c80_8080, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(command_length(&words, 0), 9, "POLY_FT4");
        assert_eq!(command_length(&[0x3800_0000; 8], 0), 8, "POLY_G4");
        assert_eq!(command_length(&[0x6480_8080, 0, 0, 0], 0), 4, "SPRT");
        assert_eq!(command_length(&[0x7480_8080, 0, 0], 0), 3, "SPRT_8");
        assert_eq!(command_length(&[0x6000_0000, 0, 0], 0), 3, "TILE");
        assert_eq!(command_length(&[0x4800_0000, 1, 2, 3, 0x5555_5555], 0), 5);
        assert_eq!(command_length(&[0x5800_0000, 1, 2, 3, 0x5555_5555], 0), 5);
        assert_eq!(command_length(&[0x4800_0000, 1, 2], 0), 0, "no terminator");
        assert_eq!(command_length(&[0xe100_0000], 0), 1);
    }

    fn walk() -> Walk {
        Walk {
            offset: (0, 0),
            texpage: 0x0105,
            left: 0,
            width: 320,
            centre: 160,
            squeeze: (3, 4),
            room: (0, 0),
            undo: Vec::new(),
            stats: Stats::default(),
        }
    }

    #[test]
    fn overlay_commands_squeeze_toward_the_centre() {
        let mut walk = walk();
        // A flat quad from x 0 to 80: 40 .. 100.
        let quad = [0x2800_00ff, 0, 80, 0x0010_0000, 0x0010_0050];
        let mut out = Vec::new();
        walk.squeeze_command(&quad, &mut out);
        assert_eq!(out, [0x2800_00ff, 40, 100, 0x0010_0028, 0x0010_0064]);
        // A full-width tile stays.
        let tile = [0x6000_0000, 0x0000_0000, 0x0010_0140];
        out.clear();
        walk.squeeze_command(&tile, &mut out);
        assert_eq!(out, tile);
        // A 16x16 tile at (200, 8) becomes a 12x16 variable tile at 190.
        let tile = [0x7800_ff00, 0x0008_00c8];
        out.clear();
        walk.squeeze_command(&tile, &mut out);
        assert_eq!(out, [0x6000_ff00, 0x0008_00be, 0x0010_000c]);
        // A 40-wide VRAM copy to x 200 (centre 220 -> 205) keeps its size.
        let copy = [0x8000_0000, 0x0100_0300, 0x0020_00c8, 0x0008_0028];
        out.clear();
        walk.squeeze_command(&copy, &mut out);
        assert_eq!(out, [0x8000_0000, 0x0100_0300, 0x0020_00b9, 0x0008_0028]);
        // With a drawing offset of 100: vertex 60 is screen 160 (stays).
        walk.offset = (100, 0);
        out.clear();
        walk.squeeze_command(&[0x4000_0000, 60, 0x0005_0000], &mut out);
        assert_eq!(out, [0x4000_0000, 60, 0x0005_000f], "screen 100 -> 115");
    }

    #[test]
    fn sprites_become_textured_quads() {
        let mut walk = walk();
        // A 32x16 sprite at (0, 20), u 64 v 32, CLUT 0x7f00, neutral colour.
        let sprite = [0x6480_8080, 0x0014_0000, 0x7f00_2040, 0x0010_0020];
        let mut out = Vec::new();
        walk.squeeze_command(&sprite, &mut out);
        assert_eq!(
            out,
            [
                0x2d80_8080,
                0x0014_0028,
                0x7f00_2040,
                0x0014_0040,
                0x0105_2060,
                0x0024_0028,
                0x0000_3040,
                0x0024_0040,
                0x0000_3060,
            ]
        );
        assert_eq!(walk.stats.rebuilt, 1);
    }
}
