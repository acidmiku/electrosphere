//! The launcher's pages (Play, Display, Controls) and the values they edit.
//!
//! The pages show what a player sets: the window, the picture and the CRT,
//! the controls. What only a developer sets (texture packs, debug views,
//! paths) stays in the settings file; the less used settings fold away
//! ("Advanced graphics", "Gamepad buttons").
use super::discs::{Discs, Readiness};
use super::theme::{self, MUTED, TEXT};
use crate::gamepad::{ButtonMap, PadChoice, PadState, PsButton};
use crate::gpu_renderer::MAX_SCALE;
use crate::keyboard;
use crate::present::crt;
use crate::settings::{Filter, Renderer, Scaling, Settings, Widescreen, WindowMode};
use egui::{ComboBox, Response, RichText, Slider, Ui, vec2};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The window sizes offered: 3x to 6x the game's 320x240 picture.
pub const SIZES: [(u32, u32); 4] = [(960, 720), (1280, 960), (1600, 1200), (1920, 1440)];

/// The pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Play,
    Display,
    Controls,
}

impl Page {
    pub const ALL: [(Self, &'static str, &'static str); 3] = [
        (Self::Play, "play", "Play"),
        (Self::Display, "display", "Display"),
        (Self::Controls, "controls", "Controls"),
    ];
}

/// The values being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub settings: Settings,
    /// "Custom" is chosen in the window-size list.
    pub custom_size: bool,
}

impl Form {
    pub fn new(settings: &Settings) -> Self {
        let display = &settings.display;
        Self {
            custom_size: !SIZES.contains(&(display.window_width, display.window_height)),
            settings: settings.clone(),
        }
    }

    /// The settings the form shows.
    pub fn settings(&self) -> Settings {
        self.settings.clone()
    }
}

// ---- checks ------------------------------------------------------------------------------

/// A problem with the values: it stops Save and Play.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub error: bool,
    pub text: String,
}

/// The problems of `settings`.
pub fn problems(settings: &Settings) -> Vec<Problem> {
    match settings.check() {
        Ok(()) => Vec::new(),
        Err(text) => vec![Problem { error: true, text }],
    }
}

/// `path` made absolute for display (an empty path is the working folder).
pub fn shown(path: &Path) -> PathBuf {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

// ---- layout marks (the test hook) --------------------------------------------------------

/// The screen rectangles of named controls, in points, for the
/// `AC3_LAUNCHER_LAYOUT` test hook.
#[derive(Default)]
pub struct Marks(pub BTreeMap<String, egui::Rect>);

impl Marks {
    pub fn put(&mut self, name: impl Into<String>, response: &Response) {
        self.0.insert(name.into(), response.rect);
    }
}

// ---- labels ------------------------------------------------------------------------------

fn size_label((width, height): (u32, u32)) -> String {
    match SIZES.iter().position(|&size| size == (width, height)) {
        Some(index) => format!("{width} x {height} ({}x)", index + 3),
        None => format!("{width} x {height}"),
    }
}

fn scale_label(scale: u32) -> String {
    let base = if scale == 1 {
        " (the original)".to_owned()
    } else {
        format!(" ({} x {})", 320 * scale, 240 * scale)
    };
    format!("{scale}x{base}")
}

fn widescreen_label(widescreen: Widescreen) -> &'static str {
    match widescreen {
        Widescreen::Off => "Off (4:3)",
        Widescreen::Wide16x9 => "16:9",
        Widescreen::Wide16x10 => "16:10",
        Widescreen::Wide21x9 => "21:9",
    }
}

fn scaling_label(scaling: Scaling) -> &'static str {
    match scaling {
        Scaling::Aspect => "4:3 with bars",
        Scaling::Stretch => "Stretch to the window",
        Scaling::Integer => "Integer (sharpest)",
    }
}

fn filter_label(filter: Filter) -> &'static str {
    match filter {
        Filter::Nearest => "Nearest (sharp pixels)",
        Filter::Linear => "Linear (smooth)",
    }
}

