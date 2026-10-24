use super::{BlendMode, Frame};

impl Frame {
    /// Flat GP0 line, including the PSX shallow-octant pixel-center bias.
    pub(super) fn draw_line(&mut self, words: &[u32], tpage: u16) {
        let point = |word: u32| self.position(word).map(i32::from);
        let [mut x, mut y] = point(words[1]);
        let [mut end_x, mut end_y] = point(words[2]);
        if (end_x - x).abs() > 1023 || (end_y - y).abs() > 511 {
            return;
        }
        if x > end_x {
            std::mem::swap(&mut x, &mut end_x);
            std::mem::swap(&mut y, &mut end_y);
        }
        let dx = end_x - x;
        let dy = (end_y - y).abs();
        let sy = (end_y - y).signum();
        let axis = dx == 0 || dy == 0;
        let major_x = dx >= dy;
        let steps = dx.max(dy);
        let mut error = if major_x { 3 * dy - dx } else { 2 * dx - dy };
        let color = (words[0] as u16 >> 3) & 31
            | ((words[0] >> 11) as u16 & 31) << 5
            | ((words[0] >> 19) as u16 & 31) << 10;
        for step in 0..=steps {
            // Pinned soft GPU uses inclusive endpoints for axis lines and
            // exclusive bottom/right draw limits for octants and points.
            let exclusive = i32::from(!axis || steps == 0);
            if x >= self.draw_area[0].max(0)
                && y >= self.draw_area[1].max(0)
                && x <= (self.draw_area[2] - exclusive).min(i32::from(self.width) - 1)
                && y <= (self.draw_area[3] - exclusive).min(i32::from(self.height) - 1)
            {
                let index = y as usize * usize::from(self.width) + x as usize;
                let mode = (words[0] & 0x02000000 != 0).then(|| BlendMode::from_tpage(tpage));
                self.write_pixel(index, color, mode);
            }
            if step == steps {
                break;
            }
            if axis {
                if dx == 0 {
                    y += sy;
                } else {
                    x += 1;
                }
            } else if major_x {
                if error > 0 {
                    y += sy;
                    error -= 2 * dx;
                }
                error += 2 * dy;
                x += 1;
            } else {
                if error > 0 {
                    x += 1;
                    error -= 2 * dy;
                }
                error += 2 * dx;
                y += sy;
            }
        }
    }
}
