//! On-screen messages: short notes drawn over the picture at window
//! resolution with the built-in [`font`](super::font).
//!
//! [`Osd`] keeps the messages and their expiry; [`Osd::frame`] rasterizes
//! the visible ones (one dark box per line, white text with a shadow) into
//! an RGBA [`Image`] at one pixel per font pixel. The renderer draws it
//! scaled by a whole factor ([`osd_scale`](super::scale::osd_scale)).
use super::font;
use std::time::{Duration, Instant};

/// Most messages shown at once; a new one pushes out the oldest.
pub const MAX_MESSAGES: usize = 4;
/// Messages fade out over the last this long.
pub const FADE: Duration = Duration::from_millis(400);
const PAD_X: usize = 3;
const PAD_Y: usize = 2;
const GAP: usize = 2;
/// Opacity of a line's box at full strength.
const BOX_ALPHA: u32 = 170;

/// Straight-alpha RGBA8 pixels, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let at = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[at],
            self.rgba[at + 1],
            self.rgba[at + 2],
            self.rgba[at + 3],
        ]
    }
}

#[derive(Clone, Debug)]
struct Message {
    text: String,
    until: Instant,
}

/// The messages on screen.
#[derive(Clone, Debug, Default)]
pub struct Osd {
    messages: Vec<Message>,
    /// A line that stays until replaced or cleared (the frame rate), above
    /// the messages.
    status: Option<String>,
    /// The lines and opacities of the last image handed out.
    shown: Vec<(String, u8)>,
}

impl Osd {
    /// Show `text` as the status line, or remove it with `None`.
    pub fn set_status(&mut self, text: Option<String>) {
        self.status = text;
    }

    /// Show `text` (one line) for `duration` from `now`.
    pub fn show(&mut self, text: impl Into<String>, duration: Duration, now: Instant) {
        if self.messages.len() == MAX_MESSAGES {
            self.messages.remove(0);
        }
        self.messages.push(Message {
            text: text.into(),
            until: now + duration,
        });
    }

    /// The visible lines at `now` with their opacity (fading over [`FADE`]);
    /// expired messages are dropped.
    pub fn lines(&mut self, now: Instant) -> Vec<(String, u8)> {
        self.messages.retain(|message| message.until > now);
        let status = self.status.iter().map(|text| (text.clone(), 255));
        status
            .chain(self.messages.iter().map(|message| {
                let left = message.until - now;
                let alpha = if left >= FADE {
                    255
                } else {
                    (255 * left.as_micros() / FADE.as_micros()) as u8
                };
                (message.text.clone(), alpha)
            }))
            .collect()
    }

    /// `Some` when the picture changed since the last call: the new image,
    /// or `None` once nothing is left to show.
    pub fn frame(&mut self, now: Instant) -> Option<Option<Image>> {
        let lines = self.lines(now);
        if lines == self.shown {
            return None;
        }
        self.shown = lines;
        Some((!self.shown.is_empty()).then(|| rasterize(&self.shown)))
    }
}

