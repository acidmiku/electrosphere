//! The launcher: `ac3` with no arguments (or `ac3 launcher`) opens the
//! game's front window: the background art, three pages (Play, Display,
//! Controls) and Play.
//!
//! - **Window.** winit and wgpu, the crates of the game window, with egui on
//!   top (egui-winit for input, egui-wgpu for drawing). The launcher is its
//!   own process: winit allows one event loop per process (a second
//!   `EventLoop::new` fails with `RecreationAttempt`, in winit 0.30 on every
//!   platform), and the game's `machine_player::run` makes its own.
//! - **What a player sets.** [`form::Form`] holds the values shown: the
//!   window, the picture, the CRT ([`preview`] draws the chosen look with
//!   the game's own shader code), the gamepad and the keyboard. Developer
//!   settings (texture packs, debug views, paths) stay in the file. Save
//!   writes through [`settings::Document`], which edits the file in place:
//!   comments and unknown keys stay. A file that is not TOML is kept as
//!   `ac3.toml.bak` and replaced. Nothing is written until Save or Play.
//!   Without a file the pages show [`Settings::recommended`].
//! - **Discs and the card.** [`discs`] finds the Japanese discs next to the
//!   launcher (or where the settings name them) and says what is missing;
//!   memory card 1 is `card1.mcr` beside the settings file, made on the
//!   first run.
//! - **Play** saves, then starts `<this program> machine --settings FILE`
//!   (with the discs found, and `--data`/`--disc` when the launcher was
//!   given them) in the same console, hides the window and exits once the
//!   game has run for [`START_GRACE`]. When the game stops with an error
//!   before that, the window comes back with the error.
//! - **Keys.** Enter plays (or presses the focused control), Esc quits (or
//!   leaves a list). While a key binding waits for its key, the next key
//!   press is that key (Esc cancels). With unsaved changes the first Quit
//!   only warns.
//! - **Test hooks.** [`GAME_VARIABLE`] replaces the program Play starts;
//!   [`LAYOUT_VARIABLE`] names a JSON file that receives the controls'
//!   rectangles in window pixels after each frame, for scripted clicks.
//!
//! [`settings::Document`]: crate::settings::Document
pub mod discs;
pub mod form;
pub mod preview;
pub mod theme;

use crate::gamepad::{self, Bindings, Gamepads, PadState};
use crate::machine_card::CardSource;
use crate::movie_pack::RgbaImage;
use crate::settings::{self, Document, Settings};
use discs::{Discs, Readiness};
use egui::{Color32, RichText, vec2};
use form::{Capture, Form, Marks, Page};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

/// How long the game must run before the launcher exits. An error exit
/// before that brings the launcher back with the error.
pub const START_GRACE: Duration = Duration::from_secs(3);
/// Test hook: the program Play starts instead of this one.
pub const GAME_VARIABLE: &str = "AC3_LAUNCHER_GAME";
/// Test hook: a JSON file for the controls' rectangles.
pub const LAYOUT_VARIABLE: &str = "AC3_LAUNCHER_LAYOUT";
/// The window title.
pub const TITLE: &str = "Ace Combat 3 launcher";
/// How often the gamepads are read for the Controls page.
const PAD_POLL: Duration = Duration::from_millis(50);
/// How long a changed button map rests before the gamepads follow it.
const PAD_REBUILD: Duration = Duration::from_millis(400);
/// How often a starting game is checked.
const GAME_POLL: Duration = Duration::from_millis(50);
/// The background art, beside the pages.
const ART: &[u8] = include_bytes!("background.webp");

/// What the launcher works with, as `main` finds it.
pub struct Options {
    /// The settings file to edit and to start the game with.
    pub settings: PathBuf,
    /// The extracted disc folder and the disc image as `ac3 machine` finds
    /// them by default (logged).
    pub data_dir: PathBuf,
    pub disc: PathBuf,
    /// Passed to the game after `machine --settings FILE`.
    pub game_args: Vec<OsString>,
}

