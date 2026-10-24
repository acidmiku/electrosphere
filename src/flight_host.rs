//! Host side of the machine's GPU device calls (`ac3_machine::HostCall`).
//!
//! The translated engine builds GPU packets, ordering tables and environment
//! packets in machine RAM exactly as the original does; the SDK routines that
//! hand them to hardware are logged as host calls. This module reproduces the
//! DMA semantics of those hand-overs and feeds the words to the native
//! VRAM-targeted [`Gpu`], synchronously at the original call point.
//!
//! With the GPU renderer on, the same words (and the packets' RAM
//! addresses) also go to its [`Recorder`], each just before the software
//! GPU executes it ([`GpuDevice::call_with`]).
use crate::gpu_renderer::Recorder;
use ac3_graphics::{Gpu, Vram};
use ac3_machine::{Device, HostCall, Machine};
use std::{cell::RefCell, fmt, rc::Rc};

/// DMA linked-list terminator in a packet header's link field.
pub const LIST_END: u32 = 0x00ff_ffff;
/// Upper bound on packets in one list; a longer chain is a corrupted table.
pub const MAX_LIST_PACKETS: usize = 1 << 20;

/// libgpu routines whose device work the host performs (their canonical
/// host-call ids, [`ac3_machine::host_ids`]). The translated libgpu
/// (`ac3_flight::sdk` for PAL) builds every packet, environment and queue
/// word in RAM itself and reaches the hardware only through these.
pub mod sdk {
    use ac3_machine::host_ids;
    /// 800A8360 (table +0x18): start GPU DMA channel 2 in linked-list mode
    /// at `[list, param, size]` (DrawOTag, PutDrawEnv, ClearImage packets,
    /// and MoveImage's one-packet list whose GP0(80h) copies a VRAM
    /// rectangle within VRAM).
    pub const DMA_LIST: u32 = host_ids::DMA_LIST;
    /// 800A82E8 (table +0x10): a GP1 control word `[command]`.
    pub const GP1: u32 = host_ids::GP1;
    /// 800A83A8 (table +0x30): GP1(10h) information `[index]` -> GPUREAD.
    pub const INFO: u32 = host_ids::GPU_INFO;
    /// 800A7BFC (table +0xC, the ClearImage transfer) reads GPUSTAT once
    /// `[rect, colour]` -> GPUSTAT.
    pub const CLEAR_STATUS: u32 = host_ids::GPU_CLEAR_STATUS;
    /// 800A8A5C (table +0x3C): DrawSync's wait -> poll count (0: idle).
    pub const SYNC: u32 = host_ids::GPU_SYNC;
    /// 800A7B04 (table +0x38): GPUSTAT read (GetODE) -> GPUSTAT.
    pub const STATUS: u32 = host_ids::GPU_STATUS;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DmaError {
    /// A link or packet word outside main RAM.
    Address(u32),
    /// The chain did not terminate within `MAX_LIST_PACKETS`.
    Unterminated(u32),
}
impl fmt::Display for DmaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Address(address) => write!(f, "GPU DMA address outside RAM: {address:#010x}"),
            Self::Unterminated(start) => {
                write!(f, "GPU DMA list from {start:#010x} does not terminate")
            }
        }
    }
}
impl std::error::Error for DmaError {}

