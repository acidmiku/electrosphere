//! Host SPU device for the address-preserving machine (`ac3_machine`).
//!
//! The translated game code reaches the sound hardware through libspu
//! routines that `ac3_flight::sdk` logs as [`HostCall`]s with the original
//! arguments (struct arguments are guest pointers). [`SpuDevice`] is the
//! device side of those calls. Each libspu routine is translated here from
//! its MIPS instructions down to its SPU register stores: the same stores,
//! with the same values and in the same order, as the original (every store
//! carries its original instruction address, see [`RegisterWrite`]). RAM the
//! routines write that `sdk.rs` does not reproduce (SpuInit's and
//! SpuSetReverbModeParam's libspu state) is written here too.
//!
//! Below the register file the device models the SPU itself: 512 KiB of
//! sound RAM (data port FIFO and DMA), 24 voices (ADPCM blocks with loop
//! flags, the pitch counter with Gaussian interpolation, ADSR envelopes,
//! fixed and sweep volumes, noise and pitch modulation), the CD audio input
//! and the main volume, producing 44.1 kHz stereo ([`SpuDevice::mix`]).
//! Reverb registers are modelled, the reverb engine is not (AC3 never enables
//! it: its only SpuSetReverb call turns reverb off).
//!
//! The arithmetic follows the PS1 SPU documentation (nocash psx-spx) and the
//! pinned PCSX-Redux oracle where the `ac3-audio` renderers established its
//! exact behaviour (Gaussian rounding, envelope rate counters, the six-sample
//! key-on latency, signed fixed-volume division). Where Redux leaves out a
//! hardware stage (main volume, CD input volume) [`OutputStage`] selects it.
//!
//! Timing: a host call takes effect at once (the host has no CPU cycle
//! count); the host mixes one field of the release per delivered VBlank
//! ([`SpuDevice::mix_field`], [`FieldLength`]: PAL 881.9 samples, NTSC
//! 735.735). The libspu words the device reads and writes in guest RAM are
//! the release's ([`LibSpu`], [`SpuDevice::set_release`]). Register reads
//! see the voices as of the last register store, as the oracle does. The
//! only busy-wait the device turns into SPU time is SpuInit's key-on/key-off
//! delay ([`SpuDevice::mix`] returns those samples first).
//!
//! Validation (`tests/machine_audio.rs`): every register store of 13,570
//! original libspu calls (boot, first hub, flight) in order, with the final
//! register file and sound RAM; the translated sound update on original
//! RAM; 703,758 + 615,575 stereo frames of Redux's mixed output (hub and
//! flight, driven by the original store schedule) and its key-status reads;
//! 19 sounds against the `ac3-audio` renderers. Hardware behaviour the
//! oracle does not show, kept from psx-spx: an end-without-repeat block mutes
//! (release at level 0) while the voice keeps decoding, end+repeat+loop-start
//! loops, pitch above 3FFFh runs at 4000h, voice 1/3 and CD output go to the
//! capture area (sound RAM 0..0xFFF); unvalidated: sweeps, noise, pitch
//! modulation, CD input mixing, the hardware output stage.
use ac3_machine::{Device, HostCall, host_ids};
use std::collections::VecDeque;

/// The crate's error convention (`crate::Result`), spelled out so the module
/// also builds inside its integration test.
type AudioResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Sound RAM bytes.
pub const SPU_RAM_BYTES: usize = 0x8_0000;
const SPU_RAM_MASK: u32 = 0x7_ffff;
/// Output sample rate.
pub const SAMPLE_RATE: u32 = 44_100;
/// CPU cycles per SPU output sample (33.8688 MHz / 44.1 kHz).
pub const CYCLES_PER_SAMPLE: u64 = 768;
/// CPU cycles per PAL field in the pinned Redux timing model (314 lines of
/// 2157 cycles).
pub const CYCLES_PER_PAL_FIELD: u64 = 677_298;
/// CPU cycles per NTSC field: one field of [`FieldRate::NTSC`]
/// (60000/1001 Hz) at 33.8688 MHz, 565,044.48 cycles (14,126,112 / 25).
///
/// [`FieldRate::NTSC`]: ac3_machine::game::FieldRate::NTSC
pub const CYCLES_PER_NTSC_FIELD: FieldLength = FieldLength::new(14_126_112, 25);

/// CPU cycles of one field (one VBlank of guest time) as an exact fraction,
/// `cycles / per`: the SPU mixes that many samples per delivered VBlank
/// ([`SpuDevice::mix_field`]) and busy-wait loops deliver a VBlank per
/// field of their cycles (`machine_movie::BusyClock`).
///
/// - **PAL**: [`CYCLES_PER_PAL_FIELD`], the pinned Redux model the PAL audio
///   and movie captures were validated with: 881.9 samples per field, 0.012 %
///   fewer than 44.1 kHz at the 50 Hz field clock.
/// - **NTSC**: [`CYCLES_PER_NTSC_FIELD`], exactly one 59.94 Hz field: 735.735
///   samples per field, 44.1 kHz at the field clock with no drift. Redux's
///   NTSC model (263 lines of 2146 cycles, a 60.009 Hz field) would leave the
///   59.94 Hz window 0.12 % short of audio; the Japanese release has no
///   captures pinned to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldLength {
    pub cycles: u64,
    pub per: u64,
}

impl FieldLength {
    pub const PAL: Self = Self::new(CYCLES_PER_PAL_FIELD, 1);
    pub const NTSC: Self = CYCLES_PER_NTSC_FIELD;
    pub const fn new(cycles: u64, per: u64) -> Self {
        Self { cycles, per }
    }
    /// The field of a TV standard.
    pub fn of(video: ac3_machine::game::VideoStandard) -> Self {
        match video {
            ac3_machine::game::VideoStandard::Pal => Self::PAL,
            ac3_machine::game::VideoStandard::Ntsc => Self::NTSC,
        }
    }
    /// Output samples per field, as a float (reports).
    pub fn samples(self) -> f64 {
        self.cycles as f64 / (self.per * CYCLES_PER_SAMPLE) as f64
    }
}

impl Default for FieldLength {
    fn default() -> Self {
        Self::PAL
    }
}
/// SPU register file base (`0x1F801C00`); offsets below are relative to it.
pub const SPU_BASE: u32 = 0x1f80_1c00;
/// Samples between a key-on write and the voice's first output sample in
/// the pinned Redux oracle (the `ac3-audio` voices' six startup ticks).
pub const KEY_ON_LATENCY: u8 = 6;

/// libspu entry points answered by [`SpuDevice`] (their canonical host-call
/// ids, [`ac3_machine::host_ids`]).
pub mod calls {
    /// 8009E218 SpuInit() (`_SpuInit(0)`; named SsUtReverbOff in boot.rs).
    pub const SPU_INIT: u32 = super::host_ids::SPU_INIT;
    /// 8009D928: programmed-I/O upload `[source, size, SPU address]`.
    pub const SPU_WRITE_PIO: u32 = super::host_ids::SPU_WRITE_PIO;
    /// 8009DC4C `_spu_t(op, value, size)` `[op, value, size, SPU address]`.
    pub const SPU_TRANSFER: u32 = super::host_ids::SPU_TRANSFER;
    /// 8009DFF8 `_spu_FsetRXXa` register store `[register index, value]`.
    pub const SPU_SET_REGISTER: u32 = super::host_ids::SPU_SET_REGISTER;
    /// 8009E2B8 SpuSetReverb `[on, enabled]`.
    pub const SPU_SET_REVERB: u32 = super::host_ids::SPU_SET_REVERB;
    /// 8009E498 SpuSetReverbModeParam `[attr]` (returns 0 or -1).
    pub const SPU_SET_REVERB_MODE_PARAM: u32 = super::host_ids::SPU_SET_REVERB_MODE_PARAM;
    /// 8009EE48 SpuSetReverbDepth `[attr, mask]`.
    pub const SPU_SET_REVERB_DEPTH: u32 = super::host_ids::SPU_SET_REVERB_DEPTH;
    /// 8009EEF8 `_SpuSetAnyVoice` `[on, voices, low, high]` (returns the
    /// old 24-bit register value).
    pub const SPU_SET_ANY_VOICE: u32 = super::host_ids::SPU_SET_ANY_VOICE;
    /// 8009F3C8 SpuSetKey `[on, voices]`.
    pub const SPU_SET_KEY: u32 = super::host_ids::SPU_SET_KEY;
    /// 8009F888 SpuSetCommonAttr `[attr]`.
    pub const SPU_SET_COMMON_ATTR: u32 = super::host_ids::SPU_SET_COMMON_ATTR;
    /// 8009FCDC SpuGetAllKeysStatus `[status]` (logged; see
    /// [`SPU_KEY_STATUS`]).
    pub const SPU_GET_ALL_KEYS_STATUS: u32 = super::host_ids::SPU_GET_ALL_KEYS_STATUS;
    /// 8009FD4C: one voice of SpuGetAllKeysStatus `[status, voice, keyed]`,
    /// answered with the status byte the original stores (from the key-on
    /// bit and the voice's ENVX register).
    pub const SPU_KEY_STATUS: u32 = super::host_ids::SPU_KEY_STATUS;
    /// 8009FD68 SpuSetVoiceAttr `[attr]`.
    pub const SPU_SET_VOICE_ATTR: u32 = super::host_ids::SPU_SET_VOICE_ATTR;
    /// 800A18E0 CdMix `[atv]` (CD-ROM audio attenuation; returns 1).
    pub const CD_MIX: u32 = super::host_ids::CD_MIX;
    /// 8009DBA4 `_spu_Fr_` `[buf, SPU address (8-byte units), blocks]`: DMA
    /// read of sound RAM into guest RAM (SpuReadDecodedData's capture
    /// buffers).
    pub const SPU_READ_DMA: u32 = super::host_ids::SPU_READ_DMA;
    /// 8009F358 SpuReadDecodedData's SPUSTAT read `[buf, part]` -> SPUSTAT
    /// (bit 11: the capture half being written).
    pub const SPU_CAPTURE_STATUS: u32 = super::host_ids::SPU_CAPTURE_STATUS;
}

/// Register offsets from [`SPU_BASE`].
pub mod reg {
    pub const VOICE_VOLUME_LEFT: u32 = 0x0;
    pub const VOICE_VOLUME_RIGHT: u32 = 0x2;
    pub const VOICE_PITCH: u32 = 0x4;
    pub const VOICE_START: u32 = 0x6;
    pub const VOICE_ADSR1: u32 = 0x8;
    pub const VOICE_ADSR2: u32 = 0xa;
    pub const VOICE_ENVX: u32 = 0xc;
    pub const VOICE_REPEAT: u32 = 0xe;
    pub const MAIN_VOLUME_LEFT: u32 = 0x180;
    pub const MAIN_VOLUME_RIGHT: u32 = 0x182;
    pub const REVERB_VOLUME_LEFT: u32 = 0x184;
    pub const REVERB_VOLUME_RIGHT: u32 = 0x186;
    pub const KEY_ON: u32 = 0x188;
    pub const KEY_OFF: u32 = 0x18c;
    pub const PITCH_MODULATION: u32 = 0x190;
    pub const NOISE: u32 = 0x194;
    pub const REVERB_ON: u32 = 0x198;
    pub const END: u32 = 0x19c;
    pub const REVERB_BASE: u32 = 0x1a2;
    pub const IRQ_ADDRESS: u32 = 0x1a4;
    pub const TRANSFER_ADDRESS: u32 = 0x1a6;
    pub const TRANSFER_FIFO: u32 = 0x1a8;
    pub const CONTROL: u32 = 0x1aa;
    pub const TRANSFER_CONTROL: u32 = 0x1ac;
    pub const STATUS: u32 = 0x1ae;
    pub const CD_VOLUME_LEFT: u32 = 0x1b0;
    pub const CD_VOLUME_RIGHT: u32 = 0x1b2;
    pub const EXTERN_VOLUME_LEFT: u32 = 0x1b4;
    pub const EXTERN_VOLUME_RIGHT: u32 = 0x1b6;
    pub const CURRENT_MAIN_VOLUME_LEFT: u32 = 0x1b8;
    pub const CURRENT_MAIN_VOLUME_RIGHT: u32 = 0x1ba;
    pub const REVERB_CONFIG: u32 = 0x1c0;
    pub const CURRENT_VOICE_VOLUME: u32 = 0x200;
}

