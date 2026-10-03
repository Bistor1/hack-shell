mod clipboard;
mod config;
mod gpu;
mod keys;
mod session;

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, Notify};
use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{ClipboardType, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use egui_winit::State as EguiState;
use session::{notify, Sessions, UserEvent};
use vte::ansi::Rgb;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::config::{Bookmark, Config, Profile, Scheme};
use crate::gpu::{apply_image_delta, paint_cells, paint_egui, CpuImage, DrawList, Fonts};
use crate::session::Pane;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    alacritty_terminal::tty::setup_env();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("event loop");
    let mut app = App::new(event_loop.create_proxy());
    if let Err(err) = event_loop.run_app(&mut app) {
        eprintln!("hack-shell: {err}");
        std::process::exit(1);
    }
}

struct Soft {
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
}

struct App {
    wake: winit::event_loop::EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    soft: Option<Soft>,
    egui_textures: std::collections::HashMap<egui::TextureId, CpuImage>,
    egui: Option<EguiState>,
    sessions: Sessions,
    config: Config,
    fonts: Option<Fonts>,
    tabs: Vec<Tab>,
    active: usize,
    modifiers: ModifiersState,
    cursor: PhysicalPosition<f64>,
    term_rect: [f32; 4],
    find_open: bool,
    find_query: String,
    profile_open: bool,
    bookmarks_open: bool,
    about_open: bool,
    context: Option<egui::Pos2>,
    fullscreen: bool,
    window_focused: bool,
    blink_on: bool,
    last_blink: Instant,
    bell_until: Option<Instant>,
    dragging_split: Option<Vec<usize>>,
    scale: f32,
    profile_index: usize,
    should_quit: bool,
}

struct Tab {
    root: Node,
    focused: u64,
    activity: bool,
    bell: bool,
}

enum Node {
    Pane(u64),
    Split { vertical: bool, ratio: f32, a: Box<Node>, b: Box<Node> },
}

struct Splitter {
    path: Vec<usize>,
    vertical: bool,
    origin: f32,
    span: f32,
    hit: [f32; 4],
}

impl App {
    fn new(wake: winit::event_loop::EventLoopProxy<UserEvent>) -> Self {
        Self {
            wake: wake.clone(),
            window: None,
            soft: None,
            egui_textures: std::collections::HashMap::new(),
            egui: None,
            sessions: Sessions::new(wake),
            config: Config::load(),
            fonts: None,
            tabs: Vec::new(),
            active: 0,
            modifiers: ModifiersState::empty(),
            cursor: PhysicalPosition::new(0.0, 0.0),
            term_rect: [0.0, 0.0, 800.0, 600.0],
            find_open: false,
            find_query: String::new(),
            profile_open: false,
            bookmarks_open: false,
            about_open: false,
            context: None,
            fullscreen: false,
            window_focused: true,
            blink_on: true,
            last_blink: Instant::now(),
            bell_until: None,
            dragging_split: None,
            scale: 1.0,
            profile_index: 0,
            should_quit: false,
        }
    }

    fn profile(&self) -> Profile {
        self.config.profile(&self.config.default_profile)
    }

    fn scheme_for(&self, name: &str) -> Scheme {
        self.config.scheme(name)
    }

    fn ensure_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("hack-shell")
            .with_inner_size(LogicalSize::new(1080.0, 680.0))
            .with_min_inner_size(LogicalSize::new(320.0, 180.0))
            .with_decorations(true);
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        self.scale = window.scale_factor() as f32;
        let context = softbuffer::Context::new(window.clone()).expect("software context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("software surface");
        let egui_ctx = egui::Context::default();
        egui_ctx.set_visuals(egui::Visuals::dark());
        let wake = self.wake.clone();
        egui_ctx.set_request_repaint_callback(move |_| {
            let _ = wake.send_event(UserEvent::Wake);
        });
        let egui = EguiState::new(
            egui_ctx,
            egui::ViewportId::ROOT,
            &window,
            Some(self.scale),
            None,
            None,
        );
        let profile = self.profile();
        self.fonts = Some(Fonts::load(&profile.font_family, profile.font_size));
        self.window = Some(window);
        self.soft = Some(Soft { surface });
        self.egui = Some(egui);
        self.new_tab(None, None);
    }

    fn new_tab(&mut self, profile: Option<Profile>, directory: Option<String>) {
        let mut profile = profile.unwrap_or_else(|| self.profile());
        if let Some(dir) = directory {
            profile.working_directory = dir;
        }
        let fonts = self.fonts.as_ref().unwrap();
        let id = self.sessions.spawn(&profile, 80, 24, (fonts.cell_w, fonts.cell_h));
        self.tabs.push(Tab { root: Node::Pane(id), focused: id, activity: false, bell: false });
        self.active = self.tabs.len() - 1;
        self.request_redraw();
    }

    fn new_window(&self) {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
    }

    fn focused_id(&self) -> Option<u64> {
        self.tabs.get(self.active).map(|t| t.focused)
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn close_focused_pane(&mut self) {
        let Some(id) = self.focused_id() else { return };
        self.close_pane(id);
    }

    fn close_pane(&mut self, id: u64) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if let Some(new_root) = remove_pane(&mut tab.root, id) {
            tab.root = new_root;
            if tab.focused == id {
                tab.focused = first_pane(&tab.root).unwrap_or(id);
            }
            self.sessions.close(id);
        } else {
            self.sessions.close(id);
            self.tabs.remove(self.active);
            if self.tabs.is_empty() {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                self.new_tab(None, None);
            } else if self.active >= self.tabs.len() {
                self.active = self.tabs.len() - 1;
            }
        }
        self.request_redraw();
    }