/// Run the launcher until it quits or has started the game.
pub fn run(options: Options) -> crate::Result<()> {
    let event_loop = EventLoop::new().map_err(|e| format!("launcher window: {e}"))?;
    let mut app = App {
        launcher: Launcher::new(options),
        ctx: egui::Context::default(),
        view: None,
        failure: None,
        starting: None,
        repaint_at: None,
        layout: std::env::var_os(LAYOUT_VARIABLE).map(|path| (PathBuf::from(path), String::new())),
        art: crate::movie_pack::decode_image(ART, "webp")
            .ok()
            .map(Arc::new),
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("launcher window: {e}"))?;
    match app.failure {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

// ---- the launcher's state ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Info,
    Good,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
struct Message {
    tone: Tone,
    text: String,
}

impl Message {
    fn new(tone: Tone, text: impl Into<String>) -> Self {
        Self {
            tone,
            text: text.into(),
        }
    }
    fn colour(&self) -> Color32 {
        match self.tone {
            Tone::Info => theme::MUTED,
            Tone::Good => theme::GOOD,
            Tone::Warning => theme::WARN,
            Tone::Error => theme::ERROR,
        }
    }
}

/// What the buttons and keys ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Play,
    Save,
    Defaults,
    Quit,
    Rescan,
}

struct Launcher {
    options: Options,
    /// The settings file's folder: relative paths in it start there.
    base: PathBuf,
    form: Form,
    /// The settings as the file holds them.
    saved: Settings,
    page: Page,
    /// About the file as loaded, and the card.
    notes: Vec<Message>,
    /// The result of the last action.
    status: Option<Message>,
    /// Quit was asked for once with unsaved changes.
    quit_armed: bool,
    /// The gamepads for the Controls page (none while the game starts).
    pads: Option<Pads>,
    marks: Marks,
    /// A control had keyboard focus, or a list was open, after the last
    /// frame (then Enter and Esc are theirs).
    focus_before: bool,
    popup_before: bool,
    discs: Discs,
    /// Memory card 1 in words.
    card: String,
    /// A key binding waiting for its key.
    capture: Option<Capture>,
    /// The background art as egui's texture, made with the first frame.
    art: Option<egui::TextureHandle>,
}

impl Launcher {
    fn new(options: Options) -> Self {
        let base = options
            .settings
            .parent()
            .map(Path::to_owned)
            .unwrap_or_default();
        let (mut settings, mut notes) = load(&options.settings);
        // Memory card 1 is a file beside the settings, made on the first run.
        if settings.paths.card1.is_none() {
            settings.paths.card1 = Some(settings::CARD.into());
        }
        if !base.as_os_str().is_empty()
            && let Err(error) = std::fs::create_dir_all(&base)
        {
            eprintln!("ac3: launcher: cannot make {}: {error}", base.display());
        }
        let card = ensure_card(&settings, &base, &mut notes);
        let form = Form::new(&settings);
        let saved = settings.clone();
        let discs = find_discs(&options, &settings, &base);
        let (readiness, headline, _) = discs.status();
        eprintln!(
            "ac3: launcher: settings {}; discs: {headline} ({readiness:?}); {card}",
            form::shown(&options.settings).display(),
        );
        Self {
            pads: Some(Pads::new(&saved.controls)),
            options,
            base,
            form,
            saved,
            page: Page::Play,
            notes,
            status: None,
            quit_armed: false,
            marks: Marks::default(),
            focus_before: false,
            popup_before: false,
            discs,
            card,
            capture: None,
            art: None,
        }
    }

    fn dirty(&self) -> bool {
        self.form.settings() != self.saved
    }

    /// Look for the discs again.
    fn rescan(&mut self) {
        self.discs = find_discs(&self.options, &self.form.settings, &self.base);
    }

    /// The CRT preview the Display page shows (`None` on other pages).
    fn preview_want(&self) -> Option<preview::Want> {
        if self.page != Page::Display {
            return None;
        }
        let display = &self.form.settings.display;
        Some(if display.crt {
            let index = crate::present::crt::index(&display.crt_preset).unwrap_or(0);
            let folder = settings::Display::crt_folder(&self.base);
            preview::Want::Crt(crate::present::crt::path(
                &folder,
                index,
                display.crt_curved,
            ))
        } else {
            preview::Want::Plain
        })
    }

    /// Save the form to the settings file. Returns where a file that was not
    /// TOML was kept.
    fn save(&mut self) -> Result<Option<PathBuf>, String> {
        let settings = self.form.settings();
        if let Some(problem) = form::problems(&settings).into_iter().find(|p| p.error) {
            return Err(problem.text);
        }
        let path = &self.options.settings;
        let mut kept = None;
        let mut document = match std::fs::read_to_string(path) {
            Ok(text) => match Document::parse(&text) {
                Ok(document) => document,
                Err(_) => {
                    let backup = backup_path(path);
                    std::fs::write(&backup, &text)
                        .map_err(|e| format!("{}: {e}", backup.display()))?;
                    kept = Some(backup);
                    Document::default_file()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Document::default_file(),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let before = document.to_string();
        document.set(&settings)?;
        if kept.is_some() || !path.is_file() || document.to_string() != before {
            document.write(path).map_err(|e| e.to_string())?;
        }
        eprintln!("ac3: launcher: saved {}", path.display());
        self.saved = settings;
        self.notes = load(path).1;
        self.quit_armed = false;
        Ok(kept)
    }

    /// Save, then start the game: this program, or [`GAME_VARIABLE`]'s.
    fn play(&mut self) -> Result<Child, String> {
        self.save()?;
        let program = match std::env::var_os(GAME_VARIABLE) {
            Some(program) => PathBuf::from(program),
            None => std::env::current_exe()
                .map_err(|e| format!("The game did not start: this program's path: {e}"))?,
        };
        self.start(&program)
    }

    /// `program machine --settings FILE`, the options passed on and the
    /// discs found.
    fn game_command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .arg("machine")
            .arg("--settings")
            .arg(&self.options.settings)
            .args(&self.options.game_args)
            .args(self.discs.game_args());
        command
    }

    /// Start the game (sharing this console).
    fn start(&self, program: &Path) -> Result<Child, String> {
        let mut command = self.game_command(program);
        eprintln!("ac3: launcher: starting {command:?}");
        command
            .spawn()
            .map_err(|e| format!("The game did not start ({}: {e}).", program.display()))
    }

    /// Whether to close now: with unsaved changes the first request warns.
    fn quit(&mut self) -> bool {
        if self.dirty() && !self.quit_armed {
            self.quit_armed = true;
            self.status = Some(Message::new(
                Tone::Warning,
                "Unsaved changes: Quit again to drop them, or Save.",
            ));
            return false;
        }
        true
    }

    /// The game stopped with `status` while starting.
    fn game_stopped(&mut self, status: ExitStatus) {
        eprintln!("ac3: launcher: the game stopped at once ({status})");
        self.status = Some(Message::new(
            Tone::Error,
            format!("The game stopped at once ({status}). Its messages are in the console window."),
        ));
        self.pads = Some(Pads::new(&self.form.settings.controls));
    }

    /// A key pressed while a binding waits for one: bound, refused or
    /// cancelled.
    fn captured(&mut self, code: KeyCode) {
        let Some(capture) = self.capture.take() else {
            return;
        };
        if code == KeyCode::Escape {
            return;
        }
        match crate::keyboard::name(code) {
            Some(name) => {
                self.form
                    .settings
                    .controls
                    .keys
                    .bind(capture.button, capture.slot, name);
                self.status = None;
            }
            None => {
                self.status = Some(Message::new(
                    Tone::Warning,
                    "That key belongs to the game window (Escape, the function keys) or cannot be used.",
                ));
            }
        }
    }

    fn ui(
        &mut self,
        ui: &mut egui::Ui,
        preview: Option<&Result<egui::TextureId, String>>,
    ) -> Option<Action> {
        self.marks.0.clear();
        let mut action = None;
        let (enter, escape) = ui.ctx().input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let settings = self.form.settings();
        let problems = form::problems(&settings);
        let blocked = problems.iter().any(|problem| problem.error);
        let dirty = settings != self.saved;
        if dirty
            && self
                .status
                .as_ref()
                .is_some_and(|status| status.tone == Tone::Good)
        {
            // "Saved." is out of date after an edit.
            self.status = None;
        }
        let (readiness, headline, detail) = self.discs.status();
        let playable = !blocked && readiness != Readiness::Missing;
        if self.capture.is_none() && !self.focus_before && !self.popup_before {
            if enter && playable {
                action = Some(Action::Play);
            } else if escape {
                action = Some(Action::Quit);
            }
        }

        self.paint_background(ui.ctx());

        egui::Panel::top("header")
            .resizable(false)
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: 40,
                right: 40,
                top: 22,
                bottom: 12,
            }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    logo(ui);
                    ui.add_space(56.0);
                    for (page, key, name) in Page::ALL {
                        let item = tab(ui, self.page == page, name);
                        self.marks.put(format!("page.{key}"), &item);
                        if item.clicked() {
                            self.page = page;
                            self.capture = None;
                        }
                        ui.add_space(22.0);
                    }
                });
            });

        egui::Panel::bottom("footer")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::BACKGROUND.gamma_multiply(0.94))
                    .inner_margin(egui::Margin::symmetric(40, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let file = form::shown(&self.options.settings);
                    let name = file.file_name().map_or_else(
                        || file.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    let link = ui
                        .link(RichText::new(name).size(14.0))
                        .on_hover_text(format!("{}: open its folder", file.display()));
                    self.marks.put("footer.settings_file", &link);
                    if link.clicked() {
                        show_file(&file);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("Esc  Quit").size(14.0).color(theme::MUTED));
                        ui.label(RichText::new("|").color(theme::LINE));
                        ui.label(RichText::new("Enter  Play").size(14.0).color(theme::MUTED));
                    });
                });
            });

        egui::Panel::bottom("actions")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL.gamma_multiply(0.94))
                    .inner_margin(egui::Margin::symmetric(40, 14)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let disc = theme::status(ui, form::readiness_colour(readiness), &headline)
                        .on_hover_text(&detail);
                    self.marks.put("status.discs", &disc);
                    let message = problems
                        .first()
                        .map(|problem| Message::new(Tone::Error, problem.text.clone()))
                        .or_else(|| self.status.clone())
                        .or_else(|| dirty.then(|| Message::new(Tone::Warning, "Unsaved changes")));
                    if let Some(message) = message {
                        ui.add_space(14.0);
                        ui.label(RichText::new("|").color(theme::LINE));
                        ui.add_space(14.0);
                        theme::status(ui, message.colour(), &message.text);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let play = theme::play_button(ui, playable).on_hover_text(match () {
                            () if playable => "Save, then start the game (Enter)".to_owned(),
                            () if blocked => "Fix the problem first".to_owned(),
                            () => detail.clone(),
                        });
                        ui.add_space(14.0);
                        let quit = theme::button(ui, "Quit", 110.0);
                        let defaults = theme::button(ui, "Defaults", 110.0).on_hover_text(
                            "Every setting back to the recommended one; Save or Play writes them",
                        );
                        let save = theme::button(ui, "Save", 110.0);
                        for (name, item, asked) in [
                            ("play", &play, Action::Play),
                            ("save", &save, Action::Save),
                            ("defaults", &defaults, Action::Defaults),
                            ("quit", &quit, Action::Quit),
                        ] {
                            self.marks.put(format!("button.{name}"), item);
                            if item.clicked() {
                                action = Some(asked);
                            }
                        }
                    });
                });
                for note in &self.notes {
                    ui.label(RichText::new(&note.text).size(13.0).color(note.colour()));
                }
            });

        egui::CentralPanel::no_frame().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(self.page)
                .auto_shrink(false)
                .show(ui, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin {
                            left: 40,
                            right: 24,
                            top: 6,
                            bottom: 28,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(theme::COLUMN);
                            if let Some(asked) = self.page_ui(ui, preview) {
                                action = Some(asked);
                            }
                        });
                });
        });

        self.focus_before = ui.ctx().memory(|memory| memory.focused()).is_some();
        self.popup_before = ui.ctx().any_popup_open();
        action
    }

    /// The page's title, its "Restore defaults" and the page itself.
    fn page_ui(
        &mut self,
        ui: &mut egui::Ui,
        preview: Option<&Result<egui::TextureId, String>>,
    ) -> Option<Action> {
        let title = match self.page {
            Page::Play => None,
            Page::Display => Some("Display"),
            Page::Controls => Some("Controls"),
        };
        if let Some(title) = title {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(46.0).color(theme::TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let restore = ui.link(
                        RichText::new("Restore defaults")
                            .size(15.0)
                            .color(theme::MUTED),
                    );
                    self.marks.put("page.restore_defaults", &restore);
                    if restore.clicked() {
                        let recommended = Settings::recommended();
                        let settings = &mut self.form.settings;
                        match self.page {
                            Page::Display => {
                                let folder = settings.display.crt_folder.clone();
                                settings.display = recommended.display;
                                settings.display.crt_folder = folder;
                                settings.effects = recommended.effects;
                            }
                            _ => settings.controls = recommended.controls,
                        }
                        self.form = Form::new(&self.form.settings);
                    }
                });
            });
            theme::rule(ui);
        }
        match self.page {
            Page::Play => {
                let quick_save = quick_save_note(&self.form.settings, &self.base);
                let names = self
                    .pads
                    .as_ref()
                    .map_or(&[][..], |pads| pads.names.as_slice());
                let view = form::PlayView {
                    discs: &self.discs,
                    card: &self.card,
                    quick_save: &quick_save,
                    pads: names,
                };
                if self.form.play_page(ui, &mut self.marks, &view) {
                    return Some(Action::Rescan);
                }
            }
            Page::Display => self.form.display_page(ui, &mut self.marks, preview),
            Page::Controls => {
                let (names, state) = match &self.pads {
                    Some(pads) => (pads.names.as_slice(), pads.state),
                    None => (&[][..], PadState::default()),
                };
                let view = form::PadView {
                    backend: gamepad::BACKEND,
                    names,
                    state,
                };
                self.form
                    .controls_page(ui, &mut self.marks, &view, &mut self.capture);
            }
        }
        None
    }

    /// The page behind the panels: the background colour, the art on the
    /// right fading into it.
    fn paint_background(&self, ctx: &egui::Context) {
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::background());
        painter.rect_filled(screen, 0.0, theme::BACKGROUND);
        let Some(art) = &self.art else {
            return;
        };
        let rect = egui::Rect::from_min_max(
            egui::pos2(screen.left() + screen.width() * 0.36, screen.top()),
            screen.max,
        );
        // Cover the area, keeping the art's right side (its subject).
        let size = art.size_vec2();
        let (area, image) = (rect.width() / rect.height(), size.x / size.y);
        let uv = if image > area {
            let width = area / image;
            egui::Rect::from_min_max(egui::pos2(1.0 - width, 0.0), egui::pos2(1.0, 1.0))
        } else {
            let height = image / area;
            egui::Rect::from_min_max(
                egui::pos2(0.0, (1.0 - height) / 2.0),
                egui::pos2(1.0, (1.0 + height) / 2.0),
            )
        };
        painter.image(art.id(), rect, uv, Color32::WHITE);
        let fade = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(rect.left() + rect.width() * 0.45, rect.bottom()),
        );
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(fade.left_top(), theme::BACKGROUND);
        mesh.colored_vertex(fade.right_top(), Color32::TRANSPARENT);
        mesh.colored_vertex(fade.left_bottom(), theme::BACKGROUND);
        mesh.colored_vertex(fade.right_bottom(), Color32::TRANSPARENT);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 3, 2);
        painter.add(mesh);
    }

    /// The layout hook's JSON: the controls' rectangles in window pixels.
    fn layout_json(&self, pixels_per_point: f32) -> String {
        let scale = |value: f32| (value * pixels_per_point).round();
        let widgets: serde_json::Map<String, serde_json::Value> = self
            .marks
            .0
            .iter()
            .map(|(name, rect)| {
                let rect = [
                    scale(rect.min.x),
                    scale(rect.min.y),
                    scale(rect.max.x),
                    scale(rect.max.y),
                ];
                (name.clone(), serde_json::json!(rect))
            })
            .collect();
        let page = Page::ALL
            .iter()
            .find(|(page, _, _)| *page == self.page)
            .map_or("", |(_, key, _)| key);
        let settings = self.form.settings();
        let problems: Vec<_> = form::problems(&settings)
            .into_iter()
            .map(|problem| problem.text)
            .collect();
        let (readiness, headline, _) = self.discs.status();
        serde_json::to_string_pretty(&serde_json::json!({
            "pixels_per_point": pixels_per_point,
            "page": page,
            "dirty": settings != self.saved,
            "status": self.status.as_ref().map(|status| status.text.clone()),
            "notes": self.notes.iter().map(|note| note.text.clone()).collect::<Vec<_>>(),
            "problems": problems,
            "discs": format!("{readiness:?}: {headline}"),
            "capture": self.capture.map(|c| format!("{:?} {}", c.button, c.slot)),
            "pads": self.pads.as_ref().map(|pads| pads.names.clone()),
            "widgets": widgets,
        }))
        .unwrap_or_default()
    }
}