/// Hardware addresses written outside the SPU register file.
pub mod io {
    /// DMA channel 4 (SPU) address, block control and channel control.
    pub const DMA4_MADR: u32 = 0x1f80_10c0;
    pub const DMA4_BCR: u32 = 0x1f80_10c4;
    pub const DMA4_CHCR: u32 = 0x1f80_10c8;
    /// CD-ROM index/status and the three indexed registers.
    pub const CDROM_INDEX: u32 = 0x1f80_1800;
    pub const CDROM_1: u32 = 0x1f80_1801;
    pub const CDROM_2: u32 = 0x1f80_1802;
    pub const CDROM_3: u32 = 0x1f80_1803;
}

/// libspu state in guest RAM (resident data and BSS) of a game version:
/// the words the device's translated libspu routines read and write. The
/// device takes its release's table ([`SpuDevice::set_release`]); it is
/// configuration, not device state (save states leave it out).
///
/// The Japanese release links the same libspu build: every word is the PAL
/// word moved with its data run (+0x2180 in the small-data area, +0x2228 in
/// the BSS), each checked against the JP instructions that address it
/// (`mips.normalize` of the aligned SDK routines; `_SpuInit`, `SpuSetKey`,
/// `SpuSetReverb`, `_spu_t`, `_spu_FsetRXXa`, `_spu_note2pitch`, ...; 1–13
/// sites per word, no disagreement).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LibSpu {
    pub keyed: u32,
    pub transfer_mode: u32,
    pub reverb_on: u32,
    pub reverb_reserved: u32,
    pub reverb_start: u32,
    /// SpuReverbAttr-like record: mode +0, depth L/R +4/+6, delay +8,
    /// feedback +0xC.
    pub reverb: u32,
    pub key_pending_on: u32,
    pub key_pending: u32,
    pub voice_note: u32,
    pub buffered: u32,
    pub started: u32,
    pub transfer_address: u32,
    pub io_mode: u32,
    pub io_mode_2: u32,
    pub align: u32,
    pub address_shift: u32,
    pub block: u32,
    pub block_mask: u32,
    pub transfer_done: u32,
    pub transfer_callback: u32,
    pub irq_callback: u32,
    /// 16-byte block SpuInit uploads to SPU 0x1000.
    pub init_block: u32,
    pub dma_read: u32,
    pub dma_source: u32,
    pub dma_blocks: u32,
    pub reserved_1: u32,
    pub reserved_2: u32,
    pub reserved_areas: u32,
    pub zero_buffer: u32,
    pub semitones: u32,
    pub fine: u32,
    pub reverb_offsets: u32,
    pub reverb_params: u32,
    pub key_buffer: u32,
}

impl LibSpu {
    /// The European release (SCES-02066).
    pub const PAL: Self = Self {
        keyed: 0x800b_25d0,
        transfer_mode: 0x800b_25d4,
        reverb_on: 0x800b_25d8,
        reverb_reserved: 0x800b_25dc,
        reverb_start: 0x800b_25e0,
        reverb: 0x800b_25e8,
        key_pending_on: 0x800b_25f8,
        key_pending: 0x800b_25fc,
        voice_note: 0x800b_2600,
        buffered: 0x800b_2630,
        started: 0x800b_2634,
        transfer_address: 0x800b_265c,
        io_mode: 0x800b_2660,
        io_mode_2: 0x800b_2664,
        align: 0x800b_2668,
        address_shift: 0x800b_266c,
        block: 0x800b_2670,
        block_mask: 0x800b_2674,
        transfer_done: 0x800b_2678,
        transfer_callback: 0x800b_267c,
        irq_callback: 0x800b_2680,
        init_block: 0x800b_2684,
        dma_read: 0x800b_2694,
        dma_source: 0x800b_2698,
        dma_blocks: 0x800b_269c,
        reserved_1: 0x800b_26a4,
        reserved_2: 0x800b_26a8,
        reserved_areas: 0x800b_26ac,
        zero_buffer: 0x800b_26b4,
        semitones: 0x800b_2ab4,
        fine: 0x800b_2acc,
        reverb_offsets: 0x800b_2bd4,
        reverb_params: 0x800b_2c04,
        key_buffer: 0x800c_5c78,
    };
    /// The Japanese release (SLPS-02020/02021).
    pub const JP: Self = Self {
        keyed: 0x800b_4750,
        transfer_mode: 0x800b_4754,
        reverb_on: 0x800b_4758,
        reverb_reserved: 0x800b_475c,
        reverb_start: 0x800b_4760,
        reverb: 0x800b_4768,
        key_pending_on: 0x800b_4778,
        key_pending: 0x800b_477c,
        voice_note: 0x800b_4780,
        buffered: 0x800b_47b0,
        started: 0x800b_47b4,
        transfer_address: 0x800b_47dc,
        io_mode: 0x800b_47e0,
        io_mode_2: 0x800b_47e4,
        align: 0x800b_47e8,
        address_shift: 0x800b_47ec,
        block: 0x800b_47f0,
        block_mask: 0x800b_47f4,
        transfer_done: 0x800b_47f8,
        transfer_callback: 0x800b_47fc,
        irq_callback: 0x800b_4800,
        init_block: 0x800b_4804,
        dma_read: 0x800b_4814,
        dma_source: 0x800b_4818,
        dma_blocks: 0x800b_481c,
        reserved_1: 0x800b_4824,
        reserved_2: 0x800b_4828,
        reserved_areas: 0x800b_482c,
        zero_buffer: 0x800b_4834,
        semitones: 0x800b_4c34,
        fine: 0x800b_4c4c,
        reverb_offsets: 0x800b_4d54,
        reverb_params: 0x800b_4d84,
        key_buffer: 0x800c_7ea0,
    };
    /// Every word, in declaration order (tests).
    pub fn words(&self) -> [u32; 34] {
        [
            self.keyed,
            self.transfer_mode,
            self.reverb_on,
            self.reverb_reserved,
            self.reverb_start,
            self.reverb,
            self.key_pending_on,
            self.key_pending,
            self.voice_note,
            self.buffered,
            self.started,
            self.transfer_address,
            self.io_mode,
            self.io_mode_2,
            self.align,
            self.address_shift,
            self.block,
            self.block_mask,
            self.transfer_done,
            self.transfer_callback,
            self.irq_callback,
            self.init_block,
            self.dma_read,
            self.dma_source,
            self.dma_blocks,
            self.reserved_1,
            self.reserved_2,
            self.reserved_areas,
            self.zero_buffer,
            self.semitones,
            self.fine,
            self.reverb_offsets,
            self.reverb_params,
            self.key_buffer,
        ]
    }
}

impl Default for LibSpu {
    fn default() -> Self {
        Self::PAL
    }
}

/// One store to a hardware register, as the original instruction at `pc`
/// performs it (`pc` 0 for writes made through [`SpuDevice::write_register`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RegisterWrite {
    pub pc: u32,
    /// Physical address (`0x1F80xxxx`).
    pub address: u32,
    pub value: u32,
    /// Store width in bytes.
    pub width: u8,
}

/// Output stage selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OutputStage {
    /// Documented hardware: CD input scaled by the CD volume registers, the
    /// mix scaled by the main volume, then saturated to -8000h..+7FFFh.
    #[default]
    Hardware,
    /// The pinned PCSX-Redux mixer used as the validation oracle: CD input
    /// and voices summed without the CD and main volume registers, then
    /// saturated to -7FFFh..+7FFFh.
    Redux,
}

/// Envelope phase of a voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Phase {
    #[default]
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Inspectable voice state (tests and tooling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceState {
    /// The voice produces samples (keyed on at least once and started).
    pub running: bool,
    /// Key-on accepted, first sample not yet produced.
    pub pending: bool,
    pub phase: Phase,
    pub level: i32,
    /// Byte address of the next ADPCM block.
    pub address: u32,
    pub loop_address: u32,
}

// Mathematical PSX SPU Gaussian interpolation ROM values (512 signed integers),
// the same hardware table as ac3-audio's (psx-spx).
const GAUSS: [i32; 512] = [
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1,
    1, 2, 2, 2, 3, 3, 3, 4, 4, 5, 5, 6, 7, 7, 8, 9, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 21,
    22, 24, 25, 27, 28, 30, 32, 33, 35, 37, 39, 41, 44, 46, 48, 51, 53, 56, 58, 61, 64, 67, 70, 73,
    77, 80, 84, 87, 91, 95, 99, 103, 107, 111, 116, 120, 125, 130, 135, 140, 145, 150, 156, 161,
    167, 173, 179, 186, 192, 199, 205, 212, 219, 227, 234, 242, 250, 257, 266, 274, 283, 291, 300,
    309, 319, 328, 338, 348, 358, 369, 379, 390, 401, 412, 424, 436, 448, 460, 473, 485, 498, 512,
    525, 539, 553, 567, 582, 597, 612, 627, 643, 659, 675, 692, 708, 726, 743, 761, 779, 797, 816,
    835, 854, 874, 894, 914, 935, 956, 977, 999, 1020, 1043, 1066, 1089, 1112, 1136, 1160, 1184,
    1209, 1234, 1260, 1286, 1312, 1339, 1366, 1394, 1422, 1450, 1479, 1508, 1537, 1567, 1598, 1628,
    1660, 1691, 1723, 1756, 1789, 1822, 1856, 1890, 1924, 1959, 1995, 2031, 2067, 2104, 2141, 2179,
    2217, 2256, 2295, 2334, 2374, 2415, 2456, 2497, 2539, 2582, 2624, 2668, 2712, 2756, 2801, 2846,
    2892, 2938, 2985, 3032, 3079, 3128, 3176, 3225, 3275, 3325, 3376, 3427, 3479, 3531, 3584, 3637,
    3691, 3745, 3799, 3855, 3910, 3967, 4023, 4081, 4138, 4197, 4255, 4315, 4374, 4435, 4495, 4557,
    4619, 4681, 4744, 4807, 4871, 4935, 5000, 5065, 5131, 5197, 5264, 5332, 5399, 5468, 5536, 5606,
    5676, 5746, 5817, 5888, 5959, 6032, 6104, 6177, 6251, 6325, 6400, 6475, 6550, 6626, 6702, 6779,
    6856, 6934, 7012, 7091, 7170, 7249, 7329, 7409, 7490, 7571, 7653, 7735, 7817, 7900, 7983, 8066,
    8150, 8234, 8319, 8404, 8489, 8575, 8661, 8748, 8834, 8922, 9009, 9097, 9185, 9273, 9362, 9451,
    9541, 9630, 9720, 9811, 9901, 9992, 10083, 10174, 10266, 10358, 10450, 10542, 10635, 10727,
    10820, 10913, 11007, 11100, 11194, 11288, 11382, 11476, 11571, 11665, 11760, 11855, 11950,
    12045, 12140, 12236, 12331, 12427, 12522, 12618, 12714, 12809, 12905, 13001, 13097, 13193,
    13289, 13385, 13481, 13577, 13673, 13769, 13865, 13961, 14056, 14152, 14248, 14343, 14439,
    14534, 14630, 14725, 14820, 14915, 15010, 15104, 15199, 15293, 15387, 15481, 15575, 15669,
    15762, 15855, 15948, 16041, 16133, 16226, 16317, 16409, 16500, 16592, 16682, 16773, 16863,
    16953, 17042, 17131, 17220, 17308, 17396, 17484, 17571, 17658, 17744, 17830, 17916, 18001,
    18086, 18170, 18254, 18337, 18420, 18502, 18584, 18665, 18746, 18826, 18905, 18985, 19063,
    19141, 19219, 19295, 19372, 19447, 19522, 19597, 19671, 19744, 19816, 19888, 19959, 20030,
    20100, 20169, 20238, 20306, 20373, 20439, 20505, 20570, 20634, 20698, 20760, 20822, 20884,
    20944, 21004, 21063, 21121, 21178, 21235, 21290, 21345, 21399, 21452, 21505, 21556, 21607,
    21657, 21706, 21754, 21801, 21848, 21893, 21938, 21982, 22025, 22066, 22107, 22148, 22187,
    22225, 22262, 22299, 22334, 22369, 22402, 22435, 22467, 22498, 22527, 22556, 22584, 22611,
    22637, 22662, 22686, 22709, 22731, 22752, 22772, 22791, 22809, 22826, 22842, 22857, 22872,
    22885, 22897, 22908, 22918, 22927, 22935, 22942, 22948, 22953, 22957, 22960, 22962, 22963,
];

/// SPU ADPCM prediction filters (psx-spx).
const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

/// Guest memory as the device sees it: RAM (2 MiB mirrors) and scratchpad.
struct Guest<'a> {
    ram: &'a mut [u8],
    scratch: &'a mut [u8],
    /// The release's libspu words.
    libspu: LibSpu,
}