    fn split_focused(&mut self, vertical: bool) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let id = tab.focused;
        let profile = self.sessions.get(id).map(|p| {
            self.config.profile(&p.profile_name)
        });
        let Some(profile) = profile else { return };
        let fonts = self.fonts.as_ref().unwrap();
        let new_id = self.sessions.spawn(&profile, 80, 24, (fonts.cell_w, fonts.cell_h));
        if !split_at(&mut tab.root, id, new_id, vertical) {
            self.sessions.close(new_id);
            return;
        }
        tab.focused = new_id;
        self.request_redraw();
    }

    fn focus_neighbor(&mut self, dx: i32, dy: i32) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let mut panes = Vec::new();
        let mut splits = Vec::new();
        layout_node(&tab.root, self.term_rect, &mut panes, &mut splits);
        let Some(cur) = panes.iter().find(|p| p.id == tab.focused) else { return };
        let cx = cur.x + cur.w * 0.5;
        let cy = cur.y + cur.h * 0.5;
        let mut best: Option<(u64, f32)> = None;
        for p in &panes {
            if p.id == cur.id {
                continue;
            }
            let px = p.x + p.w * 0.5;
            let py = p.y + p.h * 0.5;
            let vx = px - cx;
            let vy = py - cy;
            let ok = if dx != 0 { vx.signum() as i32 == dx && vx.abs() > vy.abs() * 0.4 } else { vy.signum() as i32 == dy && vy.abs() > vx.abs() * 0.4 };
            if !ok {
                continue;
            }
            let dist = vx * vx + vy * vy;
            if best.map(|(_, d)| dist < d).unwrap_or(true) {
                best = Some((p.id, dist));
            }
        }
        if let Some((id, _)) = best {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.focused = id;
            }
            self.request_redraw();
        }
    }

    fn apply_font_zoom(&mut self, delta: f32) {
        let fonts = self.fonts.as_mut().unwrap();
        let next = (fonts.size() + delta).clamp(6.0, 48.0);
        fonts.set_size(next);
        // Force every pane to pick up the new cell size.
        for pane in &mut self.sessions.panes {
            pane.cols = 0;
        }
        self.request_redraw();
    }

    fn publish_selection(&self, id: u64) {
        let Some(pane) = self.sessions.get(id) else { return };
        if let Some(text) = session::selection_text(pane) {
            clipboard::publish(&text, pane.copy_on_select);
        }
    }

    fn paste_into(&self, id: u64, primary: bool) {
        let Some(pane) = self.sessions.get(id) else { return };
        if pane.read_only || pane.exited {
            return;
        }
        let text = if primary { clipboard::paste_primary() } else { clipboard::paste_clipboard() };
        let Some(text) = text else { return };
        let bracketed = session::mode_of(pane).contains(TermMode::BRACKETED_PASTE);
        self.sessions.write(id, keys::bracket_paste(&text, bracketed));
    }

    fn copy_focused(&self) {
        if let Some(id) = self.focused_id() {
            let Some(pane) = self.sessions.get(id) else { return };
            if let Some(text) = session::selection_text(pane) {
                clipboard::copy_clipboard(&text);
                clipboard::publish(&text, false);
            }
        }
    }

    fn handle_key(&mut self, event: &winit::event::KeyEvent) {
        if event.state != ElementState::Pressed || event.repeat && self.egui_wants_keyboard() {
            if event.state != ElementState::Pressed {
                return;
            }
        }
        if event.state != ElementState::Pressed {
            return;
        }
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();

        if event.logical_key == Key::Named(NamedKey::F11) {
            self.toggle_fullscreen();
            return;
        }
        if ctrl && shift && !alt {
            match event.logical_key.as_ref() {
                Key::Character(s) if s == "t" || s == "T" => {
                    self.new_tab(None, None);
                    return;
                },
                Key::Character(s) if s == "w" || s == "W" => {
                    self.close_focused_pane();
                    return;
                },
                Key::Character(s) if s == "n" || s == "N" => {
                    self.new_window();
                    return;
                },
                Key::Character(s) if s == "c" || s == "C" => {
                    self.copy_focused();
                    return;
                },
                Key::Character(s) if s == "v" || s == "V" => {
                    if let Some(id) = self.focused_id() {
                        self.paste_into(id, false);
                    }
                    return;
                },
                Key::Character(s) if s == "f" || s == "F" => {
                    self.find_open = !self.find_open;
                    self.request_redraw();
                    return;
                },
                Key::Character(s) if s == "d" || s == "D" => {
                    self.split_focused(true);
                    return;
                },
                Key::Character(s) if s == "e" || s == "E" => {
                    self.split_focused(false);
                    return;
                },
                Key::Character(s) if s == "k" || s == "K" => {
                    if let Some(id) = self.focused_id() {
                        if let Some(pane) = self.sessions.get_mut(id) {
                            session::clear_scrollback(pane);
                        }
                    }
                    self.request_redraw();
                    return;
                },
                Key::Character(s) if s == "=" || s == "+" => {
                    self.apply_font_zoom(1.0);
                    return;
                },
                Key::Character(s) if s == "-" || s == "_" => {
                    self.apply_font_zoom(-1.0);
                    return;
                },
                Key::Character(s) if s == "0" => {
                    let size = self.profile().font_size;
                    if let Some(fonts) = self.fonts.as_mut() {
                        fonts.set_size(size);
                    }
                    for pane in &mut self.sessions.panes {
                        pane.cols = 0;
                    }
                    self.request_redraw();
                    return;
                },
                Key::Named(NamedKey::ArrowLeft) => {
                    self.focus_neighbor(-1, 0);
                    return;
                },
                Key::Named(NamedKey::ArrowRight) => {
                    self.focus_neighbor(1, 0);
                    return;
                },
                Key::Named(NamedKey::ArrowUp) => {
                    self.focus_neighbor(0, -1);
                    return;
                },
                Key::Named(NamedKey::ArrowDown) => {
                    self.focus_neighbor(0, 1);
                    return;
                },
                _ => {},
            }
        }
        if ctrl && !shift && !alt {
            match &event.logical_key {
                Key::Named(NamedKey::PageUp) => {
                    self.cycle_tab(-1);
                    return;
                },
                Key::Named(NamedKey::PageDown) => {
                    self.cycle_tab(1);
                    return;
                },
                Key::Character(s) if s == "=" || s == "+" => {
                    self.apply_font_zoom(1.0);
                    return;
                },
                Key::Character(s) if s == "-" => {
                    self.apply_font_zoom(-1.0);
                    return;
                },
                Key::Character(s) if s == "0" => {
                    let size = self.profile().font_size;
                    if let Some(fonts) = self.fonts.as_mut() {
                        fonts.set_size(size);
                    }
                    for pane in &mut self.sessions.panes {
                        pane.cols = 0;
                    }
                    self.request_redraw();
                    return;
                },
                _ => {},
            }
        }
        if self.egui_wants_keyboard() {
            return;
        }
        let Some(id) = self.focused_id() else { return };
        let Some(pane) = self.sessions.get(id) else { return };
        if pane.exited {
            if matches!(event.logical_key, Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Escape)) {
                self.close_pane(id);
            }
            return;
        }
        let mode = session::mode_of(pane);
        if !shift && matches!(event.logical_key, Key::Named(NamedKey::PageUp)) && !mode.contains(TermMode::ALT_SCREEN) {
            if let Some(pane) = self.sessions.get_mut(id) {
                session::scroll(pane, pane.rows as i32);
            }
            self.request_redraw();
            return;
        }
        if !shift && matches!(event.logical_key, Key::Named(NamedKey::PageDown)) && !mode.contains(TermMode::ALT_SCREEN) {
            if let Some(pane) = self.sessions.get_mut(id) {
                session::scroll(pane, -(pane.rows as i32));
            }
            self.request_redraw();
            return;
        }
        if let Some(bytes) = keys::encode(event, self.modifiers, mode) {
            self.sessions.write(id, bytes);
        }
    }

    fn egui_wants_keyboard(&self) -> bool {
        self.egui.as_ref().map(|e| e.egui_ctx().egui_wants_keyboard_input()).unwrap_or(false)
    }

    fn cycle_tab(&mut self, dir: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let n = self.tabs.len() as isize;
        self.active = ((self.active as isize + dir).rem_euclid(n)) as usize;
        self.request_redraw();
    }

    fn toggle_fullscreen(&mut self) {
        let Some(window) = &self.window else { return };
        self.fullscreen = !self.fullscreen;
        window.set_fullscreen(self.fullscreen.then_some(Fullscreen::Borderless(None)));
    }

    fn on_mouse_button(&mut self, button: MouseButton, state: ElementState) {
        if self.egui.as_ref().map(|e| e.egui_ctx().egui_wants_pointer_input()).unwrap_or(false) && self.dragging_split.is_none()
        {
            return;
        }
        let (px, py) = (self.cursor.x as f32, self.cursor.y as f32);
        if state == ElementState::Released && button == MouseButton::Left {
            self.dragging_split = None;
            if let Some(id) = self.focused_id() {
                if let Some(pane) = self.sessions.get_mut(id) {
                    pane.selecting = false;
                }
                self.publish_selection(id);
            }
            return;
        }
        if state != ElementState::Pressed {
            return;
        }
        if button == MouseButton::Left {
            if let Some(path) = self.splitter_at(px, py) {
                self.dragging_split = Some(path);
                return;
            }
        }
        let Some(hit) = self.pane_at(px, py) else { return };
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focused = hit.id;
        }
        let id = hit.id;
        match button {
            MouseButton::Middle => {
                self.paste_into(id, true);
            },
            MouseButton::Right => {
                self.context = Some(egui::pos2(px / self.scale, py / self.scale));
                self.request_redraw();
            },
            MouseButton::Left => {
                let fonts = self.fonts.as_ref().unwrap();
                let pane = self.sessions.get(id).unwrap();
                let local_x = px - hit.x - pane.padding * self.scale;
                let local_y = py - hit.y - pane.padding * self.scale;
                let col_f = local_x / fonts.cell_w;
                let row_f = local_y / fonts.cell_h;
                let point = session::mouse_point(col_f, row_f, pane.cols.max(1), pane.rows.max(1), session::display_offset(pane));
                let mode = session::mode_of(pane);
                let mouse_mode = mode.intersects(TermMode::MOUSE_MODE);
                if mouse_mode && !self.modifiers.shift_key() {
                    let btn = 0u8;
                    self.sessions.write(id, keys::mouse_report(btn, point.column.0 + 1, row_from_point(point, pane.rows, session::display_offset(pane)), false, self.modifiers));
                    return;
                }
                if self.modifiers.control_key() {
                    if let Some(url) = session::url_at(pane, point) {
                        session::open_url(&url);
                        return;
                    }
                }
                let now = Instant::now();
                let pane = self.sessions.get_mut(id).unwrap();
                let same = pane.last_click_point == Some(point) && now.duration_since(pane.last_click) < Duration::from_millis(400);
                pane.click_count = if same { (pane.click_count % 3) + 1 } else { 1 };
                pane.last_click = now;
                pane.last_click_point = Some(point);
                let ty = match pane.click_count {
                    2 => SelectionType::Semantic,
                    3 => SelectionType::Lines,
                    _ => {
                        if self.modifiers.alt_key() {
                            SelectionType::Block
                        } else {
                            SelectionType::Simple
                        }
                    },
                };
                if pane.click_count == 1 && !self.modifiers.alt_key() && !self.modifiers.shift_key() {
                    session::clear_selection(pane);
                }
                session::start_selection(pane, point, ty);
            },
            _ => {},
        }
        self.request_redraw();
    }

    fn on_cursor_moved(&mut self, pos: PhysicalPosition<f64>) {
        self.cursor = pos;
        if let Some(path) = &self.dragging_split {
            let path = path.clone();
            self.drag_split(&path);
            self.request_redraw();
            return;
        }
        let pressed = self.sessions.get(self.focused_id().unwrap_or(0)).map(|p| p.selecting).unwrap_or(false);
        if !pressed {
            return;
        }
        let (px, py) = (pos.x as f32, pos.y as f32);
        let Some(hit) = self.pane_at(px, py) else { return };
        let id = self.focused_id();
        if id != Some(hit.id) {
            return;
        }
        let fonts = self.fonts.as_ref().unwrap();
        let pane = self.sessions.get(hit.id).unwrap();
        let local_x = px - hit.x - pane.padding * self.scale;
        let local_y = py - hit.y - pane.padding * self.scale;
        let point = session::mouse_point(
            local_x / fonts.cell_w,
            local_y / fonts.cell_h,
            pane.cols.max(1),
            pane.rows.max(1),
            session::display_offset(pane),
        );
        if let Some(pane) = self.sessions.get_mut(hit.id) {
            session::update_selection(pane, point);
        }
        self.request_redraw();
    }

    fn on_scroll(&mut self, delta: MouseScrollDelta) {
        if self.egui.as_ref().map(|e| e.egui_ctx().egui_wants_pointer_input()).unwrap_or(false) {
            return;
        }
        let (px, py) = (self.cursor.x as f32, self.cursor.y as f32);
        let Some(hit) = self.pane_at(px, py) else { return };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32 / 40.0),
        };
        if lines.abs() < 0.01 {
            return;
        }
        let pane = self.sessions.get(hit.id).unwrap();
        let mode = session::mode_of(pane);
        if mode.intersects(TermMode::MOUSE_MODE) && !self.modifiers.shift_key() {
            let btn = if lines > 0.0 { 64 } else { 65 };
            let fonts = self.fonts.as_ref().unwrap();
            let local_x = px - hit.x - pane.padding * self.scale;
            let local_y = py - hit.y - pane.padding * self.scale;
            let point = session::mouse_point(local_x / fonts.cell_w, local_y / fonts.cell_h, pane.cols.max(1), pane.rows.max(1), session::display_offset(pane));
            self.sessions.write(hit.id, keys::mouse_report(btn, point.column.0 + 1, row_from_point(point, pane.rows, session::display_offset(pane)), false, self.modifiers));
            return;
        }
        if let Some(pane) = self.sessions.get_mut(hit.id) {
            session::scroll(pane, lines.round() as i32);
        }
        self.request_redraw();
    }

    fn pane_at(&self, x: f32, y: f32) -> Option<PaneBox> {
        let tab = self.tabs.get(self.active)?;
        let mut panes = Vec::new();
        let mut splits = Vec::new();
        layout_node(&tab.root, self.term_rect, &mut panes, &mut splits);
        panes.into_iter().find(|p| x >= p.x && y >= p.y && x < p.x + p.w && y < p.y + p.h)
    }

    fn splitter_at(&self, x: f32, y: f32) -> Option<Vec<usize>> {
        let tab = self.tabs.get(self.active)?;
        let mut panes = Vec::new();
        let mut splits = Vec::new();
        layout_node(&tab.root, self.term_rect, &mut panes, &mut splits);
        splits.into_iter().find(|s| x >= s.hit[0] && y >= s.hit[1] && x < s.hit[0] + s.hit[2] && y < s.hit[1] + s.hit[3]).map(|s| s.path)
    }

    fn drag_split(&mut self, path: &[usize]) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let mut panes = Vec::new();
        let mut splits = Vec::new();
        layout_node(&tab.root, self.term_rect, &mut panes, &mut splits);
        let Some(split) = splits.into_iter().find(|s| s.path == path) else { return };
        let pos = if split.vertical { self.cursor.x as f32 } else { self.cursor.y as f32 };
        let ratio = ((pos - split.origin) / split.span).clamp(0.12, 0.88);
        if let Some(node) = node_mut(&mut tab.root, path) {
            if let Node::Split { ratio: r, .. } = node {
                *r = ratio;
            }
        }
    }

    fn handle_term_event(&mut self, id: u64, event: Event) {
        match event {
            Event::Wakeup => {
                if self.tabs.get(self.active).map(|t| !pane_in(&t.root, id) || t.focused != id).unwrap_or(true) {
                    for tab in &mut self.tabs {
                        if pane_in(&tab.root, id) && self.sessions.get(id).map(|p| p.notify_activity).unwrap_or(false) {
                            tab.activity = true;
                        }
                    }
                }
                self.request_redraw();
            },
            Event::Title(title) => {
                if let Some(pane) = self.sessions.get_mut(id) {
                    pane.title = title;
                }
                self.request_redraw();
            },
            Event::ResetTitle => {
                if let Some(pane) = self.sessions.get_mut(id) {
                    pane.title = pane.profile_name.clone();
                }
            },
            Event::Bell => {
                self.bell_until = Some(Instant::now() + Duration::from_millis(120));
                for tab in &mut self.tabs {
                    if pane_in(&tab.root, id) {
                        tab.bell = true;
                    }
                }
                if self.sessions.get(id).map(|p| p.notify_bell).unwrap_or(false) {
                    let title = self.sessions.get(id).map(|p| p.title.clone()).unwrap_or_default();
                    notify("Bell", &title);
                }
                self.request_redraw();
            },
            Event::ChildExit(_) | Event::Exit => {
                if let Some(pane) = self.sessions.get_mut(id) {
                    pane.exited = true;
                    if pane.notify_exit {
                        notify("Process exited", &pane.title);
                    }
                }
                self.request_redraw();
            },
            Event::ClipboardStore(kind, text) => {
                match kind {
                    ClipboardType::Clipboard => clipboard::copy_clipboard(&text),
                    ClipboardType::Selection => clipboard::publish(&text, false),
                }
            },
            Event::ClipboardLoad(kind, formatter) => {
                let text = match kind {
                    ClipboardType::Clipboard => clipboard::paste_clipboard().unwrap_or_default(),
                    ClipboardType::Selection => clipboard::paste_primary().unwrap_or_default(),
                };
                let formatted = formatter(&text);
                self.sessions.write(id, formatted.into_bytes());
            },
            Event::PtyWrite(text) => self.sessions.write(id, text.into_bytes()),
            Event::ColorRequest(index, formatter) => {
                let scheme = self.sessions.get(id).map(|p| self.scheme_for(&p.scheme)).unwrap_or_else(Scheme::breeze_dark);
                let rgb = palette_rgb(&scheme, index);
                self.sessions.write(id, formatter(rgb).into_bytes());
            },
            Event::TextAreaSizeRequest(formatter) => {
                let fonts = self.fonts.as_ref().unwrap();
                let pane = self.sessions.get(id);
                let cols = pane.map(|p| p.cols).unwrap_or(80) as u16;
                let rows = pane.map(|p| p.rows).unwrap_or(24) as u16;
                let size = alacritty_terminal::event::WindowSize {
                    num_cols: cols,
                    num_lines: rows,
                    cell_width: fonts.cell_w as u16,
                    cell_height: fonts.cell_h as u16,
                };
                self.sessions.write(id, formatter(size).into_bytes());
            },
            Event::MouseCursorDirty | Event::CursorBlinkingChange => self.request_redraw(),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("chrome").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New tab    Ctrl+Shift+T").clicked() {
                        self.new_tab(None, None);
                        ui.close();
                    }
                    if ui.button("New window    Ctrl+Shift+N").clicked() {
                        self.new_window();
                        ui.close();
                    }
                    if ui.button("Close tab    Ctrl+Shift+W").clicked() {
                        self.close_focused_pane();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        self.should_quit = true;
                        ui.close();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Copy    Ctrl+Shift+C").clicked() {
                        self.copy_focused();
                        ui.close();
                    }
                    if ui.button("Paste    Ctrl+Shift+V").clicked() {
                        if let Some(id) = self.focused_id() {
                            self.paste_into(id, false);
                        }
                        ui.close();
                    }
                    if ui.button("Paste selection    middle click").clicked() {
                        if let Some(id) = self.focused_id() {
                            self.paste_into(id, true);
                        }
                        ui.close();
                    }
                    if ui.button("Find    Ctrl+Shift+F").clicked() {
                        self.find_open = true;
                        ui.close();
                    }
                    if ui.button("Clear scrollback    Ctrl+Shift+K").clicked() {
                        if let Some(id) = self.focused_id() {
                            if let Some(pane) = self.sessions.get_mut(id) {
                                session::clear_scrollback(pane);
                            }
                        }
                        ui.close();
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.button("Split right    Ctrl+Shift+D").clicked() {
                        self.split_focused(true);
                        ui.close();
                    }
                    if ui.button("Split down    Ctrl+Shift+E").clicked() {
                        self.split_focused(false);
                        ui.close();
                    }
                    if ui.button("Fullscreen    F11").clicked() {
                        self.toggle_fullscreen();
                        ui.close();
                    }
                    if ui.button("Read-only").clicked() {
                        if let Some(id) = self.focused_id() {
                            if let Some(pane) = self.sessions.get_mut(id) {
                                pane.read_only = !pane.read_only;
                            }
                        }
                        ui.close();
                    }
                });
                ui.menu_button("Bookmarks", |ui| {
                    let bookmarks = self.config.bookmarks.clone();
                    for mark in &bookmarks {
                        if ui.button(&mark.name).clicked() {
                            let mut profile = self.profile();
                            profile.working_directory = mark.directory.clone();
                            if !mark.command.is_empty() {
                                profile.command = mark.command.clone();
                            }
                            self.new_tab(Some(profile), None);
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("Add current directory").clicked() {
                        let dir = self.focused_id().and_then(|id| self.sessions.get(id)).and_then(session::cwd_of).map(|p| p.display().to_string()).unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".into()));
                        self.config.bookmarks.push(Bookmark { name: dir.clone(), directory: dir, command: String::new() });
                        let _ = self.config.save();
                        ui.close();
                    }
                    if ui.button("Manage bookmarks…").clicked() {
                        self.bookmarks_open = true;
                        ui.close();
                    }
                });
                if ui.button("Profiles").clicked() {
                    self.profile_open = true;
                }
                ui.separator();
                let titles: Vec<(usize, String, bool, bool)> = self.tabs.iter().enumerate().map(|(i, tab)| {
                    let title = self.sessions.get(tab.focused).map(|p| p.title.clone()).unwrap_or_else(|| "shell".into());
                    (i, title, tab.activity, tab.bell)
                }).collect();
                for (i, title, activity, bell) in titles {
                    let mut label = title;
                    if label.chars().count() > 22 {
                        label = label.chars().take(21).collect::<String>() + "…";
                    }
                    if activity {
                        label = format!("● {label}");
                    }
                    if bell {
                        label = format!("🔔 {label}");
                    }
                    let selected = i == self.active;
                    if ui.selectable_label(selected, label).clicked() {
                        self.active = i;
                        if let Some(tab) = self.tabs.get_mut(i) {
                            tab.activity = false;
                            tab.bell = false;
                        }
                    }
                    if ui.small_button("×").clicked() {
                        self.active = i;
                        if let Some(id) = self.focused_id() {
                            self.close_pane(id);
                        }
                    }
                }
            });
        });

        if self.find_open {
            egui::Panel::bottom("find").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Find");
                    let resp = ui.text_edit_singleline(&mut self.find_query);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.jump_search(true);
                    }
                    if ui.button("Next").clicked() {
                        self.jump_search(true);
                    }
                    if ui.button("Prev").clicked() {
                        self.jump_search(false);
                    }
                    if ui.button("Close").clicked() {
                        self.find_open = false;
                    }
                });
            });
        }

        if self.profile_open {
            self.profile_window(&ctx);
        }
        if self.bookmarks_open {
            self.bookmark_window(&ctx);
        }
        if self.about_open {
            egui::Window::new("About hack-shell").open(&mut self.about_open).show(&ctx, |ui| {
                ui.label("GPU terminal for EndeavourOS.");
                ui.label("Select text, then middle-click in another window to paste it.");
                ui.label("Ctrl+Shift+C / V use the normal clipboard. Copy-on-select fills both.");
            });
        }
        if let Some(pos) = self.context {
            egui::Area::new(egui::Id::new("ctx-menu")).fixed_pos(pos).show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    if ui.button("Copy").clicked() {
                        self.copy_focused();
                        self.context = None;
                    }
                    if ui.button("Paste").clicked() {
                        if let Some(id) = self.focused_id() {
                            self.paste_into(id, false);
                        }
                        self.context = None;
                    }
                    if ui.button("Paste selection").clicked() {
                        if let Some(id) = self.focused_id() {
                            self.paste_into(id, true);
                        }
                        self.context = None;
                    }
                    if ui.button("Open URL").clicked() {
                        if let Some(id) = self.focused_id() {
                            if let Some(pane) = self.sessions.get(id) {
                                let fonts = self.fonts.as_ref().unwrap();
                                if let Some(hit) = self.pane_at(self.cursor.x as f32, self.cursor.y as f32) {
                                    let point = session::mouse_point(
                                        (self.cursor.x as f32 - hit.x) / fonts.cell_w,
                                        (self.cursor.y as f32 - hit.y) / fonts.cell_h,
                                        pane.cols.max(1),
                                        pane.rows.max(1),
                                        session::display_offset(pane),
                                    );
                                    if let Some(url) = session::url_at(pane, point) {
                                        session::open_url(&url);
                                    }
                                }
                            }
                        }
                        self.context = None;
                    }
                });
            });
        }

        let avail = ui.available_rect_before_wrap();
        let px = ctx.pixels_per_point();
        self.term_rect = [avail.min.x * px, avail.min.y * px, avail.width() * px, avail.height() * px];
    }

    fn profile_window(&mut self, ctx: &egui::Context) {
        let mut open = self.profile_open;
        egui::Window::new("Profiles").open(&mut open).show(ctx, |ui| {
            if self.config.profiles.is_empty() {
                self.config.profiles.push(Profile::default());
            }
            self.profile_index = self.profile_index.min(self.config.profiles.len() - 1);
            let names: Vec<String> = self.config.profiles.iter().map(|p| p.name.clone()).collect();
            egui::ComboBox::from_label("Profile").selected_text(&names[self.profile_index]).show_ui(ui, |ui| {
                for (i, name) in names.iter().enumerate() {
                    ui.selectable_value(&mut self.profile_index, i, name);
                }
            });
            let profile = &mut self.config.profiles[self.profile_index];
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut profile.name);
            });
            ui.horizontal(|ui| {
                ui.label("Font");
                ui.text_edit_singleline(&mut profile.font_family);
            });
            ui.add(egui::Slider::new(&mut profile.font_size, 8.0..=28.0).text("Size"));
            ui.add(egui::Slider::new(&mut profile.opacity, 0.2..=1.0).text("Opacity"));
            ui.horizontal(|ui| {
                ui.label("Scheme");
                ui.text_edit_singleline(&mut profile.scheme);
            });
            ui.horizontal(|ui| {
                ui.label("Command");
                ui.text_edit_singleline(&mut profile.command);
            });
            ui.horizontal(|ui| {
                ui.label("Directory");
                ui.text_edit_singleline(&mut profile.working_directory);
            });
            ui.add(egui::Slider::new(&mut profile.scrollback, 100..=100_000).text("Scrollback"));
            ui.checkbox(&mut profile.copy_on_select, "Copy selection to clipboard and primary");
            ui.checkbox(&mut profile.notify_on_bell, "Notify on bell");
            ui.checkbox(&mut profile.notify_on_exit, "Notify when the process exits");
            ui.checkbox(&mut profile.notify_on_activity, "Notify on background activity");
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    self.config.default_profile = self.config.profiles[self.profile_index].name.clone();
                    let _ = self.config.save();
                }
                if ui.button("New profile").clicked() {
                    let mut p = self.config.profiles[self.profile_index].clone();
                    p.name = format!("{} copy", p.name);
                    self.config.profiles.push(p);
                    self.profile_index = self.config.profiles.len() - 1;
                }
                if ui.button("Use for new tabs").clicked() {
                    self.config.default_profile = self.config.profiles[self.profile_index].name.clone();
                    let _ = self.config.save();
                }
            });
            ui.label("Schemes: Breeze Dark, Breeze Light, Solarized Dark");
        });
        self.profile_open = open;
    }

    fn bookmark_window(&mut self, ctx: &egui::Context) {
        let mut open = self.bookmarks_open;
        let mut remove = None;
        let mut open_here: Option<(String, String)> = None;
        egui::Window::new("Bookmarks").open(&mut open).show(ctx, |ui| {
            for (i, mark) in self.config.bookmarks.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut mark.name);
                    ui.text_edit_singleline(&mut mark.directory);
                    if ui.button("Open").clicked() {
                        open_here = Some((mark.directory.clone(), mark.command.clone()));
                    }
                    if ui.button("Delete").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if ui.button("Save").clicked() {
                let _ = self.config.save();
            }
        });
        if let Some((dir, cmd)) = open_here {
            let mut profile = self.profile();
            profile.working_directory = dir;
            if !cmd.is_empty() {
                profile.command = cmd;
            }
            self.new_tab(Some(profile), None);
        }
        if let Some(i) = remove {
            self.config.bookmarks.remove(i);
            let _ = self.config.save();
        }
        self.bookmarks_open = open;
    }

    fn jump_search(&mut self, forward: bool) {
        let query = self.find_query.clone();
        if query.is_empty() {
            return;
        }
        let Some(id) = self.focused_id() else { return };
        let Some(pane) = self.sessions.get_mut(id) else { return };
        let text = session::dump_output(pane);
        if forward {
            if text.contains(&query) {
                session::scroll_top(pane);
            }
        } else {
            session::scroll_bottom(pane);
        }
        self.request_redraw();
    }

    fn redraw(&mut self) {
        if self.last_blink.elapsed() > Duration::from_millis(530) {
            self.blink_on = !self.blink_on;
            self.last_blink = Instant::now();
        }
        let Some(window) = self.window.clone() else { return };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let mut egui_state = self.egui.take().unwrap();
        let raw = egui_state.take_egui_input(&window);
        let ctx = egui_state.egui_ctx().clone();
        let full = ctx.run_ui(raw, |ui| self.ui(ui));
        let _ = egui_state.handle_platform_output(&window, full.platform_output.clone());
        let pixels_per_point = ctx.pixels_per_point();
        self.scale = pixels_per_point;
        let jobs = ctx.tessellate(full.shapes, pixels_per_point);
        self.egui = Some(egui_state);
        for (id, deltas) in &full.textures_delta.set {
            for delta in deltas {
                apply_image_delta(&mut self.egui_textures, *id, delta);
            }
        }
        for id in &full.textures_delta.free {
            self.egui_textures.remove(id);
        }

        self.sync_pane_sizes();
        let list = self.build_draw_list(size);
        let Some(soft) = self.soft.as_mut() else { return };
        let (Some(w), Some(h)) = (std::num::NonZeroU32::new(size.width), std::num::NonZeroU32::new(size.height)) else { return };
        if soft.surface.resize(w, h).is_err() {
            return;
        }
        let mut buffer = match soft.surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(err) => {
                log::warn!("framebuffer: {err}");
                return;
            },
        };
        buffer.fill(0x001b1e20);
        let fonts = self.fonts.as_ref().unwrap();
        paint_cells(fonts, &list, size.width, size.height, &mut buffer);
        paint_egui(&jobs, &self.egui_textures, pixels_per_point, size.width, size.height, &mut buffer);
        if let Err(err) = buffer.present() {
            log::warn!("present: {err}");
        }
    }

    fn sync_pane_sizes(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let mut panes = Vec::new();
        let mut splits = Vec::new();
        layout_node(&tab.root, self.term_rect, &mut panes, &mut splits);
        let fonts = self.fonts.as_ref().unwrap();
        let cell = (fonts.cell_w, fonts.cell_h);
        let mut resizes = Vec::new();
        for pane_box in panes {
            let padding = self.sessions.get(pane_box.id).map(|p| p.padding).unwrap_or(6.0) * self.scale;
            let cols = ((pane_box.w - padding * 2.0) / cell.0).floor().max(2.0) as usize;
            let rows = ((pane_box.h - padding * 2.0) / cell.1).floor().max(1.0) as usize;
            resizes.push((pane_box.id, cols, rows));
        }
        for (id, cols, rows) in resizes {
            self.sessions.resize(id, cols, rows, cell);
        }
    }

    fn build_draw_list(&mut self, size: PhysicalSize<u32>) -> DrawList {
        let mut list = DrawList::new();
        let Some(tab) = self.tabs.get(self.active) else { return list };
        let focused = tab.focused;
        let mut panes = Vec::new();
        let mut splits = Vec::new();
        layout_node(&tab.root, self.term_rect, &mut panes, &mut splits);
        let fonts = self.fonts.as_mut().unwrap();
        let flash = self.bell_until.map(|t| t > Instant::now()).unwrap_or(false);
        for pane_box in panes {
            let Some(pane) = self.sessions.get(pane_box.id) else { continue };
            let scheme = self.config.scheme(&pane.scheme);
            let opacity = pane.opacity;
            let padding = pane.padding * self.scale;
            let scissor = scissor_of(pane_box.x, pane_box.y, pane_box.w, pane_box.h, size);
            list.begin_pane(scissor);
            let bg = scheme.bg_rgba(opacity);
            list.solid(pane_box.x, pane_box.y, pane_box.w, pane_box.h, bg);
            let origin_x = pane_box.x + padding;
            let origin_y = pane_box.y + padding;
            let cols = pane.cols.max(1);
            let rows = pane.rows.max(1);
            let term = pane.term.lock();
            let content = term.renderable_content();
            let selection = content.selection;
            let cursor_point = content.cursor.point;
            let cursor_shape = content.cursor.shape;
            let show_cursor = pane_box.id == focused && self.window_focused && (self.blink_on || !cursor_blinks(&term));
            let query = if self.find_open { self.find_query.to_lowercase() } else { String::new() };
            let mut row_text: Vec<(usize, String)> = Vec::new();
            let mut cells: Vec<(alacritty_terminal::index::Point, alacritty_terminal::term::cell::Cell)> = Vec::new();
            for indexed in content.display_iter {
                cells.push((indexed.point, indexed.cell.clone()));
            }
            let _ = content;
            // Group by visual row for search highlight.
            let mut line_buf = String::new();
            let mut line_points: Vec<i32> = Vec::new();
            let mut current_line = None;
            for (point, cell) in &cells {
                if current_line != Some(point.line) {
                    if let Some(line) = current_line {
                        row_text.push((line.0 as usize, std::mem::take(&mut line_buf)));
                        let _ = line;
                    }
                    current_line = Some(point.line);
                    line_points.clear();
                }
                if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    line_buf.push(cell.c);
                }
                let _ = line_points;
            }
            let search_cols = search_columns(&cells, &query);

            for (point, cell) in &cells {
                let (col, row) = point_to_cell(*point, rows, term.grid().display_offset());
                if col >= cols || row >= rows {
                    continue;
                }
                let x = origin_x + col as f32 * fonts.cell_w;
                let y = origin_y + row as f32 * fonts.cell_h;
                let wide = if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 };
                let selected = selection.as_ref().map(|sel| sel.contains(*point)).unwrap_or(false) || search_cols.contains(&(row, col));
                let mut fg = resolve_color(&scheme, content_color_fg(&term, cell.fg), true);
                let mut bg_c = resolve_color(&scheme, content_color_bg(&term, cell.bg), false);
                if cell.flags.contains(Flags::INVERSE) || selected {
                    std::mem::swap(&mut fg, &mut bg_c);
                }
                if selected {
                    let sel = scheme.selection();
                    bg_c = sel;
                }
                if cell.flags.contains(Flags::DIM) {
                    fg = [fg[0] * 0.7, fg[1] * 0.7, fg[2] * 0.7];
                }
                let bg_a = if selected { opacity.max(0.85) } else { opacity };
                list.solid(x, y, fonts.cell_w * wide, fonts.cell_h, [bg_c[0], bg_c[1], bg_c[2], bg_a]);
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) || cell.flags.contains(Flags::HIDDEN) || cell.c == ' ' || cell.c == '\0' {
                    continue;
                }
                let style = match (cell.flags.contains(Flags::BOLD), cell.flags.contains(Flags::ITALIC)) {
                    (true, true) => 3,
                    (true, false) => 1,
                    (false, true) => 2,
                    _ => 0,
                };
                let g = fonts.glyph(style, cell.c);
                let gx = x + g.bearing[0];
                let gy = y + fonts.ascent - g.bearing[1] - g.size[1];
                list.glyph(gx, gy, g, [fg[0], fg[1], fg[2], 1.0]);
                if cell.flags.contains(Flags::UNDERLINE) {
                    let uy = y + fonts.cell_h - 2.0;
                    list.solid(x, uy, fonts.cell_w * wide, 1.0, [fg[0], fg[1], fg[2], 1.0]);
                }
                if cell.flags.intersects(Flags::STRIKEOUT) {
                    let sy = y + fonts.cell_h * 0.55;
                    list.solid(x, sy, fonts.cell_w * wide, 1.0, [fg[0], fg[1], fg[2], 1.0]);
                }
            }
            if show_cursor {
                let (col, row) = point_to_cell(cursor_point, rows, term.grid().display_offset());
                if col < cols && row < rows {
                    let x = origin_x + col as f32 * fonts.cell_w;
                    let y = origin_y + row as f32 * fonts.cell_h;
                    let c = scheme.cursor();
                    let color = [c[0], c[1], c[2], 0.9];
                    match cursor_shape {
                        CursorShape::Beam => list.solid(x, y, 2.0, fonts.cell_h, color),
                        CursorShape::Underline => list.solid(x, y + fonts.cell_h - 2.0, fonts.cell_w, 2.0, color),
                        _ => list.solid(x, y, fonts.cell_w, fonts.cell_h, [c[0], c[1], c[2], 0.45]),
                    }
                }
            }
            if pane.exited {
                list.solid(pane_box.x, pane_box.y + pane_box.h - 28.0, pane_box.w, 28.0, [0.1, 0.1, 0.1, 0.85]);
            }
            if flash && pane_box.id == focused {
                list.solid(pane_box.x, pane_box.y, pane_box.w, pane_box.h, [1.0, 1.0, 1.0, 0.18]);
            }
            let _ = (cursor_shape, size);
        }
        list
    }
}