/// The logo: the game's name over "electrosphere", and "native port".
fn logo(ui: &mut egui::Ui) {
    ui.vertical(|ui| {
        ui.label(
            RichText::new(theme::spaced("ACE COMBAT 3"))
                .size(27.0)
                .color(theme::TEXT),
        );
        ui.label(
            RichText::new(theme::spaced("electrosphere"))
                .size(15.0)
                .color(theme::TEXT),
        );
    });
    ui.add_space(14.0);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 44.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::LINE);
    ui.add_space(14.0);
    ui.label(
        RichText::new(theme::spaced("NATIVE PORT"))
            .size(12.0)
            .color(theme::MUTED),
    );
}

/// A page tab: lime and underlined when it is the page shown.
fn tab(ui: &mut egui::Ui, selected: bool, name: &str) -> egui::Response {
    let text =
        RichText::new(name)
            .size(19.0)
            .color(if selected { theme::ACCENT } else { theme::TEXT });
    let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
    if selected {
        let rect = response.rect;
        ui.painter().line_segment(
            [
                rect.left_bottom() + vec2(-6.0, 7.0),
                rect.right_bottom() + vec2(6.0, 7.0),
            ],
            egui::Stroke::new(2.0, theme::ACCENT),
        );
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Show `file` in the system's file manager.
fn show_file(file: &Path) {
    #[cfg(windows)]
    let result = Command::new("explorer")
        .arg(format!("/select,{}", file.display()))
        .spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg("-R").arg(file).spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let result = Command::new("xdg-open")
        .arg(file.parent().unwrap_or(Path::new(".")))
        .spawn();
    if let Err(error) = result {
        eprintln!("ac3: launcher: cannot open {}: {error}", file.display());
    }
}

/// Open `folder` in the system's file manager.
fn open_folder(folder: &Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(error) = Command::new(program).arg(folder).spawn() {
        eprintln!("ac3: launcher: cannot open {}: {error}", folder.display());
    }
}

/// The discs to play: `--disc` given to the launcher, else `[paths] disc`,
/// else those next to the settings file.
fn find_discs(options: &Options, settings: &Settings, base: &Path) -> Discs {
    let given = options
        .game_args
        .iter()
        .position(|arg| arg == "--disc")
        .and_then(|at| options.game_args.get(at + 1))
        .map(PathBuf::from);
    let folder = if base.as_os_str().is_empty() {
        Path::new(".")
    } else {
        base
    };
    let mut discs = match (given, settings.paths.disc(base)) {
        (Some(disc), _) => Discs::named(&disc, None),
        (None, Some(disc)) => Discs::named(&disc, settings.paths.disc2(base).as_deref()),
        (None, None) => Discs::scan(folder),
    };
    discs.check_resident();
    discs
}

/// Memory card 1 in words; its file is made (empty and formatted) when
/// missing.
fn ensure_card(settings: &Settings, base: &Path, notes: &mut Vec<Message>) -> String {
    match settings.paths.card(0, base) {
        Some(CardSource::File(path)) => {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            if path.is_file() {
                return format!("{name}: your saves");
            }
            match crate::machine_card::MemoryCard::open(&path).and_then(|card| card.save()) {
                Ok(()) => {
                    eprintln!("ac3: launcher: made memory card 1 at {}", path.display());
                    format!("{name}: new and empty, ready for saves")
                }
                Err(error) => {
                    notes.push(Message::new(
                        Tone::Error,
                        format!(
                            "Memory card 1 cannot be made at {} ({error}).",
                            path.display()
                        ),
                    ));
                    format!("{name}: cannot be made")
                }
            }
        }
        Some(CardSource::Absent) => "No card in slot 1".to_owned(),
        _ => "Kept in memory: saves are lost when the game closes".to_owned(),
    }
}

/// Where F5 and F8 save and load, in words.
fn quick_save_note(settings: &Settings, base: &Path) -> String {
    let card1 = settings.paths.card(0, base).unwrap_or(CardSource::Blank);
    let path = settings
        .paths
        .quick_save(base)
        .unwrap_or_else(|| crate::savestate::default_slot(&card1));
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    format!("F5 saves, F8 loads ({name})")
}

/// `path` with `.bak` added (`ac3.toml.bak`).
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// A TOML error's first and last lines (its position and its reason).
fn compact(error: &str) -> String {
    let lines: Vec<_> = error
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match lines.as_slice() {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [first, .., last] => format!("{first}: {last}"),
    }
}

/// The settings in `path` and notes about the file (no file: the
/// recommended settings).
fn load(path: &Path) -> (Settings, Vec<Message>) {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let note = format!("No {name} yet: the recommended settings, written by Save or Play.");
            return (
                Settings::recommended(),
                vec![Message::new(Tone::Info, note)],
            );
        }
        Err(error) => {
            let note =
                format!("{name} cannot be read ({error}); showing the recommended settings.");
            return (
                Settings::recommended(),
                vec![Message::new(Tone::Error, note)],
            );
        }
    };
    let document = match Document::parse(&text) {
        Ok(document) => document,
        Err(error) => {
            let note = format!(
                "{name} is not valid TOML ({}). Showing the recommended settings; Save keeps the \
                 file as {name}.bak and writes a new one.",
                compact(&error)
            );
            return (
                Settings::recommended(),
                vec![Message::new(Tone::Error, note)],
            );
        }
    };
    let contents = document.read();
    let mut notes = Vec::new();
    if let Some(error) = &contents.error {
        notes.push(Message::new(
            Tone::Warning,
            format!(
                "{name}: {}. These keys show their defaults, which Save writes: {}.",
                compact(error),
                contents.reset.join(", ")
            ),
        ));
    }
    (contents.settings, notes)
}

