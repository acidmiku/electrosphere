//! The launcher's look: a dark navy page beside the background art, lime
//! accents, thin type; and the widgets the pages share (sections, labelled
//! rows, switches, the Play button).
use egui::{Color32, CornerRadius, Response, RichText, Sense, Stroke, Ui, Vec2, vec2};

/// The page behind everything.
pub const BACKGROUND: Color32 = Color32::from_rgb(0x08, 0x12, 0x1c);
/// Panels and fields.
pub const PANEL: Color32 = Color32::from_rgb(0x0e, 0x1d, 0x2a);
pub const FIELD: Color32 = Color32::from_rgb(0x12, 0x24, 0x33);
pub const LINE: Color32 = Color32::from_rgb(0x24, 0x3b, 0x4e);
pub const TEXT: Color32 = Color32::from_rgb(0xe2, 0xea, 0xf0);
pub const MUTED: Color32 = Color32::from_rgb(0x8d, 0xa2, 0xb4);
/// The lime of Play, switches and the page tab.
pub const ACCENT: Color32 = Color32::from_rgb(0xc8, 0xf2, 0x3c);
/// Text on the accent.
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x0a, 0x18, 0x10);
/// The selected choice of a row (a teal field).
pub const SELECTED: Color32 = Color32::from_rgb(0x12, 0x4a, 0x63);
pub const GOOD: Color32 = Color32::from_rgb(0x9b, 0xe1, 0x5d);
pub const WARN: Color32 = Color32::from_rgb(0xf5, 0xa6, 0x23);
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x6b, 0x5e);

/// The label column of the rows.
pub const LABELS: f32 = 170.0;
/// The width of the pages' column (the art shows beside it).
pub const COLUMN: f32 = 760.0;
/// The Play page's narrower column, clear of the art's face.
pub const PLAY_COLUMN: f32 = 520.0;

/// Visuals and text sizes for `ctx`.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        for (text_style, font) in style.text_styles.iter_mut() {
            font.size = match text_style {
                egui::TextStyle::Small => 12.0,
                egui::TextStyle::Heading => 34.0,
                egui::TextStyle::Monospace => 13.0,
                egui::TextStyle::Button => 15.0,
                _ => 15.0,
            };
        }
        style.spacing.button_padding = vec2(14.0, 7.0);
        style.spacing.item_spacing = vec2(10.0, 8.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.combo_width = 240.0;
        let visuals = &mut style.visuals;
        visuals.panel_fill = Color32::TRANSPARENT;
        visuals.window_fill = PANEL;
        visuals.extreme_bg_color = FIELD;
        visuals.faint_bg_color = PANEL;
        visuals.override_text_color = Some(TEXT);
        visuals.hyperlink_color = ACCENT;
        visuals.selection.bg_fill = SELECTED;
        visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        visuals.warn_fg_color = WARN;
        visuals.error_fg_color = ERROR;
        let radius = CornerRadius::same(4);
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = radius;
        }
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        visuals.widgets.inactive.bg_fill = FIELD;
        visuals.widgets.inactive.weak_bg_fill = FIELD;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x18, 0x31, 0x44);
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x18, 0x31, 0x44);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, MUTED);
        visuals.widgets.active.bg_fill = SELECTED;
        visuals.widgets.active.weak_bg_fill = SELECTED;
        visuals.widgets.open.bg_fill = FIELD;
        visuals.widgets.open.weak_bg_fill = FIELD;
    });
}

/// `text` with a space between its letters and three between its words
/// (the logo, section names).
pub fn spaced(text: &str) -> String {
    let mut out = String::new();
    let mut previous: Option<char> = None;
    for c in text.chars() {
        match (previous, c) {
            (_, ' ') => out.push_str("   "),
            (Some(p), _) if p != ' ' => {
                out.push(' ');
                out.push(c);
            }
            _ => out.push(c),
        }
        previous = Some(c);
    }
    out
}

/// A section: its name in spaced capitals over a rule.
pub fn section(ui: &mut Ui, name: &str) {
    ui.add_space(14.0);
    ui.label(
        RichText::new(spaced(&name.to_uppercase()))
            .size(13.0)
            .color(MUTED),
    );
    ui.add_space(2.0);
}

/// A thin rule across the column.
pub fn rule(ui: &mut Ui) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, LINE);
    ui.add_space(6.0);
}

/// A row: `label` in the label column, then `add`'s controls.
pub fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(LABELS, 30.0), Sense::hover());
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(15.5),
            TEXT,
        );
        add(ui)
    })
    .inner
}

/// A muted note under a row, indented to the controls.
pub fn note(ui: &mut Ui, text: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.add_space(LABELS + ui.spacing().item_spacing.x);
        ui.label(RichText::new(text.into()).size(13.0).color(MUTED));
    });
}