fn cursor_blinks(term: &alacritty_terminal::term::Term<session::Proxy>) -> bool {
    term.cursor_style().blinking
}

fn content_color_fg(term: &alacritty_terminal::term::Term<session::Proxy>, color: Color) -> Color {
    let _ = term;
    color
}

fn content_color_bg(term: &alacritty_terminal::term::Term<session::Proxy>, color: Color) -> Color {
    let _ = term;
    color
}

fn row_from_point(point: Point, rows: usize, offset: usize) -> usize {
    point_to_cell(point, rows, offset).1 + 1
}

fn point_to_cell(point: Point, rows: usize, offset: usize) -> (usize, usize) {
    let row = point.line.0 + (rows as i32 - 1) + offset as i32;
    (point.column.0, row.max(0) as usize)
}

fn search_columns(cells: &[(Point, alacritty_terminal::term::cell::Cell)], query: &str) -> std::collections::HashSet<(usize, usize)> {
    let mut hits = std::collections::HashSet::new();
    if query.is_empty() {
        return hits;
    }
    let mut line = String::new();
    let mut cols = Vec::new();
    let mut current = None;
    let mut flush = |line: &mut String, cols: &mut Vec<(i32, usize)>, hits: &mut std::collections::HashSet<(usize, usize)>| {
        let lower = line.to_lowercase();
        let mut from = 0;
        while let Some(rel) = lower[from..].find(query) {
            let start = from + rel;
            for i in start..start + query.chars().count() {
                if let Some((line_i, col)) = cols.get(i) {
                    hits.insert((*line_i as usize, *col));
                }
            }
            from = start + query.len().max(1);
            if from >= lower.len() {
                break;
            }
        }
        line.clear();
        cols.clear();
    };
    for (point, cell) in cells {
        if current != Some(point.line) {
            flush(&mut line, &mut cols, &mut hits);
            current = Some(point.line);
        }
        if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            line.push(cell.c);
            cols.push((point.line.0, point.column.0));
        }
    }
    flush(&mut line, &mut cols, &mut hits);
    hits
}