// ---- gamepads ------------------------------------------------------------------------------

/// The connected gamepads, read with the button map being edited.
struct Pads {
    pads: Gamepads,
    /// The mapping last asked for, and when.
    wanted: Bindings,
    changed: Instant,
    state: PadState,
    names: Vec<String>,
    next_poll: Instant,
}

/// `bindings` for reading only (no rumble), as `Gamepads` keeps them.
fn probe(bindings: &Bindings) -> Bindings {
    Bindings {
        rumble: false,
        ..bindings.clone()
    }
    .sanitized()
}

impl Pads {
    fn new(bindings: &Bindings) -> Self {
        let wanted = probe(bindings);
        let now = Instant::now();
        Self {
            pads: Gamepads::new(&wanted),
            wanted,
            changed: now,
            state: PadState::default(),
            names: Vec::new(),
            next_poll: now,
        }
    }

    /// Read the pads when due, with `bindings`; true when what they show
    /// changed.
    fn poll(&mut self, bindings: &Bindings, now: Instant) -> bool {
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now + PAD_POLL;
        let wanted = probe(bindings);
        if wanted != self.wanted {
            self.wanted = wanted;
            self.changed = now;
        }
        if *self.pads.bindings() != self.wanted && now.duration_since(self.changed) >= PAD_REBUILD {
            // `Gamepads` maps with the bindings it starts with.
            self.pads = Gamepads::new(&self.wanted);
        }
        let state = self.pads.poll();
        let names = self.pads.names();
        let changed = state != self.state || names != self.names;
        self.state = state;
        self.names = names;
        changed
    }
}

