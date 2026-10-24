//! Presentation: the game window (winit), the GPU scaler (wgpu) and the
//! on-screen messages.
//!
//! [`Presenter`] owns the window and its surface. The game loop hands it
//! every displayed frame ([`Presenter::set_frame`]) and calls
//! [`Presenter::render`]. Presentation only shows the game; it never paces
//! it (the loop paces VBlanks by the wall clock).
//!
//! With the GPU renderer on ([`crate::gpu_renderer`]), the presenter also
//! owns its [`Executor`] on the same device: the loop hands it each pass's
//! recorded frame ([`Presenter::execute`]) and the display area to show
//! ([`Presenter::set_vram_frame`]), which is drawn from the card's N x VRAM
//! straight into the picture, without a readback.
//!
//! - [`scale`]: where the picture goes (4:3, stretch, integer; the wider
//!   aspect of widescreen flight, [`Presenter::set_aspect`]), pure math.
//! - [`gpu`]: the wgpu pipeline, textures and drawing, also used offscreen.
//! - [`osd`] and [`font`]: on-screen messages in a built-in bitmap font.
//! - [`movie`]: a movie pack's image over the movie frame
//!   ([`Presenter::set_movie`]).
//! - [`shader`]: a RetroArch slang shader preset over the picture, movies
//!   included: [`crt`]'s bundled looks (`[display] crt`; F2 turns the CRT
//!   off and on, Shift+F3 steps to the next look, F4 swaps curved and flat)
//!   or a preset of one's own (`shader_preset`; Shift+F2 loads it again).
//! - [`picker`]: debug, Shift+F3 steps through a folder of presets of
//!   one's own.
pub mod crt;
pub mod font;
pub mod gpu;
pub mod movie;
pub mod osd;
pub mod picker;
pub mod scale;
pub mod shader;

use crate::gpu_renderer::{self, DisplayArea, Executor, Frame};
use crate::settings::{self, WindowMode};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::{Fullscreen, Window, WindowId},
};

/// The game window, its GPU surface and the on-screen messages.
pub struct Presenter {
    window: Arc<Window>,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: gpu::Renderer,
    osd: osd::Osd,
    /// Alt is held (for Alt+Enter).
    alt: bool,
    /// Shift is held (for Shift+F2 and Shift+F3).
    shift: bool,
    /// Debug: the shader preset pack Shift+F3 steps through.
    picker: Option<picker::Picker>,
    /// The bundled CRT look shown or to show (`None` with a preset of
    /// one's own).
    crt: Option<crt::Crt>,
    /// Whether the surface is configured (not while the window has no area).
    configured: bool,
    /// Surface problems already reported, so each is logged once.
    reported: Vec<&'static str>,
    /// The GPU renderer (when the settings ask for it and the card runs it)
    /// and its options.
    hw: Option<(Executor, gpu_renderer::Options)>,
    /// The GPU renderer's display area to show; `None`: the software frame.
    hw_area: Option<DisplayArea>,
    /// The widescreen aspect the current picture is shown at (`None`: 4:3).
    aspect: Option<ac3_flight::widescreen::Aspect>,
    /// Adapter, backend, format, present mode and renderer, as logged at
    /// start.
    pub description: String,
}