impl Guest<'_> {
    fn byte(&mut self, address: u32) -> &mut u8 {
        let physical = address & 0x1fff_ffff;
        if (0x1f80_0000..0x1f80_0400).contains(&physical) {
            let len = self.scratch.len();
            &mut self.scratch[(physical as usize - 0x1f80_0000) % len.max(1)]
        } else {
            let len = self.ram.len();
            &mut self.ram[(physical as usize & 0x1f_ffff) % len.max(1)]
        }
    }
    fn lbu(&mut self, address: u32) -> u32 {
        u32::from(*self.byte(address))
    }
    fn lhu(&mut self, address: u32) -> u32 {
        self.lbu(address) | (self.lbu(address.wrapping_add(1)) << 8)
    }
    fn lh(&mut self, address: u32) -> i32 {
        i32::from(self.lhu(address) as u16 as i16)
    }
    fn lw(&mut self, address: u32) -> u32 {
        self.lhu(address) | (self.lhu(address.wrapping_add(2)) << 16)
    }
    fn sb(&mut self, address: u32, value: u32) {
        *self.byte(address) = value as u8;
    }
    fn sh(&mut self, address: u32, value: u32) {
        self.sb(address, value);
        self.sb(address.wrapping_add(1), value >> 8);
    }
    fn sw(&mut self, address: u32, value: u32) {
        self.sh(address, value);
        self.sh(address.wrapping_add(2), value >> 16);
    }
}

/// ADSR envelope generator (psx-spx rate formula; the exponential-increase
/// knee adds 8 to the rate at level 6000h and above, rate counters carry
/// across phase changes, exponential decreases floor `step * level >> 15`).
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct Envelope {
    phase: Phase,
    level: i32,
    counter: u32,
}

fn rate_step(rate: u32, increase: bool) -> i32 {
    let base = if increase {
        7 - (rate & 3) as i32
    } else {
        -8 + (rate & 3) as i32
    };
    base << 11u32.saturating_sub(rate >> 2)
}

impl Envelope {
    fn step_due(&mut self, rate: u32) -> bool {
        let period = 1u32 << (rate >> 2).saturating_sub(11).min(31);
        self.counter = self.counter.wrapping_add(1);
        if self.counter < period {
            false
        } else {
            self.counter = 0;
            true
        }
    }
    fn increase(&mut self, rate: u32, exponential: bool) {
        let rate = rate
            + if exponential && self.level >= 0x6000 {
                8
            } else {
                0
            };
        if self.step_due(rate) {
            self.level = (self.level + rate_step(rate, true)).min(0x7fff);
        }
    }
    fn decrease(&mut self, rate: u32, exponential: bool) {
        if self.step_due(rate) {
            let step = rate_step(rate, false);
            let delta = if exponential {
                (step * self.level) >> 15
            } else {
                step
            };
            self.level = (self.level + delta).max(0);
        }
    }
    /// One 44.1 kHz envelope tick; returns the level after it.
    fn tick(&mut self, adsr1: u16, adsr2: u16) -> i32 {
        let (adsr1, adsr2) = (u32::from(adsr1), u32::from(adsr2));
        match self.phase {
            Phase::Attack => {
                self.increase((adsr1 >> 8) & 0x7f, adsr1 & 0x8000 != 0);
                if self.level == 0x7fff {
                    self.phase = Phase::Decay;
                }
            }
            Phase::Decay => {
                self.decrease(((adsr1 >> 4) & 0xf) << 2, true);
                if (self.level >> 11) as u32 <= adsr1 & 0xf {
                    self.phase = Phase::Sustain;
                }
            }
            Phase::Sustain => {
                let rate = (adsr2 >> 6) & 0x7f;
                let exponential = adsr2 & 0x8000 != 0;
                if adsr2 & 0x4000 == 0 {
                    self.increase(rate, exponential);
                } else {
                    self.decrease(rate, exponential);
                }
            }
            Phase::Release => self.decrease((adsr2 & 0x1f) << 2, adsr2 & 0x20 != 0),
        }
        self.level
    }
}

/// Volume register stage (fixed volume or sweep, psx-spx). `level` is the
/// current volume in -8000h..+7FFFh; a fixed register holds level / 2.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct Volume {
    level: i32,
    counter: u32,
}

impl Volume {
    fn tick(&mut self, register: u16) -> i32 {
        let register = u32::from(register);
        if register & 0x8000 == 0 {
            // Bits 0..14: signed volume / 2.
            self.level = (((register << 17) as i32) >> 17) * 2;
            return self.level;
        }
        let exponential = register & 0x4000 != 0;
        let decrease = register & 0x2000 != 0;
        let negative = register & 0x1000 != 0;
        let rate = register & 0x7f;
        let mut magnitude = self.level.abs().min(0x7fff);
        let mut envelope = Envelope {
            phase: Phase::Sustain,
            level: magnitude,
            counter: self.counter,
        };
        if decrease {
            envelope.decrease(rate, exponential);
        } else {
            envelope.increase(rate, exponential);
        }
        self.counter = envelope.counter;
        magnitude = envelope.level;
        self.level = if negative { -magnitude } else { magnitude };
        self.level
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Voice {
    running: bool,
    pending: bool,
    delay: u8,
    current: u32,
    loop_address: u32,
    ignore_loop_flags: bool,
    end_pending: bool,
    repeat_pending: bool,
    decoded: [i32; 28],
    position: usize,
    history: [i32; 2],
    window: [i32; 4],
    counter: u32,
    envelope: Envelope,
    volume: [Volume; 2],
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            running: false,
            pending: false,
            delay: 0,
            current: 0,
            loop_address: 0,
            ignore_loop_flags: false,
            end_pending: false,
            repeat_pending: false,
            decoded: [0; 28],
            position: 28,
            history: [0; 2],
            window: [0; 4],
            counter: 0,
            envelope: Envelope::default(),
            volume: [Volume::default(); 2],
        }
    }
}

impl Voice {
    /// The ENVX register: the envelope level (0 for a fresh key-on until
    /// its first envelope step, as the original reads it).
    fn envx(&self) -> u16 {
        self.envelope.level as u16
    }
}

/// The SPU device: register file, sound RAM, voices and mixer. Save states
/// serialize the whole device.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SpuDevice {
    sound_ram: Vec<u8>,
    #[serde(with = "crate::savestate::array")]
    registers: [u16; 0x200],
    voices: [Voice; 24],
    transfer_address: u32,
    fifo: Vec<u16>,
    end_flags: u32,
    noise_level: u16,
    noise_timer: i32,
    main_volume: [Volume; 2],
    capture_index: u32,
    cd_input: VecDeque<[i16; 2]>,
    /// Output produced inside host calls, returned first by `mix`.
    ahead: VecDeque<[i16; 2]>,
    /// CdMix attenuation (L→L, L→R, R→R, R→L) last applied to the CD-ROM.
    cd_attenuation: [u8; 4],
    cd_attenuation_pending: [u8; 4],
    cdrom_index: u8,
    field_cycles: u64,
    field_samples: u64,
    samples: u64,
    log: Option<Vec<RegisterWrite>>,
    /// Output stage (hardware by default).
    pub output: OutputStage,
    /// The release: its libspu words and its field length (PAL by default;
    /// [`Self::set_release`]). Configuration, not device state.
    #[serde(skip)]
    libspu: LibSpu,
    #[serde(skip)]
    field: FieldLength,
}

impl Default for SpuDevice {
    fn default() -> Self {
        Self::new()
    }
}

/// `true` for the host calls [`SpuDevice`] answers.
pub fn handles(function: u32) -> bool {
    use calls::*;
    matches!(
        function,
        SPU_INIT
            | SPU_WRITE_PIO
            | SPU_TRANSFER
            | SPU_SET_REGISTER
            | SPU_SET_REVERB
            | SPU_SET_REVERB_MODE_PARAM
            | SPU_SET_REVERB_DEPTH
            | SPU_SET_ANY_VOICE
            | SPU_SET_KEY
            | SPU_SET_COMMON_ATTR
            | SPU_GET_ALL_KEYS_STATUS
            | SPU_KEY_STATUS
            | SPU_SET_VOICE_ATTR
            | CD_MIX
            | SPU_READ_DMA
            | SPU_CAPTURE_STATUS
    )
}

impl SpuDevice {
    /// Power-on state: zero registers and sound RAM, voices idle.
    pub fn new() -> Self {
        Self {
            sound_ram: vec![0; SPU_RAM_BYTES],
            registers: [0; 0x200],
            voices: [Voice::default(); 24],
            transfer_address: 0,
            fifo: Vec::new(),
            end_flags: 0,
            noise_level: 1,
            noise_timer: 0,
            main_volume: [Volume::default(); 2],
            capture_index: 0,
            cd_input: VecDeque::new(),
            ahead: VecDeque::new(),
            cd_attenuation: [0; 4],
            cd_attenuation_pending: [0; 4],
            cdrom_index: 0,
            field_cycles: 0,
            field_samples: 0,
            samples: 0,
            log: None,
            output: OutputStage::Hardware,
            libspu: LibSpu::PAL,
            field: FieldLength::PAL,
        }
    }

    /// Answer for the release whose libspu words are `libspu`, mixing one
    /// field of `field` per delivered VBlank. A save state's device takes
    /// the running one's release ([`Self::release`]).
    pub fn set_release(&mut self, libspu: LibSpu, field: FieldLength) {
        self.libspu = libspu;
        self.field = field;
    }
    /// The release set by [`Self::set_release`] (PAL by default).
    pub fn release(&self) -> (LibSpu, FieldLength) {
        (self.libspu, self.field)
    }

    /// Sound RAM (512 KiB).
    pub fn sound_ram(&self) -> &[u8] {
        &self.sound_ram
    }
    /// Register `offset` (from [`SPU_BASE`]) as last written by the CPU.
    pub fn register(&self, offset: u32) -> u16 {
        self.registers[(offset as usize >> 1) & 0x1ff]
    }
    /// Register `offset` as a CPU read returns it: ENVX is the envelope,
    /// ENDX the end flags, STATUS mirrors the control mode bits, the current
    /// volumes are the volume stages; everything else reads back as written.
    pub fn read_register(&self, offset: u32) -> u16 {
        let offset = offset & 0x3fe;
        match offset {
            0..0x180 if offset & 0xf == reg::VOICE_ENVX => self.voices[offset as usize >> 4].envx(),
            reg::END => self.end_flags as u16,
            0x19e => (self.end_flags >> 16) as u16,
            reg::STATUS => self.register(reg::CONTROL) & 0x3f,
            reg::CURRENT_MAIN_VOLUME_LEFT => self.main_volume[0].level as u16,
            reg::CURRENT_MAIN_VOLUME_RIGHT => self.main_volume[1].level as u16,
            0x200..0x260 => {
                let voice = &self.voices[(offset as usize - 0x200) >> 2];
                voice.volume[(offset as usize >> 1) & 1].level as u16
            }
            _ => self.register(offset),
        }
    }
    /// Envelope, decoder and key state of `voice`.
    pub fn voice_state(&self, voice: usize) -> VoiceState {
        let v = &self.voices[voice];
        VoiceState {
            running: v.running,
            pending: v.pending,
            phase: v.envelope.phase,
            level: v.envelope.level,
            address: v.current,
            loop_address: v.loop_address,
        }
    }
    /// CdMix attenuation (L→L, L→R, R→R, R→L) the CD-ROM applies to its
    /// audio output, for the host's XA decoder (`XaDecoder::set_volume_matrix`).
    pub fn cd_attenuation(&self) -> [u8; 4] {
        self.cd_attenuation
    }
    /// Output samples produced since creation.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Replace sound RAM and the register file with a saved image (e.g. a
    /// Redux save state's `spu.ram` / `spu.ports`) without side effects;
    /// voices stay idle. The transfer pointer follows the transfer address.
    pub fn load_state(&mut self, sound_ram: &[u8], ports: &[u8]) {
        let n = sound_ram.len().min(SPU_RAM_BYTES);
        self.sound_ram[..n].copy_from_slice(&sound_ram[..n]);
        for (i, pair) in ports.chunks_exact(2).take(0x200).enumerate() {
            self.registers[i] = u16::from_le_bytes([pair[0], pair[1]]);
        }
        self.transfer_address = u32::from(self.register(reg::TRANSFER_ADDRESS)) << 3;
        for (v, voice) in self.voices.iter_mut().enumerate() {
            voice.loop_address = u32::from(self.registers[v * 8 + 7]) << 3;
        }
    }