fn renderer_label(renderer: Renderer) -> &'static str {
    match renderer {
        Renderer::Gpu => "Graphics card",
        Renderer::Software => "Software (the original, 1x)",
        Renderer::Auto => "Auto (graphics card above 1x)",
    }
}

/// The PlayStation buttons, with their names.
const PS_BUTTONS: [(PsButton, &str); 17] = [
    (PsButton::None, "(none)"),
    (PsButton::Cross, "Cross"),
    (PsButton::Circle, "Circle"),
    (PsButton::Square, "Square"),
    (PsButton::Triangle, "Triangle"),
    (PsButton::L1, "L1"),
    (PsButton::R1, "R1"),
    (PsButton::L2, "L2"),
    (PsButton::R2, "R2"),
    (PsButton::L3, "L3"),
    (PsButton::R3, "R3"),
    (PsButton::Select, "Select"),
    (PsButton::Start, "Start"),
    (PsButton::Up, "Up"),
    (PsButton::Down, "Down"),
    (PsButton::Left, "Left"),
    (PsButton::Right, "Right"),
];

pub fn ps_label(button: PsButton) -> &'static str {
    PS_BUTTONS
        .iter()
        .find(|(choice, _)| *choice == button)
        .map_or("?", |(_, name)| name)
}

/// The PlayStation buttons held in `bits` (`Host::buttons` order), by name.
pub fn pressed(bits: u16) -> String {
    let names: Vec<_> = PS_BUTTONS
        .iter()
        .filter(|(button, _)| bits & button.bit() != 0)
        .map(|(_, name)| *name)
        .collect();
    if names.is_empty() {
        "nothing".to_owned()
    } else {
        names.join(" + ")
    }
}

/// Where a gamepad button's PlayStation button is in [`ButtonMap`].
type Field = fn(&mut ButtonMap) -> &mut PsButton;

/// The gamepad buttons: their `[controls.buttons]` key, their Xbox name and
/// their field.
pub const PAD_BUTTONS: [(&str, &str, Field); 17] = [
    ("a", "A", |map| &mut map.a),
    ("b", "B", |map| &mut map.b),
    ("x", "X", |map| &mut map.x),
    ("y", "Y", |map| &mut map.y),
    ("lb", "LB", |map| &mut map.lb),
    ("rb", "RB", |map| &mut map.rb),
    ("lt", "LT", |map| &mut map.lt),
    ("rt", "RT", |map| &mut map.rt),
    ("back", "View (Back)", |map| &mut map.back),
    ("start", "Menu (Start)", |map| &mut map.start),
    ("dpad_up", "D-pad up", |map| &mut map.dpad_up),
    ("dpad_down", "D-pad down", |map| &mut map.dpad_down),
    ("dpad_left", "D-pad left", |map| &mut map.dpad_left),
    ("dpad_right", "D-pad right", |map| &mut map.dpad_right),
    ("left_stick", "Left stick click", |map| &mut map.left_stick),
    ("right_stick", "Right stick click", |map| {
        &mut map.right_stick
    }),
    ("guide", "Guide (Xbox)", |map| &mut map.guide),
];

/// A fraction as a whole percentage, for the sliders.
fn percent(value: f64, _: std::ops::RangeInclusive<usize>) -> String {
    format!("{:.0} %", value * 100.0)
}

fn parse_percent(text: &str) -> Option<f64> {
    let text = text.trim().trim_end_matches('%').trim();
    text.parse::<f64>().ok().map(|value| value / 100.0)
}

/// A slider value kept to whole percent, so the file gets e.g. `0.2`.
fn round_percent(value: &mut f64) {
    *value = (*value * 100.0).round() / 100.0;
}

/// The keys shown while playing.
pub const HOTKEYS: [(&str, &str); 10] = [
    ("F2", "CRT screen off / on"),
    ("Shift+F3", "Next CRT look"),
    ("F4", "Curved / flat glass"),
    ("F3", "Frame rate"),
    ("F5 / F8", "Quick save / quick load"),
    ("F6", "Flight effects off / on"),
    ("F7", "Change discs"),
    ("Alt+Enter", "Window / borderless"),
    ("Start", "Pause"),
    ("Esc", "Quit"),
];

// ---- pages -----------------------------------------------------------------------------

