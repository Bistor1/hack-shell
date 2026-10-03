use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::tty::{self, Shell};
use alacritty_terminal::vte::ansi::Handler;
use winit::event_loop::EventLoopProxy;

use crate::config::Profile;

pub enum UserEvent {
    Wake,
}

#[derive(Clone)]
pub struct Proxy {
    pub id: u64,
    tx: Sender<(u64, Event)>,
    wake: EventLoopProxy<UserEvent>,
}

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        let _ = self.tx.send((self.id, event));
        let _ = self.wake.send_event(UserEvent::Wake);
    }
}

pub struct TermSize {
    pub cols: usize,
    pub lines: usize,
}

impl Dimensions for TermSize {
    fn columns(&self) -> usize {
        self.cols.max(2)
    }
    fn screen_lines(&self) -> usize {
        self.lines.max(1)
    }
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
}

pub struct Pane {
    pub id: u64,
    pub term: Arc<FairMutex<Term<Proxy>>>,
    pub notifier: Notifier,
    pub profile_name: String,
    pub title: String,
    pub exited: bool,
    pub read_only: bool,
    pub copy_on_select: bool,
    pub notify_bell: bool,
    pub notify_exit: bool,
    pub notify_activity: bool,
    pub opacity: f32,
    pub scheme: String,
    pub padding: f32,
    pub child_pid: Option<u32>,
    pub cols: usize,
    pub rows: usize,
    pub selecting: bool,
    pub click_count: u8,
    pub last_click: std::time::Instant,
    pub last_click_point: Option<Point>,
}

pub struct Sessions {
    pub panes: Vec<Pane>,
    rx: Receiver<(u64, Event)>,
    tx: Sender<(u64, Event)>,
    next_id: u64,
    wake: EventLoopProxy<UserEvent>,
}

impl Sessions {
    pub fn new(wake: EventLoopProxy<UserEvent>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self { panes: Vec::new(), rx, tx, next_id: 1, wake }
    }

    pub fn spawn(&mut self, profile: &Profile, cols: usize, rows: usize, cell: (f32, f32)) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let proxy = Proxy { id, tx: self.tx.clone(), wake: self.wake.clone() };
        let mut cfg = TermConfig::default();
        cfg.scrolling_history = profile.scrollback.max(100);
        let size = TermSize { cols, lines: rows };
        let term = Term::new(cfg, &size, proxy.clone());
        let term = Arc::new(FairMutex::new(term));
        let mut options = tty::Options::default();
        if !profile.command.trim().is_empty() {
            let mut parts = profile.command.split_whitespace();
            if let Some(program) = parts.next() {
                options.shell = Some(Shell::new(program.to_string(), parts.map(str::to_string).collect()));
            }
        }
        if !profile.working_directory.trim().is_empty() {
            options.working_directory = Some(PathBuf::from(&profile.working_directory));
        }
        let window_size = WindowSize {
            num_cols: size.columns() as u16,
            num_lines: size.screen_lines() as u16,
            cell_width: cell.0.round().max(1.0) as u16,
            cell_height: cell.1.round().max(1.0) as u16,
        };
        let before = child_pids();
        let pty = tty::new(&options, window_size, id).expect("open pty");
        let child_pid = child_pids().into_iter().find(|pid| !before.contains(pid));
        let loop_ = EventLoop::new(term.clone(), proxy, pty, false, false).expect("pty loop");
        let notifier = Notifier(loop_.channel());
        let _join = loop_.spawn();
        self.panes.push(Pane {
            id,
            term,
            notifier,
            profile_name: profile.name.clone(),
            title: profile.name.clone(),
            exited: false,
            read_only: false,
            copy_on_select: profile.copy_on_select,
            notify_bell: profile.notify_on_bell,
            notify_exit: profile.notify_on_exit,
            notify_activity: profile.notify_on_activity,
            opacity: profile.opacity,
            scheme: profile.scheme.clone(),
            padding: profile.padding,
            child_pid,
            cols,
            rows,
            selecting: false,
            click_count: 0,
            last_click: std::time::Instant::now() - std::time::Duration::from_secs(10),
            last_click_point: None,
        });
        id
    }

    pub fn get(&self, id: u64) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    pub fn close(&mut self, id: u64) {
        if let Some(pane) = self.panes.iter().find(|p| p.id == id) {
            let _ = pane.notifier.0.send(Msg::Shutdown);
        }
        self.panes.retain(|p| p.id != id);
    }

    pub fn shutdown_all(&self) {
        for pane in &self.panes {
            let _ = pane.notifier.0.send(Msg::Shutdown);
        }
    }

    pub fn drain(&mut self) -> Vec<(u64, Event)> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    pub fn write(&self, id: u64, bytes: impl Into<std::borrow::Cow<'static, [u8]>>) {
        if let Some(pane) = self.get(id) {
            if !pane.read_only && !pane.exited {
                pane.notifier.notify(bytes);
            }
        }
    }

    pub fn resize(&mut self, id: u64, cols: usize, rows: usize, cell: (f32, f32)) {
        let Some(pane) = self.get_mut(id) else { return };
        if pane.cols == cols && pane.rows == rows {
            return;
        }
        pane.cols = cols;
        pane.rows = rows;
        let size = TermSize { cols, lines: rows };
        pane.term.lock().resize(TermSize { cols: size.columns(), lines: size.screen_lines() });
        pane.notifier.on_resize(WindowSize {
            num_cols: size.columns() as u16,
            num_lines: size.screen_lines() as u16,
            cell_width: cell.0.round().max(1.0) as u16,
            cell_height: cell.1.round().max(1.0) as u16,
        });
    }
}

