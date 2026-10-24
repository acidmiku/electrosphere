//! Host MDEC (motion decoder) for the address-preserving machine
//! (`ac3_flight`).
//!
//! The translated libpress routines (`ac3_flight::states::pending_sdk`)
//! log the MDEC's register writes and DMA transfers as [`HostCall`]s named
//! after the original routine that performs them (see the "MDEC host
//! protocol" there):
//!
//! | Call | Arguments | Device |
//! | --- | --- | --- |
//! | [`calls::MDEC_RESET`] (800AAF98) | `[mode]` | mode 0: reset and the release's default tables ([`DefaultTables`]: the MDEC(2) block, PAL [`DEFAULT_QUANT`], and the MDEC(3) block, PAL [`DEFAULT_SCALE`]); mode 1: reset only |
//! | [`calls::MDEC_IN`] (800AB088) | `[buffer, words]` | MDEC0 = `[buffer]`, then `words` words from `buffer + 4` through DMA 0 |
//! | [`calls::MDEC_OUT`] (800AB118) | `[buffer, words]` | DMA 1 reads `words` words of output into `buffer`; result 1 when complete |
//!
//! Commands (MDEC0 bits 31..29): 1 decodes macroblocks (bits 28..27 depth:
//! 0 4-bit and 1 8-bit monochrome, 2 24-bit and 3 15-bit colour; bit 26
//! signed output; bit 25 the 15-bit STP bit; bits 15..0 the parameter
//! words), 2 sets the quantization tables (64 luma bytes, then with bit 0
//! 64 chroma bytes, in zigzag order), 3 sets the IDCT scale table (64
//! signed halfwords). A decode command turns its whole run-level stream
//! into pixels at once (Cr, Cb, Y1..Y4 per 16x16 macroblock, each
//! macroblock row by row: 24-bit R, G, B bytes, 15-bit little-endian
//! words); DMA 1 then drains them in order. A partial macroblock at the
//! end of the stream is dropped, as the hardware never outputs it. A
//! transfer asking for more output than decoded writes what there is and
//! reports itself incomplete (result 0; the hardware DMA would stall).
//!
//! # Arithmetic
//!
//! With the standard IDCT scale table (the one libpress uploads) the
//! decoder is the AAN butterfly model of the pinned PCSX-Redux
//! (`core/mdec-decoder.cc`), which its authors measured against hardware
//! (residuals of at most 2): quantization prescaled by the AAN factors, a
//! DC of `clamp(dc * q0) * 128`, AC terms `(level * q * scale)` saturated at
//! the 11-bit range of `(x + 4) / 8`, the column and row butterflies with
//! 12-bit constants, and colour `Y << 10 + 1436 Cr`, `- 352 Cb - 731 Cr`,
//! `+ 1818 Cb` scaled by 2^-20 (ties down), wrapped to 9 bits and clamped
//! (24-bit), or scaled by 2^-23 (ties up) and clamped to 5 bits (15-bit).
//! Arithmetic is 32-bit and wraps as the reference's `int`. A custom scale
//! table uses the psx-spx reference IDCT instead: dequantized values clamped
//! to -0x400..0x3FF, two matrix passes with the table's upper 13 bits and
//! `(sum + 0xFFF) >> 13`. Monochrome output (not used by AC3) masks the
//! luma to 9 bits and clamps it (psx-spx).
//!
//! Validation (`tests/machine_mdec.rs`): every 24-bit slice the original's
//! DMA-1 callback 80055878 found in its buffer in the PCSX-Redux captures
//! `ovmisc-006`, `ovmisc-009`, `ovmisc-010` and `ovmisc-011`, from the
//! run-level data and tables in the same RAM, byte for byte. The 15-bit,
//! monochrome and custom-table paths are unit-tested only.
use ac3_machine::{Device, HostCall};

/// Host-call function ids (the libpress routines that touch the MDEC; their
/// canonical ids, [`ac3_machine::host_ids`]).
pub mod calls {
    use ac3_machine::host_ids;
    /// 800AAF98: the MDEC reset of DecDCTReset `[mode]`.
    pub const MDEC_RESET: u32 = host_ids::MDEC_RESET;
    /// 800AB088: the command and input DMA of DecDCTin `[buffer, words]`.
    pub const MDEC_IN: u32 = host_ids::MDEC_IN;
    /// 800AB118: the output DMA of DecDCTout `[buffer, words]`.
    pub const MDEC_OUT: u32 = host_ids::MDEC_OUT;
}

