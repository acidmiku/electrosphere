//! Host side of the movie pipeline (states 5 and 6) next to the MDEC
//! ([`crate::machine_mdec`]): XA-ADPCM sectors from the CD drive into the
//! SPU's CD input, the guest clock of the movie decoder's busy waits, and
//! the 24-bit display read-out the movies use.
//!
//! * XA: the drive (`ac3_flight::cdstream::CdDrive`) hands every sector it
//!   plays as XA-ADPCM (Setmode bit 6; with the filter, only the Setfilter
//!   file and channel) to its [`CdAudio`] sink. [`XaSink`] queues them;
//!   at each VBlank the host's [`XaPlayer`] decodes the queue with
//!   `ac3_audio::xa::XaDecoder` (4-bit ADPCM, stereo or mono, 37.8 or 18.9
//!   kHz: every profile on the PAL and Japanese discs; resampled to 44.1
//!   kHz as the SPU's CD input takes it, 18.9 kHz by entering every frame
//!   twice; CdMix's attenuation from the SPU device) and pushes the PCM
//!   into [`SpuDevice::push_cd_audio`], which mixes it through the CD input
//!   volume. A sector yields 2352 frames (stereo 37.8 kHz), 4704 (stereo
//!   18.9, mono 37.8) or 9408 (mono 18.9). The decoder restarts its
//!   predictors when the file or channel changes or after a stream's last
//!   sector. Mute (CD command 0x0B) queues silence of the same length.
//! * Busy waits: `ac3_flight::states::busy` (the frame poll of 800555C0
//!   and the slice wait of 8005578C) make one host call per iteration with
//!   its estimated CPU cycles. [`BusyClock`] adds them up and yields a
//!   VBlank for every field of the release's [`FieldLength`] (PAL
//!   [`CYCLES_PER_PAL_FIELD`](crate::machine_audio::CYCLES_PER_PAL_FIELD)
//!   cycles, NTSC 565,044.48), which the host delivers as during a VSync
//!   wait.
//! * Display: GP1(08) bit 4 selects 24-bit output, three bytes per pixel
//!   from the start halfword ([`display_rgb`]).
//! * Movie frames on the screen ([`MovieTracker`], only while a movie pack
//!   is in use): which movie frame each MDEC decode is, where its slices
//!   land in VRAM, and whether the field the screen shows holds it. It reads
//!   RAM and never writes anything: presentation only. A frame whose last
//!   slice arrived is reported ([`CompleteFrame`]): the hub's clips, which
//!   the hub draws as a texture, are replaced from there by the GPU
//!   renderer (`gpu_renderer::replace`, "Movie frames").
use crate::machine_audio::{FieldLength, SpuDevice};
use crate::machine_mdec;
use crate::movie_disc::Depth;
use ac3_audio::xa::XaDecoder;
use ac3_flight::cdstream::{CdAudio, SECTOR_BYTES};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

/// XA sectors the drive played, not yet decoded.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct XaQueue {
    #[serde(with = "crate::savestate::boxed_arrays")]
    pub sectors: VecDeque<Box<[u8; SECTOR_BYTES]>>,
    /// The drive's mute (CD command 0x0B) / demute (0x0C) state.
    pub muted: bool,
}

/// The drive's audio sink: queues sectors for the host's [`XaPlayer`].
#[derive(Clone, Default)]
pub struct XaSink(pub Rc<RefCell<XaQueue>>);

impl CdAudio for XaSink {
    fn xa_sector(&mut self, sector: &[u8; SECTOR_BYTES]) {
        self.0.borrow_mut().sectors.push_back(Box::new(*sector));
    }
    /// CdMix's attenuation is applied by [`XaPlayer`] from the SPU device,
    /// which answers the CdMix call.
    fn mix(&mut self, _attenuation: [u8; 4]) {}
    fn mute(&mut self, muted: bool) {
        self.0.borrow_mut().muted = muted;
    }
}

/// Decodes queued XA sectors into the SPU's CD input.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct XaPlayer {
    decoder: Option<XaDecoder>,
    stream: Option<(u8, u8)>,
    /// Sectors decoded and sectors the decoder rejected (8-bit or another
    /// unplayable XA coding, or a damaged header).
    pub sectors: u64,
    pub rejected: u64,
}