    /// Record every register store from now on ([`take_writes`](Self::take_writes)).
    /// Data-port (FIFO) halfwords are not recorded; compare sound RAM instead.
    pub fn set_logging(&mut self, enabled: bool) {
        self.log = enabled.then(Vec::new);
    }
    pub fn take_writes(&mut self) -> Vec<RegisterWrite> {
        self.log.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Queue CD audio input at 44.1 kHz: frames after the CD-ROM's
    /// attenuation matrix ([`cd_attenuation`](Self::cd_attenuation)), as
    /// `ac3_audio::xa::XaDecoder` emits them. One frame is consumed per
    /// output sample; an empty queue inputs silence.
    pub fn push_cd_audio(&mut self, pcm: &[[i16; 2]]) {
        self.cd_input.extend(pcm.iter().copied());
    }
    /// CD input frames queued and not yet mixed.
    pub fn cd_audio_queued(&self) -> usize {
        self.cd_input.len()
    }

    /// Mix `frames` output samples (44.1 kHz stereo). Samples the device
    /// already produced inside a host call (see [`advance`](Self::advance))
    /// come first.
    pub fn mix(&mut self, frames: usize) -> Vec<[i16; 2]> {
        let early = frames.min(self.ahead.len());
        let mut pcm: Vec<[i16; 2]> = self.ahead.drain(..early).collect();
        pcm.extend((early..frames).map(|_| self.tick()));
        pcm
    }
    /// Let `samples` of SPU time pass inside a host call (the original's
    /// busy-wait while the SPU works); the output is kept for the next
    /// [`mix`](Self::mix).
    fn advance(&mut self, samples: usize) {
        for _ in 0..samples {
            let frame = self.tick();
            self.ahead.push_back(frame);
        }
    }
    /// Mix the samples of one field of the release ([`FieldLength`] at
    /// [`CYCLES_PER_SAMPLE`]): PAL 881 or 882 (the Redux field of
    /// [`CYCLES_PER_PAL_FIELD`]), NTSC 735 or 736 (735.735 on average).
    pub fn mix_field(&mut self) -> Vec<[i16; 2]> {
        // `field_cycles` counts in units of 1/`per` cycle (PAL: cycles).
        self.field_cycles += self.field.cycles;
        let due = self.field_cycles / (CYCLES_PER_SAMPLE * self.field.per);
        let frames = (due - self.field_samples) as usize;
        self.field_samples = due;
        self.mix(frames)
    }

    // ---- register file -----------------------------------------------------------

    fn record(&mut self, pc: u32, address: u32, value: u32, width: u8) {
        if let Some(log) = &mut self.log {
            log.push(RegisterWrite {
                pc,
                address,
                value,
                width,
            });
        }
    }

    /// A CPU halfword store to SPU register `offset`, with its hardware
    /// effect (key on/off, transfer pointer, data port, control).
    pub fn write_register(&mut self, offset: u32, value: u16) {
        self.store(0, offset, value);
    }

    fn store(&mut self, pc: u32, offset: u32, value: u16) {
        let offset = offset & 0x3fe;
        if offset != reg::TRANSFER_FIFO {
            self.record(pc, SPU_BASE + offset, u32::from(value), 2);
        }
        self.registers[offset as usize >> 1] = value;
        match offset {
            0..0x180 if offset & 0xf == reg::VOICE_REPEAT => {
                let voice = &mut self.voices[offset as usize >> 4];
                voice.loop_address = u32::from(value) << 3;
                voice.ignore_loop_flags = true;
            }
            reg::KEY_ON => self.key_on(u32::from(value)),
            0x18a => self.key_on(u32::from(value & 0xff) << 16),
            reg::KEY_OFF => self.key_off(u32::from(value)),
            0x18e => self.key_off(u32::from(value & 0xff) << 16),
            reg::TRANSFER_ADDRESS => self.transfer_address = u32::from(value) << 3,
            reg::TRANSFER_FIFO => {
                if self.fifo.len() < 32 {
                    self.fifo.push(value);
                }
            }
            // Manual write: the FIFO goes to sound RAM.
            reg::CONTROL if (value >> 4) & 3 == 1 => {
                for half in std::mem::take(&mut self.fifo) {
                    self.write_sound_ram(self.transfer_address, &half.to_le_bytes());
                    self.transfer_address = (self.transfer_address + 2) & SPU_RAM_MASK;
                }
            }
            _ => {}
        }
    }

    /// Key on: the voice is silent from the next sample, its envelope reads
    /// 0, and it starts [`KEY_ON_LATENCY`] samples later (a key-off in
    /// between is overridden by the start, as in the oracle).
    fn key_on(&mut self, bits: u32) {
        for (v, voice) in self.voices.iter_mut().enumerate() {
            if bits & (1 << v) != 0 {
                voice.pending = true;
                voice.delay = KEY_ON_LATENCY;
                voice.ignore_loop_flags = false;
                voice.envelope.level = 0;
                self.end_flags &= !(1 << v);
            }
        }
    }

    fn key_off(&mut self, bits: u32) {
        for (v, voice) in self.voices.iter_mut().enumerate() {
            if bits & (1 << v) != 0 && voice.running {
                voice.envelope.phase = Phase::Release;
            }
        }
    }

    fn write_sound_ram(&mut self, address: u32, bytes: &[u8]) {
        for (i, &byte) in bytes.iter().enumerate() {
            self.sound_ram[((address as usize) + i) & SPU_RAM_MASK as usize] = byte;
        }
    }

    // ---- libspu routines ------------------------------------------------------------

    /// One libspu host call (see [`calls`]); `None` for calls whose result
    /// the original does not produce here or that are not SPU calls.
    pub fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        let mut g = Guest {
            ram,
            scratch,
            libspu: self.libspu,
        };
        let [a0, a1, a2, a3] = call.args;
        match call.function {
            calls::SPU_INIT => {
                self.spu_init(&mut g);
                None
            }
            calls::SPU_WRITE_PIO => {
                self.write_pio(&mut g, a0, a1);
                None
            }
            calls::SPU_TRANSFER => {
                self.transfer(&mut g, a0, a1, a2);
                None
            }
            calls::SPU_SET_REGISTER => {
                self.store(0x8009_e090, a0 << 1, a1 as u16);
                None
            }
            calls::SPU_SET_REVERB => {
                let control = self.register(reg::CONTROL);
                let value = if a1 != 0 {
                    control | 0x80
                } else {
                    control & 0xff7f
                };
                self.store(0x8009_e368, reg::CONTROL, value);
                None
            }
            calls::SPU_SET_REVERB_MODE_PARAM => Some(self.set_reverb_mode_param(&mut g, a0)),
            calls::SPU_SET_REVERB_DEPTH => {
                let mask = g.lw(a0);
                if mask == 0 || mask & 2 != 0 {
                    let left = g.lhu(a0.wrapping_add(8));
                    self.store(0x8009_ee74, reg::REVERB_VOLUME_LEFT, left as u16);
                }
                if mask == 0 || mask & 4 != 0 {
                    let right = g.lhu(a0.wrapping_add(0xa));
                    self.store(0x8009_eea8, reg::REVERB_VOLUME_RIGHT, right as u16);
                }
                None
            }
            calls::SPU_SET_ANY_VOICE => Some(self.set_any_voice(a0, a1, a2, a3)),
            calls::SPU_SET_KEY => {
                let bits = a1 & 0x00ff_ffff;
                match a0 {
                    1 => {
                        self.store(0x8009_f4a0, reg::KEY_ON, bits as u16);
                        self.store(0x8009_f4a8, 0x18a, (bits >> 16) as u16);
                    }
                    0 => {
                        self.store(0x8009_f55c, reg::KEY_OFF, bits as u16);
                        self.store(0x8009_f560, 0x18e, (bits >> 16) as u16);
                    }
                    _ => {}
                }
                None
            }
            calls::SPU_SET_COMMON_ATTR => {
                self.set_common_attr(&mut g, a0);
                None
            }
            calls::SPU_GET_ALL_KEYS_STATUS => None,
            calls::SPU_KEY_STATUS => Some(self.key_status(a1, a2 != 0)),
            calls::SPU_SET_VOICE_ATTR => {
                self.set_voice_attr(&mut g, a0);
                None
            }
            calls::CD_MIX => {
                self.cd_mix(&mut g, a0);
                Some(1)
            }
            calls::SPU_READ_DMA => {
                self.read_dma(&mut g, a0, a1, a2);
                None
            }
            calls::SPU_CAPTURE_STATUS => Some(u32::from(self.capture_status())),
            _ => None,
        }
    }

    /// 8009FCDC/8009FD4C: the status byte of `voice`: keyed (800B25D0) with
    /// a nonzero ENVX 1, keyed and silent 3, released and sounding 2, else 0.
    fn key_status(&self, voice: u32, keyed: bool) -> u32 {
        let sounding = self.read_register((voice & 0x1f) * 16 + reg::VOICE_ENVX) != 0;
        match (keyed, sounding) {
            (true, true) => 1,
            (true, false) => 3,
            (false, true) => 2,
            (false, false) => 0,
        }
    }

    /// 8009E218 SpuInit → 8009D548 `_SpuInit(0)`: 8009D6A8 `_spu_init(0)`
    /// (registers, the 16-byte block at SPU 0x1000, all voices keyed on and
    /// off at volume 0), the 24 sample notes 0xC000, 8009D630 SpuStart's
    /// flag, the reverb record and work area (mode 0), and the libspu flags.
    /// The kernel/libetc parts (ResetCallback, DMACallback, OpenEvent) are
    /// not device state and are not reproduced.
    fn spu_init(&mut self, g: &mut Guest) {
        // _spu_init(0)
        g.sw(g.libspu.io_mode, 0);
        g.sw(g.libspu.io_mode_2, 0);
        g.sh(g.libspu.transfer_address, 0);
        self.store(0x8009_d6f4, reg::MAIN_VOLUME_LEFT, 0);
        self.store(0x8009_d6f8, reg::MAIN_VOLUME_RIGHT, 0);
        self.store(0x8009_d700, reg::CONTROL, 0);
        self.store(0x8009_d710, reg::MAIN_VOLUME_LEFT, 0);
        self.store(0x8009_d714, reg::MAIN_VOLUME_RIGHT, 0);
        g.sw(g.libspu.align, 2);
        g.sw(g.libspu.address_shift, 3);
        g.sw(g.libspu.block, 8);
        g.sw(g.libspu.block_mask, 7);
        self.store(0x8009_d7c0, reg::TRANSFER_CONTROL, 4);
        self.store(0x8009_d7c8, reg::REVERB_VOLUME_LEFT, 0);
        self.store(0x8009_d7cc, reg::REVERB_VOLUME_RIGHT, 0);
        self.store(0x8009_d7d0, reg::KEY_OFF, 0xffff);
        self.store(0x8009_d7d4, 0x18e, 0xffff);
        self.store(0x8009_d7d8, reg::REVERB_ON, 0);
        self.store(0x8009_d7dc, 0x19a, 0);
        for i in 0..10 {
            g.sh(g.libspu.key_buffer + i * 2, 0);
        }
        g.sh(g.libspu.transfer_address, 0x200);
        for (pc, offset) in [
            (0x8009_d818, reg::PITCH_MODULATION),
            (0x8009_d81c, 0x192),
            (0x8009_d820, reg::NOISE),
            (0x8009_d824, 0x196),
            (0x8009_d828, reg::CD_VOLUME_LEFT),
            (0x8009_d82c, reg::CD_VOLUME_RIGHT),
            (0x8009_d830, reg::EXTERN_VOLUME_LEFT),
            (0x8009_d834, reg::EXTERN_VOLUME_RIGHT),
        ] {
            self.store(pc, offset, 0);
        }
        let block = g.libspu.init_block;
        self.write_pio(g, block, 0x10);
        for v in 0..24u32 {
            let base = v * 16;
            self.store(0x8009_d858, base + reg::VOICE_VOLUME_LEFT, 0);
            self.store(0x8009_d85c, base + reg::VOICE_VOLUME_RIGHT, 0);
            self.store(0x8009_d860, base + reg::VOICE_PITCH, 0x3fff);
            self.store(0x8009_d864, base + reg::VOICE_START, 0x200);
            self.store(0x8009_d868, base + reg::VOICE_ADSR1, 0);
            self.store(0x8009_d86c, base + reg::VOICE_ADSR2, 0);
        }
        self.store(0x8009_d890, reg::KEY_ON, 0xffff);
        self.store(0x8009_d898, 0x18a, 0xff);
        // 8009D894..8009D8B0: four `_spu_Fw1ts` waits; the SPU starts the voices.
        self.advance(usize::from(KEY_ON_LATENCY) + 1);
        self.store(0x8009_d8c0, reg::KEY_OFF, 0xffff);
        self.store(0x8009_d8c8, 0x18e, 0xff);
        // 8009D8C4..8009D8E0: four more waits; the voices release.
        self.advance(usize::from(KEY_ON_LATENCY) + 1);
        g.sw(g.libspu.transfer_done, 1);
        self.store(0x8009_d900, reg::CONTROL, 0xc000);
        g.sw(g.libspu.transfer_callback, 0);
        g.sw(g.libspu.irq_callback, 0);
        // _SpuInit(0)
        for v in 0..24 {
            g.sh(g.libspu.voice_note + v * 2, 0xc000);
        }
        if g.lw(g.libspu.started) == 0 {
            g.sw(g.libspu.started, 1);
        }
        let start = g.lw(g.libspu.reverb_offsets);
        g.sw(g.libspu.reverb_on, 0);
        g.sw(g.libspu.reverb_reserved, 0);
        g.sw(g.libspu.reverb, 0);
        g.sh(g.libspu.reverb + 4, 0);
        g.sh(g.libspu.reverb + 6, 0);
        g.sw(g.libspu.reverb + 8, 0);
        g.sw(g.libspu.reverb + 0xc, 0);
        g.sw(g.libspu.reverb_start, start);
        self.store(0x8009_dfd0, reg::REVERB_BASE, start as u16);
        for address in [
            g.libspu.reserved_1,
            g.libspu.reserved_2,
            g.libspu.reserved_areas,
            g.libspu.transfer_mode,
            g.libspu.io_mode,
            g.libspu.keyed,
            g.libspu.key_pending,
            g.libspu.key_pending_on,
            g.libspu.buffered,
        ] {
            g.sw(address, 0);
        }
    }