/// `true` for the host calls [`MdecDevice`] answers.
pub fn handles(function: u32) -> bool {
    matches!(
        function,
        calls::MDEC_RESET | calls::MDEC_IN | calls::MDEC_OUT
    )
}

/// 800B9934: libpress's MDEC(2) block (0x40000001, then the luma and chroma
/// quantization tables) that DecDCTReset(0) uploads.
pub const DEFAULT_QUANT: u32 = 0x800b_9934;
/// 800B99B8: libpress's MDEC(3) block (0x60000000, then the IDCT scale
/// table).
pub const DEFAULT_SCALE: u32 = 0x800b_99b8;

/// Where a release keeps libpress's default MDEC blocks, which
/// DecDCTReset(0) uploads. Configuration, not device state
/// ([`MdecDevice::set_release`]).
///
/// The Japanese release links the same libpress: its blocks are the PAL
/// ones moved +0x2180 with the small-data area, as the two JP DecDCTReset
/// instructions that address them say (800AAE00/800AAE10 of the aligned
/// routine, `mips.normalize`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefaultTables {
    pub quant: u32,
    pub scale: u32,
}

impl DefaultTables {
    pub const PAL: Self = Self {
        quant: DEFAULT_QUANT,
        scale: DEFAULT_SCALE,
    };
    pub const JP: Self = Self {
        quant: 0x800b_bab4,
        scale: 0x800b_bb38,
    };
}

impl Default for DefaultTables {
    fn default() -> Self {
        Self::PAL
    }
}

/// Zigzag position to raster index.
const ZSCAN: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// AAN scale factors `s(u) s(v)` in raster order, 2^20 = 1 (s(0) = 1,
/// s(k) = sqrt(2) cos(k pi / 16)).
const AAN_SCALES: [i32; 64] = [
    1048576, 1454417, 1370031, 1232995, 1048576, 823861, 567485, 289301, 1454417, 2017334, 1900287,
    1710213, 1454417, 1142728, 787125, 401273, 1370031, 1900287, 1790031, 1610986, 1370031,
    1076426, 741455, 377991, 1232995, 1710213, 1610986, 1449849, 1232995, 968758, 667292, 340183,
    1048576, 1454417, 1370031, 1232995, 1048576, 823861, 567485, 289301, 823861, 1142728, 1076426,
    968758, 823861, 647303, 445870, 227303, 567485, 787125, 741455, 667292, 567485, 445870, 307121,
    156569, 289301, 401273, 377991, 340183, 289301, 227303, 156569, 79818,
];

/// The standard IDCT scale table (rows by frequency, 2^15 = 1: row 0 is
/// 1/sqrt(2), row u cos((2x + 1) u pi / 16)), as libpress uploads it and
/// psx-spx lists it.
pub const STANDARD_SCALE: [i16; 64] = {
    const T: [u16; 64] = [
        0x5a82, 0x5a82, 0x5a82, 0x5a82, 0x5a82, 0x5a82, 0x5a82, 0x5a82, //
        0x7d8a, 0x6a6d, 0x471c, 0x18f8, 0xe707, 0xb8e3, 0x9592, 0x8275, //
        0x7641, 0x30fb, 0xcf04, 0x89be, 0x89be, 0xcf04, 0x30fb, 0x7641, //
        0x6a6d, 0xe707, 0x8275, 0xb8e3, 0x471c, 0x7d8a, 0x18f8, 0x9592, //
        0x5a82, 0xa57d, 0xa57d, 0x5a82, 0x5a82, 0xa57d, 0xa57d, 0x5a82, //
        0x471c, 0x8275, 0x18f8, 0x6a6d, 0x9592, 0xe707, 0x7d8a, 0xb8e3, //
        0x30fb, 0x89be, 0x7641, 0xcf04, 0xcf04, 0x7641, 0x89be, 0x30fb, //
        0x18f8, 0xb8e3, 0x6a6d, 0x8275, 0x7d8a, 0x9592, 0x471c, 0xe707, //
    ];
    let mut table = [0i16; 64];
    let mut i = 0;
    while i < 64 {
        table[i] = T[i] as i16;
        i += 1;
    }
    table
};

