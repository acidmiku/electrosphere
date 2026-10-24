//! Rectangles and coarse VRAM bitmaps (16 x 16-pixel tiles of the 1 x
//! VRAM, one 64-bit word per row of tiles), for the recorder's two kinds of
//! bookkeeping: what was drawn on the card since the last `shadow` update,
//! and where the 1 x `mirror` may differ from the software VRAM.

/// VRAM size in 1 x pixels.
pub const WIDTH: i32 = 1024;
pub const HEIGHT: i32 = 512;
/// Tile size in pixels.
pub const TILE: i32 = 16;
const ROWS: usize = (HEIGHT / TILE) as usize;

/// A rectangle in 1 x VRAM pixels; `w` or `h` 0 is empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const VRAM: Rect = Rect {
        x: 0,
        y: 0,
        w: WIDTH,
        h: HEIGHT,
    };

    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    /// The rectangle from (`x0`, `y0`) to (`x1`, `y1`), both inclusive.
    pub fn inclusive(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self {
            x: x0,
            y: y0,
            w: (x1 - x0 + 1).max(0),
            h: (y1 - y0 + 1).max(0),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    /// The common part of two rectangles (empty when they do not meet).
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect {
            x,
            y,
            w: (right - x).max(0),
            h: (bottom - y).max(0),
        }
    }
    /// Clipped to VRAM.
    pub fn clipped(&self) -> Rect {
        self.intersect(&Rect::VRAM)
    }
    pub fn overlaps(&self, other: &Rect) -> bool {
        !self.intersect(other).is_empty()
    }
}

/// One bit per 16 x 16 tile of VRAM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tiles([u64; ROWS]);

impl Default for Tiles {
    fn default() -> Self {
        Self::empty()
    }
}