fn child_pids() -> Vec<u32> {
    let me = std::process::id();
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else { return out };
    for ent in dir.flatten() {
        let Ok(pid) = ent.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        let Some((_, rest)) = stat.split_once(") ") else { continue };
        let mut fields = rest.split_whitespace();
        let _state = fields.next();
        if fields.next().and_then(|s| s.parse().ok()) == Some(me) {
            out.push(pid);
        }
    }
    out
}

pub fn mouse_point(x: f32, y: f32, cols: usize, rows: usize, display_offset: usize) -> Point {
    let col = (x.floor() as usize).min(cols.saturating_sub(1));
    let row = (y.floor() as usize).min(rows.saturating_sub(1));
    // Line 0 is the bottom of the screen. Top of the viewport is negative.
    let line = row as i32 - (rows as i32 - 1) - display_offset as i32;
    Point::new(Line(line), Column(col))
}

pub fn start_selection(pane: &mut Pane, point: Point, ty: SelectionType) {
    let mut term = pane.term.lock();
    term.selection = Some(Selection::new(ty, point, Side::Left));
    pane.selecting = true;
}

pub fn update_selection(pane: &mut Pane, point: Point) {
    let mut term = pane.term.lock();
    if let Some(sel) = term.selection.as_mut() {
        sel.update(point, Side::Right);
    }
}

pub fn selection_text(pane: &Pane) -> Option<String> {
    let term = pane.term.lock();
    term.selection_to_string().filter(|s| !s.is_empty())
}

pub fn clear_selection(pane: &mut Pane) {
    pane.term.lock().selection = None;
    pane.selecting = false;
}

pub fn scroll(pane: &mut Pane, lines: i32) {
    pane.term.lock().scroll_display(Scroll::Delta(lines));
}

pub fn scroll_top(pane: &mut Pane) {
    pane.term.lock().scroll_display(Scroll::Top);
}

pub fn scroll_bottom(pane: &mut Pane) {
    pane.term.lock().scroll_display(Scroll::Bottom);
}

pub fn clear_scrollback(pane: &mut Pane) {
    let mut term = pane.term.lock();
    // Drop history by resizing onto itself after resetting the grid history if available.
    let cols = term.columns();
    let lines = term.screen_lines();
    term.scroll_display(Scroll::Bottom);
    // Clear visible screen and history via the parser-facing API.
    term.clear_screen(vte::ansi::ClearMode::All);
    let _ = (cols, lines);
}

pub fn dump_output(pane: &Pane) -> String {
    let term = pane.term.lock();
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    term.bounds_to_string(start, end)
}

pub fn cwd_of(pane: &Pane) -> Option<PathBuf> {
    let pid = pane.child_pid?;
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

pub fn mode_of(pane: &Pane) -> TermMode {
    *pane.term.lock().mode()
}

pub fn display_offset(pane: &Pane) -> usize {
    pane.term.lock().grid().display_offset()
}

/// Best-effort URL under a grid point, including OSC 8 hyperlinks.
pub fn url_at(pane: &Pane, point: Point) -> Option<String> {
    let term = pane.term.lock();
    if let Some(cell) = term.grid().iter_from(point).next() {
        if let Some(link) = cell.hyperlink() {
            return Some(link.uri().to_string());
        }
    }
    let line = term.bounds_to_string(
        Point::new(point.line, Column(0)),
        Point::new(point.line, term.last_column()),
    );
    find_url(&line, point.column.0)
}

fn find_url(line: &str, col: usize) -> Option<String> {
    let bytes = line.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let rest = &line[start..];
        let rel = rest.find("http://").or_else(|| rest.find("https://")).or_else(|| rest.find("www."));
        let Some(rel) = rel else { break };
        let abs = start + rel;
        let tail = &line[abs..];
        let end = tail
            .find(|c: char| c.is_whitespace() || "<>\"'()[]{}".contains(c))
            .map(|i| abs + i)
            .unwrap_or(line.len());
        if col >= abs && col < end {
            let mut url = line[abs..end].trim_end_matches('.').to_string();
            if url.starts_with("www.") {
                url = format!("https://{url}");
            }
            return Some(url);
        }
        start = end.max(abs + 1);
    }
    None
}

pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

pub fn notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "hack-shell", "-i", "utilities-terminal", summary, body])
        .spawn();
}