const END_OF_BLOCK: u16 = 0xfe00;

/// `(x + 2^(n-1)) >> n` (ties up).
const fn round_shift(x: i32, n: u32) -> i32 {
    x.wrapping_add((1 << n) >> 1) >> n
}
/// The AAN butterfly constants, 12 fractional bits.
const FIX_1_082392200: i32 = round_shift(18_159_528, 12);
const FIX_1_414213562: i32 = round_shift(23_726_566, 12);
const FIX_1_847759065: i32 = round_shift(31_000_253, 12);
const FIX_2_613125930: i32 = round_shift(43_840_978, 12);

fn sign10(word: u16) -> i32 {
    (i32::from(word & 0x3ff) << 22) >> 22
}
fn mul_shift(value: i32, constant: i32) -> i32 {
    value.wrapping_mul(constant) >> 12
}

/// One 1D AAN butterfly over eight coefficients (in place).
fn butterfly(p: &mut [i32; 8]) {
    let z10 = p[0].wrapping_add(p[4]);
    let z11 = p[0].wrapping_sub(p[4]);
    let z13 = p[2].wrapping_add(p[6]);
    let z12 = mul_shift(p[2].wrapping_sub(p[6]), FIX_1_414213562).wrapping_sub(z13);
    let tmp0 = z10.wrapping_add(z13);
    let tmp3 = z10.wrapping_sub(z13);
    let tmp1 = z11.wrapping_add(z12);
    let tmp2 = z11.wrapping_sub(z12);
    let z13 = p[3].wrapping_add(p[5]);
    let z10 = p[3].wrapping_sub(p[5]);
    let z11 = p[1].wrapping_add(p[7]);
    let z12 = p[1].wrapping_sub(p[7]);
    let tmp7 = z11.wrapping_add(z13);
    let z5 = z12.wrapping_sub(z10).wrapping_mul(FIX_1_847759065);
    let tmp6 = (z10.wrapping_mul(FIX_2_613125930).wrapping_add(z5) >> 12).wrapping_sub(tmp7);
    let tmp5 = mul_shift(z11.wrapping_sub(z13), FIX_1_414213562).wrapping_sub(tmp6);
    let tmp4 = (z12.wrapping_mul(FIX_1_082392200).wrapping_sub(z5) >> 12).wrapping_add(tmp5);
    *p = [
        tmp0.wrapping_add(tmp7),
        tmp1.wrapping_add(tmp6),
        tmp2.wrapping_add(tmp5),
        tmp3.wrapping_sub(tmp4),
        tmp3.wrapping_add(tmp4),
        tmp2.wrapping_sub(tmp5),
        tmp1.wrapping_sub(tmp6),
        tmp0.wrapping_sub(tmp7),
    ];
}

/// The AAN IDCT of a block whose columns `used` (bit per column; -1 = DC
/// only) hold AC terms; the result stays in the 2^10 domain.
fn aan_idct(block: &mut [i32; 64], used: i32) {
    if used == -1 {
        let dc = block[0];
        block.fill(dc);
        return;
    }
    let mut used = used;
    for column in 0..8 {
        if used & (1 << column) == 0 {
            if block[column] != 0 {
                let dc = block[column];
                for row in 0..8 {
                    block[row * 8 + column] = dc;
                }
                used |= 1 << column;
            }
            continue;
        }
        let mut p: [i32; 8] = std::array::from_fn(|row| block[row * 8 + column]);
        butterfly(&mut p);
        for (row, value) in p.into_iter().enumerate() {
            block[row * 8 + column] = value;
        }
    }
    for row in block.chunks_exact_mut(8) {
        if used == 1 {
            let first = row[0];
            row.fill(first);
        } else {
            let mut p: [i32; 8] = row.try_into().expect("8 coefficients");
            butterfly(&mut p);
            row.copy_from_slice(&p);
        }
    }
}

