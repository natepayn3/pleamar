//! The terminals a scene holds (`terminal NAME { … }`): a shell on a pty of
//! its own, its screen kept by `alacritty_terminal` —the escape codes, the
//! grid and its history, what is selected—, drawn by the render with the
//! atlas's own letters (`gpu.rs`), and the keys and the mouse handed to it
//! (`render.rs`). Each one lives while its `run:` holds, shown or not: a tab
//! that is not in front keeps its shell.
//!
//! The pty and its reading are `alacritty_terminal`'s, on Linux, macOS and
//! Windows; what the render draws and how a key becomes bytes is here, and
//! is the same everywhere.

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor, Rgb};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};

use crate::scene::ToRender;

/// How many lines of history each one keeps beyond its screen.
pub const HISTORY: usize = 10_000;

/// The colours a terminal paints with when the scene says none: ink, paper and
/// the sixteen of the ANSI codes, in that order (`colors:` takes the same).
pub const DEFAULT_COLORS: [[u8; 3]; 18] = [
    [0xe8, 0xe2, 0xd9], // ink
    [0x14, 0x16, 0x18], // paper
    [0x24, 0x27, 0x2b], // black
    [0xe0, 0x7a, 0x6c], // red
    [0x9c, 0xc4, 0x8e], // green
    [0xe3, 0xc0, 0x7a], // yellow
    [0x84, 0xa9, 0xe0], // blue
    [0xc4, 0x93, 0xd6], // magenta
    [0x7f, 0xc4, 0xbd], // cyan
    [0xd8, 0xd2, 0xc8], // white
    [0x5c, 0x61, 0x68], // bright black
    [0xf0, 0x92, 0x84], // bright red
    [0xb4, 0xd8, 0xa4], // bright green
    [0xf0, 0xd2, 0x92], // bright yellow
    [0x9f, 0xbf, 0xf0], // bright blue
    [0xd6, 0xab, 0xe6], // bright magenta
    [0x98, 0xd8, 0xd0], // bright cyan
    [0xf6, 0xf2, 0xec], // bright white
];

/// What the emulator's threads leave for the render: a counter that moves with
/// each change of its screen (the drawing compares it), its title, its bell and
/// how it ended.
pub struct Shared {
    pub version: AtomicU64,
    pub bells: AtomicU32,
    pub title: Mutex<Option<String>>,
    pub exited: Mutex<Option<i32>>,
    sender: OnceLock<EventLoopSender>,
    size: Mutex<WindowSize>,
    /// Its colours as the scene says them, to answer a program that asks (OSC 4, 10, 11).
    pub palette: Mutex<[[u8; 3]; 18]>,
    /// What its program last put on the clipboard (OSC 52), and when: a program that takes the mouse
    /// (Claude Code, vim) selects by itself and copies, and the terminal has nothing selected.
    pub copied: Mutex<Option<(Instant, String)>>,
}

impl Shared {
    fn write(&self, bytes: Vec<u8>) {
        if let Some(s) = self.sender.get() {
            let _ = s.send(Msg::Input(bytes.into()));
        }
    }
}

#[derive(Clone)]
pub struct Listener {
    shared: Arc<Shared>,
    wake: Sender<ToRender>,
}

impl EventListener for Listener {
    fn send_event(&self, e: TermEvent) {
        let s = &self.shared;
        match e {
            TermEvent::Wakeup => {
                s.version.fetch_add(1, Ordering::Relaxed);
                let _ = self.wake.send(ToRender::Wake);
            }
            TermEvent::Title(t) => {
                *s.title.lock().unwrap() = Some(t);
                s.version.fetch_add(1, Ordering::Relaxed);
                let _ = self.wake.send(ToRender::Wake);
            }
            TermEvent::ResetTitle => {
                *s.title.lock().unwrap() = Some(String::new());
                let _ = self.wake.send(ToRender::Wake);
            }
            TermEvent::Bell => {
                s.bells.fetch_add(1, Ordering::Relaxed);
                let _ = self.wake.send(ToRender::Wake);
            }
            TermEvent::ChildExit(status) => {
                *s.exited.lock().unwrap() = Some(status.code().unwrap_or(-1));
                s.version.fetch_add(1, Ordering::Relaxed);
                let _ = self.wake.send(ToRender::Wake);
            }
            TermEvent::Exit => {
                s.exited.lock().unwrap().get_or_insert(0);
                s.version.fetch_add(1, Ordering::Relaxed);
                let _ = self.wake.send(ToRender::Wake);
            }
            TermEvent::PtyWrite(text) => s.write(text.into_bytes()),
            // What a program puts on the clipboard (OSC 52), and asks to read from it.
            TermEvent::ClipboardStore(_, text) => {
                crate::platform::clipboard_write(&text);
                *s.copied.lock().unwrap() = Some((Instant::now(), text));
            }
            TermEvent::ClipboardLoad(_, format) => {
                if let Some(t) = crate::platform::clipboard_read() {
                    s.write(format(&t).into_bytes());
                }
            }
            TermEvent::ColorRequest(index, format) => {
                let p = *s.palette.lock().unwrap();
                let rgb = match index {
                    0..=15 => p[index + 2],
                    256 => p[0],
                    257 => p[1],
                    16..=255 => indexed(index as u8, &p),
                    _ => p[0],
                };
                s.write(format(Rgb { r: rgb[0], g: rgb[1], b: rgb[2] }).into_bytes());
            }
            TermEvent::TextAreaSizeRequest(format) => {
                let size = *s.size.lock().unwrap();
                s.write(format(size).into_bytes());
            }
            TermEvent::CursorBlinkingChange | TermEvent::MouseCursorDirty => {}
        }
    }
}

/// How many columns and lines it has, for the emulator.
struct Size {
    columns: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// One shell and its screen.
pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    pub shared: Arc<Shared>,
    sender: EventLoopSender,
    pub size: WindowSize,
    /// What was told to the scene last: so it is told only when it changes.
    pub told_title: Option<String>,
    pub told_alive: Option<bool>,
    pub told_bells: u32,
    pub told_folder: String,
    /// Its child, to know the folder it is in (`/proc/PID/cwd` on Linux).
    pub pid: u32,
    /// The mouse dragging a selection on it: since when it was pressed, and how many clicks.
    pub selecting: bool,
    /// What is being looked for in its history, and where it was found last.
    finding: Option<(String, Option<alacritty_terminal::term::search::Match>)>,
}

