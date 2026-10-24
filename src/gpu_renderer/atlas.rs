//! Texture replacement, the card's half: the replacement atlas and the slot
//! table the draw shader reads.
//!
//! - `atlas`: a 2D array texture (`R16Uint`) of square layers, up to 4096
//!   pixels a side, holding every replacement in use as PlayStation texels
//!   (or palette indices). Replacements are packed into shelves; layers are
//!   added as needed, up to [`BUDGET`] bytes.
//! - `slots`: one `Rgba32Uint` texel per replacement slot (1024 x 64):
//!   x | y << 16 in the layer, width | height << 16, the original's width |
//!   height << 16, layer | flags << 16 ([`PRESENT`], [`INDEXED`]). A slot
//!   without [`PRESENT`] (a replacement that does not fit) makes the shader
//!   draw the original texels.
//!
//! Every replacement is written once, before the first frame that draws it;
//! nothing is evicted until the pack is reloaded. Movie slots
//! ([`replace::is_movie_slot`]) are the exception: each keeps one place in
//! the atlas, written again with every new frame it gets (a place of
//! another size is packed anew; the old one stays unused until a reload),
//! so a clip of any length uses at most [`replace::MOVIE_SLOTS`] places.
//! The replacer never gives a movie slot a new frame while a primitive of
//! the pass being recorded draws it, and passes run in order, so every
//! draw reads the frame it was recorded with.
use super::replace::{self, Image};

/// Replacement atlas memory on the card at most.
pub const BUDGET: u64 = 1 << 30;
/// Largest atlas layer side.
pub const LAYER: u32 = 4096;
/// The slot table's width; slot `s` is texel (s % 1024, s / 1024).
pub const SLOT_COLUMNS: u32 = 1024;
const SLOT_ROWS: u32 = 64;
/// Slot flags (bits 16.. of the fourth word).
pub const PRESENT: u32 = 1;
pub const INDEXED: u32 = 2;

/// What the atlas holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct AtlasStats {
    /// Replacements placed, and those that did not fit (drawn as the original).
    pub placed: u64,
    pub rejected: u64,
    pub texels: u64,
    /// Movie frames written into their slot's place (counted in `placed`
    /// and `texels` too).
    pub movie_frames: u64,
    /// Layers on the card, their side and bytes.
    pub layers: u32,
    pub layer_size: u32,
    pub bytes: u64,
}

/// A row of replacements in a layer.
#[derive(Clone, Copy, Debug)]
struct Shelf {
    y: u32,
    height: u32,
    /// Where the next one goes.
    x: u32,
}

/// Shelf packing of square layers, without the card (tests use it alone).
#[derive(Clone, Debug)]
pub struct Packer {
    side: u32,
    max_layers: u32,
    shelves: Vec<Vec<Shelf>>,
    /// The first free row of each layer.
    tops: Vec<u32>,
    /// Each movie slot's place: layer, x, y, width, height.
    movies: [Option<[u32; 5]>; replace::MOVIE_SLOTS as usize],
}

impl Packer {
    pub fn new(side: u32, max_layers: u32) -> Self {
        Self {
            side,
            max_layers,
            shelves: Vec::new(),
            tops: Vec::new(),
            movies: [None; replace::MOVIE_SLOTS as usize],
        }
    }

    /// Layers in use.
    pub fn layers(&self) -> u32 {
        self.shelves.len() as u32
    }

    pub fn clear(&mut self) {
        self.shelves.clear();
        self.tops.clear();
        self.movies = [None; replace::MOVIE_SLOTS as usize];
    }

    /// The place (layer, x, y) of the image of replacement slot `slot`: a
    /// movie slot keeps its place while the size stays (its frames are
    /// written there in turn); anything else gets a new one
    /// ([`Self::place`]).
    pub fn place_slot(&mut self, slot: u32, width: u32, height: u32) -> Option<(u32, u32, u32)> {
        if !replace::is_movie_slot(slot) {
            return self.place(width, height);
        }
        let movie = (slot - replace::FIRST_MOVIE_SLOT) as usize;
        if let Some([layer, x, y, w, h]) = self.movies[movie]
            && (w, h) == (width, height)
        {
            return Some((layer, x, y));
        }
        let (layer, x, y) = self.place(width, height)?;
        self.movies[movie] = Some([layer, x, y, width, height]);
        Some((layer, x, y))
    }

