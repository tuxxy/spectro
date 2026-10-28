//! The window: winit for events, softbuffer to show the rendered pixels.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use spectro_core::canvas::Canvas;
use spectro_core::render::{Axes, Renderer, Selection, plot_rect};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::media::{self, Stream};
use crate::pipeline::{Job, Request};
use crate::player::{Player, Region};
use crate::settings::Settings;
use crate::{VERSION, dialogs};

/// How long the size must stay still before the analysis is redone.
const RESIZE_SETTLE: Duration = Duration::from_millis(160);

/// Frame interval for the playhead while a selection plays.
const PLAYHEAD_FRAME: Duration = Duration::from_millis(33);

pub enum UserEvent {
    Progress,
    Open(PathBuf),
    Save(PathBuf),
}

#[derive(Clone, Copy, PartialEq)]
enum Panel {
    None,
    Help,
    About,
    Settings,
}

struct Gfx {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    _context: softbuffer::Context<Rc<Window>>,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    gfx: Option<Gfx>,
    renderer: Renderer,
    settings: Settings,
    path: Option<PathBuf>,
    streams: Result<Vec<Stream>, String>,
    stream: usize,
    channel: usize,
    job: Option<Job>,
    restart_at: Option<Instant>,
    panel: Panel,
    mods: ModifiersState,
    status: Option<String>,
    cursor: Option<(f32, f32)>,
    /// The last full picture, reused while only the pointer moves.
    base: Option<Canvas>,
    base_dirty: bool,
    /// Where a selection drag started, in device pixels.
    drag: Option<(f32, f32)>,
    selection: Option<Region>,
    player: Option<Player>,
}

pub fn run(file: Option<PathBuf>, settings: Settings, stream: usize, channel: usize) -> Result<(), String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().map_err(|e| e.to_string())?;
    let mut app = App {
        proxy: event_loop.create_proxy(),
        gfx: None,
        renderer: Renderer::new(),
        settings,
        path: None,
        streams: Ok(Vec::new()),
        stream: 0,
        channel: 0,
        job: None,
        restart_at: None,
        panel: Panel::None,
        mods: ModifiersState::empty(),
        status: None,
        cursor: None,
        base: None,
        base_dirty: true,
        drag: None,
        selection: None,
        player: None,
    };
    if let Some(f) = file {
        app.load(f, stream, channel);
    }
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}

fn command_key() -> &'static str {
    if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" }
}

impl App {
    fn current(&self) -> Option<&Stream> {
        self.streams.as_ref().ok().and_then(|s| s.get(self.stream))
    }

    fn load(&mut self, path: PathBuf, stream: usize, channel: usize) {
        self.streams = media::probe(&path);
        self.path = Some(path);
        self.stream = stream;
        self.channel = channel;
        if let Ok(s) = &self.streams {
            self.stream = self.stream.min(s.len().saturating_sub(1));
            let ch = s.get(self.stream).map_or(1, |s| s.channels.max(1) as usize);
            self.channel = self.channel.min(ch - 1);
        }
        self.status = None;
        self.clear_selection();
        self.restart();
        if let Some(g) = &self.gfx {
            g.window.set_title(&self.title());
        }
    }

    fn axes(&self) -> Option<Axes> {
        let g = self.gfx.as_ref()?;
        let s = self.current().filter(|s| s.problem().is_none())?;
        let size = g.window.inner_size();
        let scale = g.window.scale_factor() as f32;
        Some(Axes::new(
            size.width as usize,
            size.height as usize,
            scale,
            s.duration,
            s.sample_rate,
            self.settings.freq_scale,
        ))
    }

    /// The region between the drag start and the pointer.
    fn dragged_region(&self) -> Option<Region> {
        let (start, end, axes) = (self.drag?, self.cursor?, self.axes()?);
        let (t0, f0) = axes.to_data(start.0, start.1);
        let (t1, f1) = axes.to_data(end.0, end.1);
        Some(Region { t0: t0.min(t1), t1: t0.max(t1), f0: f0.min(f1), f1: f0.max(f1) })
    }

    fn clear_selection(&mut self) {
        self.selection = None;
        self.player = None;
        self.drag = None;
    }

    fn play(&mut self) {
        self.player = None;
        let (Some(path), Some(region), Some(s)) = (self.path.clone(), self.selection, self.current()) else { return };
        let nyquist = s.sample_rate as f64 / 2.0;
        match Player::start(&path, self.stream, region, nyquist) {
            Ok(p) => self.player = Some(p),
            Err(e) => self.status = Some(e),
        }
        self.redraw_later();
    }