fn resolve_color(scheme: &Scheme, color: Color, fg: bool) -> [f32; 3] {
    match color {
        Color::Spec(Rgb { r, g, b }) => [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0],
        Color::Indexed(idx) => indexed(scheme, idx),
        Color::Named(named) => match named {
            NamedColor::Foreground => scheme.fg(),
            NamedColor::Background => [scheme.bg_rgba(1.0)[0], scheme.bg_rgba(1.0)[1], scheme.bg_rgba(1.0)[2]],
            NamedColor::Cursor => scheme.cursor(),
            NamedColor::Black => scheme.ansi(0),
            NamedColor::Red => scheme.ansi(1),
            NamedColor::Green => scheme.ansi(2),
            NamedColor::Yellow => scheme.ansi(3),
            NamedColor::Blue => scheme.ansi(4),
            NamedColor::Magenta => scheme.ansi(5),
            NamedColor::Cyan => scheme.ansi(6),
            NamedColor::White => scheme.ansi(7),
            NamedColor::BrightBlack => scheme.ansi(8),
            NamedColor::BrightRed => scheme.ansi(9),
            NamedColor::BrightGreen => scheme.ansi(10),
            NamedColor::BrightYellow => scheme.ansi(11),
            NamedColor::BrightBlue => scheme.ansi(12),
            NamedColor::BrightMagenta => scheme.ansi(13),
            NamedColor::BrightCyan => scheme.ansi(14),
            NamedColor::BrightWhite => scheme.ansi(15),
            _ => {
                if fg { scheme.fg() } else { [scheme.bg_rgba(1.0)[0], scheme.bg_rgba(1.0)[1], scheme.bg_rgba(1.0)[2]] }
            },
        },
    }
}