    /// 8009D928: programmed-I/O upload of `size` bytes from guest `source`
    /// to the transfer address (800B265C), in FIFO chunks of up to 64 bytes
    /// (whole halfwords, so an odd size sends one more byte).
    fn write_pio(&mut self, g: &mut Guest, source: u32, size: u32) {
        let address = g.lhu(g.libspu.transfer_address);
        self.store(0x8009_d95c, reg::TRANSFER_ADDRESS, address as u16);
        let mut remaining = size;
        let mut source = source;
        while remaining != 0 {
            let chunk = remaining.min(0x40);
            let mut sent = 0;
            while sent < chunk {
                let half = g.lhu(source);
                self.store(0x8009_d998, reg::TRANSFER_FIFO, half as u16);
                source = source.wrapping_add(2);
                sent += 2;
            }
            let control = self.register(reg::CONTROL);
            self.store(0x8009_d9c8, reg::CONTROL, (control & 0xffcf) | 0x10);
            remaining -= chunk;
        }
        let control = self.register(reg::CONTROL);
        self.store(0x8009_da68, reg::CONTROL, control & 0xffcf);
    }

    /// 8009DC4C `_spu_t` register work (the libspu RAM is `sdk::spu_t`'s):
    /// 2 sets the transfer address register from 800B265C, 1/0 select DMA
    /// write/read in SPUCNT, 3 runs the channel-4 DMA of `size` bytes (in
    /// 64-byte blocks) from/to guest `value`, then the completion handler
    /// 8009DAE8 clears the SPUCNT transfer mode.
    fn transfer(&mut self, g: &mut Guest, op: u32, value: u32, size: u32) {
        match op {
            2 => {
                let units = g.lhu(g.libspu.transfer_address);
                self.store(0x8009_dccc, reg::TRANSFER_ADDRESS, units as u16);
            }
            1 => {
                let control = self.register(reg::CONTROL);
                self.store(0x8009_dd38, reg::CONTROL, (control & 0xffcf) | 0x20);
            }
            0 => {
                let control = self.register(reg::CONTROL);
                self.store(0x8009_dda0, reg::CONTROL, control | 0x30);
            }
            3 => {
                let read = g.lw(g.libspu.dma_read) == 1;
                let blocks = (size >> 6) + u32::from(size & 0x3f != 0);
                self.record(0x8009_de6c, io::DMA4_MADR, value, 4);
                self.record(0x8009_de88, io::DMA4_BCR, (blocks << 16) | 0x10, 4);
                let chcr = if read { 0x0100_0200 } else { 0x0100_0201 };
                self.record(0x8009_deb4, io::DMA4_CHCR, chcr, 4);
                let bytes = blocks * 64;
                let mut address = value & 0x1f_fffc;
                for _ in 0..bytes / 2 {
                    if read {
                        let at = self.transfer_address as usize;
                        let half = u32::from(self.sound_ram[at])
                            | (u32::from(self.sound_ram[(at + 1) & SPU_RAM_MASK as usize]) << 8);
                        g.sh(address, half);
                    } else {
                        let half = g.lhu(address) as u16;
                        self.write_sound_ram(self.transfer_address, &half.to_le_bytes());
                    }
                    address = address.wrapping_add(2);
                    self.transfer_address = (self.transfer_address + 2) & SPU_RAM_MASK;
                }
                let control = self.register(reg::CONTROL);
                self.store(0x8009_db1c, reg::CONTROL, control & 0xffcf);
            }
            _ => {}
        }
    }

    /// 8009DBA4 `_spu_Fr_(buf, spu, blocks)` register work (its libspu RAM,
    /// the DMA-read flag 800B2694, is `sdk::spu_fr`'s): the transfer address
    /// register, SPUCNT's DMA-read mode, the channel-4 DMA of `blocks`
    /// 64-byte blocks from sound RAM into guest `buf` (the capture buffers
    /// for SpuReadDecodedData: CD left/right at 0..0x7FF, voices 1/3 at
    /// 0x800..0xFFF), then the completion handler 8009DAE8 clears the SPUCNT
    /// transfer mode. 8009E158's DMA priority (DPCR) write is not modelled.
    fn read_dma(&mut self, g: &mut Guest, buf: u32, spu: u32, blocks: u32) {
        self.store(0x8009_dbc0, reg::TRANSFER_ADDRESS, spu as u16);
        let control = self.register(reg::CONTROL);
        self.store(0x8009_dbe4, reg::CONTROL, control | 0x30);
        self.record(0x8009_dc0c, io::DMA4_MADR, buf, 4);
        self.record(0x8009_dc1c, io::DMA4_BCR, (blocks << 16) | 0x10, 4);
        self.record(0x8009_dc34, io::DMA4_CHCR, 0x0100_0200, 4);
        let mut address = buf & 0x1f_fffc;
        for _ in 0..blocks * 32 {
            let at = self.transfer_address as usize;
            let half = u32::from(self.sound_ram[at])
                | (u32::from(self.sound_ram[(at + 1) & SPU_RAM_MASK as usize]) << 8);
            g.sh(address, half);
            address = address.wrapping_add(2);
            self.transfer_address = (self.transfer_address + 2) & SPU_RAM_MASK;
        }
        let control = self.register(reg::CONTROL);
        self.store(0x8009_db1c, reg::CONTROL, control & 0xffcf);
    }

    /// SPUSTAT as SpuReadDecodedData reads it: the mode bits
    /// ([`read_register`](Self::read_register)) and bit 11, set while the
    /// voice/CD capture writes the second half of its 0x400-byte buffers.
    pub fn capture_status(&self) -> u16 {
        self.read_register(reg::STATUS) | (((self.capture_index & 0x100) << 3) as u16)
    }

    /// `_spu_t` with its libspu RAM (as `sdk::spu_t`) and register work, for
    /// the work-area clear inside SpuSetReverbModeParam.
    fn transfer_with_state(&mut self, g: &mut Guest, op: u32, value: u32, size: u32) {
        let shift = g.lw(g.libspu.address_shift) & 31;
        match op {
            0 | 1 => g.sw(g.libspu.dma_read, u32::from(op == 0)),
            2 => g.sh(g.libspu.transfer_address, value >> shift),
            3 => {
                g.sw(g.libspu.dma_source, value);
                g.sw(
                    g.libspu.dma_blocks,
                    (size >> 6) + u32::from(size & 0x3f != 0),
                );
            }
            _ => return,
        }
        self.transfer(g, op, value, size);
    }

    /// 8009E408 `_SpuIsInAllocateArea_(start)`.
    fn in_reserved_area(g: &mut Guest, start: u32) -> bool {
        let start = start.wrapping_shl(g.lw(g.libspu.address_shift));
        let mut area = g.lw(g.libspu.reserved_areas);
        if area == 0 {
            return false;
        }
        loop {
            let word = g.lw(area);
            if word & 0x8000_0000 == 0 {
                if word & 0x4000_0000 != 0 {
                    return false;
                }
                let base = word & 0x0fff_ffff;
                if base >= start || start < base.wrapping_add(g.lw(area.wrapping_add(4))) {
                    return true;
                }
            }
            area = area.wrapping_add(8);
        }
    }

    /// 8009F1B8 SpuClearReverbWorkArea(mode) as SpuSetReverbModeParam calls
    /// it: DMA zeros over the mode's work area in 0x400-byte chunks (the
    /// kernel event waits are not modelled). Returns 0, or -1.
    fn clear_reverb_work_area(&mut self, g: &mut Guest, mode: u32) -> u32 {
        if mode >= 10 {
            return u32::MAX;
        }
        let start = g.lw(g.libspu.reverb_offsets + mode * 4);
        if Self::in_reserved_area(g, start) {
            return u32::MAX;
        }
        let shift = g.lw(g.libspu.address_shift);
        let (mut remaining, mut address) = if mode == 0 {
            (0x10u32.wrapping_shl(shift), 0xfff0u32.wrapping_shl(shift))
        } else {
            (
                0x1_0000u32.wrapping_sub(start).wrapping_shl(shift),
                start.wrapping_shl(shift),
            )
        };
        let io_mode = g.lw(g.libspu.io_mode);
        if io_mode == 1 {
            g.sw(g.libspu.io_mode, 0);
        }
        let callback = g.lw(g.libspu.transfer_callback);
        if callback != 0 {
            g.sw(g.libspu.transfer_callback, 0);
        }
        loop {
            let (chunk, more) = if remaining < 0x401 {
                (remaining, false)
            } else {
                (0x400, true)
            };
            self.transfer_with_state(g, 2, address, 0);
            self.transfer_with_state(g, 1, 0, 0);
            let zeros = g.libspu.zero_buffer;
            self.transfer_with_state(g, 3, zeros, chunk);
            remaining = remaining.wrapping_sub(0x400);
            address = address.wrapping_add(0x400);
            if !more {
                break;
            }
        }
        if io_mode == 1 {
            g.sw(g.libspu.io_mode, io_mode);
        }
        if callback != 0 {
            g.sw(g.libspu.transfer_callback, callback);
        }
        0
    }