/// The psx-spx reference IDCT with a custom scale table: two matrix passes
/// with the table's upper 13 bits and `(sum + 0xFFF) >> 13`, the result
/// scaled to the 2^10 domain of [`aan_idct`].
fn matrix_idct(block: &mut [i32; 64], scale: &[i16; 64]) {
    let mut source = *block;
    for pass in 0..2 {
        let mut target = [0i32; 64];
        for x in 0..8 {
            for y in 0..8 {
                let sum: i64 = (0..8)
                    .map(|z| i64::from(source[y + 8 * z]) * i64::from(scale[x + 8 * z] >> 3))
                    .sum();
                let mut value = ((sum + 0xfff) >> 13) as i32;
                if pass == 1 {
                    value = value.wrapping_mul(1024);
                }
                target[x + 8 * y] = value;
            }
        }
        source = target;
    }
    *block = source;
}

/// The run-level stream of one decode command.
struct Stream<'a> {
    words: &'a [u16],
    at: usize,
}
impl Stream<'_> {
    fn next(&mut self) -> Option<u16> {
        let word = *self.words.get(self.at)?;
        self.at += 1;
        Some(word)
    }
}

/// 9-bit wrap and saturation of a scaled channel (the reference's CLAMP8):
/// -128..127.
fn wrap9_clamp(value: i32) -> i32 {
    let v = ((value & 0x1ff) ^ 0x100) - 0x100;
    v.clamp(-128, 127)
}
/// `(c + 2^19 - 1) >> 20`: the 24-bit scaling (ties down).
fn scale8(value: i32) -> i32 {
    value.wrapping_add((1 << 19) - 1) >> 20
}

/// Output format of a decode command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// 0 4-bit, 1 8-bit, 2 24-bit, 3 15-bit.
    pub depth: u32,
    pub signed: bool,
    pub stp: bool,
}
impl Format {
    pub fn from_command(command: u32) -> Self {
        Self {
            depth: (command >> 27) & 3,
            signed: command & (1 << 26) != 0,
            stp: command & (1 << 25) != 0,
        }
    }
    /// Output bytes per macroblock (16x16 colour, 8x8 monochrome).
    pub fn macroblock_bytes(self) -> usize {
        match self.depth {
            0 => 32,
            1 => 64,
            2 => 768,
            _ => 512,
        }
    }
}

/// The host MDEC; see the module documentation. Save states serialize the
/// whole device.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MdecDevice {
    /// Quantization tables (luma, chroma), zigzag order.
    #[serde(with = "crate::savestate::array_of_arrays")]
    quant: [[u8; 64]; 2],
    /// IDCT scale table (the hardware has none until one is uploaded).
    #[serde(with = "crate::savestate::array")]
    scale: [i16; 64],
    output: Vec<u8>,
    cursor: usize,
    /// Macroblocks decoded since creation.
    pub macroblocks: u64,
    /// Output DMA transfers that found too little output.
    pub stalls: u64,
    /// The release's default blocks (PAL by default).
    #[serde(skip)]
    tables: DefaultTables,
}

impl Default for MdecDevice {
    fn default() -> Self {
        Self::new()
    }
}

fn read_u32(ram: &[u8], address: u32) -> u32 {
    let at = (address & 0x1f_fffc) as usize;
    u32::from_le_bytes([ram[at], ram[at + 1], ram[at + 2], ram[at + 3]])
}

impl MdecDevice {
    /// Power-on state: zero tables, no output.
    pub fn new() -> Self {
        Self {
            quant: [[0; 64]; 2],
            scale: [0; 64],
            output: Vec::new(),
            cursor: 0,
            macroblocks: 0,
            stalls: 0,
            tables: DefaultTables::PAL,
        }
    }
    /// Upload the release's default blocks at DecDCTReset(0). A save
    /// state's device takes the running one's ([`Self::release`]).
    pub fn set_release(&mut self, tables: DefaultTables) {
        self.tables = tables;
    }
    /// The release's default blocks ([`Self::set_release`]).
    pub fn release(&self) -> DefaultTables {
        self.tables
    }
    pub fn quant_tables(&self) -> &[[u8; 64]; 2] {
        &self.quant
    }
    pub fn scale_table(&self) -> &[i16; 64] {
        &self.scale
    }
    /// Decoded bytes not yet read by the output DMA.
    pub fn pending(&self) -> usize {
        self.output.len() - self.cursor
    }