impl XaPlayer {
    /// Decode every queued sector into `spu`'s CD input.
    pub fn feed(&mut self, queue: &RefCell<XaQueue>, spu: &mut SpuDevice) {
        let mut queue = queue.borrow_mut();
        while let Some(sector) = queue.sectors.pop_front() {
            let stream = (sector[16], sector[17]);
            let attenuation = spu.cd_attenuation();
            let decoder = self
                .decoder
                .get_or_insert_with(|| XaDecoder::new(stream.0, stream.1, attenuation));
            if self.stream != Some(stream) {
                decoder.select_stream(stream.0, stream.1);
                self.stream = Some(stream);
            }
            decoder.set_volume_matrix(attenuation);
            let mut decoded = decoder.decode_sector(&sector[..]);
            if matches!(decoded, Ok(None)) && sector[18] & 4 != 0 {
                // A new stream after the last one's end sector.
                decoder.select_stream(stream.0, stream.1);
                decoded = decoder.decode_sector(&sector[..]);
            }
            match decoded {
                Ok(Some(pcm)) => {
                    self.sectors += 1;
                    if queue.muted {
                        spu.push_cd_audio(&vec![[0, 0]; pcm.frames.len()]);
                    } else {
                        spu.push_cd_audio(&pcm.frames);
                    }
                }
                Ok(None) => {}
                Err(_) => self.rejected += 1,
            }
        }
    }
}

/// Guest time spent in busy-wait loops: CPU cycles in units of 1/`per` of
/// the release's [`FieldLength`] (PAL: cycles).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BusyClock {
    cycles: u64,
}

impl BusyClock {
    /// Let `cycles` pass in fields of `field`; returns the VBlanks that
    /// fell due.
    pub fn advance(&mut self, cycles: u32, field: FieldLength) -> u64 {
        self.cycles += u64::from(cycles) * field.per;
        let due = self.cycles / field.cycles;
        self.cycles %= field.cycles;
        due
    }
}

/// Displayed pixels as 0RGB words from 1024x512 VRAM halfwords, starting
/// at `start` (x in halfwords): 15-bit (STP ignored), or with `rgb24` three
/// bytes per pixel (R, G, B) per row from the start halfword.
pub fn display_rgb(
    vram: &[u16],
    start: (usize, usize),
    width: usize,
    height: usize,
    rgb24: bool,
) -> Vec<u32> {
    let (x0, y0) = start;
    let mut out = Vec::with_capacity(width * height);
    for row in 0..height {
        let y = (y0 + row) & 0x1ff;
        let line = &vram[y * 1024..y * 1024 + 1024];
        for column in 0..width {
            if rgb24 {
                let byte = |i: usize| {
                    let half = line[(x0 + i / 2) & 0x3ff];
                    if i.is_multiple_of(2) {
                        half & 0xff
                    } else {
                        half >> 8
                    }
                };
                let at = column * 3;
                let (r, g, b) = (byte(at), byte(at + 1), byte(at + 2));
                out.push((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b));
            } else {
                let p = line[(x0 + column) & 0x3ff];
                let expand = |v: u16| {
                    let v = u32::from(v & 0x1f);
                    (v << 3) | (v >> 2)
                };
                out.push((expand(p) << 16) | (expand(p >> 5) << 8) | expand(p >> 10));
            }
        }
    }
    out
}

// ---- movie frames on the screen (presentation only) -------------------------------------

/// The Japanese release's movie players, as the tracker finds a decode's
/// movie and frame (RAM words; `ac3_flight_jp` `states`, `ov2_p5_movie`).
pub mod jp_players {
    /// gp of the Japanese resident.
    pub const GP: u32 = 0x800b_cb74;
    /// `[gp+0x4D4]`: the GMV decoder of state 5 and the in-mission event
    /// player (80057720): +8/+0xC the VLC (run-level) buffers, +0x11030 /
    /// +0x11032 frame width / height, +0x11034 the frame number of the VLC
    /// buffer last filled (the one DecDCTin sends), +0x11038 the stream
    /// file (the SPH resource).
    pub const DECODER: u32 = GP + 0x4d4;
    pub const DECODER_BUFFERS: u32 = 8;
    pub const DECODER_WIDTH: u32 = 0x1_1030;
    pub const DECODER_FRAME: u32 = 0x1_1034;
    pub const DECODER_FILE: u32 = 0x1_1038;
    /// `[0x80112E18]`: overlay 2's playback block (the hub's clips): +8/+0xC
    /// the run-level buffers, +0x11050 / +0x11052 width / height, +0x11054
    /// the frame number, +0x11058 the stream file.
    pub const HUB: u32 = 0x8011_2e18;
    pub const HUB_BUFFERS: u32 = 8;
    pub const HUB_WIDTH: u32 = 0x1_1050;
    pub const HUB_FRAME: u32 = 0x1_1054;
    pub const HUB_FILE: u32 = 0x1_1058;
}

