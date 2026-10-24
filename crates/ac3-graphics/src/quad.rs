use super::{BlendMode, Error, Frame, Result, TextureWindow, Vram, modulate};

#[derive(Clone, Copy)]
struct Vertex {
    x: i64,
    y: i64,
    u: i64,
    v: i64,
    rgb: [i64; 3],
}

fn edge(a: Vertex, b: Vertex, x: i64, y: i64) -> i64 {
    (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x)
}
fn top_left(a: Vertex, b: Vertex) -> bool {
    b.y < a.y || (b.y == a.y && b.x > a.x)
}

impl Frame {
    pub(super) fn draw_flat_triangle(
        &mut self,
        vram: &Vram,
        words: &[u32],
        tpage: u16,
        window: TextureWindow,
    ) -> Result<()> {
        let vertices = std::array::from_fn(|i| {
            let xy = self.position(words[i + 1]);
            Vertex {
                x: i64::from(xy[0]),
                y: i64::from(xy[1]),
                u: 0,
                v: 0,
                rgb: [0; 3],
            }
        });
        self.draw_triangle(vram, vertices, tpage, 0, words[0], window)
    }
    pub(super) fn draw_quad(
        &mut self,
        vram: &Vram,
        words: &[u32],
        tpage: u16,
        window: TextureWindow,
    ) -> Result<()> {
        let textured = words[0] & 0x0400_0000 != 0;
        if textured && (tpage >> 7) & 3 == 3 {
            return Err(Error::Unsupported("reserved texture depth"));
        }
        let vertices: [Vertex; 4] = std::array::from_fn(|i| {
            let xy = self.position(words[1 + i * if textured { 2 } else { 1 }]);
            let uv = if textured { words[2 + i * 2] } else { 0 };
            Vertex {
                x: i64::from(xy[0]),
                y: i64::from(xy[1]),
                u: i64::from(uv as u8),
                v: i64::from((uv >> 8) as u8),
                rgb: [0; 3],
            }
        });
        let clut = if textured { (words[2] >> 16) as u16 } else { 0 };
        let command = words[0];
        for triangle in [
            [vertices[0], vertices[1], vertices[2]],
            [vertices[1], vertices[2], vertices[3]],
        ] {
            self.draw_triangle(vram, triangle, tpage, clut, command, window)?;
        }
        Ok(())
    }