    /// 8009E498 SpuSetReverbModeParam(attr): attr +0 mask (0 = all), +4
    /// mode (bit 8: clear the work area), +8/+0xA depth, +0xC delay, +0x10
    /// feedback. A new mode (< 10, outside reserved areas, else -1) copies
    /// its 68-byte register set from 800B2C04, stores mode/start/delay/
    /// feedback at 800B25E8.., zeros the depth (registers and 800B25EC) and
    /// turns reverb off around the update; delay/feedback (modes 7 and 8)
    /// rewrite their registers; otherwise depth is written. Then 8009E978
    /// writes the register set, the work area is cleared on request and the
    /// work-area base (800B25E0) goes to mBASE. Returns 0 or -1.
    fn set_reverb_mode_param(&mut self, g: &mut Guest, attr: u32) -> u32 {
        let mask = g.lw(attr);
        let all = mask == 0;
        let mut params = [0u16; 32];
        let mut param_mask = 0u32;
        let load = |g: &mut Guest, mode: u32, params: &mut [u16; 32]| -> u32 {
            let base = g.libspu.reverb_params + mode.wrapping_mul(0x44);
            for (k, value) in params.iter_mut().enumerate() {
                *value = g.lhu(base + 4 + k as u32 * 2) as u16;
            }
            g.lw(base)
        };
        let mut clear = false;
        let mut mode_set = false;
        let mut delay_set = false;
        let mut feedback_set = false;
        if all || mask & 1 != 0 {
            let mut mode = g.lw(attr.wrapping_add(4));
            if mode & 0x100 != 0 {
                mode &= !0x100;
                clear = true;
            }
            if mode >= 10 {
                return u32::MAX;
            }
            let start = g.lw(g.libspu.reverb_offsets + mode * 4);
            if Self::in_reserved_area(g, start) {
                return u32::MAX;
            }
            mode_set = true;
            g.sw(g.libspu.reverb, mode);
            g.sw(g.libspu.reverb_start, start);
            param_mask = load(g, mode, &mut params);
            let (delay, feedback) = match mode {
                7 => (0x7f, 0x7f),
                8 => (0x7f, 0),
                _ => (0, 0),
            };
            g.sw(g.libspu.reverb + 0xc, feedback);
            g.sw(g.libspu.reverb + 8, delay);
        }
        let mode = g.lw(g.libspu.reverb) as i32;
        let echo = (7..9).contains(&mode);
        // `(x * 0x81020409 >> 32) + x >> 6`, minus the sign: x / 127.
        let div127 = |x: u32| -> u32 {
            let high = ((i64::from(x as i32) * i64::from(0x8102_0409u32 as i32)) >> 32) as i32;
            ((high.wrapping_add(x as i32) >> 6).wrapping_sub((x as i32) >> 31)) as u32
        };
        if (all || mask & 8 != 0) && echo {
            delay_set = true;
            if !mode_set {
                load(g, mode as u32, &mut params);
                param_mask = 0x0c01_1c00;
            }
            let delay = g.lw(attr.wrapping_add(0xc));
            g.sw(g.libspu.reverb + 8, delay);
            let long = div127(delay << 13);
            let short = div127(delay << 12);
            params[10] = long.wrapping_sub(u32::from(params[0])) as u16;
            params[11] = short.wrapping_sub(u32::from(params[1])) as u16;
            params[16] = u32::from(params[17]).wrapping_add(short) as u16;
            params[12] = u32::from(params[13]).wrapping_add(short) as u16;
            params[27] = u32::from(params[29]).wrapping_add(short) as u16;
            params[26] = u32::from(params[28]).wrapping_add(short) as u16;
        }
        if (all || mask & 0x10 != 0) && echo {
            feedback_set = true;
            if !mode_set {
                if delay_set {
                    param_mask |= 0x80;
                } else {
                    load(g, mode as u32, &mut params);
                    param_mask = 0x80;
                }
            }
            let feedback = g.lw(attr.wrapping_add(0x10));
            g.sw(g.libspu.reverb + 0xc, feedback);
            params[7] = div127(feedback.wrapping_mul(0x8100)) as u16;
        }
        let mut was_on = false;
        if mode_set {
            let control = self.register(reg::CONTROL);
            was_on = control & 0x80 != 0;
            if was_on {
                self.store(0x8009_e830, reg::CONTROL, control & 0xff7f);
            }
            self.store(0x8009_e8a4, reg::REVERB_VOLUME_LEFT, 0);
            self.store(0x8009_e8a8, reg::REVERB_VOLUME_RIGHT, 0);
            g.sh(g.libspu.reverb + 4, 0);
            g.sh(g.libspu.reverb + 6, 0);
        } else {
            if all || mask & 2 != 0 {
                let left = g.lhu(attr.wrapping_add(8));
                self.store(0x8009_e854, reg::REVERB_VOLUME_LEFT, left as u16);
                g.sh(g.libspu.reverb + 4, left);
            }
            if all || mask & 4 != 0 {
                let right = g.lhu(attr.wrapping_add(0xa));
                self.store(0x8009_e884, reg::REVERB_VOLUME_RIGHT, right as u16);
                g.sh(g.libspu.reverb + 6, right);
            }
        }
        if mode_set || delay_set || feedback_set {
            self.set_reverb_attr(param_mask, &params);
        }
        if clear {
            let mode = g.lw(g.libspu.reverb);
            self.clear_reverb_work_area(g, mode);
        }
        if mode_set {
            let start = g.lw(g.libspu.reverb_start);
            self.store(0x8009_dfd0, reg::REVERB_BASE, start as u16);
            if was_on {
                let control = self.register(reg::CONTROL);
                self.store(0x8009_e934, reg::CONTROL, control | 0x80);
            }
        }
        0
    }

    /// 8009E978 `_spu_setReverbAttr`: register k of the 32 reverb registers
    /// (1F801DC0 + 2k) when the mask is 0 or has bit k.
    fn set_reverb_attr(&mut self, mask: u32, params: &[u16; 32]) {
        for (k, &value) in params.iter().enumerate() {
            if mask == 0 || mask & (1 << k) != 0 {
                let k = k as u32;
                let pc = match k {
                    0..16 => 0x8009_e9a4 + 0x24 * k,
                    16..31 => 0x8009_ebe8 + 0x28 * (k - 16),
                    _ => 0x8009_ee3c,
                };
                self.store(pc, reg::REVERB_CONFIG + 2 * k, value);
            }
        }
    }

    /// 8009EEF8 `_SpuSetAnyVoice(on, voices, low, high)` (unbuffered): set
    /// (1), clear (0) or write (8) the 24-bit mask in registers `low`/`high`
    /// (halfword indices). Returns the old 24-bit value.
    fn set_any_voice(&mut self, on: u32, voices: u32, low: u32, high: u32) -> u32 {
        let (low, high) = ((low << 1) & 0x3fe, (high << 1) & 0x3fe);
        let old_low = self.register(low);
        let old_high = self.register(high);
        let old = u32::from(old_low) | ((u32::from(old_high) & 0xff) << 16);
        let upper = ((voices >> 16) & 0xff) as u16;
        match on {
            1 => {
                self.store(0x8009_f00c, low, old_low | voices as u16);
                self.store(0x8009_f020, high, old_high | upper);
            }
            0 => {
                self.store(0x8009_f0d8, low, old_low & !(voices as u16));
                self.store(0x8009_f0f0, high, old_high & !upper);
            }
            8 => {
                self.store(0x8009_f188, low, voices as u16);
                self.store(0x8009_f194, high, upper);
            }
            _ => {}
        }
        old
    }

    /// The 8009F8E0/8009F9A0 and 8009FEA4/8009FF84 volume-mode cases: the
    /// sweep bits of mode 1..7 (linear/exponential increase/decrease,
    /// inverted phase), 0 for a direct volume.
    fn sweep_bits(mode: i32) -> u32 {
        match mode {
            1..=7 => 0x8000 + 0x1000 * (mode as u32 - 1),
            _ => 0,
        }
    }

    /// A volume register value: direct `volume & 0x7FFF`, or a sweep mode's
    /// bits with the rate clamped to 0..0x7F.
    fn volume_value(volume: i32, sweep: u32) -> u16 {
        if sweep == 0 {
            (volume & 0x7fff) as u16
        } else {
            let rate = volume.clamp(0, 0x7f);
            ((rate as u32 & 0x7fff) | sweep) as u16
        }
    }

    /// 8009F888 SpuSetCommonAttr(attr): +0 mask (0 = all), +4/+6 main
    /// volume, +8/+A its modes, +10/+12 CD volume, +14 CD reverb, +18 CD
    /// mix, +1C/+1E external volume, +20 external reverb, +24 external mix.
    fn set_common_attr(&mut self, g: &mut Guest, attr: u32) {
        let mask = g.lw(attr);
        let all = mask == 0;
        for (side, pc) in [(0u32, 0x8009_f958), (1, 0x8009_fa18)] {
            if all || mask & (1 << side) != 0 {
                let sweep = if all || mask & (4 << side) != 0 {
                    let mode = g.lh(attr + 8 + side * 2);
                    if (0..8).contains(&mode) {
                        Self::sweep_bits(mode)
                    } else {
                        0
                    }
                } else {
                    0
                };
                let volume = g.lh(attr + 4 + side * 2);
                let offset = reg::MAIN_VOLUME_LEFT + side * 2;
                self.store(pc, offset, Self::volume_value(volume, sweep));
            }
        }
        for (bit, field, offset, pc) in [
            (0x40, 0x10, reg::CD_VOLUME_LEFT, 0x8009_fa3c),
            (0x80, 0x12, reg::CD_VOLUME_RIGHT, 0x8009_fa60),
            (0x400, 0x1c, reg::EXTERN_VOLUME_LEFT, 0x8009_fa84),
            (0x800, 0x1e, reg::EXTERN_VOLUME_RIGHT, 0x8009_faa8),
        ] {
            if all || mask & bit != 0 {
                let value = g.lhu(attr + field);
                self.store(pc, offset, value as u16);
            }
        }
        for (bit, field, control_bit, pc) in [
            (0x100, 0x14, 0x4, 0x8009_fafc),
            (0x200, 0x18, 0x1, 0x8009_fb50),
            (0x1000, 0x20, 0x8, 0x8009_fba4),
            (0x2000, 0x24, 0x2, 0x8009_fbf8),
        ] {
            if all || mask & bit != 0 {
                let control = self.register(reg::CONTROL);
                let value = if g.lw(attr + field) == 0 {
                    control & !control_bit
                } else {
                    control | control_bit
                };
                self.store(pc, reg::CONTROL, value);
            }
        }
    }

    /// 8009DFF8 `_spu_FsetRXXa(reg, address)`: the aligned address in SPU
    /// units (800B2668..800B2674) stored to register `reg`.
    fn set_address_register(&mut self, g: &mut Guest, index: u32, address: u32) {
        let mut address = address;
        if g.lw(g.libspu.align) != 0 {
            let block = g.lw(g.libspu.block);
            // The original's divu has a `break 7` for a zero block size.
            if block != 0 && !address.is_multiple_of(block) {
                address = address.wrapping_add(block) & !g.lw(g.libspu.block_mask);
            }
        }
        let value = address >> (g.lw(g.libspu.address_shift) & 31);
        self.store(0x8009_e090, index << 1, value as u16);
    }

    /// 800A0368 `_spu_note2pitch(center_note, center_fine, note, fine)`
    /// (the tables at 800B2AB4/800B2ACC), as `sdk::spu_note2pitch`.
    fn note_to_pitch(
        g: &mut Guest,
        center_note: u32,
        center_fine: u32,
        note: u32,
        fine: u32,
    ) -> u32 {
        let fine_sum = fine.wrapping_add(center_fine) & 0xffff;
        let semitones =
            (note.wrapping_add(fine_sum >> 7).wrapping_sub(center_note) << 16) as i32 >> 16;
        let octave = ((i64::from(semitones) * 0x2aaa_aaab) >> 33) as i32 - (semitones >> 31);
        let mut shift = octave.wrapping_sub(2);
        let mut semitone = semitones.wrapping_sub(octave.wrapping_mul(12));
        if (((semitone as u32) << 16) as i32) < 0 {
            semitone = semitone.wrapping_add(12);
            shift = octave.wrapping_sub(3);
        }
        let index = ((((semitone as u32) << 16) as i32) >> 15) as u32;
        let base = g.lhu(g.libspu.semitones.wrapping_add(index)) as i32;
        let tune = g.lhu(g.libspu.fine.wrapping_add((fine_sum & 0x7f) << 1)) as i32;
        let pitch = base.wrapping_mul(tune) >> 16;
        let shift = ((shift as u32) << 16) as i32 >> 16;
        let pitch = if shift >= 0 {
            0x3fff
        } else {
            let right = shift.wrapping_neg() as u32;
            let round = 1u32 << (right.wrapping_sub(1) & 31);
            (pitch as u32).wrapping_add(round) >> (right & 31)
        };
        pitch & 0xffff
    }