fn indexed(scheme: &Scheme, idx: u8) -> [f32; 3] {
    match idx {
        0..=15 => scheme.ansi(idx as usize),
        16..=231 => {
            let i = idx - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let c = |n: u8| if n == 0 { 0.0 } else { (55 + 40 * n) as f32 / 255.0 };
            [c(r), c(g), c(b)]
        },
        _ => {
            let v = (8 + 10 * (idx - 232)) as f32 / 255.0;
            [v, v, v]
        },
    }
}

fn palette_rgb(scheme: &Scheme, index: usize) -> Rgb {
    let c = if index < 16 { scheme.ansi(index) } else { indexed(scheme, index as u8) };
    Rgb { r: (c[0] * 255.0) as u8, g: (c[1] * 255.0) as u8, b: (c[2] * 255.0) as u8 }
}

fn scissor_of(x: f32, y: f32, w: f32, h: f32, size: PhysicalSize<u32>) -> [u32; 4] {
    let x0 = x.max(0.0) as u32;
    let y0 = y.max(0.0) as u32;
    let x1 = (x + w).clamp(0.0, size.width as f32) as u32;
    let y1 = (y + h).clamp(0.0, size.height as f32) as u32;
    [x0.min(size.width), y0.min(size.height), x1.saturating_sub(x0), y1.saturating_sub(y0)]
}

