//! The window: winit for events, softbuffer to show the rendered pixels.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use spectro_core::analysis::Envelope;
use spectro_core::canvas::Canvas;
use spectro_core::render::{Axes, Panel as RenderPanel, Renderer, Section, Selection, plot_rect, strip_rect};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::media::{self, Stream};
use crate::menu::{Action, MenuBar, MenuState};
use crate::pipeline::{Job, Request};
use crate::player::{Player, Region};
use crate::recent;
use crate::settings::Settings;
use crate::{VERSION, dialogs};

/// How long the size must stay still before the analysis is redone.
const RESIZE_SETTLE: Duration = Duration::from_millis(160);

/// The narrowest views allowed, in seconds and hertz.
const MIN_TIME_SPAN: f64 = 0.02;
const MIN_FREQ_SPAN: f64 = 20.0;

/// Frame interval for the playhead while a selection plays.
const PLAYHEAD_FRAME: Duration = Duration::from_millis(33);

pub enum UserEvent {
    Progress,
    Open(PathBuf),
    Save(PathBuf),
    /// A menu item's id (menus are native on macOS only).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Menu(String),
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
    menu: Option<MenuBar>,
    recent: Vec<PathBuf>,
    /// Zoomed time and frequency ranges; `None` shows everything.
    zoom_time: Option<(f64, f64)>,
    zoom_freq: Option<(f64, f64)>,
    /// Loudness of the whole file, kept from the last full analysis.
    overview: Option<Vec<Envelope>>,
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
        menu: None,
        recent: recent::load(),
        zoom_time: None,
        zoom_freq: None,
        overview: None,
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
        if self.streams.is_ok() {
            recent::add(&mut self.recent, &path);
            recent::save(&self.recent);
        }
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
        self.zoom_time = None;
        self.zoom_freq = None;
        self.overview = None;
        self.restart();
        self.sync_menu();
        if let Some(g) = &self.gfx {
            g.window.set_title(&self.title());
        }
    }

    fn axes(&self) -> Option<Axes> {
        let g = self.gfx.as_ref()?;
        self.current().filter(|s| s.problem().is_none())?;
        let size = g.window.inner_size();
        let scale = g.window.scale_factor() as f32;
        Some(Axes::new(
            size.width as usize,
            size.height as usize,
            scale,
            self.time_range(),
            self.freq_range(),
            self.settings.freq_scale,
        ))
    }

    fn duration(&self) -> f64 {
        self.current().map_or(0.0, |s| s.duration)
    }

    fn nyquist(&self) -> f64 {
        self.current().map_or(1.0, |s| s.sample_rate.max(2) as f64 / 2.0)
    }

    fn time_range(&self) -> (f64, f64) {
        self.zoom_time.unwrap_or((0.0, self.duration()))
    }

    fn freq_range(&self) -> (f64, f64) {
        self.zoom_freq.unwrap_or((0.0, self.nyquist()))
    }

    /// Shows `t0..t1` seconds, kept inside the file; re-analysis follows
    /// once the view stops changing.
    fn set_time_range(&mut self, t0: f64, t1: f64) {
        let dur = self.duration();
        if dur <= 0.0 {
            return;
        }
        let span = (t1 - t0).clamp(MIN_TIME_SPAN.min(dur), dur);
        let start = t0.clamp(0.0, dur - span);
        self.zoom_time = if span >= dur - 1e-9 { None } else { Some((start, start + span)) };
        if self.job.as_ref().is_none_or(|j| (j.request.t0, j.request.t1) != self.time_range()) {
            self.restart_at = Some(Instant::now() + RESIZE_SETTLE);
        }
        self.redraw_later();
    }

    fn set_freq_range(&mut self, f0: f64, f1: f64) {
        let ny = self.nyquist();
        let span = (f1 - f0).clamp(MIN_FREQ_SPAN.min(ny), ny);
        let start = f0.clamp(0.0, ny - span);
        self.zoom_freq = if span >= ny - 1e-9 { None } else { Some((start, start + span)) };
        self.redraw_later();
    }

    /// Scales the time span by `factor` (below 1 zooms in) around `anchor` seconds.
    fn zoom_time_by(&mut self, factor: f64, anchor: f64) {
        let (t0, t1) = self.time_range();
        let span = (t1 - t0) * factor;
        let k = if t1 > t0 { (anchor - t0) / (t1 - t0) } else { 0.5 };
        self.set_time_range(anchor - k * span, anchor - k * span + span);
    }

    /// The time under the pointer, or the middle of the view.
    fn anchor_time(&self) -> f64 {
        let (t0, t1) = self.time_range();
        match (self.cursor, self.axes()) {
            (Some(c), Some(a)) if a.contains(c.0, c.1) => a.to_data(c.0, c.1).0,
            _ => 0.5 * (t0 + t1),
        }
    }

    /// The loudness strip's rectangle, in device pixels.
    fn strip(&self) -> Option<spectro_core::render::Plot> {
        let g = self.gfx.as_ref()?;
        let size = g.window.inner_size();
        Some(strip_rect(size.width as usize, size.height as usize, g.window.scale_factor() as f32))
    }

    /// Scroll: pan in time (Alt: in frequency); Cmd or Ctrl: zoom in time.
    fn scroll(&mut self, dx: f64, dy: f64) {
        let Some(axes) = self.axes() else { return };
        let zoom = if cfg!(target_os = "macos") { self.mods.super_key() } else { self.mods.control_key() };
        if zoom {
            self.zoom_time_by((-dy / 300.0).exp(), self.anchor_time());
        } else if self.mods.alt_key() {
            let (f0, f1) = self.freq_range();
            let df = dy * (f1 - f0) / axes.plot.h.max(1) as f64;
            self.set_freq_range(f0 + df, f1 + df);
        } else {
            let (t0, t1) = self.time_range();
            let dt = -(dx + dy) * (t1 - t0) / axes.plot.w.max(1) as f64;
            self.set_time_range(t0 + dt, t1 + dt);
        }
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
                // A click in the loudness strip centres the view there.
                if let (Some(e), true, Some(_)) = (self.strip(), self.zoom_time.is_some(), &self.overview) {
                    let inside = cur.0 >= e.x as f32 && cur.0 < (e.x + e.w) as f32;
                    if inside && cur.1 >= e.y as f32 && cur.1 < (e.y + e.h) as f32 {
                        let t = (cur.0 - e.x as f32) as f64 / e.w as f64 * self.duration();
                        let (t0, t1) = self.time_range();
                        self.set_time_range(t - (t1 - t0) / 2.0, t + (t1 - t0) / 2.0);
                        return;
                    }
                }
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
                self.sync_menu();
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
            sample_rate: s.sample_rate,
            samples: s.samples(),
            t0: self.time_range().0,
            t1: self.time_range().1,
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
            return Some(format!(
                "Drop an audio file here\nor press {}-O to open one\nF1 shows everything spectro can do",
                command_key()
            ));
        }
        if let Some(s) = &self.status {
            return Some(s.clone());
        }
        let job_error = self.job.as_ref().and_then(|j| j.shared.lock().ok().and_then(|s| s.error.clone()));
        job_error.or_else(|| self.streams.as_ref().err().cloned())
    }

    /// The open card's title and sections.
    fn panel_content(&self) -> Option<(&'static str, Vec<Section>)> {
        let cmd = command_key();
        let row = |k: &str, d: &str| (k.to_string(), d.to_string());
        let section = |heading: &str, rows: Vec<(String, String)>| Section { heading: heading.into(), rows };
        let s = &self.settings;
        match self.panel {
            Panel::None => None,
            Panel::Help => {
                let mut files = vec![
                    row(&format!("{cmd}  O"), "Open a file, or drop one on the window"),
                    row(&format!("{cmd}  S"), "Save the picture as a PNG image"),
                ];
                if cfg!(target_os = "macos") {
                    files.push(row(&format!("{cmd}  Shift  R"), "Show the file in Finder"));
                    files.push(row("", "Open Recent is in the File menu"));
                }
                files.push(row(&format!("{cmd}  W"), "Close the window"));
                let settings_key = if cfg!(target_os = "macos") { format!("{cmd}  ,") } else { format!("{cmd}  E") };
                Some((
                    "spectro Help",
                    vec![
                        section("Files", files),
                        section(
                            "Listening",
                            vec![
                                row("Drag", "Select a region: it loops, filtered to its band"),
                                row("Space", "Stop or restart the loop"),
                                row("Click  Esc", "Clear the selection"),
                            ],
                        ),
                        section(
                            "Zoom",
                            vec![
                                row("Pinch", "Zoom in time around the pointer"),
                                row(&format!("{cmd}  Scroll"), "Zoom in time, with a mouse"),
                                row("+  -", "Zoom in or out"),
                                row("Z", "Zoom into the selected box"),
                                row("0", "Show the whole file"),
                                row("Scroll", "Move along in time (also Left and Right)"),
                                row("Alt  Scroll", "Move up or down in frequency"),
                                row("", "Click the loudness strip to jump there"),
                            ],
                        ),
                        section(
                            "Reading the picture",
                            vec![
                                row("", "Time runs left to right, frequency bottom to top"),
                                row("", "Colour is level in dBFS; the bar on the right is the key"),
                                row("", "The strip on top is loudness: peak and RMS"),
                                row("", "Hover to read time, frequency and level"),
                            ],
                        ),
                        section(
                            "Picture",
                            vec![
                                row("p  P", "Next or previous palette"),
                                row("y  Y", "Frequency scale: linear, mel, log"),
                                row("g", "Glow on loud parts"),
                                row("u  U", "Raise or lower the top of the range"),
                                row("l  L", "Raise or lower the bottom of the range"),
                            ],
                        ),
                        section(
                            "Analysis",
                            vec![
                                row("w  W", "Larger or smaller DFT window"),
                                row("f  F", "Next or previous window function"),
                                row("c  C", "Next or previous channel"),
                                row("s  S", "Next or previous stream"),
                            ],
                        ),
                        section(
                            "Help",
                            vec![
                                row("F1", "Show or hide this help"),
                                row("Shift  F1", "About spectro"),
                                row(&settings_key, "Current settings"),
                            ],
                        ),
                    ],
                ))
            }
            Panel::About => Some((
                "About spectro",
                vec![section(
                    "",
                    vec![
                        row("", &format!("Version {VERSION}")),
                        row("", "An acoustic spectrum analyser, after Spek"),
                        row("", "GNU General Public License, version 3 or later"),
                        row("", "no_std Rust engine with no unsafe code"),
                        row("", "Audio decoded and played by FFmpeg"),
                        row("", "Type: Source Sans 3, SIL Open Font License"),
                    ],
                )],
            )),
            Panel::Settings => Some((
                "Settings",
                vec![
                    section(
                        "Picture",
                        vec![
                            row("p  P", &format!("Palette: {}", s.palette.name())),
                            row("y  Y", &format!("Frequency scale: {}", s.freq_scale.name())),
                            row("g", &format!("Glow: {}", if s.glow { "on" } else { "off" })),
                            row("u  U", &format!("Upper limit: {} dBFS", s.upper_db)),
                            row("l  L", &format!("Lower limit: {} dBFS", s.lower_db)),
                        ],
                    ),
                    section(
                        "Analysis",
                        vec![
                            row("f  F", &format!("Window function: {}", s.window.name())),
                            row("w  W", &format!("Window size: {} points", 1u32 << s.fft_bits)),
                        ],
                    ),
                ],
            )),
        }
    }

    fn paint(&mut self, canvas: &mut Canvas, scale: f32, base_only: bool) {
        let title = self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let desc = if self.path.is_some() { self.description() } else { String::new() };
        let message = self.message();
        let panel = self.panel_content();
        let job = self.job.as_ref().map(|j| j.shared.clone());
        let guard = job.as_ref().and_then(|j| j.lock().ok());
        let spec = guard.as_ref().map(|g| &g.spectrogram);
        let mut v = crate::view(&title, &desc, self.current(), spec, &self.settings, scale);
        v.time_range = self.time_range();
        v.freq_range = self.freq_range();
        if let Some(j) = &self.job {
            v.data_range = (j.request.t0, j.request.t1);
        }
        v.overview = self.overview.as_deref();
        v.message = message.as_deref();
        v.panel = panel.as_ref().map(|(title, sections)| RenderPanel { title, sections, footer: "Esc to close" });
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

    /// Maps a key press to an action; menus produce the same actions.
    fn key(&mut self, event_loop: &ActiveEventLoop, key: Key) {
        let command = if cfg!(target_os = "macos") { self.mods.super_key() } else { self.mods.control_key() };
        let action = match key {
            Key::Named(NamedKey::F1) if self.mods.shift_key() => Action::About,
            Key::Named(NamedKey::F1) => Action::Help,
            Key::Named(NamedKey::Escape) => Action::ClearSelection,
            Key::Named(NamedKey::Space) => Action::TogglePlay,
            Key::Named(NamedKey::ArrowLeft) => Action::Pan(-2),
            Key::Named(NamedKey::ArrowRight) => Action::Pan(2),
            Key::Character(s) if command => match s.to_lowercase().as_str() {
                "o" => Action::Open,
                "s" => Action::Save,
                "r" if self.mods.shift_key() => Action::Reveal,
                "e" | "," => Action::Settings,
                "?" | "/" => Action::Help,
                "q" | "w" => Action::Close,
                "=" | "+" => Action::ZoomIn,
                "-" => Action::ZoomOut,
                "0" => Action::ZoomReset,
                _ => return,
            },
            Key::Character(s) => {
                let mut chars = s.chars();
                match (chars.next(), chars.next()) {
                    (Some('=' | '+'), None) => Action::ZoomIn,
                    (Some('-'), None) => Action::ZoomOut,
                    (Some('0'), None) => Action::ZoomReset,
                    (Some('z' | 'Z'), None) => Action::ZoomToSelection,
                    (Some(c), None) => Action::Key(c),
                    _ => return,
                }
            }
            _ => return,
        };
        self.act(event_loop, action);
    }

    fn toggle_panel(&mut self, panel: Panel) {
        self.panel = if self.panel == panel { Panel::None } else { panel };
    }

    /// Applies a settings change and redoes the analysis if it needs to.
    fn change_settings(&mut self, change: impl FnOnce(&mut Settings) -> bool) {
        let before = self.settings;
        if !change(&mut self.settings) || before == self.settings {
            return;
        }
        self.settings.save();
        if before.fft_bits != self.settings.fft_bits || before.window != self.settings.window {
            self.restart();
        }
    }

    fn act(&mut self, event_loop: &ActiveEventLoop, action: Action) {
        match action {
            Action::Open => {
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    if let Some(p) = dialogs::open() {
                        let _ = proxy.send_event(UserEvent::Open(p));
                    }
                });
            }
            Action::OpenRecent(i) => {
                if let Some(p) = self.recent.get(i).cloned() {
                    self.load(p, 0, 0);
                }
            }
            Action::ClearRecent => {
                self.recent.clear();
                recent::save(&self.recent);
            }
            Action::Save => {
                let Some(path) = &self.path else { return };
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                let name = format!("{}.png", stem.unwrap_or_else(|| "spectrogram".into()));
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    if let Some(p) = dialogs::save(&name) {
                        let _ = proxy.send_event(UserEvent::Save(p));
                    }
                });
            }
            Action::Reveal => {
                if let Some(path) = &self.path {
                    dialogs::reveal(path);
                }
            }
            Action::Close => event_loop.exit(),
            Action::About => self.toggle_panel(Panel::About),
            Action::Settings => self.toggle_panel(Panel::Settings),
            Action::Help => self.toggle_panel(Panel::Help),
            Action::TogglePlay => {
                if self.selection.is_none() {
                    return;
                }
                if self.player.is_some() {
                    self.player = None;
                } else {
                    self.play();
                }
            }
            Action::ClearSelection => {
                if self.panel != Panel::None {
                    self.panel = Panel::None;
                } else {
                    self.clear_selection();
                    self.status = None;
                }
            }
            Action::Set(key, value) => self.change_settings(|s| s.set(&key, &value)),
            Action::ZoomIn => self.zoom_time_by(0.5, self.anchor_time()),
            Action::ZoomOut => self.zoom_time_by(2.0, self.anchor_time()),
            Action::ZoomReset => {
                self.zoom_freq = None;
                let dur = self.duration();
                self.set_time_range(0.0, dur);
            }
            Action::ZoomToSelection => {
                let Some(r) = self.selection else { return };
                self.set_time_range(r.t0, r.t1);
                self.set_freq_range(r.f0, r.f1);
            }
            Action::Pan(n) => {
                let (t0, t1) = self.time_range();
                let dt = (t1 - t0) * n as f64 / 10.0;
                self.set_time_range(t0 + dt, t1 + dt);
            }
            Action::Key(c) => {
                let streams = self.streams.as_ref().map_or(0, |s| s.len());
                let channels = self.current().map_or(0, |s| s.channels as usize);
                let step = |v: usize, n: usize, fwd: bool| if fwd { (v + 1) % n } else { (v + n - 1) % n };
                match c {
                    'c' | 'C' if channels > 0 => {
                        self.channel = step(self.channel, channels, c == 'c');
                        self.overview = None;
                        self.restart();
                    }
                    's' | 'S' if streams > 0 => {
                        self.clear_selection();
                        self.zoom_time = None;
                        self.zoom_freq = None;
                        self.overview = None;
                        self.stream = step(self.stream, streams, c == 's');
                        let ch = self.current().map_or(1, |s| s.channels.max(1) as usize);
                        self.channel = self.channel.min(ch - 1);
                        self.restart();
                    }
                    _ => self.change_settings(|s| s.key(c)),
                }
            }
        }
        self.sync_menu();
        self.redraw_later();
    }

    /// Keeps the loudness of the whole file once a full analysis finishes,
    /// so the strip can show it while zoomed.
    fn capture_overview(&mut self) {
        let Some(job) = &self.job else { return };
        let full = job.request.t0 <= 0.0 && job.request.t1 >= self.duration() - 1e-9;
        if !full || self.overview.is_some() {
            return;
        }
        if let Ok(sh) = job.shared.lock()
            && sh.spectrogram.filled() == sh.spectrogram.columns()
        {
            self.overview = Some(sh.spectrogram.envelopes().to_vec());
        }
    }

    fn sync_menu(&mut self) {
        if let Some(m) = &mut self.menu {
            m.sync(&MenuState {
                settings: &self.settings,
                has_file: self.path.is_some(),
                has_selection: self.selection.is_some(),
                recent: &self.recent,
            });
        }
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
        self.menu = Some(MenuBar::install(self.proxy.clone()));
        self.sync_menu();
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

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Progress => self.capture_overview(),
            UserEvent::Menu(id) => {
                if let Some(a) = Action::parse(&id) {
                    self.act(event_loop, a);
                }
            }
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
            WindowEvent::MouseWheel { delta, .. } if self.panel == Panel::None => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64 * 40.0, y as f64 * 40.0),
                    MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
                };
                self.scroll(dx, dy);
            }
            WindowEvent::PinchGesture { delta, .. } if self.panel == Panel::None => {
                self.zoom_time_by((1.0 - delta).clamp(0.2, 5.0), self.anchor_time());
            }
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
