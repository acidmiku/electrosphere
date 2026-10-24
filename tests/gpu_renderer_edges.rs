//! The GPU renderer's picture edges against the software renderer's, on a
//! synthetic frame built like a Japanese flight frame (traced from a JP
//! flight at 2x): a 320 x 240 display, double-buffered at VRAM y 0 and 256,
//! each buffer drawn inside a 240-line draw area with the offset at its
//! centre, and never cleared: the sky's polygons cover the whole buffer
//! over what earlier frames left there.
//!
//! Above 1x the renderer once drew every polygon half a 1x pixel right of
//! and below the pixel grid. A full-screen polygon then left the first N / 2
//! rows and columns of the draw area undrawn, so the picture's top rows and
//! left columns showed stale pixels (yellow rows and a grey column in JP
//! flight at 2x), and every polygon edge sat half a pixel off the 1x
//! picture's. Here every edge lies on whole pixels (axis-aligned, integer
//! positions), so at every scale each N x sample must show its 1x pixel,
//! within one 5-bit step per channel for shaded and blended colours (the
//! renderer interpolates at its own sample positions); at 1x exactly. No
//! sample may keep the stale colour, and the pixels just outside each draw
//! area (the 16 rows below a 240-line buffer, the columns right of it) must
//! stay untouched.
//!
//! Needs a graphics adapter; skipped without one unless `AC3_REQUIRE_GPU`
//! is set.
use ac3_graphics::{Gpu, Vram, rgba8};
use ac3_port::gpu_renderer::execute::offscreen_device;
use ac3_port::gpu_renderer::record::command_color;
use ac3_port::gpu_renderer::{DisplayArea, Executor, Options, Recorder};

/// The display: 320 x 240 from VRAM y 0 or 256.
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const BUFFERS: [u32; 2] = [0, 256];
/// What earlier frames left in a buffer: (255, 255, 132), the olive-yellow
/// of the stale rows in JP flight.
const STALE: u32 = 0x84_ffff;
/// The pixels just outside a draw area.
const MARK: u32 = 0xff_00ff;

fn xy(x: i32, y: i32) -> u32 {
    (x as u32 & 0xffff) | ((y as u32 & 0xffff) << 16)
}

fn uv(u: u32, v: u32) -> u32 {
    u | (v << 8)
}

/// A 24-bit command colour.
fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

/// A quad's corners (top left, top right, bottom left, bottom right) from
/// `x0, y0` to `x1, y1`.
fn corners(x0: i32, y0: i32, x1: i32, y1: i32) -> [u32; 4] {
    [xy(x0, y0), xy(x1, y0), xy(x0, y1), xy(x1, y1)]
}

/// A flat quad (GP0 28, or 2A semi-transparent).
fn flat_quad(op: u32, color: u32, [x0, y0, x1, y1]: [i32; 4]) -> Vec<u32> {
    let [a, b, c, d] = corners(x0, y0, x1, y1);
    vec![(op << 24) | color, a, b, c, d]
}

/// A Gouraud quad (GP0 38) with corner colours; planar when `colors[3]` is
/// `colors[1] + colors[2] - colors[0]`.
fn gouraud_quad(colors: [u32; 4], [x0, y0, x1, y1]: [i32; 4]) -> Vec<u32> {
    let [a, b, c, d] = corners(x0, y0, x1, y1);
    vec![
        0x3800_0000 | colors[0],
        a,
        colors[1],
        b,
        colors[2],
        c,
        colors[3],
        d,
    ]
}

/// A textured quad (GP0 2C modulated, 2D raw) mapping texels 1:1 from
/// `u0, v0` of 4-bit page 10 with the palette at (0, 480).
fn textured_quad(op: u32, color: u32, [x0, y0, x1, y1]: [i32; 4], [u0, v0]: [u32; 2]) -> Vec<u32> {
    let [a, b, c, d] = corners(x0, y0, x1, y1);
    let (u1, v1) = (u0 + (x1 - x0) as u32, v0 + (y1 - y0) as u32);
    let clut = 480 << 6;
    vec![
        (op << 24) | color,
        a,
        (clut << 16) | uv(u0, v0),
        b,
        (0x000a << 16) | uv(u1, v0),
        c,
        uv(u0, v1),
        d,
        uv(u1, v1),
    ]
}