impl Tiles {
    pub fn empty() -> Self {
        Self([0; ROWS])
    }
    pub fn full() -> Self {
        Self([u64::MAX; ROWS])
    }
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|&row| row == 0)
    }
    /// The tile rows and column mask that `rect` touches.
    fn span(rect: &Rect) -> Option<(std::ops::Range<usize>, u64)> {
        let rect = rect.clipped();
        if rect.is_empty() {
            return None;
        }
        let first = rect.x / TILE;
        let last = (rect.right() - 1) / TILE;
        let mask = if last - first == 63 {
            u64::MAX
        } else {
            ((1u64 << (last - first + 1)) - 1) << first
        };
        let rows = (rect.y / TILE) as usize..((rect.bottom() - 1) / TILE + 1) as usize;
        Some((rows, mask))
    }
    /// Set every tile set in `other`.
    pub fn add(&mut self, other: &Tiles) {
        for (row, more) in self.0.iter_mut().zip(other.0) {
            *row |= more;
        }
    }
    /// Set every tile `rect` touches.
    pub fn mark(&mut self, rect: &Rect) {
        if let Some((rows, mask)) = Self::span(rect) {
            for row in &mut self.0[rows] {
                *row |= mask;
            }
        }
    }
    /// Whether a set tile touches `rect`.
    pub fn intersects(&self, rect: &Rect) -> bool {
        Self::span(rect).is_some_and(|(rows, mask)| self.0[rows].iter().any(|row| row & mask != 0))
    }
    /// Clear the tiles that lie wholly inside `rect`.
    pub fn clear_inside(&mut self, rect: &Rect) {
        let rect = rect.clipped();
        // Whole tiles: round the start up and the end down.
        let x0 = (rect.x + TILE - 1) / TILE;
        let y0 = (rect.y + TILE - 1) / TILE;
        let x1 = rect.right() / TILE;
        let y1 = rect.bottom() / TILE;
        if rect.is_empty() || x1 <= x0 || y1 <= y0 {
            return;
        }
        let mask = if x1 - x0 == 64 {
            u64::MAX
        } else {
            ((1u64 << (x1 - x0)) - 1) << x0
        };
        for row in &mut self.0[y0 as usize..y1 as usize] {
            *row &= !mask;
        }
    }
    /// The set tiles that touch `within`, as whole-tile rectangles: runs
    /// along each row of tiles, joined with the rows below when they span
    /// the same columns.
    pub fn rects(&self, within: &Rect) -> Vec<Rect> {
        let Some((rows, mask)) = Self::span(within) else {
            return Vec::new();
        };
        let mut found: Vec<Rect> = Vec::new();
        // Runs of the previous tile row, still open for joining.
        let mut open: Vec<usize> = Vec::new();
        for row in rows {
            let mut bits = self.0[row] & mask;
            let mut next_open = Vec::new();
            while bits != 0 {
                let start = bits.trailing_zeros() as i32;
                let length = (!(bits >> start)).trailing_zeros() as i32;
                bits &= !(if length == 64 {
                    u64::MAX
                } else {
                    ((1u64 << length) - 1) << start
                });
                let (x, w, y) = (start * TILE, length * TILE, row as i32 * TILE);
                match open
                    .iter()
                    .copied()
                    .find(|&i| found[i].x == x && found[i].w == w && found[i].bottom() == y)
                {
                    Some(i) => {
                        found[i].h += TILE;
                        next_open.push(i);
                    }
                    None => {
                        next_open.push(found.len());
                        found.push(Rect::new(x, y, w, TILE));
                    }
                }
            }
            open = next_open;
        }
        found
    }
    /// Clear the set tiles that touch `within`; returns them as [`rects`](Self::rects).
    pub fn take(&mut self, within: &Rect) -> Vec<Rect> {
        let found = self.rects(within);
        for rect in &found {
            self.clear_inside(rect);
        }
        found
    }
    /// Clear every tile; returns the set ones as rectangles.
    pub fn take_all(&mut self) -> Vec<Rect> {
        self.take(&Rect::VRAM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_and_tests_by_tile() {
        let mut tiles = Tiles::empty();
        assert!(tiles.is_empty());
        tiles.mark(&Rect::new(20, 5, 1, 1));
        assert!(tiles.intersects(&Rect::new(16, 0, 16, 16)));
        assert!(tiles.intersects(&Rect::new(31, 15, 100, 100)));
        assert!(!tiles.intersects(&Rect::new(0, 0, 16, 16)));
        assert!(!tiles.intersects(&Rect::new(32, 0, 16, 16)));
        assert!(!tiles.intersects(&Rect::new(16, 16, 16, 16)));
        // Outside VRAM or empty: nothing.
        assert!(!tiles.intersects(&Rect::new(-50, -50, 10, 10)));
        assert!(!tiles.intersects(&Rect::new(20, 5, 0, 4)));
        tiles.mark(&Rect::new(1000, 500, 100, 100));
        assert!(tiles.intersects(&Rect::new(1023, 511, 1, 1)));
        // The last column of tiles (bit 63).
        assert_eq!(tiles.0[31], 1 << 62 | 1 << 63);
    }

    #[test]
    fn union() {
        let mut a = Tiles::empty();
        a.mark(&Rect::new(0, 0, 1, 1));
        let mut b = Tiles::empty();
        b.mark(&Rect::new(1023, 511, 1, 1));
        a.add(&b);
        assert_eq!(
            a.rects(&Rect::VRAM),
            [Rect::new(0, 0, 16, 16), Rect::new(1008, 496, 16, 16)]
        );
    }

    #[test]
    fn whole_vram_rows_use_every_bit() {
        let mut tiles = Tiles::empty();
        tiles.mark(&Rect::VRAM);
        assert_eq!(tiles, Tiles::full());
        tiles.clear_inside(&Rect::new(0, 0, 1024, 16));
        assert_eq!(tiles.0[0], 0);
        assert_eq!(tiles.0[1], u64::MAX);
    }

    #[test]
    fn clear_inside_keeps_partly_covered_tiles() {
        let mut tiles = Tiles::full();
        // Columns 1..3 fully, 0 and 3 partly; rows 1 fully.
        tiles.clear_inside(&Rect::new(8, 16, 50, 20));
        assert_eq!(tiles.0[0], u64::MAX);
        assert_eq!(tiles.0[1], !0b110);
        assert_eq!(tiles.0[2], u64::MAX);
    }

    #[test]
    fn rects_join_rows_with_the_same_runs() {
        let mut tiles = Tiles::empty();
        tiles.mark(&Rect::new(0, 0, 64, 48));
        tiles.mark(&Rect::new(320, 16, 16, 16));
        let rects = tiles.rects(&Rect::VRAM);
        assert_eq!(
            rects,
            vec![Rect::new(0, 0, 64, 48), Rect::new(320, 16, 16, 16)]
        );
        // Only the tiles touching `within`.
        assert_eq!(
            tiles.rects(&Rect::new(300, 0, 40, 20)),
            vec![Rect::new(320, 16, 16, 16)]
        );
        // Taking within a rectangle takes the tiles it touches.
        let taken = tiles.take(&Rect::new(0, 0, 10, 10));
        assert_eq!(taken, vec![Rect::new(0, 0, 16, 16)]);
        assert_eq!(
            tiles.take_all(),
            vec![
                Rect::new(16, 0, 48, 16),
                Rect::new(0, 16, 64, 32),
                Rect::new(320, 16, 16, 16)
            ]
        );
        assert!(tiles.is_empty());
        // A full row of tiles is one run.
        tiles.mark(&Rect::new(0, 0, 1024, 1));
        assert_eq!(tiles.take_all(), vec![Rect::new(0, 0, 1024, 16)]);
    }

    #[test]
    fn rectangles() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.intersect(&Rect::new(5, 5, 10, 10)), Rect::new(5, 5, 5, 5));
        assert!(a.intersect(&Rect::new(10, 0, 5, 5)).is_empty());
        assert_eq!(Rect::inclusive(2, 3, 2, 3), Rect::new(2, 3, 1, 1));
        assert!(Rect::inclusive(5, 0, 4, 0).is_empty());
        assert_eq!(
            Rect::new(-5, 500, 20, 20).clipped(),
            Rect::new(0, 500, 15, 12)
        );
    }
}