/// The movie frame an MDEC decode command is: the SPH resource streaming,
/// the stream's frame number (counting down to 0), the frame size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeId {
    pub resource: u16,
    pub number: u16,
    pub width: u16,
    pub height: u16,
    /// The hub's clip player (else state 5 / the in-mission player).
    pub hub: bool,
}

fn ram_u16(ram: &[u8], address: u32) -> u16 {
    let at = (address & 0x1f_ffff) as usize;
    u16::from_le_bytes([ram[at], ram[(at + 1) & 0x1f_ffff]])
}
fn ram_u32(ram: &[u8], address: u32) -> u32 {
    u32::from(ram_u16(ram, address)) | (u32::from(ram_u16(ram, address.wrapping_add(2))) << 16)
}

/// The decode of the run-level buffer `buffer` (DecDCTin's): the player
/// whose current run-level buffer it is, with that player's stream file and
/// frame number; `None` for any other MDEC use.
pub fn identify_decode(ram: &[u8], buffer: u32) -> Option<DecodeId> {
    use jp_players::*;
    let players = [
        (
            ram_u32(ram, DECODER),
            DECODER_BUFFERS,
            DECODER_WIDTH,
            DECODER_FRAME,
            DECODER_FILE,
            false,
        ),
        (
            ram_u32(ram, HUB),
            HUB_BUFFERS,
            HUB_WIDTH,
            HUB_FRAME,
            HUB_FILE,
            true,
        ),
    ];
    players
        .into_iter()
        .find_map(|(block, buffers, width, frame, file, hub)| {
            if block & 0xff00_0000 != 0x8000_0000 {
                return None;
            }
            let ours = [0, 4]
                .iter()
                .any(|&o| ram_u32(ram, block + buffers + o) == buffer);
            let file = ram_u32(ram, block.wrapping_add(file));
            // An SPH resource: bit 11 set, bit 12 the stream-control flag.
            (ours && file & 0x800 != 0 && file & !0x1fff == 0).then(|| DecodeId {
                resource: file as u16 & 0x1fff,
                number: ram_u16(ram, block.wrapping_add(frame)),
                width: ram_u16(ram, block.wrapping_add(width)),
                height: ram_u16(ram, block.wrapping_add(width + 2)),
                hub,
            })
        })
}

/// A decoded movie frame in VRAM, as its slices' LoadImages wrote it.
#[derive(Clone, Debug)]
pub struct VramFrame {
    pub serial: u64,
    pub id: DecodeId,
    pub depth: Depth,
    /// The frame's VRAM rectangle (halfwords: 1.5 per 24-bit pixel).
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// Its VRAM halfwords, row by row.
    pub pixels: Vec<u16>,
    /// The STP bit the MDEC sets on every 15-bit pixel (command bit 25).
    pub stp: bool,
    /// Halfword columns written so far.
    columns: Vec<bool>,
}

impl VramFrame {
    pub fn complete(&self) -> bool {
        self.columns.iter().all(|&c| c)
    }
}

/// A movie frame whose slices have all arrived in VRAM
/// ([`MovieTracker::on_load_image`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompleteFrame {
    pub serial: u64,
    pub id: DecodeId,
    pub depth: Depth,
    /// Its VRAM rectangle (x, y, w, h halfwords).
    pub rect: [u16; 4],
    pub stp: bool,
}

/// The decode command being followed: its serial, movie frame, depth and
/// STP bit.
#[derive(Clone, Copy, Debug)]
struct Current {
    serial: u64,
    id: DecodeId,
    depth: Depth,
    stp: bool,
}