impl Presenter {
    /// Open the window as `display` says and set up the GPU. Logs the
    /// adapter, format and present mode on stderr.
    pub fn new(
        event_loop: &ActiveEventLoop,
        display: &settings::Display,
        title: &str,
    ) -> crate::Result<Self> {
        let monitor = event_loop
            .primary_monitor()
            .map(|monitor| (monitor.size().width, monitor.size().height));
        // Widescreen: a window left at the default size opens in its shape.
        let defaults = settings::Display::default();
        let requested = scale::widescreen_window(
            (display.window_width, display.window_height),
            (defaults.window_width, defaults.window_height),
            display
                .widescreen
                .aspect()
                .map(ac3_flight::widescreen::Aspect::ratio),
        );
        let (width, height) = scale::fit_window(requested, monitor);
        let mut attributes = Window::default_attributes()
            .with_title(title)
            .with_inner_size(PhysicalSize::new(width, height))
            .with_min_inner_size(PhysicalSize::new(160, 120));
        let borderless = display.mode == WindowMode::Borderless;
        if borderless {
            attributes = attributes.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = Arc::new(event_loop.create_window(attributes)?);
        window.set_cursor_visible(!borderless);
        let (instance, surface, adapter) = connect(event_loop, &window)?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("ac3"),
                required_limits:
                    wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            }))?;
        device.on_uncaptured_error(Arc::new(|error| {
            eprintln!("ac3: display: GPU error: {error}");
        }));
        let capabilities = surface.get_capabilities(&adapter);
        // A plain (non-sRGB) 8-bit format passes the picture through as is;
        // with an sRGB one the renderer samples sRGB textures to match.
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
        let present_mode = present_mode(display.vsync, &capabilities.present_modes);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: Vec::new(),
        };
        let configured = size.width > 0 && size.height > 0;
        if configured {
            surface.configure(&device, &config);
        }
        let info = adapter.get_info();
        let mut renderer =
            gpu::Renderer::new(device, queue, format, display.scaling, display.filter);
        let preset = std::path::Path::new(display.shader_preset.trim());
        let own = !preset.as_os_str().is_empty();
        let picker = own.then(|| picker::Picker::new(preset)).flatten();
        let crt = (!own).then(|| crt::Crt {
            folder: display
                .crt_folder
                .clone()
                .unwrap_or_else(|| crt::FOLDER.into()),
            index: crt::index(&display.crt_preset).unwrap_or(0),
            curved: display.crt_curved,
        });
        let shader_note = match (&picker, &crt) {
            (Some(picker), _) => {
                let note = renderer.set_shader(Some(picker.current()));
                Some(picked(&renderer, picker, note))
            }
            (None, _) if own => renderer.set_shader(Some(preset)),
            (None, Some(crt)) if display.crt => Some(load_crt(&mut renderer, crt)),
            _ => None,
        };
        let hw = display.gpu_renderer().and_then(|options| {
            match Executor::new(
                renderer.device.clone(),
                renderer.queue.clone(),
                options,
                renderer.texel_format(),
            ) {
                Ok(executor) => Some((executor, options)),
                Err(error) => {
                    eprintln!(
                        "ac3: display: GPU renderer unavailable ({error}); the software renderer draws"
                    );
                    None
                }
            }
        });
        let drawing = match &hw {
            Some((_, options)) => format!(
                "GPU renderer {}x{}",
                options.scale,
                if options.dither && options.scale > 1 {
                    " dithered"
                } else {
                    ""
                }
            ),
            None => "software renderer".to_owned(),
        };
        let widescreen = match display.widescreen.aspect() {
            Some(aspect) => format!(", widescreen {} in flight", aspect.name()),
            None => String::new(),
        };
        let shader = match &shader_note {
            Some(note) => format!(", {note}"),
            None => String::new(),
        };
        let description = format!(
            "{} ({}, {:?}), {format:?}, {present_mode:?}, {}x{} {}, {} scaling, {} filter, {drawing}{widescreen}{shader}",
            info.name,
            info.backend,
            info.device_type,
            size.width,
            size.height,
            display.mode.name(),
            display.scaling.name(),
            display.filter.name(),
        );
        eprintln!("ac3: display: {description}");
        let mut presenter = Self {
            window,
            instance,
            surface,
            config,
            renderer,
            osd: osd::Osd::default(),
            alt: false,
            shift: false,
            picker,
            crt,
            configured,
            reported: Vec::new(),
            hw,
            hw_area: None,
            aspect: None,
            description,
        };
        if let Some(note) = shader_note {
            presenter.show_message(note, SHADER_NOTE);
        }
        Ok(presenter)
    }

    /// F2: the CRT (or the preset of one's own) off or on; Shift+F2: load
    /// the preset again.
    fn shader_key(&mut self) {
        let note = if self.shift {
            self.renderer.reload_shader()
        } else {
            let toggled = self.renderer.toggle_shader();
            match &self.crt {
                // The CRT was off at start: load the look now.
                Some(crt) if toggled.is_none() => Some(load_crt(&mut self.renderer, crt)),
                Some(_) if !self.renderer.shader_enabled() => Some("CRT off".to_owned()),
                Some(crt) if self.renderer.shader_loaded() => Some(format!("CRT {}", crt.label())),
                _ => toggled,
            }
        };
        self.say(
            note.unwrap_or_else(|| "No shader preset ([display] shader_preset)".into()),
            SHADER_NOTE,
        );
    }

    /// Shift+F3: the next bundled look, or (debug) the next preset of a
    /// folder of one's own; its name on screen.
    fn next_preset(&mut self) {
        let note = match (self.picker.as_mut(), self.crt.as_mut()) {
            (Some(picker), _) => {
                let path = picker.step().to_owned();
                let note = self.renderer.set_shader(Some(&path));
                picked(&self.renderer, picker, note)
            }
            (None, Some(crt)) => {
                crt.index = (crt.index + 1) % crt::LOOKS.len();
                load_crt(&mut self.renderer, crt)
            }
            (None, None) => "A preset of one's own is set ([display] shader_preset)".into(),
        };
        self.say(note, PICKED);
    }

    /// F4: the bundled look curved or flat.
    fn swap_curvature(&mut self) {
        let note = match self.crt.as_mut() {
            Some(crt) => {
                crt.curved = !crt.curved;
                load_crt(&mut self.renderer, crt)
            }
            None => "Curved and flat are for the bundled CRT looks".into(),
        };
        self.say(note, PICKED);
    }

    /// `note` on screen for `duration` and on stderr; redraw.
    fn say(&mut self, note: String, duration: Duration) {
        eprintln!("ac3: display: {note}");
        self.show_message(note, duration);
        self.window.request_redraw();
    }

    /// The GPU renderer's options, when it draws.
    pub fn gpu_renderer(&self) -> Option<gpu_renderer::Options> {
        self.hw.as_ref().map(|(_, options)| *options)
    }

    /// Run a pass recorded by the GPU renderer's recorder on the card.
    pub fn execute(&mut self, frame: &Frame) {
        if let Some((executor, _)) = &mut self.hw {
            executor.execute(frame);
        }
    }

    /// Why the GPU renderer cannot run effects, if it tried and could not.
    pub fn effects_failed(&self) -> Option<String> {
        self.hw
            .as_ref()
            .and_then(|(executor, _)| executor.effects_failed())
            .map(str::to_owned)
    }

    /// The GPU renderer's executor (effects register their card side here).
    pub fn executor_mut(&mut self) -> Option<&mut Executor> {
        self.hw.as_mut().map(|(executor, _)| executor)
    }

    /// Show `area` of the GPU renderer's VRAM (instead of a software frame)
    /// from the next [`render`](Self::render) on.
    pub fn set_vram_frame(&mut self, area: DisplayArea) {
        self.hw_area = Some(area);
    }

    pub fn window(&self) -> &Window {
        &self.window
    }
    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// Show `text` as a status line above the messages until replaced, or
    /// remove it with `None` (the frame rate).
    pub fn set_status(&mut self, text: Option<String>) {
        self.osd.set_status(text);
    }

    /// Show `text` over the picture for `duration` (it fades out at the end).
    pub fn show_message(&mut self, text: impl Into<String>, duration: Duration) {
        self.osd.show(text, duration, Instant::now());
    }

    /// Show the next pictures at `aspect` (widescreen flight) or 4:3
    /// (`None`). The loop sets it for every frame, from the aspect the game
    /// drew the displayed picture for (`ac3_flight::widescreen::shown_aspect`).
    pub fn set_aspect(&mut self, aspect: Option<ac3_flight::widescreen::Aspect>) {
        if aspect != self.aspect {
            self.aspect = aspect;
            self.renderer.aspect = aspect.map_or(scale::FOUR_BY_THREE, |a| a.ratio());
            self.window.request_redraw();
        }
    }
    /// The aspect of the current picture (`None`: 4:3).
    pub fn aspect(&self) -> Option<ac3_flight::widescreen::Aspect> {
        self.aspect
    }

    /// Upload the displayed frame: `width * height` 0RGB words.
    pub fn set_frame(&mut self, rgb: &[u32], width: usize, height: usize) {
        self.hw_area = None;
        self.renderer.set_picture(rgb, width, height);
    }

    /// Show a movie pack's image over the movie frame of the picture (laid
    /// out in the 1x picture's pixels), or none, from the next
    /// [`render`](Self::render) on.
    pub fn set_movie(&mut self, layer: Option<&crate::movie_pack::MovieLayer>) {
        self.renderer.set_movie(layer);
    }

    /// Switch between windowed and borderless fullscreen.
    pub fn toggle_fullscreen(&mut self) {
        let fullscreen = self.window.fullscreen().is_none();
        self.window
            .set_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
        self.window.set_cursor_visible(!fullscreen);
        eprintln!(
            "ac3: display: {}",
            if fullscreen { "borderless" } else { "windowed" }
        );
    }

    /// Follow a new window size.
    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.configured = size.width > 0 && size.height > 0;
        if self.configured {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.renderer.device, &self.config);
        }
    }

    /// The presentation side of a window event: resizing, redraws and
    /// Alt+Enter. Returns `true` when it used a key press, which the game
    /// must then not see.
    pub fn window_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::Resized(size) => {
                self.resize(*size);
                self.window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.alt = modifiers.state().alt_key();
                self.shift = modifiers.state().shift_key();
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.physical_key == PhysicalKey::Code(KeyCode::F2) =>
            {
                if event.state.is_pressed() && !event.repeat {
                    self.shader_key();
                }
                return true;
            }
            // Plain F3 is the game loop's (the frame rate).
            WindowEvent::KeyboardInput { event, .. }
                if self.shift && event.physical_key == PhysicalKey::Code(KeyCode::F3) =>
            {
                if event.state.is_pressed() && !event.repeat {
                    self.next_preset();
                }
                return true;
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.physical_key == PhysicalKey::Code(KeyCode::F4) =>
            {
                if event.state.is_pressed() && !event.repeat {
                    self.swap_curvature();
                }
                return true;
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.alt
                    && matches!(
                        event.physical_key,
                        PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter)
                    ) =>
            {
                if event.state.is_pressed() && !event.repeat {
                    self.toggle_fullscreen();
                }
                return true;
            }
            _ => {}
        }
        false
    }

    fn report(&mut self, problem: &'static str) {
        if !self.reported.contains(&problem) {
            eprintln!("ac3: display: {problem}");
            self.reported.push(problem);
        }
    }

    /// Draw the current frame and messages and present them. Surface
    /// trouble (minimized, outdated, lost) skips the frame; it is never an
    /// error for the game.
    pub fn render(&mut self) {
        if let Some(change) = self.osd.frame(Instant::now()) {
            self.renderer.set_overlay(change.as_ref());
        }
        if !self.configured {
            return;
        }
        let (texture, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => (texture, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => (texture, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.renderer.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.report("surface lost; recreating it");
                match self.instance.create_surface(self.window.clone()) {
                    Ok(surface) => {
                        self.surface = surface;
                        self.surface.configure(&self.renderer.device, &self.config);
                    }
                    Err(_) => self.report("surface could not be recreated"),
                }
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.report("surface validation error; frame skipped");
                return;
            }
        };
        if let (Some((executor, _)), Some(area)) = (&mut self.hw, self.hw_area)
            && let Some(picture) = executor.render_display(area)
        {
            let picture = picture.clone();
            let generation = executor.picture_generation;
            self.renderer
                .set_picture_texture(&picture, (area.width, area.height), generation);
        }
        let view = texture.texture.create_view(&Default::default());
        self.renderer
            .draw(&view, (self.config.width, self.config.height));
        self.renderer.queue.present(texture);
        if let Some(note) = self.renderer.take_shader_note() {
            eprintln!("ac3: display: {note}");
            self.show_message(note, SHADER_NOTE);
        }
        if suboptimal {
            self.surface.configure(&self.renderer.device, &self.config);
        }
    }
}

