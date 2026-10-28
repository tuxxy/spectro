//! Application actions, shared by keys and menus, and on macOS the native
//! menu bar that triggers them.

use std::path::PathBuf;

#[cfg(not(target_os = "macos"))]
use winit::event_loop::EventLoopProxy;

#[cfg(not(target_os = "macos"))]
use crate::gui::UserEvent;
use crate::settings::Settings;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Open,
    OpenRecent(usize),
    ClearRecent,
    Save,
    Reveal,
    Close,
    About,
    Settings,
    Help,
    /// One of Spek's single-key commands, e.g. `p` for the next palette.
    Key(char),
    /// Sets a preference directly, e.g. `("palette", "glacier")`.
    Set(String, String),
    TogglePlay,
    ClearSelection,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ZoomToSelection,
    /// Moves the view by this many tenths of its width.
    Pan(i32),
}

impl Action {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn id(&self) -> String {
        match self {
            Action::Open => "open".into(),
            Action::OpenRecent(i) => format!("recent:{i}"),
            Action::ClearRecent => "clear-recent".into(),
            Action::Save => "save".into(),
            Action::Reveal => "reveal".into(),
            Action::Close => "close".into(),
            Action::About => "about".into(),
            Action::Settings => "settings".into(),
            Action::Help => "help".into(),
            Action::Key(c) => format!("key:{c}"),
            Action::Set(k, v) => format!("set:{k}:{v}"),
            Action::TogglePlay => "play".into(),
            Action::ClearSelection => "clear-selection".into(),
            Action::ZoomIn => "zoom-in".into(),
            Action::ZoomOut => "zoom-out".into(),
            Action::ZoomReset => "zoom-reset".into(),
            Action::ZoomToSelection => "zoom-selection".into(),
            Action::Pan(n) => format!("pan:{n}"),
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Some(match id {
            "open" => Action::Open,
            "clear-recent" => Action::ClearRecent,
            "save" => Action::Save,
            "reveal" => Action::Reveal,
            "close" => Action::Close,
            "about" => Action::About,
            "settings" => Action::Settings,
            "help" => Action::Help,
            "play" => Action::TogglePlay,
            "clear-selection" => Action::ClearSelection,
            "zoom-in" => Action::ZoomIn,
            "zoom-out" => Action::ZoomOut,
            "zoom-reset" => Action::ZoomReset,
            "zoom-selection" => Action::ZoomToSelection,
            _ => {
                if let Some(n) = id.strip_prefix("pan:") {
                    Action::Pan(n.parse().ok()?)
                } else if let Some(i) = id.strip_prefix("recent:") {
                    Action::OpenRecent(i.parse().ok()?)
                } else if let Some(c) = id.strip_prefix("key:") {
                    let mut chars = c.chars();
                    let c = chars.next()?;
                    if chars.next().is_some() {
                        return None;
                    }
                    Action::Key(c)
                } else {
                    let (k, v) = id.strip_prefix("set:")?.split_once(':')?;
                    Action::Set(k.into(), v.into())
                }
            }
        })
    }
}

/// What the menus show as checked or enabled.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct MenuState<'a> {
    pub settings: &'a Settings,
    pub has_file: bool,
    pub has_selection: bool,
    pub recent: &'a [PathBuf],
}

#[cfg(target_os = "macos")]
pub use mac::MenuBar;

#[cfg(not(target_os = "macos"))]
pub struct MenuBar;

#[cfg(not(target_os = "macos"))]
impl MenuBar {
    pub fn install(_proxy: EventLoopProxy<UserEvent>) -> Self {
        MenuBar
    }

    pub fn sync(&mut self, _state: &MenuState) {}
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use muda::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use spectro_core::analysis::{MAX_FFT_BITS, MIN_FFT_BITS, WindowFunction};
    use spectro_core::palette::Palette;
    use spectro_core::render::FreqScale;
    use winit::event_loop::EventLoopProxy;

    use super::{Action, MenuState};
    use crate::gui::UserEvent;

    pub struct MenuBar {
        _menu: Menu,
        recent: Submenu,
        recent_shown: Option<Vec<PathBuf>>,
        /// Check items that mirror a setting: `(item, key, value)`.
        checks: Vec<(CheckMenuItem, &'static str, String)>,
        glow: CheckMenuItem,
        sonar: CheckMenuItem,
        need_file: Vec<MenuItem>,
        need_selection: Vec<MenuItem>,
    }

    fn item(action: Action, text: &str, accel: Option<&str>) -> MenuItem {
        MenuItem::with_id(action.id(), text, true, accel.and_then(|a| a.parse().ok()))
    }

    fn refs(v: &[CheckMenuItem]) -> Vec<&dyn IsMenuItem> {
        v.iter().map(|c| c as &dyn IsMenuItem).collect()
    }