/// An MDEC output transfer not yet uploaded.
#[derive(Clone, Copy, Debug)]
struct Output {
    address: u32,
    bytes: usize,
    serial: u64,
}

/// The movie frame the screen shows ([`MovieTracker::shown`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ShownFrame {
    pub id: DecodeId,
    pub serial: u64,
    pub depth: Depth,
    /// Its rectangle in the shown picture (pixels from the display start).
    pub rect: [u32; 4],
    /// The picture's size.
    pub picture: (u32, u32),
    /// Per pixel of `rect`: drawn over since the MDEC wrote it (subtitles,
    /// fades): the game's pixel stays on top.
    pub overlay: Vec<bool>,
    pub overlay_pixels: usize,
}

/// Counters for the run report.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct TrackerStats {
    pub decodes: u64,
    pub identified: u64,
    pub slices: u64,
    pub frames: u64,
}

/// Follows each movie frame from its MDEC decode command to the VRAM its
/// slices fill, so the presentation can tell which frame of which movie a
/// shown field holds, and where (see the module documentation). Only reads.
#[derive(Default)]
pub struct MovieTracker {
    serial: u64,
    current: Option<Current>,
    outputs: VecDeque<Output>,
    frames: VecDeque<VramFrame>,
    pub stats: TrackerStats,
}

/// Recent output transfers and frames kept.
const OUTPUTS: usize = 16;
const FRAMES: usize = 4;

impl MovieTracker {
    /// After the MDEC device answered `function` with `args`.
    /// Returns the movie frame of a decode command it identified.
    pub fn on_mdec(&mut self, ram: &[u8], function: u32, args: [u32; 4]) -> Option<DecodeId> {
        let [a0, a1, ..] = args;
        match function {
            machine_mdec::calls::MDEC_IN => {
                let command = ram_u32(ram, a0);
                if command >> 29 != 1 {
                    return None;
                }
                self.serial += 1;
                self.stats.decodes += 1;
                let depth = match (command >> 27) & 3 {
                    2 => Depth::Rgb24,
                    3 => Depth::Rgb15,
                    _ => {
                        self.current = None;
                        return None;
                    }
                };
                self.current = identify_decode(ram, a0).map(|id| Current {
                    serial: self.serial,
                    id,
                    depth,
                    stp: command & (1 << 25) != 0,
                });
                if self.current.is_some() {
                    self.stats.identified += 1;
                }
                return self.current.map(|current| current.id);
            }
            machine_mdec::calls::MDEC_OUT => {
                if let Some(Current { serial, .. }) = self.current {
                    if self.outputs.len() == OUTPUTS {
                        self.outputs.pop_front();
                    }
                    self.outputs.push_back(Output {
                        address: a0,
                        bytes: a1 as usize * 4,
                        serial,
                    });
                }
            }
            _ => {}
        }
        None
    }

    /// Before LoadImage(`rect`, `pixels`) writes VRAM: a slice of a decoded
    /// frame when `pixels` is an MDEC output transfer of its size. Returns
    /// the frame when this slice completed it.
    pub fn on_load_image(
        &mut self,
        ram: &[u8],
        [x, y, w, h]: [u16; 4],
        pixels: u32,
    ) -> Option<CompleteFrame> {
        let bytes = usize::from(w) * usize::from(h) * 2;
        let at = self
            .outputs
            .iter()
            .rposition(|o| o.address == pixels && o.bytes == bytes)?;
        let output = self.outputs.remove(at).expect("found");
        let Current {
            serial,
            id,
            depth,
            stp,
        } = self.current.filter(|c| c.serial == output.serial)?;
        self.stats.slices += 1;
        let index = match self.frames.iter().position(|f| f.serial == serial) {
            Some(index) => index,
            None => {
                let width = match depth {
                    Depth::Rgb24 => id.width * 3 / 2,
                    Depth::Rgb15 => id.width,
                };
                if self.frames.len() == FRAMES {
                    self.frames.pop_front();
                }
                self.frames.push_back(VramFrame {
                    serial,
                    id,
                    depth,
                    x,
                    y,
                    width,
                    height: id.height,
                    pixels: vec![0; usize::from(width) * usize::from(id.height)],
                    stp,
                    columns: vec![false; usize::from(width)],
                });
                self.frames.len() - 1
            }
        };
        let frame = &mut self.frames[index];
        let was_complete = frame.complete();
        for column in 0..w {
            let Some(fx) = (x + column).checked_sub(frame.x) else {
                continue;
            };
            if fx >= frame.width {
                continue;
            }
            for row in 0..h.min(frame.height) {
                let Some(fy) = (y + row).checked_sub(frame.y) else {
                    continue;
                };
                if fy >= frame.height {
                    continue;
                }
                let source = pixels + 2 * (u32::from(row) * u32::from(w) + u32::from(column));
                frame.pixels[usize::from(fy) * usize::from(frame.width) + usize::from(fx)] =
                    ram_u16(ram, source);
            }
            frame.columns[usize::from(fx)] = true;
        }
        if was_complete || !frame.complete() {
            return None;
        }
        self.stats.frames += 1;
        Some(CompleteFrame {
            serial,
            id,
            depth,
            rect: [frame.x, frame.y, frame.width, frame.height],
            stp,
        })
    }