    /// Reset (MDEC1 bit 31): pending output is dropped; tables stay.
    pub fn reset(&mut self) {
        self.output.clear();
        self.cursor = 0;
    }

    /// One command with its parameter words (little-endian, as DMA 0 sends
    /// them).
    pub fn command(&mut self, command: u32, parameters: &[u8]) {
        match command >> 29 {
            1 => {
                let words: Vec<u16> = parameters
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                self.decode(Format::from_command(command), &words);
            }
            2 => {
                if parameters.len() >= 64 {
                    self.quant[0].copy_from_slice(&parameters[..64]);
                }
                if command & 1 != 0 && parameters.len() >= 128 {
                    self.quant[1].copy_from_slice(&parameters[64..128]);
                }
            }
            3 => {
                for (i, pair) in parameters.chunks_exact(2).take(64).enumerate() {
                    self.scale[i] = i16::from_le_bytes([pair[0], pair[1]]);
                }
            }
            _ => {}
        }
    }

    /// Decode a run-level stream into output, macroblock by macroblock.
    pub fn decode(&mut self, format: Format, words: &[u16]) {
        if self.cursor > 0 {
            self.output.drain(..self.cursor);
            self.cursor = 0;
        }
        let mut stream = Stream { words, at: 0 };
        let colour = format.depth >= 2;
        loop {
            let blocks = if colour { 6 } else { 1 };
            let mut decoded = [[0i32; 64]; 6];
            for (i, block) in decoded.iter_mut().take(blocks).enumerate() {
                let table = if colour && i < 2 { 1 } else { 0 };
                if !self.block(&mut stream, table, block) {
                    return;
                }
            }
            self.macroblocks += 1;
            if colour {
                self.colour(format, &decoded);
            } else {
                self.monochrome(format, &decoded[0]);
            }
        }
    }

    /// One block: run-level codes to the IDCT output (2^10 domain). `false`
    /// when the stream ends first.
    fn block(&self, stream: &mut Stream<'_>, table: usize, block: &mut [i32; 64]) -> bool {
        let quant = &self.quant[table];
        let standard = self.scale == STANDARD_SCALE;
        let mut first = match stream.next() {
            Some(word) => word,
            None => return false,
        };
        // End codes before a block's DC are padding.
        while first == END_OF_BLOCK {
            first = match stream.next() {
                Some(word) => word,
                None => return false,
            };
        }
        block.fill(0);
        let dc = sign10(first);
        let q_scale = i32::from(first >> 10);
        let spec = if q_scale == 0 {
            dc.wrapping_mul(2)
        } else {
            dc.wrapping_mul(i32::from(quant[0]))
        };
        let saturated = spec.clamp(-0x400, 0x3ff);
        block[0] = if standard {
            // Both of the reference's DC branches come to this.
            saturated.wrapping_mul(128)
        } else {
            saturated
        };
        let mut k = 0usize;
        let mut used = 0i32;
        loop {
            let Some(word) = stream.next() else {
                return false;
            };
            if word == END_OF_BLOCK {
                break;
            }
            k += usize::from(word >> 10) + 1;
            if k > 63 {
                break;
            }
            let destination = if q_scale == 0 { k } else { ZSCAN[k] };
            let level = sign10(word);
            block[destination] = if standard {
                if q_scale == 0 {
                    round_shift(
                        level
                            .wrapping_mul(16)
                            .wrapping_mul(round_shift(AAN_SCALES[k], 4)),
                        12,
                    )
                } else {
                    let q = i32::from(quant[k]);
                    let x = level.wrapping_mul(q).wrapping_mul(q_scale);
                    let v = x.wrapping_add(4) / 8;
                    let x = if v > 0x3ff {
                        0x3ff * 8
                    } else if v < -0x400 {
                        -0x400 * 8
                    } else {
                        x
                    };
                    let factor = if q != 0 {
                        round_shift(AAN_SCALES[ZSCAN[k]], 4)
                    } else {
                        0
                    };
                    round_shift(x.wrapping_mul(factor), 12)
                }
            } else {
                let spec = if q_scale == 0 {
                    level.wrapping_mul(2)
                } else {
                    level
                        .wrapping_mul(i32::from(quant[k]))
                        .wrapping_mul(q_scale)
                        .wrapping_add(4)
                        / 8
                };
                spec.clamp(-0x400, 0x3ff)
            };
            if destination > 7 {
                used |= 1 << (destination & 7);
            }
        }
        if standard {
            if k == 0 {
                used = -1;
            }
            aan_idct(block, used);
        } else {
            matrix_idct(block, &self.scale);
        }
        true
    }