    pub(super) fn draw_gouraud(
        &mut self,
        vram: &Vram,
        words: &[u32],
        tpage: u16,
        window: TextureWindow,
    ) -> Result<()> {
        let vertices: Vec<_> = words
            .chunks_exact(2)
            .map(|pair| {
                let xy = self.position(pair[1]);
                Vertex {
                    x: i64::from(xy[0]),
                    y: i64::from(xy[1]),
                    u: 0,
                    v: 0,
                    rgb: std::array::from_fn(|i| i64::from((pair[0] >> (i * 8)) as u8)),
                }
            })
            .collect();
        // PSX quad strip diagonal: v1-v2. The second triangle is emitted first.
        if vertices.len() == 4 {
            self.draw_triangle(
                vram,
                [vertices[1], vertices[3], vertices[2]],
                tpage,
                0,
                words[0],
                window,
            )?;
        }
        self.draw_triangle(
            vram,
            [vertices[0], vertices[1], vertices[2]],
            tpage,
            0,
            words[0],
            window,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_triangle(
        &mut self,
        vram: &Vram,
        mut v: [Vertex; 3],
        tpage: u16,
        clut: u16,
        command: u32,
        window: TextureWindow,
    ) -> Result<()> {
        let area = edge(v[0], v[1], v[2].x, v[2].y);
        if area == 0 {
            return Ok(());
        }
        if area < 0 {
            v.swap(1, 2);
        }
        let min_x = v.iter().map(|v| v.x).min().unwrap();
        let max_x = v.iter().map(|v| v.x).max().unwrap();
        let min_y = v.iter().map(|v| v.y).min().unwrap();
        let max_y = v.iter().map(|v| v.y).max().unwrap();
        // The PSX drops primitives exceeding its coordinate-difference limits.
        if max_x - min_x > 1023 || max_y - min_y > 511 {
            return Ok(());
        }
        let color = [command as u8, (command >> 8) as u8, (command >> 16) as u8];
        let raw = command & 0x0100_0000 != 0;
        let semi = command & 0x0200_0000 != 0;
        let textured = command & 0x0400_0000 != 0;
        let gouraud = command & 0x1000_0000 != 0;
        // Affine span gradients and edge texture accumulators use 16.16.
        // Truncating each edge step matters at exact half-texel boundaries.
        let mut sorted = v;
        sorted.sort_by_key(|point| point.y);
        let [top, middle, bottom] = sorted;
        let fraction = ((middle.y - top.y) << 16) / (bottom.y - top.y);
        let span = ((top.x - middle.x) << 16) + fraction * (bottom.x - top.x);
        if span == 0 {
            return Ok(());
        }
        let span = if span < 0 {
            span.min(-4096)
        } else {
            span.max(4096)
        };
        let gradient =
            |a: i64, b: i64, c: i64| ((((a - b) << 16) + fraction * (c - a)) << 16) / span;
        let du = gradient(top.u, middle.u, bottom.u);
        let dv = gradient(top.v, middle.v, bottom.v);
        let dc: [i64; 3] =
            std::array::from_fn(|i| gradient(top.rgb[i], middle.rgb[i], bottom.rgb[i]));
        for y in min_y.max(0).max(i64::from(self.draw_area[1]))
            ..max_y
                .min(i64::from(self.height))
                .min(i64::from(self.draw_area[3]) + 1)
        {
            let left = [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])]
                .into_iter()
                .filter_map(|(mut a, mut b)| {
                    if a.y > b.y {
                        std::mem::swap(&mut a, &mut b);
                    }
                    if y < a.y || y >= b.y {
                        return None;
                    }
                    let row = y - a.y;
                    let height = b.y - a.y;
                    Some((
                        (a.x << 16) + ((b.x - a.x) << 16) * row / height,
                        (a.u << 16) + (((b.u - a.u) << 16) / height) * row,
                        (a.v << 16) + (((b.v - a.v) << 16) / height) * row,
                        std::array::from_fn::<_, 3, _>(|i| {
                            (a.rgb[i] << 16) + ((b.rgb[i] - a.rgb[i]) << 16) * row / height
                        }),
                    ))
                })
                .min_by_key(|edge| edge.0);
            let Some((left_x, left_u, left_v, left_rgb)) = left else {
                continue;
            };
            for x in min_x.max(0).max(i64::from(self.draw_area[0]))
                ..max_x
                    .min(i64::from(self.width))
                    .min(i64::from(self.draw_area[2]) + 1)
            {
                let weights = [
                    edge(v[1], v[2], x, y),
                    edge(v[2], v[0], x, y),
                    edge(v[0], v[1], x, y),
                ];
                let edges = [(v[1], v[2]), (v[2], v[0]), (v[0], v[1])];
                if weights
                    .iter()
                    .zip(edges)
                    .any(|(&w, (a, b))| w < 0 || (w == 0 && !top_left(a, b)))
                {
                    continue;
                }
                let distance = (x << 16) - left_x;
                let color = if gouraud {
                    const DITHER: [i64; 16] =
                        [-4, 0, -3, 1, 2, -2, 3, -1, -3, 1, -4, 0, 3, -1, 2, -2];
                    let bias = if tpage & 0x200 != 0 {
                        DITHER[((y & 3) * 4 + (x & 3)) as usize]
                    } else {
                        0
                    };
                    std::array::from_fn(|i| {
                        (((left_rgb[i] + ((distance * dc[i]) >> 16) + 0x8000) >> 16) + bias)
                            .clamp(0, 255) as u8
                    })
                } else {
                    color
                };
                let u = (left_u + ((distance * du) >> 16) + 0x8000) >> 16;
                let tv = (left_v + ((distance * dv) >> 16) + 0x8000) >> 16;
                let (u, tv) = window.apply(u as u8, tv as u8);
                let texel = if textured {
                    vram.sample(tpage, clut, u, tv)?
                } else {
                    0x8000
                };
                if textured && texel == 0 {
                    continue;
                }
                let front = if !textured {
                    u16::from(color[0] >> 3)
                        | (u16::from(color[1] >> 3) << 5)
                        | (u16::from(color[2] >> 3) << 10)
                } else if raw {
                    texel
                } else {
                    modulate(texel, color)
                };
                let index = y as usize * usize::from(self.width) + x as usize;
                let mode = (semi && texel & 0x8000 != 0).then(|| BlendMode::from_tpage(tpage));
                self.write_pixel(index, front, mode);
            }
        }
        Ok(())
    }
}