    /// The movie frame in the shown field: `vram` (1024x512 halfwords) with
    /// the display at `start` of `size` pixels, 24-bit or not. The newest
    /// complete frame inside the display whose pixels the field still
    /// holds (at least half of them); the pixels that differ are overlay.
    /// The hub's clips are never shown as such (they are textures).
    pub fn shown(
        &self,
        vram: &[u16],
        start: (u16, u16),
        size: (u16, u16),
        rgb24: bool,
    ) -> Option<ShownFrame> {
        let (sx, sy) = (u32::from(start.0), u32::from(start.1));
        let (dw, dh) = (u32::from(size.0), u32::from(size.1));
        let display_halfwords = if rgb24 { dw * 3 / 2 } else { dw };
        self.frames.iter().rev().find_map(|frame| {
            if !frame.complete() || frame.id.hub || (frame.depth == Depth::Rgb24) != rgb24 {
                return None;
            }
            let (fx, fy) = (u32::from(frame.x), u32::from(frame.y));
            let (fw, fh) = (u32::from(frame.width), u32::from(frame.height));
            let inside = fx >= sx
                && fy >= sy
                && fx + fw <= sx + display_halfwords
                && fy + fh <= sy + dh
                && fx + fw <= 1024
                && fy + fh <= 512;
            let offset = (fx - sx.min(fx)) * 2;
            if !inside || (rgb24 && offset % 3 != 0) {
                return None;
            }
            let px = if rgb24 { offset / 3 } else { fx - sx };
            let (w, h) = (u32::from(frame.id.width), fh);
            let mut overlay = Vec::with_capacity((w * h) as usize);
            for row in 0..h {
                let shown = &vram[((fy + row) * 1024 + fx) as usize..][..fw as usize];
                let ours = &frame.pixels[(row * fw) as usize..][..fw as usize];
                if rgb24 {
                    let byte = |line: &[u16], i: usize| (line[i / 2] >> (8 * (i & 1))) as u8;
                    for p in 0..w as usize {
                        overlay
                            .push((0..3).any(|c| byte(shown, 3 * p + c) != byte(ours, 3 * p + c)));
                    }
                } else {
                    overlay.extend(shown.iter().zip(ours).map(|(a, b)| a != b));
                }
            }
            let overlay_pixels = overlay.iter().filter(|&&o| o).count();
            (overlay_pixels * 2 <= overlay.len()).then(|| ShownFrame {
                id: frame.id,
                serial: frame.serial,
                depth: frame.depth,
                rect: [px, fy - sy, w, h],
                picture: (dw, dh),
                overlay,
                overlay_pixels,
            })
        })
    }