/// Lines of text with their opacity, as one image: a box per line, stacked
/// with a small gap.
pub fn rasterize(lines: &[(String, u8)]) -> Image {
    let box_height = font::GLYPH_HEIGHT + 2 * PAD_Y;
    let width = lines
        .iter()
        .map(|(text, _)| font::text_width(text) + 2 * PAD_X)
        .max()
        .unwrap_or(0);
    let height = (lines.len() * (box_height + GAP)).saturating_sub(GAP);
    let mut rgba = vec![0u8; width * height * 4];
    for (index, (text, alpha)) in lines.iter().enumerate() {
        let alpha = u32::from(*alpha);
        let top = index * (box_height + GAP);
        let box_width = font::text_width(text) + 2 * PAD_X;
        for y in 0..box_height {
            for x in 0..box_width {
                // Text, then its shadow one pixel down and right, then the box.
                let (tx, ty) = (x.wrapping_sub(PAD_X), y.wrapping_sub(PAD_Y));
                let color = if font::pixel(text, tx, ty) {
                    [255, 255, 255, alpha]
                } else if font::pixel(text, tx.wrapping_sub(1), ty.wrapping_sub(1)) {
                    [0, 0, 0, alpha]
                } else {
                    [16, 16, 24, BOX_ALPHA * alpha / 255]
                };
                let at = ((top + y) * width + x) * 4;
                for (channel, value) in color.into_iter().enumerate() {
                    rgba[at + channel] = value as u8;
                }
            }
        }
    }
    Image {
        width: width as u32,
        height: height as u32,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_expire_and_fade() {
        let start = Instant::now();
        let mut osd = Osd::default();
        assert_eq!(osd.frame(start), None, "nothing shown, nothing changed");
        osd.show("Quick save", Duration::from_secs(2), start);
        let image = osd.frame(start).unwrap().unwrap();
        assert_eq!(image.height as usize, font::GLYPH_HEIGHT + 2 * PAD_Y);
        assert_eq!(
            image.width as usize,
            font::text_width("Quick save") + 2 * PAD_X
        );
        assert_eq!(osd.frame(start + Duration::from_secs(1)), None, "unchanged");
        let fading = osd.lines(start + Duration::from_millis(1800));
        assert_eq!(fading.len(), 1);
        assert!(fading[0].1 > 100 && fading[0].1 < 155, "{}", fading[0].1);
        assert!(osd.frame(start + Duration::from_millis(1800)).is_some());
        assert_eq!(osd.frame(start + Duration::from_secs(2)), Some(None));
        assert_eq!(osd.frame(start + Duration::from_secs(3)), None);
    }

    #[test]
    fn at_most_four_messages() {
        let now = Instant::now();
        let mut osd = Osd::default();
        for i in 0..6 {
            osd.show(format!("message {i}"), Duration::from_secs(5), now);
        }
        let lines = osd.lines(now);
        assert_eq!(lines.len(), MAX_MESSAGES);
        assert_eq!(lines[0].0, "message 2");
    }

    #[test]
    fn status_line_stays_above_the_messages() {
        let now = Instant::now();
        let mut osd = Osd::default();
        osd.set_status(Some("25 fps".into()));
        osd.show("quick save", Duration::from_secs(1), now);
        let later = now + Duration::from_secs(10);
        assert_eq!(
            osd.lines(now),
            vec![("25 fps".into(), 255), ("quick save".into(), 255)]
        );
        assert_eq!(osd.lines(later), vec![("25 fps".into(), 255)]);
        osd.set_status(None);
        assert!(osd.lines(later).is_empty());
    }

    #[test]
    fn rasterized_text_box_and_shadow() {
        let image = rasterize(&[("I".into(), 255), ("Long line".into(), 128)]);
        let box_height = (font::GLYPH_HEIGHT + 2 * PAD_Y) as u32;
        assert_eq!(image.height, 2 * box_height + GAP as u32);
        // 'I' first row is .###.: text at column PAD_X + 1.
        let (x, y) = (PAD_X as u32 + 1, PAD_Y as u32);
        assert_eq!(image.pixel(x, y), [255, 255, 255, 255]);
        assert_eq!(image.pixel(0, 0), [16, 16, 24, BOX_ALPHA as u8], "box");
        // The shadow of the bottom of the 'I' (.###. on row 6).
        assert_eq!(image.pixel(x + 3, y + 7), [0, 0, 0, 255]);
        // Past the short line's box: transparent.
        assert_eq!(image.pixel(image.width - 1, 0), [0, 0, 0, 0]);
        // The gap between lines is transparent; the second box is dimmer.
        assert_eq!(image.pixel(0, box_height)[3], 0);
        let second = image.pixel(0, box_height + GAP as u32);
        assert_eq!(second[3], (BOX_ALPHA * 128 / 255) as u8);
    }
}