/// A 4-bit texture of 160 x 96 texels at (640, 0) (page 10) and its 16
/// colours at (0, 480); no texel is 0 (transparent).
fn texture() -> Vec<u32> {
    let index = |u: u32, v: u32| 1 + (u * 3 + v * 5 + (u / 7) * (v / 5)) % 15;
    let mut words = vec![0xa000_0000, xy(640, 0), xy(40, 96)];
    for v in 0..96 {
        for pair in 0..20 {
            let u = pair * 8;
            words.push((0..8).fold(0, |word, k| word | (index(u + k, v) << (4 * k))));
        }
    }
    words.extend([0xa000_0000, xy(0, 480), xy(16, 1)]);
    let colour = |i: u32| ((i * 2) & 31) | (((31 - i) & 31) << 5) | (((i * 7) & 31) << 10);
    for i in (0..16).step_by(2) {
        words.push(colour(i) | (colour(i + 1) << 16));
    }
    words
}

/// The sky's colour at screen row `y` (-120..=120): a vertical gradient.
fn sky(y: i32) -> u32 {
    let t = (y + 120) as u32;
    rgb(64 + t * 96 / 240, 96 + t * 80 / 240, 160 + t * 64 / 240)
}

/// One frame into the buffer at VRAM row `top`, as the game draws it: what
/// earlier frames left, the drawing state, the sky over the whole screen,
/// the ground and a HUD along every edge; then the display on that buffer.
/// GP0 words, and GP1 words.
fn frame(top: u32) -> (Vec<u32>, Vec<u32>) {
    let t = top as i32;
    let mut words = vec![
        // Earlier frames' pixels, and marks around the draw area.
        0x0200_0000 | STALE,
        xy(0, t),
        xy(WIDTH as i32, HEIGHT as i32),
        0x0200_0000 | MARK,
        xy(WIDTH as i32, t),
        xy(16, HEIGHT as i32 + 16),
        0x0200_0000 | MARK,
        xy(0, t + HEIGHT as i32),
        xy(WIDTH as i32, 16),
        // Page 10, 4-bit, average blending, no dithering; the area and the
        // offset at its centre, as the JP flight's.
        0xe100_000a,
        0xe300_0000 | (top << 10),
        0xe400_0000 | (WIDTH - 1) | ((top + HEIGHT - 1) << 10),
        0xe500_0000 | (WIDTH / 2) | ((top + HEIGHT / 2) << 11),
    ];
    // The sky: four Gouraud bands over the whole screen.
    for band in 0..4 {
        let (y0, y1) = (-120 + 60 * band, -60 + 60 * band);
        let (c0, c1) = (sky(y0), sky(y1));
        words.extend(gouraud_quad([c0, c0, c1, c1], [-160, y0, 160, y1]));
    }
    // The ground: raw texels along the bottom and left edges.
    words.extend(textured_quad(0x2d, 0x80_8080, [-160, 30, -20, 120], [0, 0]));
    // The HUD: a box in the top-left corner, a semi-transparent bar on the
    // top edge, a line down the left edge, a modulated icon and a tile in
    // the top-right corner, a box in the bottom-right one and a shaded
    // gauge on the right edge.
    words.extend(flat_quad(0x28, rgb(64, 255, 64), [-160, -120, -100, -110]));
    words.extend(flat_quad(0x2a, rgb(255, 96, 32), [-40, -120, 40, -100]));
    words.extend([0x4000_0000 | rgb(48, 224, 48), xy(-160, -100), xy(-160, 20)]);
    words.extend(textured_quad(
        0x2c,
        rgb(128, 96, 64),
        [100, -120, 140, -100],
        [100, 60],
    ));
    words.extend([0x6000_0000 | rgb(255, 64, 64), xy(140, -120), xy(20, 8)]);
    words.extend(flat_quad(0x28, rgb(64, 192, 255), [120, 100, 160, 120]));
    let (a, b, c) = (rgb(255, 64, 64), rgb(128, 64, 192), rgb(192, 192, 64));
    words.extend(gouraud_quad(
        [a, b, c, rgb(65, 192, 192)],
        [130, -60, 160, 0],
    ));
    let display = vec![
        0x0500_0000 | (top << 10),
        0x06c4_8248,
        0x0704_0010,
        0x0800_0001,
    ];
    (words, display)
}

/// A 0RGB picture's 5-bit channels.
fn channels(pixel: u32) -> [i32; 3] {
    [16, 8, 0].map(|shift| ((pixel >> shift) & 255) as i32 >> 3)
}