/// How long a shader preset's message stays on screen.
const SHADER_NOTE: Duration = Duration::from_secs(4);
/// How long the picker's preset name stays on screen.
const PICKED: Duration = Duration::from_secs(8);

/// Load the bundled look `crt` (and turn the CRT on); the message for it.
fn load_crt(renderer: &mut gpu::Renderer, crt: &crt::Crt) -> String {
    let note = renderer.set_shader(Some(&crt.preset()));
    match note {
        Some(note) if !renderer.shader_loaded() => format!("CRT {}: {note}", crt.label()),
        _ => format!("CRT {}", crt.label()),
    }
}

/// The picker's message for its current preset, `note` being what loading
/// it said: its number and path, and why it did not load if it did not.
fn picked(renderer: &gpu::Renderer, picker: &picker::Picker, note: Option<String>) -> String {
    let label = picker.label();
    match note {
        Some(note) if !renderer.shader_loaded() => format!("Shader {label}: {note}"),
        _ => format!("Shader {label}"),
    }
}

/// Vsync: FIFO (every frame waits for a refresh; FIFO-relaxed would tear
/// whenever a 25/50 Hz frame outlasts a refresh). No vsync: immediate,
/// else mailbox, else FIFO.
pub fn present_mode(vsync: bool, available: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    use wgpu::PresentMode::{Fifo, Immediate, Mailbox};
    let preferred: &[wgpu::PresentMode] = if vsync {
        &[Fifo]
    } else {
        &[Immediate, Mailbox, Fifo]
    };
    preferred
        .iter()
        .copied()
        .find(|mode| available.contains(mode))
        .unwrap_or(Fifo)
}