/// Gamepad input for the Controls page.
pub struct PadView<'a> {
    pub backend: &'a str,
    pub names: &'a [String],
    pub state: PadState,
}

/// What the Play page shows besides the form.
pub struct PlayView<'a> {
    pub discs: &'a Discs,
    /// Memory card 1 and the quick-save slot, in words.
    pub card: &'a str,
    pub quick_save: &'a str,
    pub pads: &'a [String],
}

/// A key binding being captured: the button and its slot (0 or 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capture {
    pub button: PsButton,
    pub slot: usize,
}

/// The colour of a disc status.
pub fn readiness_colour(readiness: Readiness) -> egui::Color32 {
    match readiness {
        Readiness::Ready => theme::GOOD,
        Readiness::Partial => theme::WARN,
        Readiness::Missing => theme::ERROR,
    }
}

impl Form {
    /// The Play page. True: look for the discs again.
    pub fn play_page(&mut self, ui: &mut Ui, marks: &mut Marks, view: &PlayView<'_>) -> bool {
        ui.set_max_width(theme::PLAY_COLUMN);
        let (readiness, headline, detail) = view.discs.status();
        let mut rescan = false;
        // Ready, the bottom bar's status is enough; otherwise say what is
        // missing and what is expected.
        if readiness != Readiness::Ready {
            let title = match readiness {
                Readiness::Partial => "Almost ready",
                _ => "Game discs needed",
            };
            ui.label(RichText::new(title).size(32.0).color(TEXT));
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(theme::PANEL.gamma_multiply(0.9))
                .stroke(egui::Stroke::new(1.0, theme::LINE))
                .corner_radius(6)
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    theme::status(ui, readiness_colour(readiness), &headline);
                    ui.label(RichText::new(&detail).size(14.0).color(MUTED));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let open = theme::button(ui, "Open the folder", 160.0)
                            .on_hover_text(view.discs.folder.display().to_string());
                        marks.put("play.open_folder", &open);
                        if open.clicked() {
                            super::open_folder(&view.discs.folder);
                        }
                        let again = theme::button(ui, "Look again", 140.0).on_hover_text(format!(
                            "Disc images in {}",
                            view.discs.folder.display()
                        ));
                        marks.put("play.rescan", &again);
                        rescan = again.clicked();
                    });
                });
        }
        theme::section(ui, "Saves");
        theme::row(ui, "Memory card", |ui| ui.label(view.card));
        theme::row(ui, "Quick save", |ui| ui.label(view.quick_save));
        theme::section(ui, "Controls");
        let controller = if view.pads.is_empty() {
            "Keyboard: arrows pitch and roll, W / S throttle, A / D yaw".to_owned()
        } else {
            format!("Gamepad: {}", view.pads.join(", "))
        };
        theme::row(ui, "Controller", |ui| ui.label(controller));
        theme::section(ui, "While playing");
        egui::Grid::new("hotkeys")
            .num_columns(2)
            .spacing([28.0, 6.0])
            .show(ui, |ui| {
                for (key, what) in HOTKEYS {
                    let key = if cfg!(target_os = "macos") {
                        key.replace("Alt", "Option")
                    } else {
                        key.to_owned()
                    };
                    ui.label(RichText::new(key).color(theme::ACCENT));
                    ui.label(RichText::new(what).color(MUTED));
                    ui.end_row();
                }
            });
        if cfg!(target_os = "macos") {
            theme::note(ui, "On a Mac keyboard, hold fn with the F keys.");
        }
        rescan
    }

    /// The Display page; `preview` is the CRT preview to show (its texture,
    /// or why there is none).
    pub fn display_page(
        &mut self,
        ui: &mut Ui,
        marks: &mut Marks,
        preview: Option<&Result<egui::TextureId, String>>,
    ) {
        let custom_size = &mut self.custom_size;
        let display = &mut self.settings.display;
        theme::section(ui, "Window");
        theme::row(ui, "Mode", |ui| {
            for (mode, key, name) in [
                (WindowMode::Windowed, "windowed", "Windowed"),
                (WindowMode::Borderless, "borderless", "Borderless"),
            ] {
                let item = theme::segment(ui, &mut display.mode, mode, name);
                marks.put(format!("display.mode.{key}"), &item);
            }
        });
        theme::row(ui, "Window size", |ui| {
            let size = (display.window_width, display.window_height);
            let shown = if *custom_size {
                format!("Custom ({} x {})", size.0, size.1)
            } else {
                size_label(size)
            };
            let combo = ComboBox::from_id_salt("window_size")
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for choice in SIZES {
                        if ui
                            .selectable_label(!*custom_size && size == choice, size_label(choice))
                            .clicked()
                        {
                            (display.window_width, display.window_height) = choice;
                            *custom_size = false;
                        }
                    }
                    if ui.selectable_label(*custom_size, "Custom").clicked() {
                        *custom_size = true;
                    }
                });
            marks.put("display.window_size", &combo.response);
            if *custom_size {
                ui.add(egui::DragValue::new(&mut display.window_width).range(64..=16384));
                ui.label("x");
                ui.add(egui::DragValue::new(&mut display.window_height).range(64..=16384));
            }
        });
        if display.mode == WindowMode::Borderless {
            theme::note(
                ui,
                "Borderless fills the screen; the size is for windowed mode. Alt+Enter switches while playing.",
            );
        }
        theme::rule(ui);

        theme::section(ui, "Picture");
        egui::Grid::new("picture")
            .num_columns(4)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                let label = |ui: &mut Ui, text: &str| {
                    let width = theme::LABELS - 16.0;
                    ui.allocate_ui_with_layout(
                        vec2(width, 30.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_width(width);
                            ui.label(text);
                        },
                    );
                };
                label(ui, "Internal resolution");
                let combo = ComboBox::from_id_salt("internal_resolution")
                    .width(200.0)
                    .selected_text(scale_label(display.internal_resolution))
                    .show_ui(ui, |ui| {
                        for scale in 1..=MAX_SCALE {
                            ui.selectable_value(
                                &mut display.internal_resolution,
                                scale,
                                scale_label(scale),
                            );
                        }
                    });
                marks.put("display.internal_resolution", &combo.response);
                ui.label("Widescreen");
                let combo = ComboBox::from_id_salt("widescreen")
                    .width(150.0)
                    .selected_text(widescreen_label(display.widescreen))
                    .show_ui(ui, |ui| {
                        for widescreen in [
                            Widescreen::Off,
                            Widescreen::Wide16x9,
                            Widescreen::Wide16x10,
                            Widescreen::Wide21x9,
                        ] {
                            ui.selectable_value(
                                &mut display.widescreen,
                                widescreen,
                                widescreen_label(widescreen),
                            );
                        }
                    });
                marks.put("display.widescreen", &combo.response);
                ui.end_row();
                label(ui, "Scaling");
                let combo = ComboBox::from_id_salt("scaling")
                    .width(200.0)
                    .selected_text(scaling_label(display.scaling))
                    .show_ui(ui, |ui| {
                        for scaling in [Scaling::Aspect, Scaling::Stretch, Scaling::Integer] {
                            ui.selectable_value(
                                &mut display.scaling,
                                scaling,
                                scaling_label(scaling),
                            );
                        }
                    });
                marks.put("display.scaling", &combo.response);
                ui.label("Filter");
                let combo = ComboBox::from_id_salt("filter")
                    .width(150.0)
                    .selected_text(filter_label(display.filter))
                    .show_ui(ui, |ui| {
                        for filter in [Filter::Nearest, Filter::Linear] {
                            ui.selectable_value(&mut display.filter, filter, filter_label(filter));
                        }
                    });
                marks.put("display.filter", &combo.response);
                ui.end_row();
            });
        if display.widescreen != Widescreen::Off {
            theme::note(
                ui,
                "Flight is drawn wide with the HUD as a centred 4:3 block; menus, the hub and movies stay 4:3.",
            );
        }
        theme::rule(ui);

        theme::section(ui, "CRT screen");
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(380.0);
                theme::row(ui, "CRT shader", |ui| {
                    let item = theme::switch(ui, &mut display.crt);
                    marks.put("display.crt", &item);
                });
                theme::row(ui, "Look", |ui| {
                    ui.add_enabled_ui(display.crt, |ui| {
                        let combo = ComboBox::from_id_salt("crt_look")
                            .width(190.0)
                            .selected_text(display.crt_preset.clone())
                            .show_ui(ui, |ui| {
                                for look in &crt::LOOKS {
                                    ui.selectable_value(
                                        &mut display.crt_preset,
                                        look.name.to_owned(),
                                        look.name,
                                    );
                                }
                            });
                        marks.put("display.crt_preset", &combo.response);
                    });
                });
                theme::row(ui, "Glass", |ui| {
                    ui.add_enabled_ui(display.crt, |ui| {
                        for (curved, key, name) in
                            [(true, "curved", "Curved"), (false, "flat", "Flat")]
                        {
                            let item = theme::segment(ui, &mut display.crt_curved, curved, name);
                            marks.put(format!("display.crt_curved.{key}"), &item);
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "The picture and the movies, drawn as on a CRT (Lola Retro). While \
                         playing: F2 off / on, Shift+F3 next look, F4 curved / flat.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
            });
            ui.add_space(12.0);
            preview_image(ui, marks, preview);
        });
        theme::rule(ui);

        theme::section(ui, "Rendering");
        theme::row(ui, "Renderer", |ui| {
            let combo = ComboBox::from_id_salt("renderer")
                .width(240.0)
                .selected_text(renderer_label(display.renderer))
                .show_ui(ui, |ui| {
                    for renderer in [Renderer::Gpu, Renderer::Software, Renderer::Auto] {
                        ui.selectable_value(
                            &mut display.renderer,
                            renderer,
                            renderer_label(renderer),
                        );
                    }
                });
            marks.put("display.renderer", &combo.response);
        });
        let effects = &mut self.settings.effects;
        ui.horizontal(|ui| {
            for (key, name, value) in [
                ("vsync", "VSync", &mut display.vsync),
                ("show_fps", "Show frame rate", &mut display.show_fps),
                ("effects", "Flight effects", &mut effects.enabled),
            ] {
                ui.label(name);
                let item = theme::switch(ui, value);
                marks.put(format!("display.{key}"), &item);
                ui.add_space(18.0);
            }
        });
        ui.add_space(10.0);
        let (fold, _) = theme::fold(
            ui,
            "advanced_graphics",
            "Advanced graphics",
            "Dithering, geometry, textures & flight effects",
            |ui| advanced_graphics(ui, marks, &mut self.settings),
        );
        marks.put("display.advanced", &fold);
    }

    /// The Controls page. `capture` is the key binding waiting for a key.
    pub fn controls_page(
        &mut self,
        ui: &mut Ui,
        marks: &mut Marks,
        pads: &PadView<'_>,
        capture: &mut Option<Capture>,
    ) {
        let controls = &mut self.settings.controls;
        theme::section(ui, "Gamepad");
        if pads.names.is_empty() {
            theme::status(
                ui,
                MUTED,
                &format!(
                    "No gamepad connected ({}): connect one to test it here",
                    pads.backend
                ),
            );
        } else {
            theme::status(ui, theme::GOOD, &pads.names.join(", "));
            theme::row(ui, "Pressed now", |ui| {
                ui.label(RichText::new(pressed(pads.state.buttons)).color(theme::ACCENT));
            });
        }
        theme::row(ui, "Swap triggers", |ui| {
            let item = theme::switch(ui, &mut controls.swap_triggers);
            marks.put("controls.swap_triggers", &item);
        });
        theme::note(
            ui,
            "The triggers do what the bumpers do and back: throttle on the triggers, yaw on the bumpers (the game's default key config).",
        );
        theme::row(ui, "Vibration", |ui| {
            if cfg!(target_os = "macos") {
                // The gamepad library has no force feedback on macOS.
                ui.label(RichText::new("Not on macOS").color(MUTED));
            } else {
                let item = theme::switch(ui, &mut controls.rumble);
                marks.put("controls.rumble", &item);
            }
        });
        theme::row(ui, "Stick precision", |ui| {
            let item = theme::switch(ui, &mut controls.compensate_deadzone);
            marks.put("controls.compensate_deadzone", &item);
        });
        theme::note(
            ui,
            "Every stick movement reaches the game; off, the game's own large dead zone drops small and diagonal ones.",
        );
        theme::row(ui, "Stick dead zone", |ui| {
            let item = ui.add(
                Slider::new(&mut controls.deadzone, 0.0..=0.9)
                    .step_by(0.01)
                    .custom_formatter(percent)
                    .custom_parser(parse_percent),
            );
            if item.changed() {
                round_percent(&mut controls.deadzone);
            }
            marks.put("controls.deadzone", &item);
        });
        ui.add_space(8.0);
        let (fold, _) = theme::fold(
            ui,
            "gamepad_buttons",
            "Gamepad buttons",
            "Which PlayStation button each gamepad button presses",
            |ui| gamepad_buttons(ui, marks, &mut controls.buttons),
        );
        marks.put("controls.gamepad_buttons", &fold);
        ui.add_space(6.0);
        let (fold, _) = theme::fold(
            ui,
            "gamepad_advanced",
            "Advanced",
            "Controller type, trigger threshold",
            |ui| {
                theme::row(ui, "Controller", |ui| {
                    for (choice, key, name) in [
                        (PadChoice::Auto, "auto", "Auto"),
                        (PadChoice::DualShock, "dualshock", "DualShock"),
                        (PadChoice::Digital, "digital", "Digital pad"),
                    ] {
                        let item = theme::segment(ui, &mut controls.pad_type, choice, name);
                        marks.put(format!("controls.pad_type.{key}"), &item);
                    }
                });
                theme::note(
                    ui,
                    "Auto: a DualShock (analog sticks) when a gamepad is connected as the game starts, else a digital pad.",
                );
                theme::row(ui, "Trigger threshold", |ui| {
                    let item = ui
                        .add(
                            Slider::new(&mut controls.trigger_threshold, 0.05..=1.0)
                                .step_by(0.01)
                                .custom_formatter(percent)
                                .custom_parser(parse_percent),
                        )
                        .on_hover_text("How far LT and RT must be pulled to press their buttons.");
                    if item.changed() {
                        round_percent(&mut controls.trigger_threshold);
                    }
                    marks.put("controls.trigger_threshold", &item);
                });
            },
        );
        marks.put("controls.advanced", &fold);
        theme::rule(ui);

        theme::section(ui, "Keyboard");
        keyboard_table(ui, marks, &mut controls.keys, capture);
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Click a key to change it, then press the new key (Esc cancels). Escape and the \
                 function keys belong to the game window.",
            )
            .size(13.0)
            .color(MUTED),
        );
    }
}