    /// A colour macroblock (Cr, Cb, Y1..Y4) to 24- or 15-bit pixels.
    fn colour(&mut self, format: Format, blocks: &[[i32; 64]; 6]) {
        for y in 0..16 {
            for x in 0..16 {
                let luma = &blocks[2 + usize::from(x >= 8) + 2 * usize::from(y >= 8)];
                let value = luma[8 * (y & 7) + (x & 7)] << 10;
                let chroma = 8 * (y >> 1) + (x >> 1);
                let (cr, cb) = (blocks[0][chroma], blocks[1][chroma]);
                let terms = [
                    cr.wrapping_mul(1436),
                    cb.wrapping_mul(-352).wrapping_add(cr.wrapping_mul(-731)),
                    cb.wrapping_mul(1818),
                ];
                let channels = terms.map(|term| value.wrapping_add(term));
                if format.depth == 2 {
                    for channel in channels {
                        let v = wrap9_clamp(scale8(channel));
                        let byte = if format.signed { v } else { v + 128 };
                        self.output.push(byte as u8);
                    }
                } else {
                    let [r, g, b] = channels.map(|channel| {
                        let v = round_shift(channel, 23).clamp(-16, 15);
                        (if format.signed { v } else { v + 16 }) as u16 & 0x1f
                    });
                    let stp = if format.stp { 0x8000 } else { 0 };
                    let pixel = stp | (b << 10) | (g << 5) | r;
                    self.output.extend_from_slice(&pixel.to_le_bytes());
                }
            }
        }
    }

    /// A monochrome 8x8 block to 8- or 4-bit pixels (psx-spx: 9-bit mask,
    /// clamp, unsigned bias).
    fn monochrome(&mut self, format: Format, block: &[i32; 64]) {
        let pixels: Vec<u8> = block
            .iter()
            .map(|&v| {
                let v = wrap9_clamp(scale8(v << 10));
                (if format.signed { v } else { v + 128 }) as u8
            })
            .collect();
        if format.depth == 1 {
            self.output.extend_from_slice(&pixels);
        } else {
            for pair in pixels.chunks_exact(2) {
                self.output.push((pair[0] >> 4) | (pair[1] & 0xf0));
            }
        }
    }

    /// Output DMA: `bytes` decoded bytes into `ram` at `address`. Returns
    /// whether the transfer is complete.
    pub fn read_output(&mut self, ram: &mut [u8], address: u32, bytes: usize) -> bool {
        let available = self.pending().min(bytes);
        for i in 0..available {
            let at = (address.wrapping_add(i as u32) & 0x1f_ffff) as usize;
            ram[at] = self.output[self.cursor + i];
        }
        self.cursor += available;
        if self.cursor == self.output.len() {
            self.output.clear();
            self.cursor = 0;
        }
        if available < bytes {
            self.stalls += 1;
        }
        available == bytes
    }

    /// Output DMA into a host buffer (the movie export, `movie_disc`): the
    /// next `out.len()` decoded bytes, zeros past what there is (a stall,
    /// counted as in [`Self::read_output`]). Returns whether the transfer
    /// was complete.
    pub fn read_output_into(&mut self, out: &mut [u8]) -> bool {
        let available = self.pending().min(out.len());
        out[..available].copy_from_slice(&self.output[self.cursor..self.cursor + available]);
        out[available..].fill(0);
        self.cursor += available;
        if self.cursor == self.output.len() {
            self.output.clear();
            self.cursor = 0;
        }
        if available < out.len() {
            self.stalls += 1;
        }
        available == out.len()
    }

