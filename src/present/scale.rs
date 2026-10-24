//! Where the picture goes in the window: pure integer math, shared by the
//! window and the offscreen tests.
use crate::settings::Scaling;

/// A rectangle in target pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
    /// Whether target pixel (`x`, `y`) lies inside.
    pub fn contains(&self, x: u32, y: u32) -> bool {
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }
}

fn centered(target: (u32, u32), width: u32, height: u32) -> Rect {
    let (width, height) = (width.min(target.0), height.min(target.1));
    Rect {
        x: (target.0 - width) / 2,
        y: (target.1 - height) / 2,
        width,
        height,
    }
}

/// The shape of the shown picture (width, height): a TV's 4:3, unless
/// widescreen shows flight wider.
pub const FOUR_BY_THREE: (u32, u32) = (4, 3);

/// The largest `aspect`-shaped rectangle that fits `target`, centered
/// (letterboxed or pillarboxed), rounded to the nearest whole pixel.
fn shaped(target: (u32, u32), aspect: (u32, u32)) -> Rect {
    let (width, height) = (u64::from(target.0), u64::from(target.1));
    let (aw, ah) = (u64::from(aspect.0.max(1)), u64::from(aspect.1.max(1)));
    if width * ah >= height * aw {
        // Wider than the aspect: full height, bars left and right.
        centered(target, ((2 * height * aw + ah) / (2 * ah)) as u32, target.1)
    } else {
        centered(target, target.0, ((2 * width * ah + aw) / (2 * aw)) as u32)
    }
}

/// Whole-pixel multiples of `frame` that fit `target` and come closest to
/// an `aspect` picture: the largest vertical multiple whose matching
/// horizontal multiple (the one nearest the aspect) still fits. PlayStation
/// pixels are not square (320, 512 and 640 wide modes all fill a 4:3 TV),
/// so the two axes get their own factors. `None` when not even 1× fits.
fn integer(target: (u32, u32), frame: (u32, u32), aspect: (u32, u32)) -> Option<Rect> {
    let (fw, fh) = (u64::from(frame.0), u64::from(frame.1));
    if fw == 0 || fh == 0 {
        return None;
    }
    let most_x = u64::from(target.0) / fw;
    let most_y = u64::from(target.1) / fh;
    if most_x == 0 || most_y == 0 {
        return None;
    }
    let (aw, ah) = (u64::from(aspect.0.max(1)), u64::from(aspect.1.max(1)));
    let pick = |ny: u64| {
        // Nearest whole nx to (aspect · fh · ny) / fw, at least 1.
        let nx = ((2 * aw * fh * ny + ah * fw) / (2 * ah * fw)).max(1);
        (nx <= most_x).then_some((nx, ny))
    };
    let (nx, ny) = (1..=most_y).rev().find_map(pick).unwrap_or((1, 1));
    Some(centered(target, (fw * nx) as u32, (fh * ny) as u32))
}

/// The rectangle the `frame`-sized picture, shown at `aspect`
/// ([`FOUR_BY_THREE`] except for widescreen flight), is drawn into on a
/// `target`-sized window or texture.
pub fn picture_rect(
    target: (u32, u32),
    frame: (u32, u32),
    scaling: Scaling,
    aspect: (u32, u32),
) -> Rect {
    if target.0 == 0 || target.1 == 0 {
        return Rect::default();
    }
    match scaling {
        Scaling::Stretch => Rect {
            x: 0,
            y: 0,
            width: target.0,
            height: target.1,
        },
        Scaling::Aspect => shaped(target, aspect),
        // A window smaller than the picture falls back to the plain shape.
        Scaling::Integer => {
            integer(target, frame, aspect).unwrap_or_else(|| shaped(target, aspect))
        }
    }
}

/// Pixel size of one font pixel of the on-screen messages for a target
/// `height` (1 up to 320 lines, 3 at 960, 6 at 2160).
pub fn osd_scale(height: u32) -> u32 {
    (height / 320).max(1)
}