#[derive(Clone, Copy)]
struct PaneBox {
    id: u64,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

fn layout_node(node: &Node, rect: [f32; 4], panes: &mut Vec<PaneBox>, splits: &mut Vec<Splitter>) {
    match node {
        Node::Pane(id) => panes.push(PaneBox { id: *id, x: rect[0], y: rect[1], w: rect[2], h: rect[3] }),
        Node::Split { vertical, ratio, a, b } => {
            let (aw, ah, bw, bh, ax, ay, bx, by) = if *vertical {
                let aw = rect[2] * ratio;
                (aw, rect[3], rect[2] - aw, rect[3], rect[0], rect[1], rect[0] + aw, rect[1])
            } else {
                let ah = rect[3] * ratio;
                (rect[2], ah, rect[2], rect[3] - ah, rect[0], rect[1], rect[0], rect[1] + ah)
            };
            let path_len = splits.len();
            let _ = path_len;
            layout_node(a, [ax, ay, aw, ah], panes, splits);
            layout_node(b, [bx, by, bw, bh], panes, splits);
            // Splitter path is filled by the caller via a separate walk. Record a placeholder
            // path that drag_split resolves by geometry instead.
            let hit = if *vertical {
                [ax + aw - 3.0, ay, 6.0, ah]
            } else {
                [ax, ay + ah - 3.0, aw, 6.0]
            };
            splits.push(Splitter {
                path: Vec::new(),
                vertical: *vertical,
                origin: if *vertical { rect[0] } else { rect[1] },
                span: if *vertical { rect[2] } else { rect[3] },
                hit,
            });
        },
    }
}

fn node_mut<'a>(node: &'a mut Node, path: &[usize]) -> Option<&'a mut Node> {
    if path.is_empty() {
        return Some(node);
    }
    match node {
        Node::Split { a, b, .. } => {
            if path[0] == 0 { node_mut(a, &path[1..]) } else { node_mut(b, &path[1..]) }
        },
        Node::Pane(_) => None,
    }
}