/// The CRT preview, or why there is none.
fn preview_image(
    ui: &mut Ui,
    marks: &mut Marks,
    preview: Option<&Result<egui::TextureId, String>>,
) {
    let points = ui.ctx().pixels_per_point();
    // 1:1 with the preview's pixels, smaller if the column is narrow.
    let full = vec2(
        super::preview::OUTPUT.0 as f32 / points,
        super::preview::OUTPUT.1 as f32 / points,
    );
    let width = full.x.min(ui.available_width()).max(160.0);
    let size = vec2(width, width * 0.75);
    let response = match preview {
        Some(Ok(texture)) => ui.add(egui::Image::new((*texture, size)).corner_radius(4)),
        Some(Err(error)) => {
            let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, theme::FIELD);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No preview",
                egui::FontId::proportional(15.0),
                MUTED,
            );
            response.on_hover_text(error.as_str())
        }
        None => {
            let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, theme::FIELD);
            response
        }
    };
    marks.put("display.crt_preview", &response);
}

/// The folded advanced graphics: the renderer's options and the flight
/// effects one by one.
fn advanced_graphics(ui: &mut Ui, marks: &mut Marks, settings: &mut Settings) {
    let display = &mut settings.display;
    let upscaled = display.gpu_renderer().is_some_and(|gpu| gpu.scale > 1);
    for (key, name, value, enabled, what) in [
        (
            "upscaled_dither",
            "Dithering",
            &mut display.upscaled_dither,
            upscaled,
            "Shaded surfaces dithered above 1x, as the PlayStation does",
        ),
        (
            "subpixel",
            "Sub-pixel geometry",
            &mut display.subpixel,
            upscaled,
            "Smooth 3D edges above 1x (no vertex wobble)",
        ),
        (
            "perspective",
            "Perspective textures",
            &mut display.perspective,
            upscaled,
            "No bending of large 3D textures above 1x",
        ),
    ] {
        theme::row(ui, name, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                let item = theme::toggle(ui, value).on_hover_text(what);
                marks.put(format!("display.{key}"), &item);
                ui.label(RichText::new(what).size(13.0).color(MUTED));
            });
        });
    }
    let effects = &mut settings.effects;
    let on = effects.enabled;
    ui.add_space(6.0);
    ui.label(RichText::new("Flight effects").color(TEXT));
    ui.add_enabled_ui(on, |ui| {
        theme::row(ui, "Atmosphere", |ui| {
            let item = theme::toggle(ui, &mut effects.atmosphere);
            marks.put("effects.atmosphere", &item);
            ui.label(
                RichText::new("A height haze in place of the game's fog")
                    .size(13.0)
                    .color(MUTED),
            );
        });
        theme::row(ui, "Haze density", |ui| {
            let item = ui.add(
                Slider::new(
                    &mut effects.haze_density.0,
                    0.0..=crate::effects::atmosphere_fx::HazeDensity::MAX,
                )
                .step_by(0.1)
                .suffix("x"),
            );
            marks.put("effects.haze_density", &item);
        });
        theme::row(ui, "Far terrain", |ui| {
            let item = theme::toggle(ui, &mut effects.far_terrain);
            marks.put("effects.far_terrain", &item);
            ui.label(
                RichText::new("The terrain to the horizon")
                    .size(13.0)
                    .color(MUTED),
            );
        });
        theme::row(ui, "Far terrain detail", |ui| {
            use crate::effects::far_terrain::Quality;
            for (quality, key, name) in [
                (Quality::Low, "low", "Low"),
                (Quality::Medium, "medium", "Medium"),
                (Quality::High, "high", "High"),
            ] {
                let item = theme::segment(ui, &mut effects.far_terrain_quality, quality, name);
                marks.put(format!("effects.far_terrain_quality.{key}"), &item);
            }
        });
        theme::row(ui, "Sun", |ui| {
            let item = theme::toggle(ui, &mut effects.sun);
            marks.put("effects.sun", &item);
            ui.label(
                RichText::new("An HDR sun, lens flare and bloom")
                    .size(13.0)
                    .color(MUTED),
            );
        });
        theme::row(ui, "Bloom", |ui| {
            let item = ui.add(
                Slider::new(&mut effects.bloom.0, 0.0..=crate::effects::sun::Bloom::MAX)
                    .step_by(0.05)
                    .suffix("x"),
            );
            marks.put("effects.bloom", &item);
        });
        theme::row(ui, "Sun glare", |ui| {
            let item = ui.add(
                Slider::new(
                    &mut effects.sun_glare.0,
                    0.0..=crate::effects::sun::Glare::MAX,
                )
                .step_by(0.05)
                .suffix("x"),
            );
            marks.put("effects.sun_glare", &item);
        });
        theme::row(ui, "Star", |ui| {
            let item = theme::toggle(ui, &mut effects.sun_star.0);
            marks.put("effects.sun_star", &item);
            ui.label(
                RichText::new("The lens flare's cross-shaped star")
                    .size(13.0)
                    .color(MUTED),
            );
        });
    });
}