// ---- the window ------------------------------------------------------------------------------

struct App {
    launcher: Launcher,
    ctx: egui::Context,
    view: Option<View>,
    /// Why the launcher could not run (its window or GPU).
    failure: Option<String>,
    /// The game being started, and since when.
    starting: Option<(Child, Instant)>,
    /// When egui wants the next frame.
    repaint_at: Option<Instant>,
    /// The layout hook's file and what was last written to it.
    layout: Option<(PathBuf, String)>,
    /// The background art (`None`: it did not decode).
    art: Option<Arc<RgbaImage>>,
}

/// The window, its GPU surface and egui's input state.
struct View {
    window: Arc<Window>,
    gpu: Gpu,
    input: egui_winit::State,
}

impl View {
    fn open(event_loop: &ActiveEventLoop, ctx: &egui::Context) -> crate::Result<Self> {
        // 1280x820 points, less on a small screen (the pages scroll).
        let (mut width, mut height) = (1280.0_f64, 820.0_f64);
        if let Some(monitor) = event_loop.primary_monitor() {
            let screen = monitor.size().to_logical::<f64>(monitor.scale_factor());
            width = width.min(screen.width * 0.92);
            height = height.min(screen.height * 0.88);
        }
        let attributes = Window::default_attributes()
            .with_title(TITLE)
            .with_inner_size(LogicalSize::new(width, height))
            .with_min_inner_size(LogicalSize::new(900.0, 600.0));
        let window = Arc::new(event_loop.create_window(attributes)?);
        let gpu = Gpu::new(event_loop, &window)?;
        let input = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(gpu.device.limits().max_texture_dimension_2d as usize),
        );
        Ok(Self { window, gpu, input })
    }
}