fn first_pane(node: &Node) -> Option<u64> {
    match node {
        Node::Pane(id) => Some(*id),
        Node::Split { a, .. } => first_pane(a),
    }
}

fn pane_in(node: &Node, id: u64) -> bool {
    match node {
        Node::Pane(p) => *p == id,
        Node::Split { a, b, .. } => pane_in(a, id) || pane_in(b, id),
    }
}

fn split_at(node: &mut Node, target: u64, new_id: u64, vertical: bool) -> bool {
    match node {
        Node::Pane(id) if *id == target => {
            *node = Node::Split {
                vertical,
                ratio: 0.5,
                a: Box::new(Node::Pane(target)),
                b: Box::new(Node::Pane(new_id)),
            };
            true
        },
        Node::Split { a, b, .. } => split_at(a, target, new_id, vertical) || split_at(b, target, new_id, vertical),
        _ => false,
    }
}

fn remove_pane(node: &mut Node, id: u64) -> Option<Node> {
    match node {
        Node::Pane(p) if *p == id => None,
        Node::Split { a, b, .. } => {
            if matches!(a.as_ref(), Node::Pane(p) if *p == id) {
                return Some((**b).take_placeholder());
            }
            if matches!(b.as_ref(), Node::Pane(p) if *p == id) {
                return Some((**a).take_placeholder());
            }
            if let Some(repl) = remove_pane(a, id) {
                *a = Box::new(repl);
                return Some(node.take_placeholder());
            }
            if let Some(repl) = remove_pane(b, id) {
                *b = Box::new(repl);
                return Some(node.take_placeholder());
            }
            Some(node.take_placeholder())
        },
        Node::Pane(_) => Some(node.take_placeholder()),
    }
}