/// The folded gamepad button map.
fn gamepad_buttons(ui: &mut Ui, marks: &mut Marks, map: &mut ButtonMap) {
    egui::Grid::new("pad_buttons")
        .num_columns(4)
        .spacing([14.0, 6.0])
        .show(ui, |ui| {
            for (index, (key, name, field)) in PAD_BUTTONS.iter().enumerate() {
                ui.label(*name);
                let value = field(map);
                let combo = ComboBox::from_id_salt(("pad_button", *key))
                    .width(110.0)
                    .selected_text(ps_label(*value))
                    .show_ui(ui, |ui| {
                        for (button, label) in PS_BUTTONS {
                            ui.selectable_value(value, button, label);
                        }
                    });
                marks.put(format!("controls.buttons.{key}"), &combo.response);
                if index % 2 == 1 {
                    ui.end_row();
                }
            }
        });
}

/// The keyboard bindings: each button, what it does, its two keys.
fn keyboard_table(
    ui: &mut Ui,
    marks: &mut Marks,
    keys: &mut keyboard::KeyMap,
    capture: &mut Option<Capture>,
) {
    let buttons: Vec<(PsButton, Vec<String>)> = keys
        .buttons()
        .iter()
        .map(|(button, keys)| (*button, (*keys).clone()))
        .collect();
    let mut clear = None;
    egui::Grid::new("keyboard")
        .num_columns(4)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            for (button, bound) in &buttons {
                let name = ps_label(*button);
                ui.add_sized(
                    vec2(90.0, 28.0),
                    egui::Label::new(RichText::new(name).color(TEXT)),
                );
                ui.add_sized(
                    vec2(110.0, 28.0),
                    egui::Label::new(
                        RichText::new(keyboard::function(*button))
                            .size(13.0)
                            .color(MUTED),
                    ),
                );
                for slot in 0..2 {
                    let waiting = *capture
                        == Some(Capture {
                            button: *button,
                            slot,
                        });
                    let text = if waiting {
                        "Press a key...".to_owned()
                    } else {
                        bound
                            .get(slot)
                            .map_or("-".to_owned(), |key| keyboard::label(key).to_owned())
                    };
                    let item = ui.add(
                        egui::Button::new(RichText::new(text).color(if waiting {
                            theme::ON_ACCENT
                        } else {
                            TEXT
                        }))
                        .fill(if waiting { theme::ACCENT } else { theme::FIELD })
                        .stroke(egui::Stroke::new(1.0, theme::LINE))
                        .min_size(vec2(130.0, 28.0)),
                    );
                    let key = format!("controls.keys.{}.{slot}", name.to_lowercase());
                    marks.put(&key, &item);
                    if item.clicked() {
                        *capture = (!waiting).then_some(Capture {
                            button: *button,
                            slot,
                        });
                    }
                    if item.secondary_clicked() && slot < bound.len() {
                        clear = Some((*button, slot));
                    }
                }
                ui.end_row();
            }
        });
    if let Some((button, slot)) = clear {
        keys.clear(button, slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_player::buttons;

    #[test]
    fn labels() {
        assert_eq!(size_label((1920, 1440)), "1920 x 1440 (6x)");
        assert_eq!(size_label((1000, 700)), "1000 x 700");
        assert_eq!(scale_label(1), "1x (the original)");
        assert_eq!(scale_label(4), "4x (1280 x 960)");
        assert_eq!(pressed(0), "nothing");
        assert_eq!(pressed(buttons::L2 | buttons::CROSS), "Cross + L2");
        assert_eq!(ps_label(PsButton::Start), "Start");
        assert_eq!(widescreen_label(Widescreen::Wide16x9), "16:9");
        assert_eq!(renderer_label(Renderer::Gpu), "Graphics card");
    }

    #[test]
    fn every_pad_button_has_a_field() {
        let mut map = ButtonMap::default();
        for (key, _, field) in PAD_BUTTONS {
            *field(&mut map) = PsButton::Start;
            let text = toml::to_string(&map).unwrap();
            assert!(text.contains(&format!("{key} = \"start\"")), "{key}");
        }
    }

    #[test]
    fn problems_are_the_settings_checks() {
        assert!(problems(&Settings::recommended()).is_empty());
        let mut settings = Settings::recommended();
        settings.display.crt_preset = "royale".into();
        let found = problems(&settings);
        assert_eq!(found.len(), 1);
        assert!(found[0].error && found[0].text.contains("crt_preset"));
        let form = Form::new(&settings);
        assert_eq!(form.settings(), settings);
        assert!(!Form::new(&Settings::recommended()).custom_size);
    }
}
