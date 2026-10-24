# ac3-graphics

Native PlayStation graphics for the Ace Combat 3 port: TIM loading, a VRAM
model, two packet renderers, fixed-point transforms, RTPS/RTPT projection and
integer scalar math. No MIPS or GPU timing emulation is involved, and no game
assets are embedded.

## `Gpu`: the complete GP0/GP1 stream into VRAM

```rust
use ac3_graphics::{Gpu, Vram};
# fn example(vram: Vram, gp0: &[u32]) -> ac3_graphics::Result<()> {
let mut gpu = Gpu::new(vram);
gpu.write_gp0_words(gp0)?; // DMA payloads, direct writes, A0 transfers
gpu.write_gp1(0x0500_0000)?; // display start
let shown = gpu.display_pixels(320, 256)?;
# let _ = shown;
# Ok(())
# }
```

`Gpu` draws into the whole 1024x512 VRAM: every polygon, line, polyline and
rectangle variant, E1-E6 state, GP0 02 fill, 80 VRAM copy, A0 upload and C0
readback (no data produced). Commands may be split across calls. Its rules
follow PCSX-Redux's software GPU at revision `e1693b7f`, including pair
versus single-pixel write differences, dithering, draw-area edge rules and
culling. `GpuStats` counts commands and rasterizer paths. Inputs that need
memory outside the 1024x512 VRAM fail with `Error::Unsupported`.

Original first-flight streams (1,164,404 GP0 words, 124,449 primitives, 278
game frames) reproduce every VRAM word and displayed pixel; see
`docs/FLIGHT_GPU.md`. Asset-free unit tests in `src/gpu_tests.rs` cover the
rules the flight streams do not exercise.

## `Frame`: the established menu/briefing renderer

```rust
use ac3_graphics::{Frame, Vram, render_packets, tim};
# fn example(tim_bytes: &[u8], packets: &[Vec<u32>]) -> ac3_graphics::Result<()> {
let image = tim::parse(tim_bytes)?;
let mut vram = Vram::new();
vram.load_tim(&image)?;
let mut frame = Frame::new(320, 512)?;
render_packets(&mut frame, &vram, packets)?;
let rgba_pixels = frame.rgba8();
# let _ = rgba_pixels;
# Ok(())
# }
```

`Frame` renders ordered packets into a separate framebuffer while sampling a
read-only (or upload-only) VRAM. Its behaviour is what the validated player
and the menu, hub, briefing and aircraft tests depend on; it is kept
unchanged. `tim::parse` borrows validated pixel/palette blocks with their
original VRAM coordinates, and `Vram::load_tim` writes pixels before the
palette, matching AC3.

## Tests

```sh
cargo test -p ac3-graphics
AC3_DATA_DIR=<project> cargo test --release -p ac3-graphics -- --include-ignored
```

The ignored tests compare against original captures under `analysis/`:
menu TIM uploads and frames (`local_data`, `quad_capture`), isolated and
natural transform/projection/scalar oracles, and the flight GPU replay
(`flight_frames`). See `docs/MENU_GRAPHICS.md` and `docs/FLIGHT_GPU.md` for
the evidence and its limits.