    /// Left button: drag a box to select and loop it; click to clear.
    fn mouse(&mut self, state: ElementState) {
        if self.panel != Panel::None {
            return;
        }
        match state {
            ElementState::Pressed => {
                let Some(cur) = self.cursor else { return };
                if self.axes().is_some_and(|a| a.contains(cur.0, cur.1)) {
                    self.drag = Some(cur);
                    self.redraw_cursor();
                }
            }
            ElementState::Released => {
                let Some(start) = self.drag else { return };
                let region = self.dragged_region();
                self.drag = None;
                let end = self.cursor.unwrap_or(start);
                let min = 4.0 * self.gfx.as_ref().map_or(1.0, |g| g.window.scale_factor() as f32);
                if (end.0 - start.0).abs() < min || (end.1 - start.1).abs() < min {
                    self.clear_selection();
                } else {
                    self.selection = region;
                    self.play();
                }
                self.redraw_cursor();
            }
        }
    }

    fn title(&self) -> String {
        match self.path.as_ref().and_then(|p| p.file_name()) {
            Some(n) => format!("{} \u{2013} Spectro", n.to_string_lossy()),
            None => "Spectro".into(),
        }
    }

    fn columns(&self) -> usize {
        match &self.gfx {
            Some(g) => {
                let size = g.window.inner_size();
                plot_rect(size.width as usize, size.height as usize, g.window.scale_factor() as f32).w as usize
            }
            None => 0,
        }
    }

    /// Starts (or restarts) the analysis for the current file and settings.
    fn restart(&mut self) {
        self.restart_at = None;
        self.job = None;
        let columns = self.columns();
        let (Some(path), Some(s)) = (self.path.clone(), self.current().cloned()) else { return };
        if columns == 0 || s.problem().is_some() {
            return;
        }
        let req = Request {
            path,
            stream: self.stream,
            channel: self.channel,
            channels: s.channels as usize,
            samples: s.samples(),
            fft_bits: self.settings.fft_bits,
            window: self.settings.window,
            columns,
        };
        let proxy = self.proxy.clone();
        self.job = Some(Job::spawn(req, move || {
            let _ = proxy.send_event(UserEvent::Progress);
        }));
        self.redraw_later();
    }

    fn redraw_later(&mut self) {
        self.base_dirty = true;
        self.redraw_cursor();
    }

    /// Redraws without rebuilding the cached picture.
    fn redraw_cursor(&self) {
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }

    fn description(&self) -> String {
        match &self.streams {
            Err(e) => e.clone(),
            Ok(s) if let Some(p) = s.get(self.stream).and_then(Stream::problem) => p.into(),
            Ok(s) => {
                media::describe(s, self.stream, self.channel, 1 << self.settings.fft_bits, self.settings.window.name())
            }
        }
    }

    fn message(&self) -> Option<String> {
        if self.path.is_none() {
            return Some(format!("Drop an audio file here\nor press {}-O to open one", command_key()));
        }
        if let Some(s) = &self.status {
            return Some(s.clone());
        }
        let job_error = self.job.as_ref().and_then(|j| j.shared.lock().ok().and_then(|s| s.error.clone()));
        job_error.or_else(|| self.streams.as_ref().err().cloned())
    }

    fn panel_rows(&self) -> Option<(&'static str, Vec<(String, String)>)> {
        let cmd = command_key();
        let row = |k: &str, d: &str| (k.to_string(), d.to_string());
        match self.panel {
            Panel::None => None,
            Panel::Help => Some((
                "Keyboard",
                vec![
                    row(&format!("{cmd}-O"), "Open a file (or drop one on the window)"),
                    row(&format!("{cmd}-S"), "Save the picture as PNG"),
                    row(&format!("{cmd}-E"), "Settings"),
                    row("F1 / Shift-F1", "This help / About"),
                    row("c  C", "Next / previous channel"),
                    row("s  S", "Next / previous stream"),
                    row("p  P", "Next / previous palette"),
                    row("f  F", "Next / previous window function"),
                    row("w  W", "Larger / smaller window"),
                    row("u  U", "Raise / lower the upper limit"),
                    row("l  L", "Raise / lower the lower limit"),
                    row("y  Y", "Next / previous frequency scale"),
                    row("g", "Glow on loud components"),
                    row("Drag", "Select a region and loop it, filtered to its band"),
                    row("Space", "Stop / restart the loop"),
                    row("Click", "Clear the selection"),
                    row("Esc", "Close this panel, or clear the selection"),
                ],
            )),
            Panel::About => Some((
                "About spectro",
                vec![
                    row("Version", VERSION),
                    row("What", "Acoustic spectrum analyser, after Spek"),
                    row("Licence", "GNU GPL, version 3 or later"),
                    row("Engine", "no_std Rust, no unsafe code"),
                    row("Audio", "Decoded by FFmpeg"),
                    row("Type", "Source Sans 3, SIL Open Font License"),
                ],
            )),
            Panel::Settings => {
                let s = &self.settings;
                Some((
                    "Settings",
                    vec![
                        row("p  P", &format!("Palette: {}", s.palette.name())),
                        row("f  F", &format!("Window function: {}", s.window.name())),
                        row("w  W", &format!("Window size: {}", 1u32 << s.fft_bits)),
                        row("u  U", &format!("Upper limit: {} dBFS", s.upper_db)),
                        row("l  L", &format!("Lower limit: {} dBFS", s.lower_db)),
                        row("y  Y", &format!("Frequency scale: {}", s.freq_scale.name())),
                        row("g", &format!("Glow: {}", if s.glow { "on" } else { "off" })),
                        row("", "Saved automatically"),
                    ],
                ))
            }
        }
    }