trait TakeNode {
    fn take_placeholder(&mut self) -> Node;
}

impl TakeNode for Node {
    fn take_placeholder(&mut self) -> Node {
        std::mem::replace(self, Node::Pane(0))
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.ensure_window(event_loop);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
        let events = self.sessions.drain();
        for (id, event) in events {
            self.handle_term_event(id, event);
        }
        self.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else { return };
        let response = self.egui.as_mut().map(|e| e.on_window_event(&window, &event));
        let consumed = response.map(|r| r.consumed).unwrap_or(false);
        if response.map(|r| r.repaint).unwrap_or(false) {
            window.request_redraw();
        }
        if self.should_quit {
            self.sessions.shutdown_all();
            event_loop.exit();
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.sessions.shutdown_all();
                event_loop.exit();
            },
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::KeyboardInput { event, .. } if !consumed => self.handle_key(&event),
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(position),
            WindowEvent::MouseInput { state, button, .. } if !consumed => self.on_mouse_button(button, state),
            WindowEvent::MouseWheel { delta, .. } if !consumed => self.on_scroll(delta),
            WindowEvent::Ime(Ime::Commit(text)) => {
                if let Some(id) = self.focused_id() {
                    self.sessions.write(id, text.into_bytes());
                }
            },
            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                if let Some(id) = self.focused_id() {
                    if let Some(pane) = self.sessions.get_mut(id) {
                        pane.term.lock().is_focused = focused;
                    }
                }
                self.request_redraw();
            },
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor as f32;
                self.request_redraw();
            },
            _ => {},
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.bell_until.is_some() || self.window_focused {
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500)));
        }
        if self.bell_until.map(|t| t > Instant::now()).unwrap_or(false) {
            self.request_redraw();
        }
    }
}