    fn sep() -> PredefinedMenuItem {
        PredefinedMenuItem::separator()
    }

    fn submenu(text: &str, items: &[&dyn IsMenuItem]) -> Submenu {
        Submenu::with_items(text, true, items).expect("menu items")
    }

    impl MenuBar {
        /// Builds the menu bar and sends each chosen item to the event loop.
        pub fn install(proxy: EventLoopProxy<UserEvent>) -> Self {
            let proxy = Mutex::new(proxy);
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                if let Ok(p) = proxy.lock() {
                    let _ = p.send_event(UserEvent::Menu(e.id.0));
                }
            }));

            let mut checks = Vec::new();
            let mut check = |key: &'static str, value: &str, text: &str| {
                let c = CheckMenuItem::with_id(Action::Set(key.into(), value.into()).id(), text, true, false, None);
                checks.push((c.clone(), key, value.to_string()));
                c
            };
            let palettes: Vec<CheckMenuItem> =
                Palette::ALL.iter().map(|p| check("palette", p.id(), p.name())).collect();
            let scales: Vec<CheckMenuItem> =
                FreqScale::ALL.iter().map(|f| check("freq_scale", f.id(), f.name())).collect();
            let windows: Vec<CheckMenuItem> =
                WindowFunction::ALL.iter().map(|w| check("window_function", w.id(), w.name())).collect();
            let sizes: Vec<CheckMenuItem> = (MIN_FFT_BITS..=MAX_FFT_BITS)
                .map(|b| {
                    let n = (1u32 << b).to_string();
                    check("window_size", &n, &format!("{n} points"))
                })
                .collect();
            let glow = CheckMenuItem::with_id(Action::Key('g').id(), "Glow", true, true, "G".parse().ok());
            let sonar = CheckMenuItem::with_id(Action::Key('m').id(), "Submarine Mode", true, false, "M".parse().ok());

            let save = item(Action::Save, "Save Image\u{2026}", Some("CmdOrCtrl+S"));
            let reveal = item(Action::Reveal, "Show in Finder", Some("CmdOrCtrl+Shift+R"));
            let play = item(Action::TogglePlay, "Play / Stop Loop", Some("Space"));
            let clear = item(Action::ClearSelection, "Clear Selection", None);

            let app = submenu(
                "spectro",
                &[
                    &item(Action::About, "About spectro", None),
                    &sep(),
                    &item(Action::Settings, "Settings\u{2026}", Some("CmdOrCtrl+Comma")),
                    &sep(),
                    &PredefinedMenuItem::services(None),
                    &sep(),
                    &PredefinedMenuItem::hide(None),
                    &PredefinedMenuItem::hide_others(None),
                    &PredefinedMenuItem::show_all(None),
                    &sep(),
                    &item(Action::Close, "Quit spectro", Some("CmdOrCtrl+Q")),
                ],
            );
            let recent = Submenu::new("Open Recent", true);
            let file = submenu(
                "File",
                &[
                    &item(Action::Open, "Open\u{2026}", Some("CmdOrCtrl+O")),
                    &recent,
                    &sep(),
                    &save,
                    &reveal,
                    &sep(),
                    &item(Action::Close, "Close Window", Some("CmdOrCtrl+W")),
                ],
            );

            let steps = |next: char, prev: char, what: &str| {
                (
                    item(Action::Key(next), &format!("Next {what}"), Some(&next.to_ascii_uppercase().to_string())),
                    item(
                        Action::Key(prev),
                        &format!("Previous {what}"),
                        Some(&format!("Shift+{}", prev.to_ascii_uppercase())),
                    ),
                )
            };
            let group = |text: &str, checks: &[CheckMenuItem], (n, p): (MenuItem, MenuItem)| {
                let s = sep();
                let mut items = refs(checks);
                items.push(&s);
                items.push(&n);
                items.push(&p);
                submenu(text, &items)
            };
            let palette_menu = group("Palette", &palettes, steps('p', 'P', "Palette"));
            let scale_menu = group("Frequency Scale", &scales, steps('y', 'Y', "Scale"));
            let window_menu = group("Window Function", &windows, steps('f', 'F', "Function"));
            let size_menu = {
                let (s, larger, smaller) = (
                    sep(),
                    item(Action::Key('w'), "Larger", Some("W")),
                    item(Action::Key('W'), "Smaller", Some("Shift+W")),
                );
                let mut items = refs(&sizes);
                items.push(&s);
                items.push(&larger);
                items.push(&smaller);
                submenu("Window Size", &items)
            };
            let range_menu = submenu(
                "Dynamic Range",
                &[
                    &item(Action::Key('u'), "Raise Upper Limit", Some("U")),
                    &item(Action::Key('U'), "Lower Upper Limit", Some("Shift+U")),
                    &sep(),
                    &item(Action::Key('l'), "Raise Lower Limit", Some("L")),
                    &item(Action::Key('L'), "Lower Lower Limit", Some("Shift+L")),
                ],
            );
            let (next_channel, prev_channel) = steps('c', 'C', "Channel");
            let (next_stream, prev_stream) = steps('s', 'S', "Stream");
            let zoom_selection = item(Action::ZoomToSelection, "Zoom to Selection", Some("Z"));
            let view = submenu(
                "View",
                &[
                    &item(Action::ZoomIn, "Zoom In", Some("CmdOrCtrl+Equal")),
                    &item(Action::ZoomOut, "Zoom Out", Some("CmdOrCtrl+Minus")),
                    &zoom_selection,
                    &item(Action::ZoomReset, "Show Whole File", Some("CmdOrCtrl+Digit0")),
                    &sep(),
                    &palette_menu,
                    &scale_menu,
                    &window_menu,
                    &size_menu,
                    &range_menu,
                    &glow,
                    &sonar,
                    &sep(),
                    &next_channel,
                    &prev_channel,
                    &next_stream,
                    &prev_stream,
                ],
            );
            let playback = submenu("Playback", &[&play, &clear]);
            let window = submenu(
                "Window",
                &[
                    &PredefinedMenuItem::minimize(None),
                    &PredefinedMenuItem::maximize(Some("Zoom")),
                    &PredefinedMenuItem::fullscreen(None),
                    &sep(),
                    &PredefinedMenuItem::bring_all_to_front(None),
                ],
            );
            let help = submenu("Help", &[&item(Action::Help, "spectro Help", Some("CmdOrCtrl+Shift+Slash"))]);

            let menu = Menu::new();
            for m in [&app, &file, &view, &playback, &window, &help] {
                let _ = menu.append(m);
            }
            menu.init_for_nsapp();
            window.set_as_windows_menu_for_nsapp();
            help.set_as_help_menu_for_nsapp();

            MenuBar {
                _menu: menu,
                recent,
                recent_shown: None,
                checks,
                glow,
                sonar,
                need_file: vec![save, reveal],
                need_selection: vec![play, clear, zoom_selection],
            }
        }

        /// Updates check marks, enabled items and the recent files.
        pub fn sync(&mut self, st: &MenuState) {
            let s = st.settings;
            for (c, key, value) in &self.checks {
                let on = match *key {
                    "palette" => s.palette.id() == value,
                    "freq_scale" => s.freq_scale.id() == value,
                    "window_function" => s.window.id() == value,
                    "window_size" => (1u32 << s.fft_bits).to_string() == *value,
                    _ => false,
                };
                c.set_checked(on);
            }
            self.glow.set_checked(s.glow);
            self.sonar.set_checked(s.sonar);
            for i in &self.need_file {
                i.set_enabled(st.has_file);
            }
            for i in &self.need_selection {
                i.set_enabled(st.has_selection);
            }
            if self.recent_shown.as_deref() != Some(st.recent) {
                while self.recent.remove_at(0).is_some() {}
                for (i, p) in st.recent.iter().enumerate() {
                    let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into());
                    let _ = self.recent.append(&item(Action::OpenRecent(i), &name, None));
                }
                if !st.recent.is_empty() {
                    let _ = self.recent.append(&sep());
                }
                let clear = item(Action::ClearRecent, "Clear Menu", None);
                clear.set_enabled(!st.recent.is_empty());
                let _ = self.recent.append(&clear);
                self.recent_shown = Some(st.recent.to_vec());
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn accelerators_parse() {
            for a in [
                "CmdOrCtrl+Equal",
                "CmdOrCtrl+Minus",
                "CmdOrCtrl+Digit0",
                "Z",
                "CmdOrCtrl+O",
                "CmdOrCtrl+Comma",
                "CmdOrCtrl+Shift+Slash",
                "CmdOrCtrl+Shift+R",
                "Space",
                "P",
                "Shift+P",
            ] {
                assert!(a.parse::<muda::accelerator::Accelerator>().is_ok(), "{a}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        let all = [
            Action::Open,
            Action::OpenRecent(3),
            Action::ClearRecent,
            Action::Save,
            Action::Reveal,
            Action::Close,
            Action::About,
            Action::Settings,
            Action::Help,
            Action::Key('P'),
            Action::Set("palette".into(), "glacier".into()),
            Action::TogglePlay,
            Action::ClearSelection,
            Action::ZoomIn,
            Action::ZoomOut,
            Action::ZoomReset,
            Action::ZoomToSelection,
            Action::Pan(-2),
        ];
        for a in all {
            assert_eq!(Action::parse(&a.id()), Some(a.clone()), "{}", a.id());
        }
        assert_eq!(Action::parse("key:pp"), None);
        assert_eq!(Action::parse("nope"), None);
    }
}