    fn paint(&mut self, canvas: &mut Canvas, scale: f32, base_only: bool) {
        let title = self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let desc = if self.path.is_some() { self.description() } else { String::new() };
        let message = self.message();
        let panel = self.panel_rows();
        let job = self.job.as_ref().map(|j| j.shared.clone());
        let guard = job.as_ref().and_then(|j| j.lock().ok());
        let spec = guard.as_ref().map(|g| &g.spectrogram);
        let mut v = crate::view(&title, &desc, self.current(), spec, &self.settings, scale);
        v.message = message.as_deref();
        v.panel = panel.as_ref().map(|(t, r)| (*t, r.as_slice()));
        v.cursor = if self.panel == Panel::None && self.drag.is_none() { self.cursor } else { None };
        let region = self.dragged_region().or(self.selection);
        v.selection = region.map(|r| Selection {
            t0: r.t0,
            t1: r.t1,
            f0: r.f0,
            f1: r.f1,
            playhead: self.player.as_ref().filter(|_| self.drag.is_none()).map(Player::position),
        });
        if base_only {
            self.renderer.render_base(canvas, &v);
        } else {
            self.renderer.overlay(canvas, &v);
        }
    }

    fn redraw(&mut self) {
        let Some(g) = &self.gfx else { return };
        let size = g.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        let scale = g.window.scale_factor() as f32;
        let (w_px, h_px) = (size.width as usize, size.height as usize);
        let stale = self.base.as_ref().is_none_or(|b| b.width() != w_px || b.height() != h_px);
        if self.base_dirty || stale {
            let mut base = self.base.take().filter(|b| b.width() == w_px && b.height() == h_px);
            let base_canvas = base.get_or_insert_with(|| Canvas::new(w_px, h_px, 0));
            self.paint(base_canvas, scale, true);
            self.base = base;
            self.base_dirty = false;
        }
        let Some(mut canvas) = self.base.clone() else { return };
        self.paint(&mut canvas, scale, false);
        let Some(g) = &mut self.gfx else { return };
        if g.surface.resize(w, h).is_err() {
            return;
        }
        if let Ok(mut buf) = g.surface.buffer_mut() {
            if buf.len() == canvas.pixels().len() {
                buf.copy_from_slice(canvas.pixels());
            }
            let _ = buf.present();
        }
    }

    fn save(&mut self, path: &Path) {
        let Some(g) = &self.gfx else { return };
        let size = g.window.inner_size();
        let scale = g.window.scale_factor() as f32;
        let panel = std::mem::replace(&mut self.panel, Panel::None);
        let mut canvas = Canvas::new(size.width.max(1) as usize, size.height.max(1) as usize, 0);
        let status = self.status.take();
        let cursor = self.cursor.take();
        // The base picture leaves out the crosshair and the selection.
        self.paint(&mut canvas, scale, true);
        self.panel = panel;
        self.cursor = cursor;
        let png = spectro_core::png::encode(canvas.width(), canvas.height(), canvas.pixels());
        self.status = match std::fs::write(path, png) {
            Ok(()) => status,
            Err(e) => Some(format!("Cannot save {}: {e}", path.display())),
        };
    }