    /// 8009FD68 SpuSetVoiceAttr(attr) for every voice in `attr->voice`:
    /// mask (+4, 0 = all) bits 0x10 pitch (+14), 0x20 note (+16, pitch from
    /// the sample note: +18 when 0x40 is in the same call, else 800B2600),
    /// 1/2 volume (+8/+A) with 4/8 modes (+C/+E), 0x80 start address (+1C),
    /// 0x10000 loop address (+20), 0x20000/0x40000 ADSR1/2 (+3A/+3C), then
    /// the read-modify-write fields AR (+30, 0x100 mode +24), DR (+32), SR
    /// (+34, 0x200 mode +28), RR (+36, 0x400 mode +2C) and SL (+38).
    fn set_voice_attr(&mut self, g: &mut Guest, attr: u32) {
        let voices = g.lw(attr);
        let mask = g.lw(attr + 4);
        let all = mask == 0;
        let has = |bit: u32| all || mask & bit != 0;
        for v in 0..24u32 {
            if voices & (1 << v) == 0 {
                continue;
            }
            let base = v * 16;
            if has(0x10) {
                let pitch = g.lhu(attr + 0x14);
                self.store(0x8009_fde0, base + reg::VOICE_PITCH, pitch as u16);
            }
            if has(0x20) {
                let sample_note = if has(0x40) {
                    g.lhu(attr + 0x18)
                } else {
                    g.lhu(g.libspu.voice_note + v * 2)
                };
                let note = g.lhu(attr + 0x16);
                let pitch = Self::note_to_pitch(
                    g,
                    sample_note >> 8,
                    sample_note & 0xff,
                    note >> 8,
                    note & 0xff,
                );
                self.store(0x8009_fe3c, base + reg::VOICE_PITCH, pitch as u16);
            }
            for (side, table_pc) in [(0u32, 0x8009_ff1c), (1, 0x8009_fffc)] {
                if has(1 << side) {
                    let sweep = if has(4 << side) {
                        Self::sweep_bits(g.lh(attr + 0xc + side * 2))
                    } else {
                        0
                    };
                    let volume = g.lh(attr + 8 + side * 2);
                    let value = Self::volume_value(volume, sweep);
                    self.store(table_pc, base + side * 2, value);
                }
            }
            if has(0x80) {
                let address = g.lw(attr + 0x1c);
                self.set_address_register(g, v * 8 + 3, address);
            }
            if has(0x1_0000) {
                let address = g.lw(attr + 0x20);
                self.set_address_register(g, v * 8 + 7, address);
            }
            if has(0x2_0000) {
                let adsr1 = g.lhu(attr + 0x3a);
                self.store(0x800a_0064, base + reg::VOICE_ADSR1, adsr1 as u16);
            }
            if has(0x4_0000) {
                let adsr2 = g.lhu(attr + 0x3c);
                self.store(0x800a_0090, base + reg::VOICE_ADSR2, adsr2 as u16);
            }
            let adsr1 = base + reg::VOICE_ADSR1;
            let adsr2 = base + reg::VOICE_ADSR2;
            if has(0x800) {
                let rate = g.lhu(attr + 0x30).min(0x7f);
                let exponential = if has(0x100) && g.lw(attr + 0x24) == 5 {
                    0x80
                } else {
                    0
                };
                let old = u32::from(self.register(adsr1));
                let value = (old & 0xff) | ((rate | exponential) << 8);
                self.store(0x800a_010c, adsr1, value as u16);
            }
            if has(0x1000) {
                let rate = g.lhu(attr + 0x32).min(0xf);
                let old = u32::from(self.register(adsr1));
                self.store(0x800a_0158, adsr1, ((old & 0xff0f) | (rate << 4)) as u16);
            }
            if has(0x2000) {
                let rate = g.lhu(attr + 0x34).min(0x7f);
                let mode = if has(0x200) {
                    match g.lw(attr + 0x28) {
                        1 => 0,
                        5 => 0x200,
                        7 => 0x300,
                        _ => 0x100,
                    }
                } else {
                    0x100
                };
                let old = u32::from(self.register(adsr2));
                let value = (old & 0x3f) | ((rate | mode) << 6);
                self.store(0x800a_0210, adsr2, value as u16);
            }
            if has(0x4000) {
                let rate = g.lhu(attr + 0x36).min(0x1f);
                let exponential = if has(0x400) && g.lw(attr + 0x2c) == 7 {
                    0x20
                } else {
                    0
                };
                let old = u32::from(self.register(adsr2));
                self.store(
                    0x800a_028c,
                    adsr2,
                    ((old & 0xffc0) | rate | exponential) as u16,
                );
            }
            if has(0x8000) {
                let level = g.lhu(attr + 0x38).min(0xf);
                let old = u32::from(self.register(adsr1));
                self.store(0x800a_02d8, adsr1, ((old & 0xfff0) | level) as u16);
            }
        }
    }

    /// 800A18E0 CdMix(atv) → 800A2840: CD-ROM index 2: registers 2/3 =
    /// L→L/L→R; index 3: registers 1/2 = R→R/R→L; register 3 = 0x20
    /// (apply).
    fn cd_mix(&mut self, g: &mut Guest, atv: u32) {
        let bytes: [u32; 4] = std::array::from_fn(|i| g.lbu(atv.wrapping_add(i as u32)));
        let stores = [
            (0x800a_284c, io::CDROM_INDEX, 2),
            (0x800a_2860, io::CDROM_2, bytes[0]),
            (0x800a_2874, io::CDROM_3, bytes[1]),
            (0x800a_2884, io::CDROM_INDEX, 3),
            (0x800a_2898, io::CDROM_1, bytes[2]),
            (0x800a_28ac, io::CDROM_2, bytes[3]),
            (0x800a_28bc, io::CDROM_3, 0x20),
        ];
        for (pc, address, value) in stores {
            self.record(pc, address, value, 1);
            match (address, self.cdrom_index) {
                (io::CDROM_INDEX, _) => self.cdrom_index = value as u8 & 3,
                (io::CDROM_2, 2) => self.cd_attenuation_pending[0] = value as u8,
                (io::CDROM_3, 2) => self.cd_attenuation_pending[1] = value as u8,
                (io::CDROM_1, 3) => self.cd_attenuation_pending[2] = value as u8,
                (io::CDROM_2, 3) => self.cd_attenuation_pending[3] = value as u8,
                (io::CDROM_3, 3) if value & 0x20 != 0 => {
                    self.cd_attenuation = self.cd_attenuation_pending;
                }
                _ => {}
            }
        }
    }

    // ---- mixer ------------------------------------------------------------------------

    fn noise_tick(&mut self) -> i32 {
        let control = u32::from(self.register(reg::CONTROL));
        let step = ((control >> 8) & 3) as i32 + 4;
        let shift = (control >> 10) & 0xf;
        let level = self.noise_level;
        let parity = ((level >> 15) ^ (level >> 12) ^ (level >> 11) ^ (level >> 10) ^ 1) & 1;
        self.noise_timer -= step;
        if self.noise_timer < 0 {
            self.noise_level = (level << 1) | parity;
            self.noise_timer += 0x2_0000 >> shift;
            if self.noise_timer < 0 {
                self.noise_timer += 0x2_0000 >> shift;
            }
        }
        i32::from(self.noise_level as i16)
    }

    /// Decode the ADPCM block at the voice's address, applying the previous
    /// block's end flags first (jump to the loop address; without repeat
    /// the envelope is released at level 0) and this block's loop start.
    fn fetch_block(&mut self, v: usize) {
        let voice = &mut self.voices[v];
        if voice.end_pending {
            voice.end_pending = false;
            voice.current = voice.loop_address;
            if !voice.repeat_pending {
                voice.envelope.phase = Phase::Release;
                voice.envelope.level = 0;
            }
        }
        let address = voice.current & 0x7_fff0;
        let ram = &self.sound_ram;
        let header = ram[address as usize];
        let flags = ram[address as usize + 1];
        let shift = match header & 0xf {
            s @ 0..=12 => s,
            _ => 9,
        };
        let (a, b) = FILTERS[usize::from(header >> 4).min(4)];
        for i in 0..28 {
            let byte = ram[address as usize + 2 + i / 2];
            let nibble = (byte >> ((i & 1) * 4)) & 0xf;
            let signed = i32::from((u16::from(nibble) << 12) as i16) >> shift;
            let prediction = ((voice.history[0] * a) >> 6) + ((voice.history[1] * b) >> 6);
            let sample = (signed + prediction).clamp(-0x8000, 0x7fff);
            voice.history = [sample, voice.history[0]];
            voice.decoded[i] = sample;
        }
        if flags & 4 != 0 && !voice.ignore_loop_flags {
            voice.loop_address = address;
        }
        if flags & 1 != 0 {
            voice.end_pending = true;
            voice.repeat_pending = flags & 2 != 0;
            self.end_flags |= 1 << v;
        }
        voice.current = (address + 16) & SPU_RAM_MASK;
        voice.position = 0;
    }

    /// One voice tick: (output after the envelope, left/right contributions).
    fn voice_tick(&mut self, v: usize, previous: i32, noise: i32) -> (i32, [i32; 2]) {
        let base = v * 8;
        let registers: [u16; 6] = std::array::from_fn(|i| self.registers[base + i]);
        let modulate = v > 0 && u32::from(self.registers[0xc8]) & (1 << v) != 0
            || v >= 16 && u32::from(self.registers[0xc9]) & (1 << (v - 16)) != 0;
        let noisy = u32::from(self.registers[0xca]) | (u32::from(self.registers[0xcb]) << 16);
        let voice = &mut self.voices[v];
        let volume = [
            voice.volume[0].tick(registers[0]),
            voice.volume[1].tick(registers[1]),
        ];
        if voice.pending {
            if voice.delay > 0 {
                voice.delay -= 1;
                return (0, [0, 0]);
            }
            *voice = Voice {
                running: true,
                current: u32::from(registers[3]) << 3,
                loop_address: voice.loop_address,
                ignore_loop_flags: voice.ignore_loop_flags,
                counter: 0x3_0000,
                volume: voice.volume,
                ..Voice::default()
            };
        }
        if !voice.running {
            return (0, [0, 0]);
        }
        let mut step = u32::from(registers[2]);
        if modulate {
            let factor = (previous + 0x8000) as u32 as i32;
            let signed = i32::from(step as u16 as i16);
            step = ((signed.wrapping_mul(factor)) >> 15) as u32 & 0xffff;
        }
        if step > 0x3fff {
            step = 0x4000;
        }
        while self.voices[v].counter >= 0x1_0000 {
            if self.voices[v].position == 28 {
                self.fetch_block(v);
            }
            let voice = &mut self.voices[v];
            voice.window.rotate_left(1);
            voice.window[3] = voice.decoded[voice.position];
            voice.position += 1;
            voice.counter -= 0x1_0000;
        }
        let voice = &mut self.voices[v];
        let source = if noisy & (1 << v) != 0 {
            noise
        } else {
            let phase = ((voice.counter >> 8) & 0xff) as usize;
            let weights = [
                GAUSS[255 - phase],
                GAUSS[511 - phase],
                GAUSS[256 + phase],
                GAUSS[phase],
            ];
            weights
                .iter()
                .zip(voice.window)
                .map(|(weight, sample)| (weight * sample) >> 14)
                .sum::<i32>()
                >> 1
        };
        let level = voice.envelope.tick(registers[4], registers[5]);
        let sample = (source * level) >> 15;
        voice.counter += step << 4;
        // Signed division truncates toward zero (the oracle's arithmetic).
        let contribution = volume.map(|volume| (sample * volume) / 0x8000);
        (sample, contribution)
    }