impl Session {
    /// A shell (`command`, or the user's) in `folder`, with that many columns and lines.
    pub fn spawn(size: WindowSize, command: &[String], folder: Option<std::path::PathBuf>, env: &[(String, String)]) -> std::io::Result<Session> {
        let wake = WAKER.get().map(|w| w.lock().unwrap().clone()).ok_or_else(|| std::io::Error::other("no render to wake"))?;
        let shared = Arc::new(Shared {
            version: AtomicU64::new(1),
            bells: AtomicU32::new(0),
            title: Mutex::new(None),
            exited: Mutex::new(None),
            sender: OnceLock::new(),
            size: Mutex::new(size),
            palette: Mutex::new(DEFAULT_COLORS),
            copied: Mutex::new(None),
        });
        let listener = Listener { shared: shared.clone(), wake };
        let config = Config { scrolling_history: HISTORY, kitty_keyboard: true, ..Config::default() };
        let dims = Size { columns: size.num_cols as usize, lines: size.num_lines as usize };
        let term = Arc::new(FairMutex::new(Term::new(config, &dims, listener.clone())));
        let mut vars: HashMap<String, String> = HashMap::new();
        vars.insert("TERM".into(), "xterm-256color".into());
        vars.insert("COLORTERM".into(), "truecolor".into());
        for (k, v) in env {
            vars.insert(k.clone(), v.clone());
        }
        let options = tty::Options {
            shell: command.split_first().map(|(p, a)| tty::Shell::new(p.clone(), a.to_vec())),
            working_directory: folder.filter(|f| f.is_dir()),
            drain_on_exit: false,
            env: vars,
            #[cfg(target_os = "windows")]
            escape_args: true,
        };
        static IDS: AtomicU64 = AtomicU64::new(1);
        let pty = tty::new(&options, size, IDS.fetch_add(1, Ordering::Relaxed))?;
        #[cfg(unix)]
        let pid = pty.child().id();
        #[cfg(not(unix))]
        let pid = 0;
        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
        let sender = event_loop.channel();
        let _ = shared.sender.set(sender.clone());
        event_loop.spawn();
        Ok(Session { term, shared, sender, size, told_title: None, told_alive: None, told_bells: 0, told_folder: String::new(), pid, selecting: false, finding: None })
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn alive(&self) -> bool {
        self.shared.exited.lock().unwrap().is_none()
    }

    /// Another size: the emulator lays its screen out again and the program is told.
    pub fn resize(&mut self, size: WindowSize) {
        if (size.num_cols, size.num_lines, size.cell_width, size.cell_height) == (self.size.num_cols, self.size.num_lines, self.size.cell_width, self.size.cell_height) {
            return;
        }
        self.size = size;
        *self.shared.size.lock().unwrap() = size;
        self.term.lock().resize(Size { columns: size.num_cols as usize, lines: size.num_lines as usize });
        let _ = self.sender.send(Msg::Resize(size));
        self.shared.version.fetch_add(1, Ordering::Relaxed);
    }

    /// The folder its shell is in, where the system says it.
    pub fn folder(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            std::fs::read_link(format!("/proc/{}/cwd", self.pid)).ok().map(|p| p.display().to_string())
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    /// The link at a cell, for Ctrl+click: one a program marked (OSC 8), or an
    /// address written on its line (`https://…`, `file://…`).
    pub fn link_at(&self, col: f32, row: f32) -> Option<String> {
        let (p, _) = self.point(col, row);
        let t = self.term.lock();
        if let Some(h) = t.grid()[p].hyperlink() {
            return Some(h.uri().to_owned());
        }
        let line: String = (0..t.columns()).map(|c| t.grid()[Point::new(p.line, Column(c))].c).collect();
        let at = p.column.0;
        let chars: Vec<char> = line.chars().collect();
        for scheme in ["https://", "http://", "file://"] {
            let mut from = 0;
            let text: String = chars.iter().collect();
            while let Some(k) = text[from..].find(scheme) {
                let start = text[..from + k].chars().count();
                let mut end = start;
                while end < chars.len() && !chars[end].is_whitespace() && !matches!(chars[end], '"' | '\'' | '<' | '>' | '`' | '\0') {
                    end += 1;
                }
                // (A sentence's end is not the address's.)
                while end > start && matches!(chars[end - 1], '.' | ',' | ';' | ':' | ')' | ']') {
                    end -= 1;
                }
                if (start..end).contains(&at) {
                    return Some(chars[start..end].iter().collect());
                }
                from = from + k + scheme.len();
            }
        }
        None
    }

    /// Looks for a text in its screen and its history, without minding capitals:
    /// `older` the next one up from the last found (the first, up from the end),
    /// otherwise the next one down. The one found is selected and shown.
    pub fn find(&mut self, what: &str, older: bool) -> Result<(), String> {
        use alacritty_terminal::index::Direction;
        use alacritty_terminal::term::search::RegexSearch;
        if what.is_empty() {
            return Err("nothing to look for".into());
        }
        let escaped: String = what.chars().flat_map(|c| if "\\.+*?()|[]{}^$#&-~".contains(c) { vec!['\\', c] } else { vec![c] }).collect();
        let mut regex = RegexSearch::new(&format!("(?i){escaped}")).map_err(|e| e.to_string())?;
        let last = match &self.finding {
            Some((w, m)) if w == what => m.clone(),
            _ => None,
        };
        let mut t = self.term.lock();
        let bottom = Point::new(Line(t.screen_lines() as i32 - 1), Column(t.columns().saturating_sub(1)));
        let (origin, direction, side) = match (&last, older) {
            (Some(m), true) => (m.start().sub(&*t, alacritty_terminal::index::Boundary::None, 1), Direction::Left, Side::Left),
            (Some(m), false) => (m.end().add(&*t, alacritty_terminal::index::Boundary::None, 1), Direction::Right, Side::Right),
            (None, _) => (bottom, Direction::Left, Side::Right),
        };
        let found = t.search_next(&mut regex, origin, direction, side, None);
        match found {
            Some(m) => {
                let mut sel = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                sel.update(*m.end(), Side::Right);
                t.selection = Some(sel);
                t.scroll_to_point(*m.start());
                drop(t);
                self.finding = Some((what.to_owned(), Some(m)));
                self.shared.version.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            None => {
                drop(t);
                self.finding = Some((what.to_owned(), last));
                Err("not found".into())
            }
        }
    }

    /// Looking for is over: what it found is let go, and the screen is back at its end.
    pub fn find_end(&mut self) {
        self.finding = None;
        self.unselect();
        self.scroll_to_bottom();
    }

    /// What is selected, as text.
    pub fn selected(&self) -> Option<String> {
        self.term.lock().selection_to_string().filter(|s| !s.is_empty())
    }

    /// Pasted, as a program expects it: between the marks of a bracketed
    /// paste if it asked for them, its line ends as returns otherwise.
    pub fn paste(&self, text: &str) {
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let bytes = if bracketed {
            // (A paste that carries the closing mark would end the paste early.)
            format!("\x1b[200~{}\x1b[201~", text.replace("\x1b[201~", "")).into_bytes()
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
        };
        self.scroll_to_bottom();
        self.write(bytes);
    }

    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
        self.shared.version.fetch_add(1, Ordering::Relaxed);
    }

    pub fn scroll_to_bottom(&self) {
        let mut t = self.term.lock();
        if t.grid().display_offset() != 0 {
            t.scroll_display(Scroll::Bottom);
            self.shared.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The cell at a point of the screen as it is seen (with the history scrolled), and which half of it.
    fn point(&self, col: f32, row: f32) -> (Point, Side) {
        let t = self.term.lock();
        let offset = t.grid().display_offset() as i32;
        let cols = t.columns().max(1);
        let rows = t.screen_lines().max(1);
        let c = (col.floor().max(0.0) as usize).min(cols - 1);
        let r = (row.floor().max(0.0) as usize).min(rows - 1);
        let side = if col.fract() < 0.5 { Side::Left } else { Side::Right };
        (Point::new(Line(r as i32 - offset), Column(c)), side)
    }

    /// The mouse goes down on it: one click starts a selection, two take a word, three a line.
    pub fn select_start(&mut self, col: f32, row: f32, clicks: u8) {
        let (p, side) = self.point(col, row);
        let kind = match clicks {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.term.lock().selection = Some(Selection::new(kind, p, side));
        self.selecting = true;
        self.shared.version.fetch_add(1, Ordering::Relaxed);
    }

    pub fn select_to(&mut self, col: f32, row: f32) {
        let (p, side) = self.point(col, row);
        if let Some(s) = self.term.lock().selection.as_mut() {
            s.update(p, side);
        }
        self.shared.version.fetch_add(1, Ordering::Relaxed);
    }

    /// The mouse lets go: a click that did not drag leaves nothing selected.
    pub fn select_end(&mut self) {
        self.selecting = false;
        let mut t = self.term.lock();
        if t.selection.as_ref().is_some_and(|s| s.is_empty()) {
            t.selection = None;
        }
        self.shared.version.fetch_add(1, Ordering::Relaxed);
    }

    pub fn unselect(&self) {
        let mut t = self.term.lock();
        if t.selection.take().is_some() {
            self.shared.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    /// The mouse, told to a program that asked for it (vim, htop, less with the
    /// mouse on): a button (0 left, 1 middle, 2 right, 64/65 the wheel), pressed or let go, on a cell.
    pub fn report_mouse(&self, button: u8, down: bool, col: usize, row: usize, motion: bool) {
        let mode = self.mode();
        let code = button as u32 + if motion { 32 } else { 0 };
        let bytes = if mode.contains(TermMode::SGR_MOUSE) {
            format!("\x1b[<{};{};{}{}", code, col + 1, row + 1, if down { 'M' } else { 'm' }).into_bytes()
        } else {
            // The old form: one byte each, past 223 it cannot say where.
            if col > 222 || row > 222 {
                return;
            }
            let b = if down { code } else { 3 + if motion { 32 } else { 0 } };
            vec![0x1b, b'[', b'M', 32 + b as u8, 33 + col as u8, 33 + row as u8]
        };
        self.write(bytes);
    }
}

/// All of a scene's terminals, by name (`term`, or a copy's `term.3`), and
/// which one has the keys.
#[derive(Default)]
pub struct Registry {
    pub sessions: HashMap<&'static str, Session>,
    pub focus: Option<&'static str>,
    /// Whether the window has the keyboard at all: without it, no terminal does
    /// (its cursor is an outline, and its program is told so).
    pub window_keys: bool,
    /// The focus its programs were last told of (`\e[I`, `\e[O`).
    told_focus: Option<&'static str>,
    /// Those that started nothing, the scene having no permissions (said once).
    refused: Vec<&'static str>,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static WAKER: OnceLock<Mutex<Sender<ToRender>>> = OnceLock::new();

/// Where the emulator's threads wake the render when a screen changes.
pub fn set_waker(to_render: Sender<ToRender>) {
    let _ = WAKER.set(Mutex::new(to_render));
}

pub fn with<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut r = REGISTRY.get_or_init(|| Mutex::new(Registry::default())).lock().unwrap();
    f(&mut r)
}

/// What the screens of those drawn are now, to know whether the drawing has to
/// be made again. (One not drawn —a tab behind, writing on its own— is not
/// painted, so it does not count: when it is shown, something else changed.)
pub fn versions(drawn: &[PlacedTerminal]) -> Vec<(&'static str, u64)> {
    with(|r| {
        let mut v: Vec<(&'static str, u64)> = r.sessions.iter().filter(|(k, _)| drawn.iter().any(|t| t.name == **k)).map(|(k, s)| (*k, s.shared.version.load(Ordering::Relaxed))).collect();
        v.sort_unstable();
        v.push(("", r.focus.map_or(0, |f| f.as_ptr() as u64) ^ r.window_keys as u64));
        v
    })
}

/// A colour of the 256 a program can name by number, past the first sixteen:
/// a 6×6×6 cube and 24 greys.
pub fn indexed(i: u8, p: &[[u8; 3]; 18]) -> [u8; 3] {
    match i {
        0..=15 => p[i as usize + 2],
        16..=231 => {
            let k = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [step(k / 36), step((k / 6) % 6), step(k % 6)]
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            [g, g, g]
        }
    }
}

/// A cell's colour as the program named it, in the scene's colours.
pub fn resolve(c: TermColor, p: &[[u8; 3]; 18], set: &alacritty_terminal::term::color::Colors, bold: bool) -> [u8; 3] {
    let from_set = |k: usize| set[k].map(|r| [r.r, r.g, r.b]);
    match c {
        TermColor::Spec(Rgb { r, g, b }) => [r, g, b],
        TermColor::Indexed(i) => from_set(i as usize).unwrap_or_else(|| indexed(i, p)),
        TermColor::Named(n) => {
            let k = n as usize;
            if let Some(c) = from_set(k) {
                return c;
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => p[0],
                NamedColor::Background => p[1],
                NamedColor::Cursor => p[0],
                NamedColor::DimForeground => dim(p[0]),
                _ if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&k) => dim(p[2 + (k - NamedColor::DimBlack as usize)]),
                _ if k < 16 => {
                    // (Bold in one of the first eight is not made bright: the font says it.)
                    let _ = bold;
                    p[2 + k]
                }
                _ => p[0],
            }
        }
    }
}

pub fn dim(c: [u8; 3]) -> [u8; 3] {
    [(c[0] as f32 * 0.66) as u8, (c[1] as f32 * 0.66) as u8, (c[2] as f32 * 0.66) as u8]
}

/// A key as the bytes a program on a terminal expects: its name (an X keysym's,
/// as pleamar names keys), what it typed and the modifiers held. `None`: it is
/// not a terminal's key (a shortcut for the scene), or it writes nothing.
pub fn key_bytes(name: &str, typed: Option<&str>, ctrl: bool, alt: bool, shift: bool, mode: TermMode) -> Option<Vec<u8>> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    // The xterm number for the modifiers held besides the key: 1 none.
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let esc = |s: &str| Some(s.as_bytes().to_vec());
    let cursor = |c: char| {
        if m > 1 {
            Some(format!("\x1b[1;{m}{c}").into_bytes())
        } else if app_cursor {
            Some(format!("\x1bO{c}").into_bytes())
        } else {
            Some(format!("\x1b[{c}").into_bytes())
        }
    };
    let tilde = |n: u8| if m > 1 { Some(format!("\x1b[{n};{m}~").into_bytes()) } else { Some(format!("\x1b[{n}~").into_bytes()) };
    let fkey = |c: char| if m > 1 { Some(format!("\x1b[1;{m}{c}").into_bytes()) } else { Some(format!("\x1bO{c}").into_bytes()) };
    let alt_prefix = |mut b: Vec<u8>| {
        if alt {
            b.insert(0, 0x1b);
        }
        Some(b)
    };
    // A program that asked for the kitty keyboard protocol (Claude Code, Neovim, Helix…):
    // the keys that look the same in the old way (Ctrl+Enter is a plain Enter there)
    // come as `CSI code ; modifiers u`, and the rest as before.
    if mode.intersects(TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC) {
        if let Some(b) = kitty_bytes(name, typed, ctrl, alt, shift, m, mode) {
            return Some(b);
        }
    }
    match name {
        "Return" | "KP_Enter" => alt_prefix(vec![b'\r']),
        "BackSpace" => alt_prefix(vec![if ctrl { 0x08 } else { 0x7f }]),
        "Tab" if shift => esc("\x1b[Z"),
        "ISO_Left_Tab" => esc("\x1b[Z"),
        "Tab" => alt_prefix(vec![b'\t']),
        "Escape" => alt_prefix(vec![0x1b]),
        "Up" | "KP_Up" => cursor('A'),
        "Down" | "KP_Down" => cursor('B'),
        "Right" | "KP_Right" => cursor('C'),
        "Left" | "KP_Left" => cursor('D'),
        "Home" | "KP_Home" => cursor('H'),
        "End" | "KP_End" => cursor('F'),
        "Insert" | "KP_Insert" => tilde(2),
        "Delete" | "KP_Delete" => tilde(3),
        "Prior" | "Page_Up" | "KP_Prior" => tilde(5),
        "Next" | "Page_Down" | "KP_Next" => tilde(6),
        "F1" => fkey('P'),
        "F2" => fkey('Q'),
        "F3" => fkey('R'),
        "F4" => fkey('S'),
        "F5" => tilde(15),
        "F6" => tilde(17),
        "F7" => tilde(18),
        "F8" => tilde(19),
        "F9" => tilde(20),
        "F10" => tilde(21),
        "F11" => tilde(23),
        "F12" => tilde(24),
        _ => {
            // A letter or a sign: with Ctrl, its control character.
            let letter = typed.filter(|t| !t.is_empty()).map(str::to_owned).or_else(|| (name.chars().count() == 1).then(|| name.to_owned()));
            let t = letter?;
            if ctrl {
                let c = t.chars().next()?.to_ascii_lowercase();
                let b = match c {
                    'a'..='z' => c as u8 - b'a' + 1,
                    ' ' | '@' | '2' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '/' | '-' | '7' => 0x1f,
                    '8' | '?' => 0x7f,
                    // (Already a control character: what the system typed with Ctrl.)
                    c if (c as u32) < 0x20 => c as u8,
                    _ => return None,
                };
                return alt_prefix(vec![b]);
            }
            // (What the system typed with Ctrl held, without Ctrl's own meaning: nothing.)
            if t.chars().all(|c| c.is_control()) && name.chars().count() > 1 {
                return None;
            }
            alt_prefix(t.into_bytes())
        }
    }
}

/// A key in the kitty keyboard protocol, for the keys it changes; `None` leaves it
/// to the old way (arrows, Home, F-keys: already unambiguous).
fn kitty_bytes(name: &str, typed: Option<&str>, ctrl: bool, alt: bool, shift: bool, m: u8, mode: TermMode) -> Option<Vec<u8>> {
    let all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let csi = |code: String| {
        let tail = if m > 1 { format!(";{m}") } else { String::new() };
        Some(format!("\x1b[{code}{tail}u").into_bytes())
    };
    let special = match name {
        "Return" | "KP_Enter" => Some(13),
        "Tab" | "ISO_Left_Tab" => Some(9),
        "BackSpace" => Some(127),
        "Escape" => Some(27),
        _ => None,
    };
    if let Some(code) = special {
        // Escape always (that is what disambiguates); the others only when a
        // modifier makes them different, or when every key is asked for.
        return if code == 27 || m > 1 || all { csi(code.to_string()) } else { None };
    }
    // A letter or a sign. Without Ctrl or Alt it is text, as always —unless every
    // key is asked for as a code.
    if !(ctrl || alt || all) {
        return None;
    }
    let one = |s: &str| {
        let mut c = s.chars();
        c.next().filter(|_| c.next().is_none())
    };
    let shown = one(name).or_else(|| typed.and_then(one).filter(|c| !c.is_control()))?;
    let base = shown.to_lowercase().next().unwrap_or(shown);
    let mut code = (base as u32).to_string();
    if shift && base != shown && mode.contains(TermMode::REPORT_ALTERNATE_KEYS) {
        code.push_str(&format!(":{}", shown as u32));
    }
    csi(code)
}

// ── What the render does with them, each round ──────────────────────

use crate::gpu::PlacedTerminal;
use crate::scene::{Event, Scene};
use std::time::{Duration, Instant};

/// The render's own memory of its terminals: the one the mouse is selecting
/// in, its clicks (for two and three), and when the folders were last looked at.
pub struct Hands {
    pub drag: Option<&'static str>,
    clicks: (Instant, (f32, f32), u8),
    folders_at: Instant,
    /// Where each one's facts and texts are in the scene, found once.
    found: HashMap<String, Option<usize>>,
    /// The last place the mouse reported to a program was on (only the cell changing is news).
    reported: Option<(usize, usize)>,
}

impl Default for Hands {
    fn default() -> Self {
        Hands { drag: None, clicks: (Instant::now() - Duration::from_secs(9), (0.0, 0.0), 0), folders_at: Instant::now(), found: HashMap::new(), reported: None }
    }
}

/// The shells as the scene wants them: started where they are laid out and
/// should run, ended where `run:` stopped holding, each at its size.
pub fn sync(placed: &[PlacedTerminal], runs: &[(&'static str, bool)]) {
    with(|r| {
        for (name, run) in runs {
            if !run {
                if let Some(s) = r.sessions.remove(name) {
                    let _ = s.sender.send(Msg::Shutdown);
                }
                if r.focus == Some(*name) {
                    r.focus = None;
                }
            }
        }
        for t in placed {
            let size = WindowSize { num_cols: t.cols, num_lines: t.rows, cell_width: t.cell.0.round().max(1.0) as u16, cell_height: t.cell.1.round().max(1.0) as u16 };
            match r.sessions.get_mut(t.name) {
                Some(s) => s.resize(size),
                None if t.run && crate::permissions::none() => {
                    if !r.refused.contains(&t.name) {
                        r.refused.push(t.name);
                        println!("terminal · '{}' starts nothing: this scene runs with no permissions", t.name);
                    }
                }
                None if t.run && !runs.iter().any(|(n, run)| n == &t.name && !run) => {
                    let folder = t.folder.as_deref().filter(|f| !f.is_empty()).map(expand);
                    let env: Vec<(String, String)> = t.env.as_deref().unwrap_or("").lines().filter_map(|l| l.split_once('=')).filter(|(k, _)| !k.trim().is_empty()).map(|(k, v)| (k.trim().to_owned(), v.to_owned())).collect();
                    match Session::spawn(size, &t.command, folder, &env) {
                        Ok(s) => {
                            *s.shared.palette.lock().unwrap() = t.colors;
                            r.sessions.insert(t.name, s);
                        }
                        Err(e) => eprintln!("terminal · '{}' could not start: {e}", t.name),
                    }
                }
                None => {}
            }
        }
    });
}

/// What the scene's logic asks of one of its terminals (`sys.call("terminal.copy", "term")`):
/// `copy` what is selected, `paste` the clipboard, `write` a text as if typed
/// (a command and its return, say), `forget` its history, `select_all`; and
/// `find` a text up its history (again: the next one up), `find_newer` the next
/// one down, `find_end` to stop looking.
/// What its program put on the clipboard (OSC 52) at most `within` ago, if anything
/// (`sys.ask("terminal.copied", name, seconds)`).
pub fn copied(name: &str, within: Duration) -> Result<Option<String>, String> {
    with(|r| {
        let key = r.sessions.keys().find(|k| **k == name).copied().ok_or_else(|| format!("there is no terminal '{name}' running"))?;
        Ok(r.sessions.get(key).and_then(|s| s.shared.copied.lock().unwrap().clone()).filter(|(at, t)| at.elapsed() <= within && !t.is_empty()).map(|(_, t)| t))
    })
}

/// What is selected in that terminal, if anything (`sys.ask("terminal.selection", name)`).
pub fn selection(name: &str) -> Result<Option<String>, String> {
    with(|r| {
        let key = r.sessions.keys().find(|k| **k == name).copied().ok_or_else(|| format!("there is no terminal '{name}' running"))?;
        Ok(r.sessions.get(key).and_then(Session::selected))
    })
}

pub fn command(what: &str, name: &str, text: Option<&str>) -> Result<(), String> {
    with(|r| {
        let key = r.sessions.keys().find(|k| **k == name).copied().ok_or_else(|| format!("there is no terminal '{name}' running"))?;
        let s = r.sessions.get_mut(key).unwrap();
        match what {
            "copy" => match s.selected() {
                Some(t) => {
                    crate::platform::clipboard_write(&t);
                    Ok(())
                }
                None => Err("nothing is selected".into()),
            },
            "paste" => {
                let t = crate::platform::clipboard_read().ok_or("the clipboard has no text")?;
                s.paste(&t);
                Ok(())
            }
            "write" => {
                s.scroll_to_bottom();
                s.write(text.unwrap_or("").as_bytes().to_vec());
                Ok(())
            }
            "find" => s.find(text.unwrap_or(""), true),
            "find_newer" => s.find(text.unwrap_or(""), false),
            "find_end" => {
                s.find_end();
                Ok(())
            }
            "forget" => {
                s.term.lock().grid_mut().clear_history();
                s.shared.version.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            "select_all" => {
                let mut t = s.term.lock();
                let top = Point::new(Line(-(t.history_size() as i32)), Column(0));
                let bottom = Point::new(Line(t.screen_lines() as i32 - 1), Column(t.columns().saturating_sub(1)));
                let mut sel = Selection::new(SelectionType::Simple, top, Side::Left);
                sel.update(bottom, Side::Right);
                t.selection = Some(sel);
                s.shared.version.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            other => Err(format!("a terminal does not know '{other}': copy, paste, write, forget, select_all, find, find_newer, find_end")),
        }
    })
}

/// An address opened with what the system opens them with, loose from the scene.
fn open_link(link: &str) {
    if !(link.starts_with("http://") || link.starts_with("https://") || link.starts_with("file://") || link.starts_with("mailto:")) {
        return;
    }
    #[cfg(target_os = "linux")]
    let opened = std::process::Command::new("setsid").args(["-f", "xdg-open", link]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(link).status();
    #[cfg(target_os = "windows")]
    let opened = std::process::Command::new("cmd").args(["/C", "start", "", link]).status();
    if let Err(e) = opened {
        eprintln!("terminal · could not open {link}: {e}");
    }
}

fn expand(f: &str) -> std::path::PathBuf {
    match f.strip_prefix("~") {
        Some(rest) => std::path::PathBuf::from(format!("{}{rest}", std::env::var("HOME").unwrap_or_default())),
        None => std::path::PathBuf::from(f),
    }
}

/// What each one says, told to the scene where it declared it: `NAME.title`
/// and `NAME.folder` (texts), `NAME.alive`, `NAME.bell`, `NAME.focus`, `NAME.cols`, `NAME.rows` and
/// `NAME.pid`, its shell's process, and `NAME.back`, how many lines back through its history it is shown (facts).
pub fn report(scene: &Scene, facts: &mut [f32], texts: &mut [String], to_logic: &Sender<Event>, hands: &mut Hands) {
    let look_folders = hands.folders_at.elapsed() > Duration::from_millis(700);
    if look_folders {
        hands.folders_at = Instant::now();
    }
    let found = &mut hands.found;
    let mut fact = |name: String, v: f32, facts: &mut [f32]| {
        let k = *found.entry(name.clone()).or_insert_with(|| scene.facts.iter().position(|h| h.0 == name));
        if let Some(k) = k {
            if (facts[k] - v).abs() > f32::EPSILON {
                facts[k] = v;
                let _ = to_logic.send(Event::Fact(scene.facts[k].0, v));
            }
        }
    };
    let mut said: Vec<(String, String)> = Vec::new();
    with(|r| {
        let focus = r.focus;
        // A program that asked to know when it has the keys (vim, a TUI) is told.
        let now = if r.window_keys { focus } else { None };
        if now != r.told_focus {
            for (name, lost) in [(r.told_focus, true), (now, false)] {
                if let Some(s) = name.and_then(|n| r.sessions.get(n)) {
                    if s.mode().contains(TermMode::FOCUS_IN_OUT) {
                        s.write(if lost { b"\x1b[O".to_vec() } else { b"\x1b[I".to_vec() });
                    }
                }
            }
            r.told_focus = now;
        }
        for (name, s) in r.sessions.iter_mut() {
            let alive = s.alive();
            if s.told_alive != Some(alive) {
                s.told_alive = Some(alive);
                fact(format!("{name}.alive"), alive as u8 as f32, facts);
            }
            let bells = s.shared.bells.load(Ordering::Relaxed);
            if bells != s.told_bells {
                s.told_bells = bells;
                fact(format!("{name}.bell"), bells as f32, facts);
            }
            fact(format!("{name}.focus"), (focus == Some(*name)) as u8 as f32, facts);
            fact(format!("{name}.cols"), s.size.num_cols as f32, facts);
            fact(format!("{name}.rows"), s.size.num_lines as f32, facts);
            // (A pid fits a fact exactly: they stay under 2^22.)
            fact(format!("{name}.pid"), if alive { s.pid as f32 } else { 0.0 }, facts);
            // How many lines back through its history it is shown (0: at its end).
            let back = s.term.lock().grid().display_offset();
            fact(format!("{name}.back"), back as f32, facts);
            let title = s.shared.title.lock().unwrap().clone();
            if title.is_some() && title != s.told_title {
                s.told_title = title.clone();
                said.push((format!("{name}.title"), title.unwrap_or_default()));
            }
            if look_folders && alive {
                if let Some(f) = s.folder().filter(|f| *f != s.told_folder) {
                    s.told_folder = f.clone();
                    said.push((format!("{name}.folder"), f));
                }
            }
        }
    });
    for (name, value) in said {
        let k = *hands.found.entry(name.clone()).or_insert_with(|| scene.texts.iter().position(|t| t.0 == name));
        if let Some(k) = k {
            if texts[k] != value {
                texts[k] = value.clone();
                let _ = to_logic.send(Event::Text(scene.texts[k].0, value));
            }
        }
    }
}

/// A key, for the terminal that has them. `scenes`: the scene has a rule for
/// this combination, which then is the scene's (a new tab, closing one).
/// Whether it took it.
pub fn key(name: &str, typed: Option<&str>, ctrl: bool, alt: bool, shift: bool, logo: bool, scenes: bool, drawn: &[PlacedTerminal]) -> bool {
    let Some(focus) = with(|r| r.focus) else { return false };
    if scenes || logo || !drawn.iter().any(|t| t.name == focus) {
        return false;
    }
    with(|r| {
        let Some(s) = r.sessions.get(focus) else { return false };
        // A terminal's own: copy and paste with Shift (Ctrl+C is the program's), and the history.
        if ctrl && shift && (name == "C" || name == "c") {
            if let Some(t) = s.selected() {
                crate::platform::clipboard_write(&t);
            }
            return true;
        }
        if ctrl && shift && (name == "V" || name == "v") {
            if let Some(t) = crate::platform::clipboard_read() {
                s.paste(&t);
            }
            return true;
        }
        if shift && !ctrl && !alt {
            let page = s.size.num_lines.saturating_sub(2).max(1) as i32;
            match name {
                "Prior" | "Page_Up" => {
                    s.scroll(page);
                    return true;
                }
                "Next" | "Page_Down" => {
                    s.scroll(-page);
                    return true;
                }
                _ => {}
            }
        }
        if !s.alive() {
            return true;
        }
        match key_bytes(name, typed, ctrl, alt, shift, s.mode()) {
            Some(b) => {
                s.scroll_to_bottom();
                s.unselect();
                s.write(b);
                true
            }
            // A modifier alone: nothing to send, but still its.
            None => name.starts_with("Shift") || name.starts_with("Control") || name.starts_with("Alt") || name.starts_with("ISO_Level3"),
        }
    })
}

fn cell_at(t: &PlacedTerminal, p: (f32, f32)) -> (f32, f32) {
    (((p.0 - t.origin.0) / t.cell.0).clamp(0.0, t.cols as f32 - 0.01), ((p.1 - t.origin.1) / t.cell.1).clamp(0.0, t.rows as f32 - 0.01))
}

/// A button goes down on a terminal: it takes the keys; then the left one
/// selects (two clicks a word, three a line), the middle one pastes, and a
/// program that asked for the mouse gets it instead (unless Shift is held).
pub fn press(t: &PlacedTerminal, p: (f32, f32), button: u8, shift: bool, ctrl: bool, hands: &mut Hands) {
    let now = Instant::now();
    with(|r| {
        r.focus = Some(t.name);
        let Some(s) = r.sessions.get_mut(t.name) else { return };
        let (col, row) = cell_at(t, p);
        let mode = s.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !shift {
            s.report_mouse(button, true, col as usize, row as usize, false);
            hands.reported = Some((col as usize, row as usize));
            hands.drag = Some(t.name);
            return;
        }
        match button {
            0 if ctrl => {
                if let Some(link) = s.link_at(col, row) {
                    open_link(&link);
                }
            }
            0 => {
                let again = now.duration_since(hands.clicks.0) < Duration::from_millis(450) && (p.0 - hands.clicks.1 .0).hypot(p.1 - hands.clicks.1 .1) < 5.0;
                let n = if again { hands.clicks.2 % 3 + 1 } else { 1 };
                hands.clicks = (now, p, n);
                s.select_start(col, row, n);
                hands.drag = Some(t.name);
            }
            1 => {
                if let Some(text) = crate::platform::clipboard_read() {
                    s.paste(&text);
                }
            }
            _ => {}
        }
    });
}

/// The mouse moves with a button held, from a press on a terminal.
pub fn motion(placed: &[PlacedTerminal], p: (f32, f32), hands: &mut Hands) {
    let Some(name) = hands.drag else { return };
    let Some(t) = placed.iter().find(|t| t.name == name) else { return };
    let (col, row) = cell_at(t, p);
    with(|r| {
        let Some(s) = r.sessions.get_mut(name) else { return };
        let mode = s.mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            let cell = (col as usize, row as usize);
            if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) && hands.reported != Some(cell) {
                hands.reported = Some(cell);
                s.report_mouse(0, true, cell.0, cell.1, true);
            }
        } else if s.selecting {
            s.select_to(col, row);
        }
    });
}

/// The left button lets go.
pub fn release(placed: &[PlacedTerminal], p: Option<(f32, f32)>, hands: &mut Hands) {
    let Some(name) = hands.drag.take() else { return };
    with(|r| {
        let Some(s) = r.sessions.get_mut(name) else { return };
        if s.mode().intersects(TermMode::MOUSE_MODE) {
            let cell = placed.iter().find(|t| t.name == name).zip(p).map(|(t, p)| cell_at(t, p)).map_or(hands.reported.unwrap_or((0, 0)), |(c, r)| (c as usize, r as usize));
            s.report_mouse(0, false, cell.0, cell.1, false);
        } else {
            s.select_end();
        }
    });
}

/// The wheel over a terminal: its history, or the program's if it asked for
/// the mouse, or arrows for one on the other screen (less, man, a pager). `notches` down is positive.
pub fn wheel(t: &PlacedTerminal, p: (f32, f32), notches: f32) {
    with(|r| {
        let Some(s) = r.sessions.get(t.name) else { return };
        let mode = s.mode();
        let n = (notches.abs().ceil() as usize).max(1);
        // The render's wheel is positive upwards (as the platform turns it): negative is down.
        let down = notches < 0.0;
        if mode.intersects(TermMode::MOUSE_MODE) {
            let (col, row) = cell_at(t, p);
            for _ in 0..n {
                s.report_mouse(if down { 65 } else { 64 }, true, col as usize, row as usize, false);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let arrow: &[u8] = match (down, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOB",
                (true, false) => b"\x1b[B",
                (false, true) => b"\x1bOA",
                (false, false) => b"\x1b[A",
            };
            s.write(arrow.repeat(3 * n));
        } else {
            s.scroll(if down { -3 * n as i32 } else { 3 * n as i32 });
        }
    });
}

/// How a box-drawing or block character is drawn by hand, to meet its
/// neighbours across the cell whatever the font and the line height: its four
/// arms (left, right, up, down: 0 none, 1 light, 2 heavy, 3 double), a rounded
/// corner, or a block (a part of the cell, and how much of the ink it takes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drawn {
    Arms([u8; 4]),
    /// Rounded: the two arms it joins (left or right; up or down).
    Round { right: bool, down: bool },
    /// A part of the cell (x, y, w, h as fractions of it) in the ink at that much.
    Block([f32; 4], f32),
}

pub fn drawn(c: char) -> Option<Drawn> {
    use Drawn::*;
    let k = c as u32;
    let arms = |l: u8, r: u8, u: u8, d: u8| Some(Arms([l, r, u, d]));
    match k {
        0x2500 | 0x2504 | 0x2508 | 0x254C => arms(1, 1, 0, 0),
        0x2501 | 0x2505 | 0x2509 | 0x254D => arms(2, 2, 0, 0),
        0x2502 | 0x2506 | 0x250A | 0x254E => arms(0, 0, 1, 1),
        0x2503 | 0x2507 | 0x250B | 0x254F => arms(0, 0, 2, 2),
        0x250C => arms(0, 1, 0, 1),
        0x250D => arms(0, 2, 0, 1),
        0x250E => arms(0, 1, 0, 2),
        0x250F => arms(0, 2, 0, 2),
        0x2510 => arms(1, 0, 0, 1),
        0x2511 => arms(2, 0, 0, 1),
        0x2512 => arms(1, 0, 0, 2),
        0x2513 => arms(2, 0, 0, 2),
        0x2514 => arms(0, 1, 1, 0),
        0x2515 => arms(0, 2, 1, 0),
        0x2516 => arms(0, 1, 2, 0),
        0x2517 => arms(0, 2, 2, 0),
        0x2518 => arms(1, 0, 1, 0),
        0x2519 => arms(2, 0, 1, 0),
        0x251A => arms(1, 0, 2, 0),
        0x251B => arms(2, 0, 2, 0),
        0x251C..=0x2522 => arms(0, if k == 0x251D || k == 0x2522 { 2 } else { 1 }, 1, 1),
        0x2523 => arms(0, 2, 2, 2),
        0x2524..=0x252A => arms(if k == 0x2525 || k == 0x252A { 2 } else { 1 }, 0, 1, 1),
        0x252B => arms(2, 0, 2, 2),
        0x252C..=0x2532 => arms(1, 1, 0, if k == 0x2530 || k == 0x2532 { 2 } else { 1 }),
        0x2533 => arms(2, 2, 0, 2),
        0x2534..=0x253A => arms(1, 1, if k == 0x2538 || k == 0x253A { 2 } else { 1 }, 0),
        0x253B => arms(2, 2, 2, 0),
        0x253C..=0x254A => arms(1, 1, 1, 1),
        0x254B => arms(2, 2, 2, 2),
        0x2550 => arms(3, 3, 0, 0),
        0x2551 => arms(0, 0, 3, 3),
        0x2552..=0x2554 => arms(0, 3, 0, 3),
        0x2555..=0x2557 => arms(3, 0, 0, 3),
        0x2558..=0x255A => arms(0, 3, 3, 0),
        0x255B..=0x255D => arms(3, 0, 3, 0),
        0x255E..=0x2560 => arms(0, 3, 3, 3),
        0x2561..=0x2563 => arms(3, 0, 3, 3),
        0x2564..=0x2566 => arms(3, 3, 0, 3),
        0x2567..=0x2569 => arms(3, 3, 3, 0),
        0x256A..=0x256C => arms(3, 3, 3, 3),
        0x256D => Some(Round { right: true, down: true }),
        0x256E => Some(Round { right: false, down: true }),
        0x256F => Some(Round { right: false, down: false }),
        0x2570 => Some(Round { right: true, down: false }),
        0x2574 => arms(1, 0, 0, 0),
        0x2575 => arms(0, 0, 1, 0),
        0x2576 => arms(0, 1, 0, 0),
        0x2577 => arms(0, 0, 0, 1),
        0x2578 => arms(2, 0, 0, 0),
        0x2579 => arms(0, 0, 2, 0),
        0x257A => arms(0, 2, 0, 0),
        0x257B => arms(0, 0, 0, 2),
        0x257C => arms(1, 2, 0, 0),
        0x257D => arms(0, 0, 1, 2),
        0x257E => arms(2, 1, 0, 0),
        0x257F => arms(0, 0, 2, 1),
        // Blocks: halves, eighths and the three shades.
        0x2580 => Some(Block([0.0, 0.0, 1.0, 0.5], 1.0)),
        0x2581..=0x2587 => {
            let f = (k - 0x2580) as f32 / 8.0;
            Some(Block([0.0, 1.0 - f, 1.0, f], 1.0))
        }
        0x2588 => Some(Block([0.0, 0.0, 1.0, 1.0], 1.0)),
        0x2589..=0x258F => {
            let f = (0x2590 - k) as f32 / 8.0;
            Some(Block([0.0, 0.0, f, 1.0], 1.0))
        }
        0x2590 => Some(Block([0.5, 0.0, 0.5, 1.0], 1.0)),
        0x2591 => Some(Block([0.0, 0.0, 1.0, 1.0], 0.25)),
        0x2592 => Some(Block([0.0, 0.0, 1.0, 1.0], 0.5)),
        0x2593 => Some(Block([0.0, 0.0, 1.0, 1.0], 0.75)),
        0x2594 => Some(Block([0.0, 0.0, 1.0, 0.125], 1.0)),
        0x2595 => Some(Block([0.875, 0.0, 0.125, 1.0], 1.0)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_become_what_a_terminal_sends() {
        let none = TermMode::empty();
        assert_eq!(key_bytes("Return", None, false, false, false, none), Some(b"\r".to_vec()));
        assert_eq!(key_bytes("c", Some("c"), true, false, false, none), Some(vec![3]));
        assert_eq!(key_bytes("Up", None, false, false, false, none), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes("Up", None, false, false, false, TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes("Right", None, true, false, false, none), Some(b"\x1b[1;5C".to_vec()));
        assert_eq!(key_bytes("Page_Up", None, false, false, true, none), Some(b"\x1b[5;2~".to_vec()));
        assert_eq!(key_bytes("b", Some("b"), false, true, false, none), Some(b"\x1bb".to_vec()));
        assert_eq!(key_bytes("ntilde", Some("ñ"), false, false, false, none), Some("ñ".as_bytes().to_vec()));
        let kitty = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALTERNATE_KEYS;
        assert_eq!(key_bytes("Return", None, true, false, false, kitty), Some(b"\x1b[13;5u".to_vec()));
        assert_eq!(key_bytes("Return", None, false, false, true, kitty), Some(b"\x1b[13;2u".to_vec()));
        assert_eq!(key_bytes("Return", None, false, false, false, kitty), Some(b"\r".to_vec()));
        assert_eq!(key_bytes("Escape", None, false, false, false, kitty), Some(b"\x1b[27u".to_vec()));
        assert_eq!(key_bytes("c", Some("\u{3}"), true, false, false, kitty), Some(b"\x1b[99;5u".to_vec()));
        assert_eq!(key_bytes("D", Some("D"), false, false, true, kitty), Some(b"D".to_vec()));
        assert_eq!(key_bytes("Up", None, false, false, false, kitty), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes("F5", None, false, false, false, none), Some(b"\x1b[15~".to_vec()));
        assert_eq!(key_bytes("BackSpace", None, false, false, false, none), Some(vec![0x7f]));
        assert_eq!(key_bytes("Shift_L", None, false, false, true, none), None);
    }

    #[test]
    fn box_lines_and_blocks_are_drawn_by_hand() {
        assert_eq!(drawn('─'), Some(Drawn::Arms([1, 1, 0, 0])));
        assert_eq!(drawn('│'), Some(Drawn::Arms([0, 0, 1, 1])));
        assert_eq!(drawn('┼'), Some(Drawn::Arms([1, 1, 1, 1])));
        assert_eq!(drawn('┏'), Some(Drawn::Arms([0, 2, 0, 2])));
        assert_eq!(drawn('╬'), Some(Drawn::Arms([3, 3, 3, 3])));
        assert_eq!(drawn('╭'), Some(Drawn::Round { right: true, down: true }));
        assert_eq!(drawn('╯'), Some(Drawn::Round { right: false, down: false }));
        assert_eq!(drawn('█'), Some(Drawn::Block([0.0, 0.0, 1.0, 1.0], 1.0)));
        assert_eq!(drawn('▄'), Some(Drawn::Block([0.0, 0.5, 1.0, 0.5], 1.0)));
        assert_eq!(drawn('▒'), Some(Drawn::Block([0.0, 0.0, 1.0, 1.0], 0.5)));
        assert_eq!(drawn('a'), None);
        assert_eq!(drawn('╱'), None);
    }

    #[test]
    fn the_256_colours_are_xterms() {
        let p = DEFAULT_COLORS;
        assert_eq!(indexed(16, &p), [0, 0, 0]);
        assert_eq!(indexed(231, &p), [255, 255, 255]);
        assert_eq!(indexed(196, &p), [255, 0, 0]);
        assert_eq!(indexed(232, &p), [8, 8, 8]);
        assert_eq!(indexed(1, &p), p[3]);
    }
}