fn zero_rgb(word: u16) -> u32 {
    let [r, g, b, _] = rgba8(word);
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

/// Whether sample (`x`, `y`) of a `width` x `height` picture lies within
/// `margin` samples of its edge.
fn on_edge(x: usize, y: usize, width: usize, height: usize, margin: usize) -> bool {
    x < margin || y < margin || x + margin >= width || y + margin >= height
}

/// GP0 `words` to the recorder (first) and the software GPU.
fn send(words: &[u32], gpu: &mut Gpu, recorder: &mut Recorder) {
    for &word in words {
        recorder.gp0(word, 0, gpu.vram());
        gpu.write_gp0(word).expect("software GPU");
    }
}

#[test]
fn picture_edges_match_the_software_picture_at_every_scale() {
    let (device, queue) = match offscreen_device() {
        Ok(pair) => pair,
        Err(error) => {
            assert!(
                std::env::var_os("AC3_REQUIRE_GPU").is_none(),
                "AC3_REQUIRE_GPU: {error}"
            );
            eprintln!("skipped: {error}");
            return;
        }
    };
    let stale = zero_rgb(command_color(STALE) as u16);
    for scale in [1u32, 2, 3, 4] {
        let options = Options {
            scale,
            dither: false,
        };
        let mut executor = Executor::new(
            device.clone(),
            queue.clone(),
            options,
            wgpu::TextureFormat::Rgba8Unorm,
        )
        .expect("executor");
        let mut gpu = Gpu::new(Vram::new());
        let mut recorder = Recorder::new(options, &gpu);
        send(&texture(), &mut gpu, &mut recorder);
        for top in BUFFERS {
            let (words, display) = frame(top);
            send(&words, &mut gpu, &mut recorder);
            for word in display {
                recorder.gp1(word, gpu.vram());
                gpu.write_gp1(word).expect("software GPU");
            }
            executor.execute(&recorder.take_frame(gpu.vram()));
            let area = DisplayArea {
                x: 0,
                y: top,
                width: WIDTH,
                height: HEIGHT,
            };
            let (picture, width, height) = executor.read_display(area).expect("picture");
            let n = scale as usize;
            assert_eq!((width, height), (WIDTH as usize * n, HEIGHT as usize * n));
            let software: Vec<u32> = gpu
                .display_pixels(WIDTH as usize, HEIGHT as usize)
                .unwrap()
                .into_iter()
                .map(zero_rgb)
                .collect();
            assert!(
                !software.contains(&stale),
                "the software picture is drawn over"
            );
            let at = |x: usize, y: usize| software[(y / n) * WIDTH as usize + x / n];
            // Samples off their 1x pixel (by more than one 5-bit step
            // above 1x), on the edges (N samples deep) and inside.
            let (mut edge, mut inside, mut stale_samples) = (Vec::new(), 0, 0);
            for y in 0..height {
                for x in 0..width {
                    let (got, want) = (picture[y * width + x], at(x, y));
                    stale_samples += usize::from(got == stale);
                    let off = if scale == 1 {
                        got != want
                    } else {
                        let (a, b) = (channels(got), channels(want));
                        (0..3).any(|c| (a[c] - b[c]).abs() > 1)
                    };
                    if !off {
                        continue;
                    }
                    if on_edge(x, y, width, height, n) {
                        edge.push((x, y, got, want));
                    } else {
                        inside += 1;
                    }
                }
            }
            assert_eq!(
                stale_samples, 0,
                "{scale}x, buffer at y {top}: {stale_samples} samples keep what earlier frames left"
            );
            assert!(
                edge.is_empty(),
                "{scale}x, buffer at y {top}: {} edge samples are not their 1x pixel (x, y, card, software): {:06x?}",
                edge.len(),
                &edge[..edge.len().min(8)]
            );
            assert_eq!(
                inside, 0,
                "{scale}x, buffer at y {top}: {inside} samples inside are not their 1x pixel"
            );
            // Nothing reached past the draw area: the marked columns right
            // of the buffer and the 16 rows below it are the software's.
            let card = executor.read_vram();
            let vram = gpu.vram().words();
            let wide = 1024 * n;
            for y in top as usize..(top + HEIGHT + 16) as usize {
                for x in 0..(WIDTH + 16) as usize {
                    if x < WIDTH as usize && y < (top + HEIGHT) as usize {
                        continue;
                    }
                    for (sx, sy) in (0..n).flat_map(|sy| (0..n).map(move |sx| (sx, sy))) {
                        assert_eq!(
                            card[(y * n + sy) * wide + x * n + sx],
                            vram[y * 1024 + x],
                            "{scale}x: VRAM pixel ({x}, {y}) outside the draw area, sample ({sx}, {sy})"
                        );
                    }
                }
            }
        }
    }
}