/// The initial window size: `requested`, scaled down (keeping its shape)
/// when it would not fit in 90% of the `monitor`.
pub fn fit_window(requested: (u32, u32), monitor: Option<(u32, u32)>) -> (u32, u32) {
    let Some((mw, mh)) = monitor.filter(|&(w, h)| w > 0 && h > 0) else {
        return requested;
    };
    let (w, h) = (u64::from(requested.0), u64::from(requested.1));
    let (room_w, room_h) = (u64::from(mw) * 9 / 10, u64::from(mh) * 9 / 10);
    if w <= room_w && h <= room_h || w == 0 || h == 0 {
        return requested;
    }
    // Scale by min(room_w / w, room_h / h).
    if room_w * h <= room_h * w {
        (room_w as u32, (h * room_w / w).max(1) as u32)
    } else {
        ((w * room_h / h).max(1) as u32, room_h as u32)
    }
}

/// The window to open for widescreen at `aspect`: a window left at the
/// default 4:3 size (`default`) becomes as tall and `aspect` wide, so
/// flight fills it; any other size is kept, as the user chose it.
pub fn widescreen_window(
    requested: (u32, u32),
    default: (u32, u32),
    aspect: Option<(u32, u32)>,
) -> (u32, u32) {
    match aspect {
        Some((aw, ah)) if requested == default && aw > 0 && ah > 0 => {
            let height = u64::from(requested.1);
            let width = (2 * height * u64::from(aw) + u64::from(ah)) / (2 * u64::from(ah));
            (width as u32, requested.1)
        }
        _ => requested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn aspect_letterboxes_to_four_by_three() {
        let frame = (320, 240);
        let four_by_three =
            |target, frame| picture_rect(target, frame, Scaling::Aspect, FOUR_BY_THREE);
        assert_eq!(four_by_three((1280, 960), frame), rect(0, 0, 1280, 960));
        // 16:9: pillarbox, 1080 * 4 / 3 = 1440.
        assert_eq!(four_by_three((1920, 1080), frame), rect(240, 0, 1440, 1080));
        // Taller than 4:3: letterbox, 800 * 3 / 4 = 600.
        assert_eq!(four_by_three((800, 1000), frame), rect(0, 200, 800, 600));
        // Rounding to whole pixels: 1001 * 4 / 3 = 1334.67.
        assert_eq!(four_by_three((2000, 1001), frame), rect(332, 0, 1335, 1001));
        // The frame size does not matter: the whole picture is the TV.
        assert_eq!(
            four_by_three((1920, 1080), (512, 256)),
            rect(240, 0, 1440, 1080)
        );
    }

    #[test]
    fn widescreen_fills_its_shape() {
        let frame = (320, 256);
        let wide = |target, aspect| picture_rect(target, frame, Scaling::Aspect, aspect);
        // 16:9 fills a 16:9 window; in a 4:3 one it is letterboxed.
        assert_eq!(wide((1920, 1080), (16, 9)), rect(0, 0, 1920, 1080));
        assert_eq!(wide((1280, 960), (16, 9)), rect(0, 120, 1280, 720));
        // 16:10 in 16:9: pillarboxed, 1080 * 1.6 = 1728.
        assert_eq!(wide((1920, 1080), (16, 10)), rect(96, 0, 1728, 1080));
        // 64:27 in 2560x1080 exactly; in 3440x1440 (43:18), thin bars.
        assert_eq!(wide((2560, 1080), (64, 27)), rect(0, 0, 2560, 1080));
        assert_eq!(wide((3440, 1440), (64, 27)), rect(13, 0, 3413, 1440));
        // Integer: whole multiples nearest 16:9 per axis. In 1920x1080, 4
        // rows (1024 lines) and 16/9 * 1024 / 320 = 5.69 -> 6 columns.
        assert_eq!(
            picture_rect((1920, 1080), frame, Scaling::Integer, (16, 9)),
            rect(0, 28, 1920, 1024)
        );
        // In 2560x1440: 5 rows, 16/9 * 1280 / 320 = 7.11 -> 7 columns.
        assert_eq!(
            picture_rect((2560, 1440), frame, Scaling::Integer, (16, 9)),
            rect(160, 80, 2240, 1280)
        );
    }

    #[test]
    fn stretch_fills_the_target() {
        assert_eq!(
            picture_rect((1366, 768), (320, 256), Scaling::Stretch, FOUR_BY_THREE),
            rect(0, 0, 1366, 768)
        );
    }

    #[test]
    fn integer_takes_whole_multiples_near_four_by_three() {
        let integer = |target, frame| picture_rect(target, frame, Scaling::Integer, FOUR_BY_THREE);
        // 320x240 is square-pixel 4:3: 4x both ways in 1280x960.
        assert_eq!(integer((1280, 960), (320, 240)), rect(0, 0, 1280, 960));
        // 1920x1080: 4x (960 lines), centered.
        assert_eq!(integer((1920, 1080), (320, 240)), rect(320, 60, 1280, 960));
        // PAL 320x256: 3x (768 lines); 4/3 * 768 / 320 = 3.2 -> 3.
        assert_eq!(integer((1280, 960), (320, 256)), rect(160, 96, 960, 768));
        // 640x480: 2x.
        assert_eq!(integer((1280, 960), (640, 480)), rect(0, 0, 1280, 960));
        // 512x256 (non-square pixels): 3x tall, 2x wide = 1024x768, not
        // the squashed 2x2 = 1024x512.
        assert_eq!(integer((1280, 960), (512, 256)), rect(128, 96, 1024, 768));
        // 512x240 in 1280x960: 4x tall wants 3x wide (1536), too wide;
        // 3x tall with 2x wide fits.
        assert_eq!(integer((1280, 960), (512, 240)), rect(128, 120, 1024, 720));
        // Exact fit at 1x.
        assert_eq!(integer((320, 240), (320, 240)), rect(0, 0, 320, 240));
        // Too small for 1x: falls back to 4:3.
        assert_eq!(integer((200, 150), (320, 240)), rect(0, 0, 200, 150));
    }

    #[test]
    fn empty_targets_and_frames() {
        for scaling in [Scaling::Aspect, Scaling::Stretch, Scaling::Integer] {
            assert!(picture_rect((0, 600), (320, 240), scaling, FOUR_BY_THREE).is_empty());
            assert!(picture_rect((800, 0), (320, 240), scaling, FOUR_BY_THREE).is_empty());
        }
        assert_eq!(
            picture_rect((800, 600), (0, 0), Scaling::Integer, FOUR_BY_THREE),
            rect(0, 0, 800, 600)
        );
    }

    #[test]
    fn rect_contains_its_pixels() {
        let r = rect(2, 3, 4, 5);
        assert!(r.contains(2, 3) && r.contains(5, 7));
        assert!(!r.contains(6, 3) && !r.contains(2, 8) && !r.contains(1, 3));
    }

    #[test]
    fn osd_scale_follows_the_height() {
        assert_eq!(osd_scale(240), 1);
        assert_eq!(osd_scale(960), 3);
        assert_eq!(osd_scale(1080), 3);
        assert_eq!(osd_scale(2160), 6);
    }

    #[test]
    fn windows_shrink_to_fit_the_monitor() {
        assert_eq!(fit_window((1280, 960), Some((1920, 1080))), (1280, 960));
        assert_eq!(fit_window((1280, 960), Some((1280, 1024))), (1152, 864));
        assert_eq!(fit_window((1280, 960), Some((1366, 768))), (921, 691));
        assert_eq!(fit_window((1280, 960), None), (1280, 960));
        assert_eq!(fit_window((3000, 1000), Some((1920, 1080))), (1728, 576));
    }

    #[test]
    fn widescreen_windows_take_the_shape() {
        let default = (1280, 960);
        assert_eq!(widescreen_window(default, default, None), default);
        assert_eq!(
            widescreen_window(default, default, Some((16, 9))),
            (1707, 960)
        );
        assert_eq!(
            widescreen_window(default, default, Some((64, 27))),
            (2276, 960)
        );
        // A size the user set is kept.
        assert_eq!(
            widescreen_window((1600, 900), default, Some((16, 10))),
            (1600, 900)
        );
    }
}
