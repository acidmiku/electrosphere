//! Asset dump, read only (Milestone 1 item 5); see `docs/ASSET_DUMP.md`.
//!
//! - [`dump_archive_images`] (`ac3 dump-assets --output DIR`): every image
//!   in the disc's resources, as PNG with a manifest of its sources.
//! - [`TextureRecorder`] (`ac3 machine --dump-textures DIR`): every texture
//!   the running game uploads and draws, one PNG per palette it is drawn
//!   with, named by the replacement key ([`key`]) that texture packs use
//!   (item 7, `gpu_renderer::replace`), and the screen coverage
//!   ([`coverage`]): which texture, primitive or upload each pixel of the
//!   shown picture came from.
//!
//! Neither changes what the game does: the static dump only reads files,
//! the recorder only observes uploads and GPU packets.
mod archive;
pub mod coverage;
pub mod key;
pub mod png_io;
mod runtime;

pub use archive::dump_archive_images;
pub use runtime::{TextureRecorder, state_folder};

/// The PNG alpha convention, as recorded in both manifests.
pub const ALPHA_NOTE: &str = "alpha 0: colour 0x0000, never drawn by textured primitives; \
alpha 128: STP (bit 15) set, blended when the primitive is semi-transparent, opaque otherwise; \
alpha 255: STP clear, always opaque. RGB = (c << 3) | (c >> 2) per 5-bit channel. \
4/8-bit textures are indexed PNGs (PLTE + tRNS) keeping the original indices.";

/// The key definition in one line, as recorded in both manifests.
pub const KEY_NOTE: &str = "ac3tex-<mode>-<T>[-<P>]-<W>x<H>: T = first 8 bytes (hex) of \
SHA-256(u16le upload width in VRAM words, u16le height, upload halfwords u16le row-major); \
P (p4/p8) = first 8 bytes of SHA-256(u8 lo, u8 hi, palette entries lo..=hi u16le), \
lo/hi = smallest/largest index in the whole upload; W x H = size in texels.";