    /// A place (layer, x, y) for `width` x `height` pixels, or `None` when
    /// it is larger than a layer or every layer is full.
    pub fn place(&mut self, width: u32, height: u32) -> Option<(u32, u32, u32)> {
        if width == 0 || height == 0 || width > self.side || height > self.side {
            return None;
        }
        // The lowest shelf it fits on (not one more than twice as high).
        let mut best: Option<(usize, usize)> = None;
        for (layer, shelves) in self.shelves.iter().enumerate() {
            for (index, shelf) in shelves.iter().enumerate() {
                let fits = shelf.height >= height
                    && shelf.height <= height * 2 + 8
                    && shelf.x + width <= self.side;
                let lower = best.is_none_or(|(l, i)| self.shelves[l][i].height > shelf.height);
                if fits && lower {
                    best = Some((layer, index));
                }
            }
        }
        if let Some((layer, index)) = best {
            let shelf = &mut self.shelves[layer][index];
            let x = shelf.x;
            shelf.x += width;
            return Some((layer as u32, x, shelf.y));
        }
        // A new shelf, rounded up to 8 rows, in the first layer with room.
        let height = height.next_multiple_of(8).min(self.side);
        let layer = match (0..self.shelves.len()).find(|&l| self.tops[l] + height <= self.side) {
            Some(layer) => layer,
            None if (self.shelves.len() as u32) < self.max_layers => {
                self.shelves.push(Vec::new());
                self.tops.push(0);
                self.shelves.len() - 1
            }
            None => return None,
        };
        let y = self.tops[layer];
        self.tops[layer] += height;
        self.shelves[layer].push(Shelf {
            y,
            height,
            x: width,
        });
        Some((layer as u32, 0, y))
    }
}

/// The atlas and slot table on the card.
pub struct Atlas {
    packer: Packer,
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    /// Layers in `texture` (0: a 1 x 1 stand-in, before any replacement).
    layers: u32,
    slots: wgpu::Texture,
    pub slot_view: wgpu::TextureView,
    full_reported: bool,
    pub stats: AtlasStats,
}

fn array_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

fn layers_texture(device: &wgpu::Device, side: u32, layers: u32) -> wgpu::Texture {
    use wgpu::TextureUsages as Usage;
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ac3 texture replacements"),
        size: wgpu::Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Uint,
        usage: Usage::TEXTURE_BINDING | Usage::COPY_DST | Usage::COPY_SRC,
        view_formats: &[],
    })
}