fn earliest(a: Option<Instant>, b: Instant) -> Option<Instant> {
    Some(a.map_or(b, |a| a.min(b)))
}

impl App {
    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(view) = &mut self.view else {
            return;
        };
        if self.launcher.art.is_none()
            && let Some(art) = &self.art
        {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [art.width as usize, art.height as usize],
                &art.pixels,
            );
            self.launcher.art = Some(self.ctx.load_texture(
                "ac3 art",
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
        // The CRT preview, drawn before the frame when it changed.
        let shown = match (self.launcher.preview_want(), &self.art) {
            (Some(want), Some(art)) => {
                let gpu = &mut view.gpu;
                let preview = gpu.preview.get_or_insert_with(|| {
                    preview::Preview::new(&gpu.device, &gpu.queue, &mut gpu.renderer, art)
                });
                preview.show(&gpu.device, &gpu.queue, &want);
                preview.shown.as_ref().map(|(_, result)| result.clone())
            }
            (Some(_), None) => Some(Err("the background art did not decode".to_owned())),
            (None, _) => None,
        };
        let input = view.input.take_egui_input(&view.window);
        let launcher = &mut self.launcher;
        let mut action = None;
        let output = self.ctx.run_ui(input, |ui| {
            let asked = launcher.ui(ui, shown.as_ref());
            action = action.or(asked);
        });
        view.input
            .handle_platform_output(&view.window, output.platform_output);
        let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        if !view.gpu.paint(
            &jobs,
            output.textures_delta,
            output.pixels_per_point,
            theme::BACKGROUND,
        ) {
            view.window.request_redraw();
        }
        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |viewport| viewport.repaint_delay);
        if delay.is_zero() {
            view.window.request_redraw();
        } else if delay < Duration::from_secs(60) {
            self.repaint_at = Some(Instant::now() + delay);
        }
        self.write_layout(output.pixels_per_point);
        if let Some(action) = action {
            self.act(action, event_loop);
        }
    }

    fn act(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        let launcher = &mut self.launcher;
        match action {
            Action::Save => {
                launcher.status = Some(match launcher.save() {
                    Ok(None) => Message::new(Tone::Good, "Saved."),
                    Ok(Some(kept)) => Message::new(
                        Tone::Good,
                        format!("Saved. The old file is kept as {}.", kept.display()),
                    ),
                    Err(error) => Message::new(Tone::Error, error),
                });
            }
            Action::Defaults => {
                let current = launcher.form.settings();
                let mut recommended = Settings::recommended();
                // The paths are not the pages' to reset.
                recommended.paths = current.paths;
                recommended.display.crt_folder = current.display.crt_folder;
                launcher.form = Form::new(&recommended);
                launcher.capture = None;
                launcher.status = Some(Message::new(
                    Tone::Info,
                    "The recommended settings: Save or Play writes them.",
                ));
            }
            Action::Quit => {
                if launcher.quit() {
                    event_loop.exit();
                    return;
                }
            }
            Action::Rescan => launcher.rescan(),
            Action::Play => match launcher.play() {
                Ok(child) => {
                    launcher.pads = None;
                    launcher.status = None;
                    if let Some(view) = &self.view {
                        view.window.set_visible(false);
                    }
                    self.starting = Some((child, Instant::now()));
                    return;
                }
                Err(error) => launcher.status = Some(Message::new(Tone::Error, error)),
            },
        }
        if let Some(view) = &self.view {
            view.window.request_redraw();
        }
    }

    fn write_layout(&mut self, pixels_per_point: f32) {
        let Some((path, written)) = &mut self.layout else {
            return;
        };
        let text = self.launcher.layout_json(pixels_per_point);
        if text == *written {
            return;
        }
        let mut name = path.as_os_str().to_owned();
        name.push(".tmp");
        let temporary = PathBuf::from(name);
        let result =
            std::fs::write(&temporary, &text).and_then(|()| std::fs::rename(&temporary, &*path));
        match result {
            Ok(()) => *written = text,
            Err(error) => eprintln!("ac3: launcher: {}: {error}", path.display()),
        }
    }
}