    /// Forget every frame (after a quick load: VRAM came from the state).
    pub fn clear(&mut self) {
        self.current = None;
        self.outputs.clear();
        self.frames.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RAM with the state-5 decoder at 0x80100000 (VLC buffers 0x80120000
    /// and 0x80124000) streaming 0x9E6, frame 7 of 32x16.
    fn decoder_ram(command: u32) -> Vec<u8> {
        let mut ram = vec![0u8; 0x20_0000];
        let mut put = |address: u32, value: u32| {
            let at = (address & 0x1f_ffff) as usize;
            ram[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        let d = 0x8010_0000;
        put(jp_players::DECODER, d);
        put(d + 8, 0x8012_0000);
        put(d + 0xc, 0x8012_4000);
        put(d + jp_players::DECODER_WIDTH, 32 | (16 << 16));
        put(d + jp_players::DECODER_FRAME, 7);
        put(d + jp_players::DECODER_FILE, 0x9e6);
        put(0x8012_4000, command);
        ram
    }

    /// One decode of two 24-halfword slices at (0, 32), pixel data from
    /// `pixel(x, y)`, into `tracker`.
    fn decode_frame(
        tracker: &mut MovieTracker,
        ram: &mut [u8],
        pixel: impl Fn(usize, usize) -> u16,
    ) {
        use machine_mdec::calls::{MDEC_IN, MDEC_OUT};
        tracker.on_mdec(ram, MDEC_IN, [0x8012_4000, 0x100, 0, 0]);
        for slice in 0..2u32 {
            let buffer = 0x8013_0000 + slice * 0x1000;
            for y in 0..16 {
                for x in 0..24 {
                    let at = (buffer as usize & 0x1f_ffff) + 2 * (y * 24 + x);
                    let value = pixel(slice as usize * 24 + x, y);
                    ram[at..at + 2].copy_from_slice(&value.to_le_bytes());
                }
            }
            tracker.on_mdec(ram, MDEC_OUT, [buffer, 16 * 24 / 2, 0, 0]);
            tracker.on_load_image(ram, [slice as u16 * 24, 32, 24, 16], buffer);
        }
    }

    #[test]
    fn the_tracker_follows_a_frame_into_the_shown_field() {
        let mut ram = decoder_ram(0x3000_0100);
        let mut tracker = MovieTracker::default();
        let pixel = |x: usize, y: usize| (x * 7 + y * 131) as u16;
        decode_frame(&mut tracker, &mut ram, pixel);
        assert_eq!(
            (
                tracker.stats.decodes,
                tracker.stats.identified,
                tracker.stats.slices,
                tracker.stats.frames
            ),
            (1, 1, 2, 1)
        );
        // VRAM as the slices left it, a 24-bit 320x240 display at (0, 0).
        let mut vram = vec![0u16; 1024 * 512];
        for y in 0..16 {
            for x in 0..48 {
                vram[(32 + y) * 1024 + x] = pixel(x, y);
            }
        }
        let shown = tracker
            .shown(&vram, (0, 0), (320, 240), true)
            .expect("shown");
        assert_eq!(shown.id.resource, 0x9e6);
        assert_eq!(
            (shown.id.number, shown.id.width, shown.id.height),
            (7, 32, 16)
        );
        assert_eq!((shown.rect, shown.picture), ([0, 32, 32, 16], (320, 240)));
        assert_eq!(shown.overlay_pixels, 0);
        // A subtitle over one halfword: the two pixels sharing it.
        vram[(32 + 5) * 1024 + 4] ^= 0xffff;
        let shown = tracker.shown(&vram, (0, 0), (320, 240), true).unwrap();
        assert_eq!(shown.overlay_pixels, 2);
        assert!(shown.overlay[5 * 32 + 2] && shown.overlay[5 * 32 + 3]);
        // A 15-bit display, the other buffer, or a cleared frame: none.
        assert!(tracker.shown(&vram, (0, 0), (320, 240), false).is_none());
        assert!(tracker.shown(&vram, (0, 256), (320, 240), true).is_none());
        let cleared = vec![0u16; 1024 * 512];
        assert!(tracker.shown(&cleared, (0, 0), (320, 240), true).is_none());
        // Another decode buffer is not a movie frame; a reset forgets.
        tracker.on_mdec(
            &ram,
            machine_mdec::calls::MDEC_IN,
            [0x8015_0000, 0x10, 0, 0],
        );
        assert_eq!(tracker.stats.identified, 1);
        tracker.clear();
        assert!(tracker.shown(&vram, (0, 0), (320, 240), true).is_none());
    }

    #[test]
    fn fifteen_bit_frames_compare_pixel_for_pixel() {
        let mut ram = decoder_ram(0x3800_0100);
        let mut tracker = MovieTracker::default();
        decode_frame(&mut tracker, &mut ram, |x, y| (x + y * 48) as u16 | 0x8000);
        let mut vram = vec![0u16; 1024 * 512];
        for y in 0..16 {
            for x in 0..32 {
                vram[(32 + y) * 1024 + x] = (x + y * 48) as u16 | 0x8000;
            }
        }
        // 32 pixels wide: two 24-halfword slices cover it (16 more beyond).
        let shown = tracker.shown(&vram, (0, 0), (320, 240), false).unwrap();
        assert_eq!((shown.depth, shown.rect), (Depth::Rgb15, [0, 32, 32, 16]));
        assert_eq!(shown.overlay_pixels, 0);
    }

    /// The hub's clip player at 0x80140000 (run-level buffers 0x80150000
    /// and 0x80154000) streaming 0x810, frame 41 of 32x16, with `command`
    /// in the second buffer.
    fn hub_ram(command: u32) -> Vec<u8> {
        let mut ram = vec![0u8; 0x20_0000];
        let mut put = |address: u32, value: u32| {
            let at = (address & 0x1f_ffff) as usize;
            ram[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        let block = 0x8014_0000;
        put(jp_players::HUB, block);
        put(block + 8, 0x8015_0000);
        put(block + 0xc, 0x8015_4000);
        put(block + jp_players::HUB_WIDTH, 32 | (16 << 16));
        put(block + jp_players::HUB_FRAME, 41);
        put(block + jp_players::HUB_FILE, 0x810);
        put(0x8015_4000, command);
        ram
    }

    #[test]
    fn hub_frames_are_reported_once_complete_and_never_shown() {
        use machine_mdec::calls::{MDEC_IN, MDEC_OUT};
        // 15-bit with the STP bit (command bits 27..28 = 3, bit 25).
        let mut ram = hub_ram(0x3a00_0100);
        let mut tracker = MovieTracker::default();
        let id = tracker.on_mdec(&ram, MDEC_IN, [0x8015_4000, 0x100, 0, 0]);
        assert_eq!(
            id,
            Some(DecodeId {
                resource: 0x810,
                number: 41,
                width: 32,
                height: 16,
                hub: true
            })
        );
        // Two 16x16 slices at (832, 256), each a separate output transfer.
        let mut completed = Vec::new();
        for slice in 0..2u16 {
            let buffer = 0x8016_0000 + u32::from(slice) * 0x1000;
            for at in 0..256usize {
                let at = (buffer as usize & 0x1f_ffff) + 2 * at;
                ram[at..at + 2].copy_from_slice(&0x8421u16.to_le_bytes());
            }
            tracker.on_mdec(&ram, MDEC_OUT, [buffer, 16 * 16 / 2, 0, 0]);
            completed.push(tracker.on_load_image(&ram, [832 + slice * 16, 256, 16, 16], buffer));
        }
        let done = CompleteFrame {
            serial: 1,
            id: id.unwrap(),
            depth: Depth::Rgb15,
            rect: [832, 256, 32, 16],
            stp: true,
        };
        assert_eq!(completed, [None, Some(done)]);
        assert_eq!(tracker.stats.frames, 1);
        // A slice that is no MDEC output (or repeated) reports nothing.
        assert_eq!(
            tracker.on_load_image(&ram, [832, 256, 16, 16], 0x8016_0000),
            None
        );
        // Inside a 15-bit display it is still not a shown movie frame.
        let mut vram = vec![0u16; 1024 * 512];
        for y in 256..272 {
            vram[y * 1024 + 832..y * 1024 + 864].fill(0x8421);
        }
        assert!(tracker.shown(&vram, (640, 0), (384, 480), false).is_none());
        // Without the STP bit.
        let ram = hub_ram(0x3800_0100);
        tracker.on_mdec(&ram, MDEC_IN, [0x8015_4000, 0x100, 0, 0]);
        tracker.on_mdec(&ram, MDEC_OUT, [0x8016_0000, 16 * 32 / 2, 0, 0]);
        let done = tracker.on_load_image(&ram, [832, 384, 32, 16], 0x8016_0000);
        assert_eq!(
            done.map(|d| (d.rect, d.stp)),
            Some(([832, 384, 32, 16], false))
        );
    }

    #[test]
    fn busy_clock_yields_a_vblank_per_field() {
        use crate::machine_audio::CYCLES_PER_PAL_FIELD;
        let pal = FieldLength::PAL;
        let mut clock = BusyClock::default();
        assert_eq!(clock.advance(100, pal), 0);
        let polls = (CYCLES_PER_PAL_FIELD / 100) as u32;
        let due: u64 = (0..polls).map(|_| clock.advance(100, pal)).sum();
        assert_eq!(due, 1);
        assert_eq!(clock.advance(CYCLES_PER_PAL_FIELD as u32 * 2, pal), 2);
        // NTSC: 565,044.48 cycles a field; 1001 s of cycles are 60000 fields.
        let ntsc = FieldLength::NTSC;
        let mut clock = BusyClock::default();
        assert_eq!(clock.advance(565_044, ntsc), 0);
        assert_eq!(clock.advance(1, ntsc), 1);
        let mut clock = BusyClock::default();
        let due: u64 = (0..1001).map(|_| clock.advance(33_868_800, ntsc)).sum();
        assert_eq!(due, 60_000);
    }

    #[test]
    fn display_reads_24_bit_rows() {
        let mut vram = vec![0u16; 1024 * 512];
        // Two 24-bit pixels at (10, 3): bytes 11 22 33 44 55 66.
        vram[3 * 1024 + 10] = 0x2211;
        vram[3 * 1024 + 11] = 0x4433;
        vram[3 * 1024 + 12] = 0x6655;
        assert_eq!(
            display_rgb(&vram, (10, 3), 2, 1, true),
            vec![0x0011_2233, 0x0044_5566]
        );
        vram[0] = 0x7c1f;
        assert_eq!(display_rgb(&vram, (0, 0), 1, 1, false), vec![0x00ff_00ff]);
    }

    #[test]
    fn xa_sink_queues_and_mutes() {
        let queue = Rc::new(RefCell::new(XaQueue::default()));
        let mut sink = XaSink(queue.clone());
        let mut sector = [0u8; SECTOR_BYTES];
        sector[1..11].fill(0xff);
        sector[15] = 2;
        sector[16..24].copy_from_slice(&[1, 1, 0x64, 1, 1, 1, 0x64, 1]);
        sink.xa_sector(&sector);
        sink.mute(true);
        let mut player = XaPlayer::default();
        let mut spu = SpuDevice::new();
        player.feed(&queue, &mut spu);
        assert_eq!(player.sectors, 1);
        assert_eq!(
            spu.cd_audio_queued(),
            ac3_audio::xa::OUTPUT_FRAMES_PER_SECTOR
        );
        assert!(queue.borrow().sectors.is_empty());
    }

    /// Every profile of the discs plays (the Japanese mono 18.9 kHz among
    /// them), muted or not, with its own frame count; 8-bit is rejected.
    #[test]
    fn xa_player_plays_every_disc_profile() {
        let queue = Rc::new(RefCell::new(XaQueue::default()));
        let mut sink = XaSink(queue.clone());
        let mut player = XaPlayer::default();
        let mut spu = SpuDevice::new();
        let mut expected = 0;
        for (coding, frames, muted) in [
            (0x01, 2352, false),
            (0x05, 4704, false),
            (0x00, 4704, true),
            (0x04, 9408, false),
            (0x04, 9408, true),
        ] {
            let mut sector = [0u8; SECTOR_BYTES];
            sector[1..11].fill(0xff);
            sector[15] = 2;
            sector[16..24].copy_from_slice(&[1, 4, 0x64, coding, 1, 4, 0x64, coding]);
            sink.mute(muted);
            sink.xa_sector(&sector);
            player.feed(&queue, &mut spu);
            expected += frames;
            assert_eq!(spu.cd_audio_queued(), expected, "coding {coding:#x}");
        }
        let mut sector = [0u8; SECTOR_BYTES];
        sector[1..11].fill(0xff);
        sector[15] = 2;
        sector[16..24].copy_from_slice(&[1, 4, 0x64, 0x11, 1, 4, 0x64, 0x11]);
        sink.xa_sector(&sector);
        player.feed(&queue, &mut spu);
        assert_eq!((player.sectors, player.rejected), (5, 1));
        assert_eq!(spu.cd_audio_queued(), expected);
    }
}