    /// Run the command block at `address` (header word, then `words`
    /// parameter words) as DMA 0 would send it.
    fn command_block(&mut self, ram: &[u8], address: u32, words: usize) {
        let command = read_u32(ram, address);
        let parameters: Vec<u8> = (0..words * 4)
            .map(|i| ram[(address.wrapping_add(4 + i as u32) & 0x1f_ffff) as usize])
            .collect();
        self.command(command, &parameters);
    }
}

impl Device for MdecDevice {
    fn call(&mut self, ram: &mut [u8], _scratch: &mut [u8], call: HostCall) -> Option<u32> {
        let [a0, a1, ..] = call.args;
        match call.function {
            calls::MDEC_RESET => {
                self.reset();
                if a0 == 0 {
                    let tables = self.tables;
                    self.command_block(ram, tables.quant, 0x20);
                    self.command_block(ram, tables.scale, 0x20);
                }
                None
            }
            calls::MDEC_IN => {
                self.command_block(ram, a0, a1 as usize);
                None
            }
            calls::MDEC_OUT => Some(u32::from(self.read_output(ram, a0, a1 as usize * 4))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standard() -> MdecDevice {
        let mut mdec = MdecDevice::new();
        let mut quant = [16u8; 128];
        quant[0] = 2;
        quant[64] = 2;
        mdec.command(0x4000_0001, &quant);
        let scale: Vec<u8> = STANDARD_SCALE
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        mdec.command(0x6000_0000, &scale);
        mdec
    }

    #[test]
    fn dc_only_macroblock_is_flat() {
        let mut mdec = standard();
        // Six blocks: DC 0 for Cr/Cb, DC 64 (q 1) for the lumas.
        let mut words = vec![0x0400, END_OF_BLOCK, 0x0400, END_OF_BLOCK];
        for _ in 0..4 {
            words.extend([0x0440, END_OF_BLOCK]);
        }
        mdec.decode(Format::from_command(0x3000_0000), &words);
        assert_eq!(mdec.macroblocks, 1);
        assert_eq!(mdec.pending(), 768);
        // 64 * 2 / 8 = 16: grey 128 + 16.
        assert!(
            mdec.output.iter().all(|&b| b == 144),
            "{:?}",
            &mdec.output[..6]
        );
    }

    #[test]
    fn padding_partial_macroblocks_and_stalls() {
        let mut mdec = standard();
        let mut words = vec![END_OF_BLOCK, END_OF_BLOCK];
        for _ in 0..6 {
            words.extend([0x0400, END_OF_BLOCK]);
        }
        // A second, incomplete macroblock is dropped.
        words.extend([0x0400, END_OF_BLOCK, 0x0400]);
        mdec.decode(Format::from_command(0x3800_0000), &words);
        assert_eq!(mdec.macroblocks, 1);
        assert_eq!(mdec.pending(), 512);
        let mut ram = vec![0u8; 0x20_0000];
        assert!(mdec.read_output(&mut ram, 0x8001_0000, 256));
        assert!(!mdec.read_output(&mut ram, 0x8001_0100, 512));
        assert_eq!(mdec.stalls, 1);
        // 15-bit mid grey: 16 per channel.
        assert_eq!(u16::from_le_bytes([ram[0x1_0000], ram[0x1_0001]]), 0x4210);
    }

    #[test]
    fn host_reads_drain_like_the_output_dma() {
        let mut mdec = standard();
        // Two grey macroblocks: chroma DC 0, luma DC 64.
        let mut words = Vec::new();
        for _ in 0..2 {
            words.extend([0x0400, END_OF_BLOCK, 0x0400, END_OF_BLOCK]);
            for _ in 0..4 {
                words.extend([0x0440, END_OF_BLOCK]);
            }
        }
        mdec.decode(Format::from_command(0x3000_0000), &words);
        assert_eq!(mdec.pending(), 2 * 768);
        let mut first = vec![0u8; 768];
        assert!(mdec.read_output_into(&mut first));
        assert!(first.iter().all(|&b| b == 144));
        // A longer read than what is left: the rest is zeros, a stall.
        let mut rest = vec![1u8; 1000];
        assert!(!mdec.read_output_into(&mut rest));
        assert!(rest[..768].iter().all(|&b| b == 144));
        assert!(rest[768..].iter().all(|&b| b == 0));
        assert_eq!((mdec.pending(), mdec.stalls), (0, 1));
    }

    #[test]
    fn stp_signed_and_monochrome_formats() {
        let mut mdec = standard();
        let mut words = Vec::new();
        for _ in 0..6 {
            words.extend([0x0400, END_OF_BLOCK]);
        }
        mdec.decode(Format::from_command(0x3a00_0000), &words);
        let mut ram = vec![0u8; 0x20_0000];
        assert!(mdec.read_output(&mut ram, 0x8001_0000, 512));
        assert_eq!(u16::from_le_bytes([ram[0x1_0000], ram[0x1_0001]]), 0xc210);
        mdec.decode(Format::from_command(0x3400_0000), &words);
        assert!(mdec.read_output(&mut ram, 0x8001_0000, 768));
        assert_eq!(ram[0x1_0000], 0, "signed zero");
        // 8-bit monochrome: one block per macroblock.
        mdec.decode(Format::from_command(0x2800_0000), &[0x0440, END_OF_BLOCK]);
        assert_eq!(mdec.pending(), 64);
        assert!(mdec.read_output(&mut ram, 0x8001_0000, 64));
        assert_eq!(ram[0x1_0000], 144);
    }

    #[test]
    fn custom_scale_table_uses_the_matrix_idct() {
        let mut mdec = standard();
        let mut scale = STANDARD_SCALE;
        scale[63] = scale[63].wrapping_add(8);
        let bytes: Vec<u8> = scale.iter().flat_map(|v| v.to_le_bytes()).collect();
        mdec.command(0x6000_0000, &bytes);
        let mut words = Vec::new();
        for _ in 0..6 {
            words.extend([0x0440, END_OF_BLOCK]);
        }
        mdec.decode(Format::from_command(0x3000_0000), &words);
        // DC 128 through two passes of 0x5A82 >> 3: 128 * (2896 / 8192)^2 = 16.
        assert!(mdec.output.iter().step_by(3).all(|&r| r == 128 + 16 + 22));
    }

    #[test]
    fn reset_uploads_the_default_tables_from_ram() {
        let mut ram = vec![0u8; 0x20_0000];
        let base = (DEFAULT_QUANT & 0x1f_ffff) as usize;
        ram[base..base + 4].copy_from_slice(&0x4000_0001u32.to_le_bytes());
        for i in 0..128 {
            ram[base + 4 + i] = i as u8;
        }
        let scale = (DEFAULT_SCALE & 0x1f_ffff) as usize;
        ram[scale..scale + 4].copy_from_slice(&0x6000_0000u32.to_le_bytes());
        for (i, v) in STANDARD_SCALE.iter().enumerate() {
            ram[scale + 4 + 2 * i..scale + 6 + 2 * i].copy_from_slice(&v.to_le_bytes());
        }
        let mut mdec = MdecDevice::new();
        let mut scratch = [0u8; 0x400];
        let call = HostCall {
            function: calls::MDEC_RESET,
            args: [0; 4],
        };
        assert_eq!(mdec.call(&mut ram, &mut scratch, call), None);
        assert_eq!(mdec.quant_tables()[1][0], 64);
        assert_eq!(mdec.scale_table(), &STANDARD_SCALE);
        // The Japanese release keeps them 0x2180 higher; PAL's words there
        // are not read.
        let mut jp = MdecDevice::new();
        jp.set_release(DefaultTables::JP);
        assert_eq!(jp.release().quant - DEFAULT_QUANT, 0x2180);
        assert_eq!(jp.release().scale - DEFAULT_SCALE, 0x2180);
        let mut moved = vec![0u8; 0x20_0000];
        let jp_base = (DefaultTables::JP.quant & 0x1f_ffff) as usize;
        let jp_scale = (DefaultTables::JP.scale & 0x1f_ffff) as usize;
        moved[jp_base..jp_base + 0x84].copy_from_slice(&ram[base..base + 0x84]);
        moved[jp_scale..jp_scale + 0x84].copy_from_slice(&ram[scale..scale + 0x84]);
        assert_eq!(jp.call(&mut moved, &mut scratch, call), None);
        assert_eq!(jp.quant_tables(), mdec.quant_tables());
        assert_eq!(jp.scale_table(), &STANDARD_SCALE);
    }
}