impl Atlas {
    pub fn new(device: &wgpu::Device) -> Self {
        let limits = device.limits();
        let side = LAYER.min(limits.max_texture_dimension_2d);
        let per_layer = u64::from(side) * u64::from(side) * 2;
        let max_layers = (BUDGET / per_layer)
            .min(u64::from(limits.max_texture_array_layers))
            .max(1) as u32;
        let texture = layers_texture(device, 1, 1);
        let slots = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ac3 texture replacement slots"),
            size: wgpu::Extent3d {
                width: SLOT_COLUMNS,
                height: SLOT_ROWS,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        Self {
            packer: Packer::new(side, max_layers),
            view: array_view(&texture),
            texture,
            layers: 0,
            slot_view: slots.create_view(&Default::default()),
            slots,
            full_reported: false,
            stats: AtlasStats {
                layer_size: side,
                ..AtlasStats::default()
            },
        }
    }

    /// Write `images` (after starting over when `reset`); returns whether
    /// the atlas texture was replaced (its bind group must be made again).
    pub fn add(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        images: &[Image],
        reset: bool,
    ) -> bool {
        if reset {
            self.packer.clear();
            self.full_reported = false;
        }
        let mut grown = false;
        for image in images {
            let (width, height) = (image.image.width, image.image.height);
            let place = self.packer.place_slot(image.slot, width, height);
            if place.is_some() && replace::is_movie_slot(image.slot) {
                self.stats.movie_frames += 1;
            }
            let slot = match place {
                Some((layer, x, y)) => {
                    if layer >= self.layers {
                        self.grow(device, queue, layer + 1);
                        grown = true;
                    }
                    let bytes: Vec<u8> = image
                        .image
                        .texels
                        .iter()
                        .flat_map(|texel| texel.to_le_bytes())
                        .collect();
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x, y, z: layer },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &bytes,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(width * 2),
                            rows_per_image: Some(height),
                        },
                        wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                    );
                    self.stats.placed += 1;
                    self.stats.texels += u64::from(width * height);
                    let flags = PRESENT | if image.image.indexed { INDEXED } else { 0 };
                    [
                        x | (y << 16),
                        width | (height << 16),
                        image.original[0] | (image.original[1] << 16),
                        layer | (flags << 16),
                    ]
                }
                None => {
                    self.stats.rejected += 1;
                    let side = self.packer.side;
                    if width > side || height > side {
                        eprintln!(
                            "ac3: texture pack: {} is {width}x{height}, larger than {side}x{side}; \
                             the original is drawn",
                            image.key
                        );
                    } else if !self.full_reported {
                        self.full_reported = true;
                        eprintln!(
                            "ac3: texture pack: the replacement atlas is full ({} MiB); \
                             further replacements draw the originals",
                            self.stats.bytes >> 20
                        );
                    }
                    [0; 4]
                }
            };
            if image.slot < SLOT_COLUMNS * SLOT_ROWS {
                let bytes: Vec<u8> = slot.iter().flat_map(|word| word.to_le_bytes()).collect();
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.slots,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: image.slot % SLOT_COLUMNS,
                            y: image.slot / SLOT_COLUMNS,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(16),
                        rows_per_image: Some(1),
                    },
                    wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        grown
    }

    /// Make the atlas at least `needed` layers deep (doubling), keeping
    /// what it holds. The copy is submitted at once, so writes made after
    /// it land in the new texture after the copy.
    fn grow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, needed: u32) {
        let side = self.packer.side;
        // Double, within the budget, and at least what is needed (which the
        // packer keeps within the budget).
        let doubled = (self.layers * 2).min(self.packer.max_layers);
        let layers = needed.max(doubled);
        let texture = layers_texture(device, side, layers);
        if self.layers > 0 {
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_texture(
                self.texture.as_image_copy(),
                texture.as_image_copy(),
                wgpu::Extent3d {
                    width: side,
                    height: side,
                    depth_or_array_layers: self.layers,
                },
            );
            queue.submit([encoder.finish()]);
        }
        self.view = array_view(&texture);
        self.texture = texture;
        self.layers = layers;
        self.stats.layers = layers;
        self.stats.bytes = u64::from(layers) * u64::from(side) * u64::from(side) * 2;
        eprintln!(
            "ac3: texture pack: replacement atlas {layers} layer{} of {side}x{side} ({} MiB, at most {} MiB)",
            if layers == 1 { "" } else { "s" },
            self.stats.bytes >> 20,
            (u64::from(self.packer.max_layers) * u64::from(side) * u64::from(side) * 2) >> 20
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelves_pack_and_fill_layers() {
        let mut packer = Packer::new(64, 2);
        assert_eq!(packer.place(32, 16), Some((0, 0, 0)));
        // The same height continues the shelf; a lower one opens another.
        assert_eq!(packer.place(32, 16), Some((0, 32, 0)));
        assert_eq!(packer.place(16, 5), Some((0, 0, 16)));
        assert_eq!(packer.place(16, 8), Some((0, 16, 16)));
        // Too large for a layer, or for any shelf: none, or a new layer.
        assert_eq!(packer.place(65, 1), None);
        assert_eq!(packer.place(64, 40), Some((0, 0, 24)));
        assert_eq!(packer.place(64, 64), Some((1, 0, 0)));
        assert_eq!(packer.layers(), 2);
        assert_eq!(packer.place(64, 64), None, "both layers full");
        // Small ones still fit beside others.
        assert_eq!(packer.place(16, 8), Some((0, 32, 16)));
        packer.clear();
        assert_eq!(packer.place(64, 64), Some((0, 0, 0)));
    }

    #[test]
    fn movie_slots_keep_their_place() {
        let first = replace::FIRST_MOVIE_SLOT;
        assert!(!replace::is_movie_slot(first - 1));
        assert!(replace::is_movie_slot(first + replace::MOVIE_SLOTS - 1));
        assert!(!replace::is_movie_slot(replace::MAX_SLOTS));
        let mut packer = Packer::new(64, 4);
        // Frame after frame in one slot: one place; another slot its own.
        let place = packer.place_slot(first, 32, 16);
        assert_eq!(place, Some((0, 0, 0)));
        for _ in 0..100 {
            assert_eq!(packer.place_slot(first, 32, 16), place);
        }
        assert_eq!(packer.place_slot(first + 1, 32, 16), Some((0, 32, 0)));
        // Pack textures always get a new place.
        assert_eq!(packer.place_slot(5, 32, 16), Some((0, 0, 16)));
        assert_eq!(packer.place_slot(5, 32, 16), Some((0, 32, 16)));
        // Another size: a new place, kept from then on.
        let wide = packer.place_slot(first, 64, 32);
        assert_eq!(wide, Some((0, 0, 32)));
        assert_eq!(packer.place_slot(first, 64, 32), wide);
        assert_eq!(packer.layers(), 1);
        // A reset forgets them.
        packer.clear();
        assert_eq!(packer.place_slot(first + 1, 8, 8), Some((0, 0, 0)));
        assert_eq!(packer.place_slot(first, 64, 32), Some((0, 0, 8)));
    }
}