/// What a starting game did since the last look.
enum Start {
    Running,
    Failed(ExitStatus),
    Done,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.view.is_some() {
            return;
        }
        theme::apply(&self.ctx);
        match View::open(event_loop, &self.ctx) {
            Ok(view) => {
                view.window.request_redraw();
                self.view = Some(view);
            }
            Err(error) => {
                self.failure = Some(format!("the launcher window cannot open: {error}"));
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(view) = &mut self.view else {
            return;
        };
        // A key binding waiting for its key takes the next key press.
        if self.launcher.capture.is_some()
            && let WindowEvent::KeyboardInput { event: key, .. } = &event
        {
            if key.state == ElementState::Pressed
                && !key.repeat
                && let PhysicalKey::Code(code) = key.physical_key
            {
                self.launcher.captured(code);
                view.window.request_redraw();
            }
            return;
        }
        let response = view.input.on_window_event(&view.window, &event);
        match &event {
            WindowEvent::CloseRequested => {
                if self.launcher.quit() {
                    event_loop.exit();
                    return;
                }
                view.window.request_redraw();
            }
            WindowEvent::Resized(size) => view.gpu.resize(*size),
            WindowEvent::RedrawRequested => {
                self.redraw(event_loop);
                return;
            }
            _ => {}
        }
        if response.repaint {
            view.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let mut wake = None;
        if let Some((child, started)) = &mut self.starting {
            let start = match child.try_wait() {
                Ok(Some(status)) if !status.success() => Start::Failed(status),
                Ok(Some(_)) => Start::Done,
                Ok(None) if now.duration_since(*started) >= START_GRACE => {
                    eprintln!("ac3: launcher: the game is running; the launcher closes");
                    Start::Done
                }
                Ok(None) => Start::Running,
                Err(error) => {
                    eprintln!("ac3: launcher: the game process: {error}");
                    Start::Done
                }
            };
            match start {
                Start::Running => wake = earliest(wake, now + GAME_POLL),
                Start::Done => {
                    event_loop.exit();
                    return;
                }
                Start::Failed(status) => {
                    self.starting = None;
                    self.launcher.game_stopped(status);
                    if let Some(view) = &self.view {
                        view.window.set_visible(true);
                        view.window.focus_window();
                        view.window.request_redraw();
                    }
                }
            }
        }
        if self.starting.is_none()
            && let Some(pads) = &mut self.launcher.pads
        {
            let changed = pads.poll(&self.launcher.form.settings.controls, now);
            if changed
                && matches!(self.launcher.page, Page::Controls | Page::Play)
                && let Some(view) = &self.view
            {
                view.window.request_redraw();
            }
            wake = earliest(wake, pads.next_poll);
        }
        if let Some(at) = self.repaint_at {
            if at <= now {
                self.repaint_at = None;
                if let Some(view) = &self.view {
                    view.window.request_redraw();
                }
            } else {
                wake = earliest(wake, at);
            }
        }
        event_loop.set_control_flow(match wake {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }
}

// ---- the GPU ----------------------------------------------------------------------------------

/// The window's surface and egui's renderer.
struct Gpu {
    window: Arc<Window>,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The format egui draws in: the surface's, without sRGB.
    view_format: wgpu::TextureFormat,
    renderer: egui_wgpu::Renderer,
    /// The window has an area.
    configured: bool,
    /// The Display page's CRT preview, made when first shown.
    preview: Option<preview::Preview>,
}

impl Gpu {
    /// Vulkan, DX12 or Metal first, OpenGL when none of those works;
    /// `WGPU_BACKEND` picks instead (as the game window does). The low-power
    /// adapter suits a settings window (`WGPU_POWER_PREF` overrides).
    fn new(event_loop: &ActiveEventLoop, window: &Arc<Window>) -> crate::Result<Self> {
        let attempts = match wgpu::Backends::from_env() {
            Some(backends) => vec![backends],
            None => vec![wgpu::Backends::PRIMARY, wgpu::Backends::GL],
        };
        let mut failures = Vec::new();
        for backends in attempts {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(
                    event_loop.owned_display_handle(),
                ))
            });
            let surface = match instance.create_surface(window.clone()) {
                Ok(surface) => surface,
                Err(error) => {
                    failures.push(format!("{backends:?}: {error}"));
                    continue;
                }
            };
            let options = wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::from_env()
                    .unwrap_or(wgpu::PowerPreference::LowPower),
                compatible_surface: Some(&surface),
                ..Default::default()
            };
            let adapter = match pollster::block_on(instance.request_adapter(&options)) {
                Ok(adapter) => Ok(adapter),
                Err(_) => {
                    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                        force_fallback_adapter: true,
                        ..options
                    }))
                }
            };
            match adapter {
                Ok(adapter) => return Self::with_adapter(window, instance, surface, &adapter),
                Err(error) => failures.push(format!("{backends:?}: {error}")),
            }
        }
        Err(format!(
            "no graphics adapter can draw to the window ({})",
            failures.join("; ")
        )
        .into())
    }

    fn with_adapter(
        window: &Arc<Window>,
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        adapter: &wgpu::Adapter,
    ) -> crate::Result<Self> {
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("ac3 launcher"),
                required_limits:
                    wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            }))?;
        device.on_uncaptured_error(Arc::new(|error| {
            eprintln!("ac3: launcher: GPU error: {error}");
        }));
        let capabilities = surface.get_capabilities(adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .or_else(|| capabilities.formats.first().copied())
            .ok_or("the window surface offers no formats")?;
        // egui blends in gamma space: draw through a non-sRGB view.
        let view_format = format.remove_srgb_suffix();
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: if view_format == format {
                Vec::new()
            } else {
                vec![view_format]
            },
        };
        let configured = size.width > 0 && size.height > 0;
        if configured {
            surface.configure(&device, &config);
        }
        let info = adapter.get_info();
        eprintln!(
            "ac3: launcher: display: {} ({}), {format:?}",
            info.name, info.backend
        );
        let renderer =
            egui_wgpu::Renderer::new(&device, view_format, egui_wgpu::RendererOptions::default());
        Ok(Self {
            window: window.clone(),
            instance,
            surface,
            device,
            queue,
            config,
            view_format,
            renderer,
            configured,
            preview: None,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.configured = size.width > 0 && size.height > 0;
        if self.configured {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Upload egui's texture changes, draw `jobs` and present. False when the
    /// frame must be drawn again (the surface was set up again).
    fn paint(
        &mut self,
        jobs: &[egui::ClippedPrimitive],
        mut textures: egui::TexturesDelta,
        pixels_per_point: f32,
        background: Color32,
    ) -> bool {
        for (id, deltas) in textures.set.drain() {
            for delta in &deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, id, delta);
            }
        }
        let presented = !self.configured || self.present(jobs, pixels_per_point, background);
        for id in textures.free.drain() {
            self.renderer.free_texture(&id);
        }
        presented
    }

    fn present(
        &mut self,
        jobs: &[egui::ClippedPrimitive],
        pixels_per_point: f32,
        background: Color32,
    ) -> bool {
        let (texture, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => (texture, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => (texture, true),
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                if let Ok(surface) = self.instance.create_surface(self.window.clone()) {
                    self.surface = surface;
                    self.surface.configure(&self.device, &self.config);
                }
                return false;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return true,
        };
        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..Default::default()
        });
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point,
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ac3 launcher"),
            });
        let mut commands =
            self.renderer
                .update_buffers(&self.device, &self.queue, &mut encoder, jobs, &screen);
        {
            let channel = |value: u8| f64::from(value) / 255.0;
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ac3 launcher"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: channel(background.r()),
                            g: channel(background.g()),
                            b: channel(background.b()),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            self.renderer
                .render(&mut pass.forget_lifetime(), jobs, &screen);
        }
        commands.push(encoder.finish());
        self.queue.submit(commands);
        self.queue.present(texture);
        if suboptimal {
            self.surface.configure(&self.device, &self.config);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamepad::PsButton;

    fn options(dir: &Path, game_args: Vec<OsString>) -> Options {
        Options {
            settings: dir.join(crate::settings::FILE_NAME),
            data_dir: dir.join("extracted/disc"),
            disc: dir.join("disc.bin"),
            game_args,
        }
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ac3-launcher-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn errors_are_compacted() {
        let error = "TOML parse error at line 6, column 12\n  |\n6 | deadzone = \"wide\"\n  |            ^^^^^^\ninvalid type: string \"wide\", expected f64\n";
        assert_eq!(
            compact(error),
            "TOML parse error at line 6, column 12: invalid type: string \"wide\", expected f64"
        );
        assert_eq!(compact("one line"), "one line");
        assert_eq!(
            backup_path(Path::new("a/ac3.toml")),
            Path::new("a/ac3.toml.bak")
        );
    }

    #[test]
    fn save_play_and_quit() {
        let dir = folder("save");
        let given = || vec!["--disc".into(), "x.bin".into()];
        let mut launcher = Launcher::new(options(&dir, given()));
        assert!(!launcher.dirty());
        assert!(
            launcher.notes[0].text.contains("recommended"),
            "{:?}",
            launcher.notes
        );
        assert_eq!(launcher.form.settings, Settings::recommended());
        // Memory card 1 is made on the first run.
        assert!(dir.join(crate::settings::CARD).is_file());
        assert!(launcher.card.contains("new"), "{}", launcher.card);
        assert!(launcher.quit(), "nothing to lose");
        // An edit: the first Quit warns, the second goes.
        launcher.form.settings.display.vsync = false;
        assert!(launcher.dirty());
        assert!(!launcher.quit());
        assert!(launcher.quit());
        // Save creates the file from the commented default file.
        assert_eq!(launcher.save().unwrap(), None);
        let text = std::fs::read_to_string(&launcher.options.settings).unwrap();
        assert!(text.starts_with("# Ace Combat 3 (native port) settings."));
        assert!(text.contains("\nvsync = false\n"));
        assert!(text.contains("\ncard1 = \"card1.mcr\"\n"));
        assert!(!launcher.dirty());
        // A problem stops Save.
        launcher.form.settings.display.crt_preset = "royale".into();
        assert!(launcher.save().is_err());
        launcher.form.settings.display.crt_preset = crate::present::crt::DEFAULT.into();
        // A second run finds the card.
        assert!(
            Launcher::new(options(&dir, given()))
                .card
                .contains("your saves")
        );
        // A file that is not TOML is kept and replaced.
        std::fs::write(&launcher.options.settings, "[display\n").unwrap();
        let mut launcher = Launcher::new(options(&dir, given()));
        assert!(
            launcher.notes[0].text.contains("not valid TOML"),
            "{:?}",
            launcher.notes
        );
        let kept = launcher.save().unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "[display\n");
        assert!(Settings::load(&launcher.options.settings).is_ok());
        // Play runs `machine --settings FILE` and the options passed on (a
        // disc given there is the game's to find).
        let command = launcher.game_command(Path::new("game"));
        assert_eq!(command.get_program(), "game");
        let args: Vec<_> = command.get_args().collect();
        let os = std::ffi::OsStr::new;
        assert_eq!(
            args,
            [
                os("machine"),
                os("--settings"),
                launcher.options.settings.as_os_str(),
                os("--disc"),
                os("x.bin")
            ]
        );
        let error = launcher.start(&dir.join("no-such-game")).unwrap_err();
        assert!(error.contains("did not start"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn key_capture_binds_refuses_and_cancels() {
        let dir = folder("keys");
        let mut launcher = Launcher::new(options(&dir, Vec::new()));
        assert_eq!(
            launcher.discs.readiness(),
            Readiness::Missing,
            "no discs here"
        );
        let capture = Capture {
            button: PsButton::Cross,
            slot: 0,
        };
        launcher.capture = Some(capture);
        launcher.captured(KeyCode::KeyX);
        assert_eq!(launcher.form.settings.controls.keys.cross, ["KeyX"]);
        assert_eq!(launcher.capture, None);
        launcher.capture = Some(capture);
        launcher.captured(KeyCode::F5);
        assert!(launcher.status.is_some(), "the window's key");
        assert_eq!(launcher.form.settings.controls.keys.cross, ["KeyX"]);
        launcher.capture = Some(Capture { slot: 1, ..capture });
        launcher.captured(KeyCode::Escape);
        assert_eq!(
            launcher.form.settings.controls.keys.cross,
            ["KeyX"],
            "cancelled"
        );
        assert_eq!(launcher.capture, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_preview_follows_the_page_and_the_crt() {
        let dir = folder("preview");
        let mut launcher = Launcher::new(options(&dir, Vec::new()));
        assert_eq!(launcher.preview_want(), None, "the Play page");
        launcher.page = Page::Display;
        let Some(preview::Want::Crt(preset)) = launcher.preview_want() else {
            panic!("the CRT is on");
        };
        assert!(
            preset.ends_with("curved/trinitron-s-video.slangp"),
            "{}",
            preset.display()
        );
        launcher.form.settings.display.crt = false;
        assert_eq!(launcher.preview_want(), Some(preview::Want::Plain));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