    fn tick(&mut self) -> [i16; 2] {
        let noise = self.noise_tick();
        let mut sum = [0i32; 2];
        let mut previous = 0;
        let capture = self.capture_index;
        for v in 0..24 {
            let (sample, contribution) = self.voice_tick(v, previous, noise);
            if v == 1 || v == 3 {
                let at = if v == 1 { 0x800 } else { 0xc00 } + capture * 2;
                let value = sample.clamp(-0x8000, 0x7fff) as i16;
                self.write_sound_ram(at, &value.to_le_bytes());
            }
            previous = sample;
            sum[0] += contribution[0];
            sum[1] += contribution[1];
        }
        let control = self.register(reg::CONTROL);
        let cd = self.cd_input.pop_front().unwrap_or([0, 0]);
        let cd = if control & 1 != 0 {
            match self.output {
                OutputStage::Hardware => [0, 1].map(|side| {
                    let volume = i32::from(self.register(reg::CD_VOLUME_LEFT + side * 2) as i16);
                    (i32::from(cd[side as usize]) * volume) >> 15
                }),
                OutputStage::Redux => cd.map(i32::from),
            }
        } else {
            [0, 0]
        };
        for (side, value) in cd.iter().enumerate() {
            let at = 0x400 * side as u32 + capture * 2;
            let value = (*value).clamp(-0x8000, 0x7fff) as i16;
            self.write_sound_ram(at, &value.to_le_bytes());
        }
        self.capture_index = (capture + 1) & 0x1ff;
        sum[0] += cd[0];
        sum[1] += cd[1];
        let main = [
            self.main_volume[0].tick(self.register(reg::MAIN_VOLUME_LEFT)),
            self.main_volume[1].tick(self.register(reg::MAIN_VOLUME_RIGHT)),
        ];
        self.samples += 1;
        match self.output {
            OutputStage::Hardware => [0, 1].map(|side| {
                let value = (i64::from(sum[side]) * i64::from(main[side])) >> 15;
                value.clamp(-0x8000, 0x7fff) as i16
            }),
            OutputStage::Redux => sum.map(|value| value.clamp(-0x7fff, 0x7fff) as i16),
        }
    }
}

impl Device for SpuDevice {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        SpuDevice::call(self, ram, scratch, call)
    }
}

/// Most PCM queued for playback before the oldest is dropped: 150 ms.
const MAX_BACKLOG: usize = SAMPLE_RATE as usize * 3 / 20;
/// What a trim leaves queued: 50 ms.
const TRIMMED_BACKLOG: usize = SAMPLE_RATE as usize / 20;

/// The playback queue's bookkeeping: the frames of each buffer appended to
/// the player (one per main-loop pass), oldest first. The player reports how
/// many are still queued; the older ones have played.
///
/// Nothing else bounds the queue: audio a run produces faster than real time
/// (passes caught up after a stall, or a device clock slightly slower than
/// the system clock) would stay queued and delay every later sound by as
/// much. Past [`MAX_BACKLOG`] the oldest buffers are dropped down to
/// [`TRIMMED_BACKLOG`]: one audible skip instead of a lasting delay.
#[derive(Default)]
struct Backlog {
    buffers: VecDeque<usize>,
    /// Frames dropped to keep the latency bounded.
    dropped: u64,
}

impl Backlog {
    /// Note a buffer of `frames` appended, with `queued` buffers (the new one
    /// included) still in the player. Returns how many of the oldest to drop.
    fn append(&mut self, frames: usize, queued: usize) -> usize {
        self.buffers.push_back(frames);
        while self.buffers.len() > queued.max(1) {
            self.buffers.pop_front();
        }
        let total: usize = self.buffers.iter().sum();
        if total <= MAX_BACKLOG {
            return 0;
        }
        let mut excess = total - TRIMMED_BACKLOG;
        let mut drop = 0;
        while excess > 0 && self.buffers.len() > 1 {
            let frames = self.buffers.pop_front().unwrap_or(0);
            excess = excess.saturating_sub(frames);
            self.dropped += frames as u64;
            drop += 1;
        }
        drop
    }
}

/// Host audio output for [`SpuDevice`] PCM: rodio playback (window runs)
/// and/or a 16-bit stereo WAV file (headless runs).
pub struct AudioOutput {
    sink: Option<rodio::MixerDeviceSink>,
    player: Option<rodio::Player>,
    /// What is queued in `player` ([`Backlog`]: bounded latency).
    backlog: Backlog,
    wav: Option<std::fs::File>,
    frames: u64,
    hash: sha2::Sha256,
    /// Playback device status for the run report.
    pub device_status: serde_json::Value,
}

impl AudioOutput {
    /// `playback`: open the default output device (a failure is reported in
    /// [`device_status`](Self::device_status), not returned). `wav`: write
    /// all PCM to this file.
    pub fn new(playback: bool, wav: Option<&std::path::Path>) -> AudioResult<Self> {
        use serde_json::json;
        use std::io::Write;
        let (sink, device_status) = if playback {
            match rodio::DeviceSinkBuilder::open_default_sink() {
                Ok(mut sink) => {
                    sink.log_on_drop(false);
                    (Some(sink), json!({"status": "open"}))
                }
                Err(error) => (
                    None,
                    json!({"status": "unavailable", "reason": error.to_string()}),
                ),
            }
        } else {
            (None, json!({"status": "disabled"}))
        };
        let wav = match wav {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut file = std::fs::File::create(path)?;
                file.write_all(&wav_header(0))?;
                Some(file)
            }
            None => None,
        };
        Ok(Self {
            sink,
            player: None,
            backlog: Backlog::default(),
            wav,
            frames: 0,
            hash: sha2::Digest::new(),
            device_status,
        })
    }

    /// Queue PCM for playback and append it to the WAV file.
    pub fn submit(&mut self, pcm: &[[i16; 2]]) -> AudioResult<()> {
        use sha2::Digest;
        use std::io::Write;
        if pcm.is_empty() {
            return Ok(());
        }
        let bytes: Vec<u8> = pcm.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        self.hash.update(&bytes);
        self.frames += pcm.len() as u64;
        if let Some(file) = &mut self.wav {
            file.write_all(&bytes)?;
        }
        if let Some(sink) = &self.sink {
            let player = self
                .player
                .get_or_insert_with(|| rodio::Player::connect_new(sink.mixer()));
            let samples: Vec<f32> = pcm
                .iter()
                .flatten()
                .map(|&v| f32::from(v) / 32768.0)
                .collect();
            player.append(rodio::buffer::SamplesBuffer::new(
                rodio::nz!(2),
                rodio::nz!(44100),
                samples,
            ));
            // Bounded latency: the playback only (the WAV and the hash keep
            // everything).
            for _ in 0..self.backlog.append(pcm.len(), player.len()) {
                player.skip_one();
            }
        }
        Ok(())
    }

    /// Drop the PCM queued for playback, which belongs to the timeline a
    /// quick load leaves (`savestate`). The WAV file and the PCM hash keep
    /// everything submitted.
    pub fn flush(&mut self) {
        // A dropped player stops; the next submit connects a new one.
        self.player = None;
        self.backlog.buffers.clear();
    }

    /// Finish the WAV header; returns frames, PCM hash and device status.
    pub fn finish(&mut self) -> AudioResult<serde_json::Value> {
        use sha2::Digest;
        use std::io::{Seek, SeekFrom, Write};
        if let Some(mut file) = self.wav.take() {
            let bytes = u32::try_from(self.frames * 4).map_err(|_| "WAV exceeds 4 GiB")?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&wav_header(bytes))?;
            file.flush()?;
        }
        Ok(serde_json::json!({
            "sample_rate": SAMPLE_RATE,
            "frames": self.frames,
            "pcm_sha256": format!("{:x}", self.hash.clone().finalize()),
            "device": self.device_status,
            "playback_dropped_frames": self.backlog.dropped,
        }))
    }
}

/// RIFF/WAVE header of 16-bit stereo PCM at [`SAMPLE_RATE`] with `bytes`
/// of data.
pub fn wav_header(bytes: u32) -> [u8; 44] {
    let mut header = [0u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&bytes.saturating_add(36).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&2u16.to_le_bytes());
    header[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    header[28..32].copy_from_slice(&(SAMPLE_RATE * 4).to_le_bytes());
    header[32..34].copy_from_slice(&4u16.to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&bytes.to_le_bytes());
    header
}

#[cfg(test)]
mod backlog_tests {
    use super::*;

    /// One NTSC field of PCM.
    const FIELD: usize = 735;

    /// Played buffers are forgotten; the queue grows to 150 ms, then the
    /// oldest go down to 50 ms (the latency stays bounded).
    #[test]
    fn the_playback_backlog_stays_bounded() {
        let mut backlog = Backlog::default();
        // Real-time playback: one buffer queued at a time, nothing dropped.
        for _ in 0..100 {
            assert_eq!(backlog.append(FIELD, 1), 0);
        }
        assert_eq!(backlog.buffers.len(), 1);
        // A burst after a stall: nothing plays meanwhile.
        let mut queued = 1;
        let mut drops = Vec::new();
        for _ in 0..12 {
            queued += 1;
            let drop = backlog.append(FIELD, queued);
            queued -= drop;
            drops.push(drop);
        }
        let total = |b: &Backlog| b.buffers.iter().sum::<usize>();
        assert!(
            total(&backlog) <= MAX_BACKLOG,
            "{} frames queued",
            total(&backlog)
        );
        // The 10th queued field (7350 frames) passes 150 ms: 7 go, 3 (2205
        // frames, 50 ms) stay.
        assert_eq!(drops[..8], [0; 8]);
        assert_eq!(drops[8], 7);
        assert_eq!(backlog.dropped, 7 * FIELD as u64);
        assert!(total(&backlog) >= TRIMMED_BACKLOG);
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    /// SpuReadDecodedData's device side: `_spu_Fr_` copies the capture
    /// buffers the voices and the CD input wrote, and the status bit follows
    /// the capture half.
    #[test]
    fn decoded_data_reads_the_capture_buffers() {
        let mut spu = SpuDevice::new();
        let mut ram = vec![0u8; 0x20_0000];
        let mut scratch = vec![0u8; 0x400];
        for (i, byte) in spu.sound_ram.iter_mut().take(0x1000).enumerate() {
            *byte = (i * 7 + 3) as u8;
        }
        let call = |function, args| HostCall { function, args };
        assert_eq!(
            spu.call(
                &mut ram,
                &mut scratch,
                call(calls::SPU_CAPTURE_STATUS, [0; 4])
            ),
            Some(0)
        );
        spu.capture_index = 0x180;
        assert_eq!(
            spu.call(
                &mut ram,
                &mut scratch,
                call(calls::SPU_CAPTURE_STATUS, [0; 4])
            ),
            Some(0x800)
        );
        // SpuReadDecodedData(buf, 6): voices 1 and 3 into buf + 0x800.
        let buf = 0x8010_0000u32;
        spu.call(
            &mut ram,
            &mut scratch,
            call(calls::SPU_READ_DMA, [buf + 0x800, 0x100, 0x20, 0]),
        );
        assert_eq!(&ram[0x10_0800..0x10_1000], &spu.sound_ram[0x800..0x1000]);
        assert!(ram[0x10_0000..0x10_0800].iter().all(|&b| b == 0));
        assert_eq!(spu.register(reg::TRANSFER_ADDRESS), 0x100);
        assert_eq!(spu.register(reg::CONTROL) & 0x30, 0);
    }

    #[test]
    fn fields_mix_the_samples_of_their_release() {
        let mut pal = SpuDevice::new();
        let counts: Vec<usize> = (0..50).map(|_| pal.mix_field().len()).collect();
        assert!(counts.iter().all(|n| [881, 882].contains(n)));
        assert_eq!(counts.iter().sum::<usize>(), 677_298 * 50 / 768);
        let mut ntsc = SpuDevice::new();
        ntsc.set_release(LibSpu::JP, FieldLength::NTSC);
        let counts: Vec<usize> = (0..1000).map(|_| ntsc.mix_field().len()).collect();
        assert!(counts.iter().all(|n| [735, 736].contains(n)));
        // 1000 fields of 59.94 Hz are 1001 / 60 s: 735,735 samples exactly.
        assert_eq!(counts.iter().sum::<usize>(), 735_735);
        assert!((FieldLength::NTSC.samples() - 735.735).abs() < 1e-9);
        assert_eq!(ntsc.release(), (LibSpu::JP, FieldLength::NTSC));
        // The release is configuration: a save state's device decodes as PAL.
        let bytes = postcard::to_allocvec(&ntsc).unwrap();
        let decoded: SpuDevice = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.release(), (LibSpu::PAL, FieldLength::PAL));
    }

    #[test]
    fn the_japanese_libspu_words_move_with_their_runs() {
        for (pal, jp) in LibSpu::PAL.words().into_iter().zip(LibSpu::JP.words()) {
            let delta = if pal >= 0x800c_0000 { 0x2228 } else { 0x2180 };
            assert_eq!(jp, pal + delta, "{pal:#x}");
        }
    }
}