    fn key(&mut self, event_loop: &ActiveEventLoop, key: Key) {
        let command = if cfg!(target_os = "macos") { self.mods.super_key() } else { self.mods.control_key() };
        match key {
            Key::Named(NamedKey::F1) => {
                self.panel = if self.mods.shift_key() { Panel::About } else { Panel::Help };
            }
            Key::Named(NamedKey::Escape) => {
                if self.panel != Panel::None {
                    self.panel = Panel::None;
                } else {
                    self.clear_selection();
                    self.status = None;
                }
            }
            Key::Named(NamedKey::Space) if self.selection.is_some() => {
                if self.player.is_some() {
                    self.player = None;
                } else {
                    self.play();
                }
            }
            Key::Character(s) if command => match s.to_lowercase().as_str() {
                "o" => {
                    let proxy = self.proxy.clone();
                    std::thread::spawn(move || {
                        if let Some(p) = dialogs::open() {
                            let _ = proxy.send_event(UserEvent::Open(p));
                        }
                    });
                }
                "s" if self.path.is_some() => {
                    let stem = self.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned());
                    let name = format!("{}.png", stem.unwrap_or_else(|| "spectrogram".into()));
                    let proxy = self.proxy.clone();
                    std::thread::spawn(move || {
                        if let Some(p) = dialogs::save(&name) {
                            let _ = proxy.send_event(UserEvent::Save(p));
                        }
                    });
                }
                "e" => {
                    self.panel = if self.panel == Panel::Settings { Panel::None } else { Panel::Settings };
                }
                "q" | "w" => event_loop.exit(),
                _ => return,
            },
            Key::Character(s) => {
                let Some(c) = s.chars().next().filter(|_| s.chars().count() == 1) else { return };
                let streams = self.streams.as_ref().map_or(0, |s| s.len());
                let channels = self.current().map_or(0, |s| s.channels as usize);
                let step = |v: usize, n: usize, fwd: bool| if fwd { (v + 1) % n } else { (v + n - 1) % n };
                match c {
                    'c' | 'C' if channels > 0 => {
                        self.channel = step(self.channel, channels, c == 'c');
                        self.restart();
                    }
                    's' | 'S' if streams > 0 => {
                        self.clear_selection();
                        self.stream = step(self.stream, streams, c == 's');
                        let ch = self.current().map_or(1, |s| s.channels.max(1) as usize);
                        self.channel = self.channel.min(ch - 1);
                        self.restart();
                    }
                    _ => {
                        let before = self.settings;
                        if !self.settings.key(c) {
                            return;
                        }
                        self.settings.save();
                        let reanalyse =
                            before.fft_bits != self.settings.fft_bits || before.window != self.settings.window;
                        if reanalyse {
                            self.restart();
                        }
                    }
                }
            }
            _ => return,
        }
        self.redraw_later();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.title())
            .with_inner_size(LogicalSize::new(1040.0, 660.0))
            .with_min_inner_size(LogicalSize::new(480.0, 320.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                eprintln!("spectro: cannot open a window: {e}");
                event_loop.exit();
                return;
            }
        };
        let gfx = softbuffer::Context::new(window.clone()).and_then(|context| {
            let surface = softbuffer::Surface::new(&context, window.clone())?;
            Ok(Gfx { window, surface, _context: context })
        });
        match gfx {
            Ok(g) => self.gfx = Some(g),
            Err(e) => {
                eprintln!("spectro: cannot draw to the window: {e}");
                event_loop.exit();
                return;
            }
        }
        self.restart();
    }

    fn user_event(&mut self, _: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Progress => {}
            UserEvent::Open(p) => self.load(p, 0, 0),
            UserEvent::Save(p) => self.save(&p),
        }
        self.redraw_later();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                let have = self.job.as_ref().map(|j| j.request.columns);
                if self.path.is_some() && have != Some(self.columns()) {
                    self.restart_at = Some(Instant::now() + RESIZE_SETTLE);
                }
                self.redraw_later();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::DroppedFile(p) => {
                self.load(p, 0, 0);
                self.redraw_later();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some((position.x as f32, position.y as f32));
                self.redraw_cursor();
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => self.mouse(state),
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                self.redraw_cursor();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.key(event_loop, event.logical_key);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let mut wake = self.restart_at;
        if self.restart_at.is_some_and(|t| now >= t) {
            self.restart();
            wake = None;
        }
        if let Some(running) = self.player.as_mut().map(Player::running) {
            if running {
                // Animate the playhead.
                self.redraw_cursor();
                let next = now + PLAYHEAD_FRAME;
                wake = Some(wake.map_or(next, |w| w.min(next)));
            } else {
                self.player = None;
                self.status = Some("Playback stopped: ffplay could not play this region".into());
                self.redraw_later();
            }
        }
        event_loop.set_control_flow(wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}