fn ram_word(ram: &[u8], address: u32) -> Result<u32, DmaError> {
    // DMA addresses are 24-bit physical; only the 2 MiB of main RAM exist.
    let physical = (address & 0x00ff_ffff) as usize;
    let bytes = ram
        .get(physical..physical + 4)
        .ok_or(DmaError::Address(address))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

/// One linked-list packet at `address`: header word count (bits 24..31)
/// followed by that many GP0 words. Returns the words and the next link.
pub fn packet_at(ram: &[u8], address: u32) -> Result<(Vec<u32>, u32), DmaError> {
    let header = ram_word(ram, address)?;
    let count = header >> 24;
    let words = (1..=count)
        .map(|i| ram_word(ram, address.wrapping_add(i * 4)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((words, header & LIST_END))
}

/// GPU DMA channel 2 linked-list mode (DrawOTag): follow links from `start`
/// until the terminator, returning each packet's GP0 words in execution
/// order. Empty packets (ordering-table entries) are skipped.
pub fn walk_list(ram: &[u8], start: u32) -> Result<Vec<Vec<u32>>, DmaError> {
    Ok(walk_list_addressed(ram, start)?
        .into_iter()
        .map(|(_, words)| words)
        .collect())
}

/// The KSEG0 address (`0x8000_0000 | physical`) of a DMA address.
pub fn kseg0(address: u32) -> u32 {
    0x8000_0000 | (address & 0x1f_ffff)
}

/// [`walk_list`] with each packet's RAM address (KSEG0, its header word;
/// GP0 word `i` is at `address + 4 * (i + 1)`).
pub fn walk_list_addressed(ram: &[u8], start: u32) -> Result<Vec<(u32, Vec<u32>)>, DmaError> {
    let mut packets = Vec::new();
    let mut address = start & LIST_END;
    for _ in 0..MAX_LIST_PACKETS {
        let (words, next) = packet_at(ram, address)?;
        if !words.is_empty() {
            packets.push((kseg0(address), words));
        }
        if next == LIST_END {
            return Ok(packets);
        }
        address = next;
    }
    Err(DmaError::Unterminated(start))
}

/// The flight machine's GPU: executes DrawOTag/PutDrawEnv lists and display
/// changes on a native [`Gpu`] the moment the translated code submits them.
/// Errors are kept (the first one) rather than panicking inside the hook;
/// the host checks [`GpuDevice::error`] after each frame. Save states
/// serialize the whole device.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct GpuDevice {
    pub gpu: Gpu,
    /// DISPENV display rectangle of the last PutDispEnv (x, y, w, h).
    pub display: [i16; 4],
    pub lists: u64,
    /// `--dump-textures`: sees every packet before it executes (and the
    /// uploads, from `machine_player`); it never changes the GPU. Host-side
    /// observation, not game state: save states leave it out.
    #[serde(skip)]
    pub texture_dump: Option<Box<crate::asset_dump::TextureRecorder>>,
    error: Option<String>,
}

impl GpuDevice {
    pub fn new(vram: Vram) -> Self {
        Self {
            gpu: Gpu::new(vram),
            display: [0, 0, 320, 256],
            lists: 0,
            texture_dump: None,
            error: None,
        }
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    fn fail(&mut self, message: String) {
        self.error.get_or_insert(message);
    }
    fn submit_list(&mut self, ram: &[u8], start: u32, mut shadow: Option<&mut Recorder>) {
        match walk_list_addressed(ram, start) {
            Ok(packets) => {
                self.lists += 1;
                for (address, packet) in packets {
                    if let Some(recorder) = &mut self.texture_dump {
                        recorder.observe_gp0(&packet, self.gpu.vram());
                    }
                    let result = match shadow.as_deref_mut() {
                        Some(shadow) => self.write_observed(&packet, address, shadow),
                        None => self.gpu.write_gp0_words(&packet),
                    };
                    if let Err(error) = result {
                        self.fail(format!("GP0 list {start:#010x}: {error}"));
                        return;
                    }
                }
            }
            Err(error) => self.fail(error.to_string()),
        }
    }
    /// A packet's words, each first to `shadow` (with its RAM address and
    /// the VRAM it will read), then to the software GPU.
    fn write_observed(
        &mut self,
        packet: &[u32],
        address: u32,
        shadow: &mut Recorder,
    ) -> ac3_graphics::Result<()> {
        for (i, &word) in packet.iter().enumerate() {
            shadow.gp0(
                word,
                address.wrapping_add(4 * (i as u32 + 1)),
                self.gpu.vram(),
            );
            self.gpu.write_gp0(word)?;
        }
        Ok(())
    }
    /// Displayed pixels of the current display rectangle (RGB555/STP).
    pub fn displayed(&self) -> ac3_graphics::Result<Vec<u16>> {
        let [_, _, w, h] = self.display;
        self.gpu
            .display_pixels(w.max(0) as usize, h.max(0) as usize)
    }
    /// Display rectangle from the GP1 display state: start (05), width
    /// from the mode (08), height from the vertical range (07), doubled in
    /// 480-line interlaced mode.
    fn update_display(&mut self) {
        let display = self.gpu.display();
        let range = display.vertical_range;
        let mut height = ((range >> 10) & 0x3ff).saturating_sub(range & 0x3ff) as i16;
        if display.mode & 0x24 == 0x24 {
            height *= 2;
        }
        if height <= 0 {
            height = self.display[3];
        }
        self.display = [
            display.start_x as i16,
            display.start_y as i16,
            display.width() as i16,
            height.min(512),
        ];
    }
}

impl GpuDevice {
    /// [`Device::call`], also feeding the GPU renderer's recorder (`shadow`)
    /// the GP0 words and GP1 writes, each before the software GPU.
    pub fn call_with(
        &mut self,
        ram: &mut [u8],
        call: HostCall,
        shadow: Option<&mut Recorder>,
    ) -> Option<u32> {
        let [a0, ..] = call.args;
        match call.function {
            sdk::SYNC => Some(0),
            sdk::DMA_LIST => {
                self.submit_list(ram, a0, shadow);
                None
            }
            sdk::GP1 => {
                if let Some(recorder) = &mut self.texture_dump {
                    recorder.observe_gp1(a0);
                }
                if let Some(shadow) = shadow {
                    shadow.gp1(a0, self.gpu.vram());
                }
                if let Err(error) = self.gpu.write_gp1(a0) {
                    self.fail(format!("GP1 {a0:#010x}: {error}"));
                }
                if matches!(a0 >> 24, 0x00 | 0x05..=0x08) {
                    self.update_display();
                }
                None
            }
            sdk::INFO => Some(self.gpu.info(a0)),
            sdk::CLEAR_STATUS | sdk::STATUS => Some(self.gpu.status()),
            _ => None,
        }
    }
}

impl Device for GpuDevice {
    fn call(&mut self, ram: &mut [u8], _scratch: &mut [u8], call: HostCall) -> Option<u32> {
        self.call_with(ram, call, None)
    }
}

/// Shared handle so the host keeps access to the GPU while the machine owns
/// its device hook (`machine.device = Some(Box::new(shared.clone()))`).
#[derive(Clone)]
pub struct SharedGpu(pub Rc<RefCell<GpuDevice>>);
impl SharedGpu {
    pub fn new(vram: Vram) -> Self {
        Self(Rc::new(RefCell::new(GpuDevice::new(vram))))
    }
}
impl Device for SharedGpu {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        self.0.borrow_mut().call(ram, scratch, call)
    }
}

/// Convenience for tests: walk a list in a machine's RAM.
pub fn walk_machine_list(m: &Machine, start: u32) -> Result<Vec<Vec<u32>>, DmaError> {
    walk_list(m.ram(), start)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn machine_with(words: &[(u32, u32)]) -> Machine {
        let mut m = Machine::default();
        for &(address, value) in words {
            m.sw(address, value);
        }
        m
    }
    #[test]
    fn list_skips_empty_entries_and_stops_at_terminator() {
        // Two-entry table: entry 1 -> packet -> entry 0 -> end.
        let m = machine_with(&[
            (0x8010_0004, 0x0010_0100),
            (0x8010_0100, 0x0310_0000),
            (0x8010_0104, 0x6000_00ff),
            (0x8010_0108, 0x0000_0000),
            (0x8010_010c, 0x0010_0010),
            (0x8010_0000, 0x00ff_ffff),
        ]);
        assert_eq!(
            walk_machine_list(&m, 0x8010_0004).unwrap(),
            vec![vec![0x6000_00ff, 0, 0x0010_0010]]
        );
    }
    /// The renderer's recorder gets every word with its RAM address (KSEG0),
    /// before the software GPU executes it, and its vertex positions can
    /// come from a precision source keyed by those addresses.
    #[test]
    fn packets_report_their_addresses_to_the_recorder() {
        use crate::gpu_renderer::{Options, Recorder, VertexPrecision};
        struct Probe(RefCell<Vec<u32>>);
        impl VertexPrecision for Probe {
            fn precise_vertex(&self, address: u32, _word: u32) -> Option<[f32; 3]> {
                self.0.borrow_mut().push(address);
                None
            }
        }
        // One packet at physical 0x100100: E3, E4, a flat triangle.
        let m = machine_with(&[
            (0x8010_0100, 0x06ff_ffff),
            (0x8010_0104, 0xe300_0000),
            (0x8010_0108, 0xe407_ffff),
            (0x8010_010c, 0x2000_00ff),
            (0x8010_0110, 0x0000_0000),
            (0x8010_0114, 0x0000_0010),
            (0x8010_0118, 0x0010_0000),
        ]);
        assert_eq!(
            walk_list_addressed(m.ram(), 0x0010_0100).unwrap(),
            vec![(
                0x8010_0100,
                vec![0xe300_0000, 0xe407_ffff, 0x2000_00ff, 0, 0x10, 0x0010_0000]
            )]
        );
        let mut device = GpuDevice::new(Vram::new());
        let mut recorder = Recorder::new(
            Options {
                scale: 2,
                dither: false,
            },
            &device.gpu,
        );
        let probe = Rc::new(Probe(RefCell::new(Vec::new())));
        recorder.precision = Some(probe.clone());
        let mut ram = m.ram().to_vec();
        let call = HostCall {
            function: sdk::DMA_LIST,
            args: [0x0010_0100, 0, 0, 0],
        };
        assert_eq!(device.call_with(&mut ram, call, Some(&mut recorder)), None);
        assert_eq!(device.error(), None);
        assert_eq!(*probe.0.borrow(), [0x8010_0110, 0x8010_0114, 0x8010_0118]);
        assert_eq!(recorder.stats().triangles, 1);
        // The software GPU drew it as without a recorder.
        assert_eq!(device.gpu.vram().words()[1024 + 1], 0x1f);
    }
    #[test]
    fn cycles_and_bad_links_fail_explicitly() {
        let m = machine_with(&[(0x8010_0000, 0x0010_0000)]);
        assert_eq!(
            walk_machine_list(&m, 0x8010_0000),
            Err(DmaError::Unterminated(0x8010_0000))
        );
        let m = machine_with(&[(0x8010_0000, 0x0030_0000)]);
        assert_eq!(
            walk_machine_list(&m, 0x8010_0000),
            Err(DmaError::Address(0x0030_0000))
        );
    }
    /// MoveImage (`ac3_flight::sdk::move_image`) through libgpu's table:
    /// its one-packet list reaches the device as a DMA list and the GP0(80h)
    /// copy moves the VRAM rectangle at once.
    #[test]
    fn move_image_copies_vram_through_the_dma_list() {
        let table = 0x8010_0200;
        let mut m = machine_with(&[
            // libgpu table pointer and its +8 submit / +0x18 DMA list start.
            (0x800b_97dc, table),
            (table + 8, 0x800a_83fc),
            (table + 0x18, 0x800a_8360),
            // MoveImage's static list header and GP0(80h) command.
            (0x800b_987c, 0x04ff_ffff),
            (0x800b_9880, 0x8000_0000),
            // A fill list: TILE, red 16x8 at (4, 2).
            (0x8010_0000, 0x05ff_ffff),
            (0x8010_0004, 0xe300_0000),
            (0x8010_0008, 0xe407_fbff),
            (0x8010_000c, 0x6000_00f8),
            (0x8010_0010, 0x0002_0004),
            (0x8010_0014, 0x0008_0010),
            // RECT (4, 2, 16, 8).
            (0x8010_0100, 0x0002_0004),
            (0x8010_0104, 0x0008_0010),
        ]);
        let shared = SharedGpu::new(Vram::new());
        m.device = Some(Box::new(shared.clone()));
        m.host_call(sdk::DMA_LIST, [0x8010_0000, 0, 0, 0]);
        let result = ac3_flight::sdk::move_image(&mut m, 0x8010_0100, 0x200, 0x100);
        assert_eq!(result, 0);
        assert_eq!(m.fault(), None);
        assert_eq!(m.lw(0x800b_9884), 0x0002_0004);
        assert_eq!(m.lw(0x800b_9888), 0x0100_0200);
        assert_eq!(m.lw(0x800b_988c), 0x0008_0010);
        let device = shared.0.borrow();
        assert_eq!(device.error(), None);
        assert_eq!(device.lists, 2);
        let vram = device.gpu.vram().words();
        assert_eq!(vram[0x100 * 1024 + 0x200], 0x1f);
        assert_eq!(vram[(0x100 + 7) * 1024 + 0x200 + 15], 0x1f);
        assert_eq!(vram[(0x100 + 8) * 1024 + 0x200], 0);
        assert_eq!(vram[2 * 1024 + 4], 0x1f, "the source stays");
    }
    #[test]
    fn device_draws_submitted_list_into_vram_at_call_time() {
        let mut m = machine_with(&[
            // One-packet list: full drawing area (E3/E4), then TILE 0x60,
            // red 16x8 at (4, 2).
            (0x8010_0000, 0x05ff_ffff),
            (0x8010_0004, 0xe300_0000),
            (0x8010_0008, 0xe407_fbff),
            (0x8010_000c, 0x6000_00f8),
            (0x8010_0010, 0x0002_0004),
            (0x8010_0014, 0x0008_0010),
        ]);
        let shared = SharedGpu::new(Vram::new());
        m.device = Some(Box::new(shared.clone()));
        m.host_call(sdk::DMA_LIST, [0x8010_0000, 0, 0, 0]);
        // Rewriting the packet afterwards must not change what was drawn.
        m.sw(0x8010_000c, 0x6000_0000);
        let device = shared.0.borrow();
        assert_eq!(device.error(), None);
        assert_eq!(device.lists, 1);
        let vram = device.gpu.vram().words();
        assert_eq!(vram[2 * 1024 + 4], 0x1f);
        assert_eq!(vram[9 * 1024 + 19], 0x1f);
        assert_eq!(vram[10 * 1024 + 4], 0);
    }
}