/// An on/off switch; its state in words after it.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let size = vec2(52.0, 28.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let radius = rect.height() / 2.0;
    let track = lerp_colour(FIELD, Color32::from_rgb(0x2d, 0x4a, 0x1c), t);
    let stroke = lerp_colour(LINE, ACCENT, t);
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(radius as u8),
        track,
        Stroke::new(1.5, stroke),
        egui::StrokeKind::Inside,
    );
    let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    let knob = lerp_colour(MUTED, ACCENT, t);
    painter.circle_filled(egui::pos2(x, rect.center().y), radius - 5.0, knob);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A switch with "On" / "Off" after it.
pub fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    let response = toggle(ui, on);
    ui.label(RichText::new(if *on { "On" } else { "Off" }).color(MUTED));
    response
}

/// One of a row of choices, as a segment.
pub fn segment<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, choice: T, text: &str) -> Response {
    let selected = *value == choice;
    let button = egui::Button::new(RichText::new(text).color(if selected { TEXT } else { MUTED }))
        .fill(if selected { SELECTED } else { FIELD })
        .stroke(Stroke::new(1.0, if selected { ACCENT } else { LINE }))
        .min_size(vec2(0.0, 32.0));
    let mut response = ui.add(button);
    if response.clicked() && !selected {
        *value = choice;
        response.mark_changed();
    }
    response
}

/// A plain outlined button.
pub fn button(ui: &mut Ui, text: &str, width: f32) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).size(15.5))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, MUTED))
            .min_size(vec2(width, 44.0)),
    )
}

/// The Play button: "P L A Y" and a play triangle, painted (the default
/// font has no triangle). Disabled, it only shows.
pub fn play_button(ui: &mut Ui, enabled: bool) -> Response {
    let size = vec2(240.0, 60.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let hovered = enabled && response.hovered();
    let fill = match (enabled, hovered) {
        (false, _) => FIELD,
        (true, false) => ACCENT,
        (true, true) => Color32::from_rgb(0xd8, 0xff, 0x5c),
    };
    let colour = if enabled { ON_ACCENT } else { MUTED };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    let galley = painter.layout_no_wrap(spaced("PLAY"), egui::FontId::proportional(24.0), colour);
    let (gap, side) = (20.0, 16.0);
    let left = rect.center().x - (galley.size().x + gap + side) / 2.0;
    let top = rect.center().y - galley.size().y / 2.0;
    let text_width = galley.size().x;
    painter.galley(egui::pos2(left, top), galley, colour);
    let x = left + text_width + gap;
    let y = rect.center().y;
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(x, y - side * 0.62),
            egui::pos2(x + side, y),
            egui::pos2(x, y + side * 0.62),
        ],
        colour,
        Stroke::NONE,
    ));
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A coloured dot and `text`.
pub fn status(ui: &mut Ui, colour: Color32, text: &str) -> Response {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 6.0, colour);
        ui.label(RichText::new(text).color(TEXT))
    })
    .inner
}

/// A collapsing block in the column's style: its title, a muted summary
/// and a chevron; the body below when open.
pub fn fold<R>(
    ui: &mut Ui,
    id: &str,
    title: &str,
    summary: &str,
    body: impl FnOnce(&mut Ui) -> R,
) -> (Response, Option<R>) {
    let id = ui.make_persistent_id(id);
    let mut open = ui.data_mut(|data| *data.get_persisted_mut_or(id, false));
    let width = ui.available_width().min(COLUMN);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 52.0), Sense::click());
    if response.clicked() {
        open = !open;
        ui.data_mut(|data| data.insert_persisted(id, open));
    }
    let hovered = response.hovered();
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(6),
        if hovered {
            FIELD
        } else {
            PANEL.gamma_multiply(0.85)
        },
        Stroke::new(1.0, LINE),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.left_center() + vec2(18.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(19.0),
        TEXT,
    );
    let title_width = painter
        .layout_no_wrap(title.to_owned(), egui::FontId::proportional(19.0), TEXT)
        .size()
        .x;
    painter.text(
        rect.left_center() + vec2(18.0 + title_width + 22.0, 0.0),
        egui::Align2::LEFT_CENTER,
        summary,
        egui::FontId::proportional(14.0),
        MUTED,
    );
    painter.text(
        rect.right_center() - vec2(20.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        if open { "\u{2304}" } else { "\u{203A}" },
        egui::FontId::proportional(22.0),
        TEXT,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let inner = open.then(|| {
        ui.add_space(4.0);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 8))
            .show(ui, body)
            .inner
    });
    (response, inner)
}

fn lerp_colour(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaced_letters_and_words() {
        assert_eq!(spaced("AB"), "A B");
        assert_eq!(spaced("A BC"), "A   B C");
        assert_eq!(spaced(""), "");
        assert_eq!(
            lerp_colour(Color32::BLACK, Color32::WHITE, 0.5),
            Color32::from_gray(128)
        );
    }
}