/// The wgpu instance, the window's surface and an adapter that can draw to
/// it. Vulkan, DX12 or Metal first; OpenGL only when none of those works
/// (it also keeps EGL and WGL from being set up needlessly).
/// `WGPU_BACKEND` (vulkan, dx12, metal, gl; comma-separated) picks instead.
fn connect(
    event_loop: &ActiveEventLoop,
    window: &Arc<Window>,
) -> crate::Result<(wgpu::Instance, wgpu::Surface<'static>, wgpu::Adapter)> {
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
        let found = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())
            .and_then(|surface| {
                let adapter = choose_adapter(&instance, &surface).map_err(|e| e.to_string())?;
                Ok((surface, adapter))
            });
        match found {
            Ok((surface, adapter)) => return Ok((instance, surface, adapter)),
            Err(error) => failures.push(format!("{backends:?}: {error}")),
        }
    }
    Err(format!(
        "no graphics adapter can draw to the window ({})",
        failures.join("; ")
    )
    .into())
}

/// The adapter for the window: `WGPU_ADAPTER_NAME` (a substring of the
/// name) when set, else the high-performance one (`WGPU_POWER_PREF`
/// overrides), else a fallback (software) adapter.
fn choose_adapter(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'_>,
) -> crate::Result<wgpu::Adapter> {
    if let Ok(name) = std::env::var("WGPU_ADAPTER_NAME") {
        let wanted = name.to_lowercase();
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        return adapters
            .into_iter()
            .find(|adapter| {
                adapter.is_surface_supported(surface)
                    && adapter.get_info().name.to_lowercase().contains(&wanted)
            })
            .ok_or_else(|| {
                format!("WGPU_ADAPTER_NAME={name}: no such adapter for the window").into()
            });
    }
    let options = wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::from_env()
            .unwrap_or(wgpu::PowerPreference::HighPerformance),
        compatible_surface: Some(surface),
        ..Default::default()
    };
    match pollster::block_on(instance.request_adapter(&options)) {
        Ok(adapter) => Ok(adapter),
        Err(_) => Ok(
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                force_fallback_adapter: true,
                ..options
            }))
            .map_err(|e| format!("no adapter: {e}"))?,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_modes_follow_vsync() {
        use wgpu::PresentMode::*;
        let all = [Fifo, FifoRelaxed, Immediate, Mailbox];
        assert_eq!(present_mode(true, &all), Fifo);
        assert_eq!(present_mode(false, &all), Immediate);
        assert_eq!(present_mode(false, &[Fifo, Mailbox]), Mailbox);
        assert_eq!(present_mode(false, &[Fifo]), Fifo);
        assert_eq!(present_mode(true, &[]), Fifo);
    }
}
