//! The boundary with the system. Everything that knows about Wayland —and, the
//! day it's needed, about Win32 or AppKit— lives down here; the rest of the
//! program only knows this.
//!
//! A platform has to know how to do three things:
//!  · put up the surfaces a scene asks for (`Surface`: logical size,
//!    anchor, level, exclusive zone, screens) and hand them to the render as
//!    sheets, also when a monitor arrives or leaves, or changes scale;
//!  · tell the render where the mouse is and when it clicks;
//!  · tell the system where the mouse gets in (`PlatformWindow::update_input_region`),
//!    so that what's transparent lets the click through.
//!
//! And two that are not about windows: finding an icon by its name, and the
//! **services** —what happens in the system: workspaces, active window…—, which
//! the logic asks for by a name that is the same on every system. What a
//! system doesn't have, it says it doesn't have, and the scene decides what to do without it.

use crate::scene::{Surface, ToRender};
#[cfg(not(target_os = "linux"))]
use std::sync::mpsc::Sender;

/// A piece of system data, shaped like JSON: it's what a service tells
/// the logic, which receives it as a table.
#[derive(Clone, Debug)]
pub enum SysValue {
    Null,
    Bool(bool),
    Num(f64),
    Text(String),
    List(Vec<SysValue>),
    Map(Vec<(String, SysValue)>),
}

/// Starts listening to a service. `notify` is called with the current state and
/// then every time it changes. Returns whether this system has it.
///
/// The names are the same everywhere:
///  · `workspaces` → `{ active = 3, list = { { id, name, windows, monitor }, … } }`
///  · `window`     → `{ title, class }`
///  · `apps`       → `{ { name, exec, icon, …, terminal, actions }, … }`, once (see `desktop`)
///  · `audio`      → `{ volume = 0.54, muted = false }`
///  · `audio.level` → `{ level = 0.31 }`: how loud what plays is, up to 30 times a second (see `pulse`)
///  · `battery`    → `{ present, percent, charging }`
///  · `network`    → `{ online, kind = "wired" | "wifi" | "none", name, strength, wifi, networks }` (see `networkmanager`)
///  · `bluetooth`  → `{ present, powered, discovering, devices, pairing, pairing_id, … }` (see `bluez`)
///  · `media`      → `{ playing, title, artist, album, length, position, rate, can_seek, art, player, players }`, or `{ player = "" }` if nothing is playing
///  · `clock`      → `{ hour, minute, second, day, month, year, weekday, time, date }`, when the minute changes
///  · `clock.seconds` → the same, every second
///  · `files:x.json` → the text of that file when it changes
///
/// And two that can only be asked, with `sys.ask`: `env` (an environment variable) and
/// `clipboard` (whatever has been copied); `clipboard.set` writes to it.
/// Who is told what a service says, and the last thing it said.
struct Hub {
    listener: Box<dyn Fn(SysValue) + Send>,
    last: Option<SysValue>,
}

/// The services already running, by who asked and which: one thread each,
/// for as long as the process lives. A logic that is read again (its file
/// saved while it runs) asks for the same ones again; it is handed the
/// running one, and told at once what it last said. Started anew each time,
/// every reload left the old logic's threads behind, still watching for
/// nobody (the notifications and the tray were the first to show it).
/// `tag` says which of its subscriptions it is (the scene's `service audio as
/// sound` and the logic's `sys.watch("audio")` are two, each told apart).
static HUBS: std::sync::Mutex<Vec<((String, String, String), std::sync::Arc<std::sync::Mutex<Hub>>)>> = std::sync::Mutex::new(Vec::new());

pub fn service(from: &str, name: &str, tag: &str, notify: Box<dyn Fn(SysValue) + Send>) -> bool {
    // (Read once and done, in a thread that ends: nothing is left behind.)
    if name == "apps" {
        return start_service(from, name, notify);
    }
    let key = (from.to_owned(), name.to_owned(), tag.to_owned());
    let mut hubs = HUBS.lock().unwrap();
    if let Some((_, hub)) = hubs.iter().find(|(k, _)| *k == key) {
        let mut h = hub.lock().unwrap();
        if let Some(v) = h.last.clone() {
            notify(v);
        }
        h.listener = notify;
        return true;
    }
    let hub = std::sync::Arc::new(std::sync::Mutex::new(Hub { listener: notify, last: None }));
    let relay = hub.clone();
    let started = start_service(
        from,
        name,
        Box::new(move |v| {
            let mut h = relay.lock().unwrap();
            h.last = Some(v.clone());
            (h.listener)(v);
        }),
    );
    if started {
        hubs.push((key, hub));
    }
    started
}

fn start_service(from: &str, name: &str, notify: Box<dyn Fn(SysValue) + Send>) -> bool {
    // The time, without calling anyone.
    if name == "clock" || name == "clock.seconds" {
        return clock::service(name, notify);
    }
    // `files:settings.json`: notifies when that file changes, also if someone else touches it.
    if let Some(which) = name.strip_prefix("files:") {
        return files::watch(from, which, notify);
    }
    #[cfg(target_os = "linux")]
    if name == "apps" {
        // Reading hundreds of files is not a matter of an instant: in its own thread.
        return std::thread::Builder::new().name("apps".into()).spawn(move || notify(desktop::applications())).is_ok();
    }
    #[cfg(target_os = "linux")]
    match name {
        // Spoken to directly; through `wpctl` only if there is no server to speak to.
        "audio" if pulse::available() => return pulse::service(notify),
        "audio" => return system::audio(notify),
        // The signal, not the volume: only with a sound server, which measures it.
        "audio.level" if pulse::available() => return pulse::level_service(notify),
        "battery" => return system::battery(notify),
        "brightness" => return system::brightness(notify),
        // NetworkManager, which can be asked things too; without it iwd, which
        // can as well; with neither, the kernel, which can only be read.
        "network" if networkmanager::available() => return networkmanager::service(notify),
        "network" if iwd::available() => return iwd::service(notify),
        "network" => return system::network(notify),
        "bluetooth" if bluez::available() => return bluez::service(notify),
        "media" => return mpris::service(notify),
        "notifications" => return notifications::service(notify),
        "polkit" => return polkit::service(notify),
        "notification_history" => return notifications::history(notify),
        "tray" => return tray::service(notify),
        "thumbnails" => return thumbnails::service(notify),
        "gamepad" => return gamepad::service(notify),
        _ => {}
    }
    // `PLEAMAR_GENERIC=1` skips the Hyprland path: it's how to test here what
    // the other compositors will see.
    #[cfg(target_os = "linux")]
    if hyprland::is_present() && std::env::var_os("PLEAMAR_GENERIC").is_none() {
        return hyprland::service(name, notify);
    }
    // Any other Wayland: the protocols everyone understands.
    #[cfg(target_os = "linux")]
    if matches!(name, "window" | "workspaces") {
        return compositor::service(name, notify);
    }
    let _ = (name, notify);
    false
}

/// Whether `from`'s scene shows what Bluetooth pairing asks, and answers it:
/// only then does a pairing ask anyone (see `bluez`).
pub fn bluetooth_pairing(from: &str, handles: bool) {
    #[cfg(target_os = "linux")]
    bluez::handles(from, handles);
    #[cfg(not(target_os = "linux"))]
    let _ = (from, handles);
}

/// Ask a service to do something: `workspaces.focus`, 3.
pub fn command(from: &str, name: &str, args: &[SysValue]) -> Result<(), String> {
    if name.starts_with("files.") {
        return files::command(from, name, args);
    }
    if name == "say" {
        return match args {
            [SysValue::Text(scene), SysValue::Text(line)] => say(scene, line),
            _ => Err("`say` takes the scene and one command: sys.call(\"say\", \"bar\", \"emit card_landed 3\")".into()),
        };
    }
    if let ("clipboard.set", [SysValue::Text(t)]) = (name, args) {
        clipboard_write(t);
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if let ("apps.launch", [SysValue::Text(o)]) = (name, args) {
        return desktop::launch(o);
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("brightness.") {
        return system::brightness_command(name, args);
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("audio.") {
        return if pulse::available() { pulse::command(name, args) } else { system::audio_command(name, args) };
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("bluetooth.") {
        return bluez::command(name, args);
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("polkit.") {
        return polkit::command(name, args);
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("network.") {
        return if !networkmanager::available() && iwd::available() { iwd::command(name, args) } else { networkmanager::command(name, args) };
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("session.") {
        return system::session_command(name, args);
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("tray.") {
        return tray::command(name, args);
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("notifications.") {
        return notifications::command(name, args);
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("media.") {
        return mpris::command(name, args);
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("thumbnails.") {
        return thumbnails::command(name, args);
    }
    #[cfg(target_os = "linux")]
    if hyprland::is_present() && std::env::var_os("PLEAMAR_GENERIC").is_none() {
        return hyprland::command(name, args);
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("workspaces.") || name.starts_with("window.") {
        return compositor::command(name, args);
    }
    let _ = args;
    Err(format!("this system cannot do '{name}' yet"))
}

/// Where pleamar keeps what it has to remember from one run to the next —which
/// plugins have been approved—: each system's settings folder.
pub fn config_dir() -> std::path::PathBuf {
    let from = |v: &str| std::env::var_os(v).filter(|x| !x.is_empty()).map(std::path::PathBuf::from);
    let base = if cfg!(target_os = "windows") {
        from("APPDATA")
    } else if cfg!(target_os = "macos") {
        from("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        from("XDG_CONFIG_HOME").or_else(|| from("HOME").map(|h| h.join(".config")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("pleamar")
}

/// Ask a service something and wait for the answer: `tray.menu`, of whom.
/// It can take a while —there's another application on the other side—, and
/// that's why it's up to the logic, which can wait without it showing.
pub fn query(from: &str, name: &str, args: &[SysValue]) -> Result<SysValue, String> {
    if name.starts_with("files.") {
        return files::query(from, name, args);
    }
    match (name, args) {
        // The environment: what the session told this program at startup.
        ("env", [SysValue::Text(which)]) => {
            return Ok(std::env::var_os(which).map_or(SysValue::Null, |v| SysValue::Text(v.to_string_lossy().into_owned())));
        }
        ("env", _) => return Err("`env` takes the name of one variable: sys.ask(\"env\", \"HOME\")".into()),
        ("clipboard", []) => return Ok(clipboard_read().map_or(SysValue::Null, SysValue::Text)),
        // Is that the password of whoever owns the session? What a lock screen
        // needs in order to open. It is slow on purpose when it isn't.
        #[cfg(target_os = "linux")]
        ("auth.check", [SysValue::Text(password)]) => return auth::verify(password).map(SysValue::Bool),
        ("auth.check", _) => return Err("`auth.check` takes the password, and answers true or false: sys.ask(\"auth.check\", text.password)".into()),
        _ => {}
    }
    #[cfg(target_os = "linux")]
    if name.starts_with("tray.") {
        return tray::query(name, args);
    }
    // What is on a monitor: a pixel's colour, or the whole of it kept.
    #[cfg(target_os = "linux")]
    if name.starts_with("capture.") {
        return capture::query(name, args);
    }
    let _ = args;
    Err(format!("this system cannot answer '{name}' yet"))
}

/// Opens or closes the scene's popup number `k`: a child surface of the
/// main one, at `[x, y, width, height]` inside it, that shows the scene
/// from `origin`. The sheet reaches the render like the others; if the system
/// closes it (someone clicked outside), it reports with `ToRender::PopupClosed`.
///
/// On Wayland, an `xdg_popup`. On Windows it will be a frameless window with
/// `WS_EX_NOACTIVATE`; on macOS, an `NSPanel`.
pub fn popup(k: usize, what: Option<([i32; 4], (f32, f32))>) {
    #[cfg(target_os = "linux")]
    wayland::popup(k, what);
    let _ = (k, what);
}

/// Engages or releases the session lock for surface `which`, which is
/// `kind: lock`: `Some((bounds, origin))` engages it —one surface per monitor, with
/// those bounds centred on each— and `None` releases it. Whether it is REALLY
/// engaged is said by the system, with `ToRender::LockScreen(true)`.
///
/// On Wayland it's `ext-session-lock`: the compositor guarantees it, not the drawing.
/// On Windows it will be `LockWorkStation`, which brings its own screen; on macOS, the
/// screen saver with password.
pub fn lock_screen(which: usize, what: Option<((u32, u32), (f32, f32), crate::scene::Screens)>) {
    #[cfg(target_os = "linux")]
    wayland::lock_screen(which, what);
    #[cfg(not(target_os = "linux"))]
    let _ = (which, what);
}

/// The render has let go of the sheet of surface `id`, and with it of its
/// swapchain: what the platform kept alive for it until then can go.
pub fn sheet_released(id: u32) {
    #[cfg(target_os = "linux")]
    wayland::sheet_released(id);
    #[cfg(not(target_os = "linux"))]
    let _ = id;
}

/// Sticks surface `which` to another edge, while running. On Wayland it's a
/// layer-shell request —`set_anchor` and `set_margin` work on a live
/// surface, without creating it again—; on Windows it will be moving the window and
/// repositioning its AppBar, and on macOS moving the `NSPanel`.
/// A surface changes level while running: `level: top, overlay while open`.
pub fn relayer(which: usize, level: crate::scene::Level) {
    if let Some(h) = LAYER_HOOKS.get() {
        return (h.relayer)(which, level);
    }
    #[cfg(target_os = "linux")]
    wayland::relayer(which, level);
    let _ = (which, level);
}

/// A surface keeps more or less room for itself while running: `reserve: n while …`.
/// Something dragged out of the scene (a zone with `carries:`), from the
/// press that began it: whether a drag could start.
///
/// `icon`: the path of a PNG that goes under the pointer while it lasts (`carry_icon:`).
pub fn start_drag(text: &str, icon: Option<&str>) -> bool {
    #[cfg(target_os = "linux")]
    if LAYER_HOOKS.get().is_none() {
        return wayland::start_drag(text, icon);
    }
    let _ = (text, icon);
    false
}

/// What a text dragged out offers, kind by kind. Addresses —a file's path, a
/// `file://`, a link— go as `text/uri-list` (one per line), and as text too.
pub fn drag_offers(text: &str) -> Vec<(String, Vec<u8>)> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let is_address = |l: &str| l.starts_with('/') || l.contains("://");
    let plain = text.trim().as_bytes().to_vec();
    let mut offers = Vec::new();
    if lines.iter().all(|l| is_address(l)) {
        let uris: String = lines.iter().map(|l| if l.starts_with('/') { format!("file://{}\r\n", encode_path(l)) } else { format!("{l}\r\n") }).collect();
        offers.push(("text/uri-list".to_owned(), uris.into_bytes()));
    }
    for kind in ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"] {
        offers.push((kind.to_owned(), plain.clone()));
    }
    offers
}

/// A path as an address: what is not a plain letter, digit or `/-._~` goes as `%XX`.
fn encode_path(path: &str) -> String {
    path.bytes().map(|b| if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

pub fn rezone(which: usize, zone: i32) {
    // A platform that places surfaces itself (pleamar-wm's session) keeps none.
    if LAYER_HOOKS.get().is_some() {
        return;
    }
    #[cfg(target_os = "linux")]
    wayland::rezone(which, zone);
    let _ = (which, zone);
}

/// How many layers have been put on a monitor so far. A monitor that comes back
/// (after a deep sleep) gets its layers new, with the scene's defaults: when this
/// number moves, the render sends again what each surface has changed (edge, level,
/// room), or the new ones would sit where the scene began.
pub fn layers_placed() -> u64 {
    #[cfg(target_os = "linux")]
    return wayland::layers_placed();
    #[cfg(not(target_os = "linux"))]
    0
}

/// A surface its scene was saved with at another size, edge, margin, room or
/// level, while running: changed in place. A platform that places surfaces
/// itself (pleamar-wm's own scene) is not asked.
pub fn reshape(which: usize, surface: &crate::scene::Surface) {
    if LAYER_HOOKS.get().is_some() {
        return;
    }
    #[cfg(target_os = "linux")]
    wayland::reshape(which, surface);
    let _ = (which, surface);
}

pub fn reanchor(which: usize, anchor: crate::scene::SurfaceAnchor) {
    if let Some(h) = LAYER_HOOKS.get() {
        return (h.reanchor)(which, anchor);
    }
    #[cfg(target_os = "linux")]
    wayland::reanchor(which, anchor);
    let _ = (which, anchor);
}

/// What a platform handed over does when a surface changes level or edge
/// while running (`level: top, overlay while …`, an anchor from a fact).
pub struct LayerHooks {
    pub relayer: Box<dyn Fn(usize, crate::scene::Level) + Send + Sync>,
    pub reanchor: Box<dyn Fn(usize, crate::scene::SurfaceAnchor) + Send + Sync>,
}

static LAYER_HOOKS: std::sync::OnceLock<LayerHooks> = std::sync::OnceLock::new();

pub fn provide_layer_hooks(h: LayerHooks) {
    let _ = LAYER_HOOKS.set(h);
}

/// That a process we launch does not outlive us, not even if we're killed
/// forcibly. On Linux we ask the kernel; on Windows it will be a Job Object.
pub fn die_with_parent(command: &mut std::process::Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // It only calls `prctl`, which is one of those that can be used between `fork` and `exec`.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    let _ = command;
}

/// A scene whose input is not a terminal (started by a session, its input
/// /dev/null) still has the one it was started from as its controlling
/// terminal, and so does every program it starts: one that asks something there
/// (`sudo` wanting a password) is stopped by the kernel for good, and whatever
/// waits for it with it. Let go of it: such a program finds no terminal and
/// fails at once. Not a session's leader (it would hang up its terminal), nor
/// a scene typed into a terminal (Ctrl+C there still reaches it).
pub fn leave_terminal() {
    #[cfg(target_os = "linux")]
    unsafe {
        if libc::isatty(0) == 1 || libc::getsid(0) == libc::getpid() {
            return;
        }
        let fd = libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if fd >= 0 {
            libc::ioctl(fd, libc::TIOCNOTTY);
            libc::close(fd);
        }
    }
}

/// A process group of its own: a program that reads the terminal the scene may
/// have (`sudo` wanting a password) is stopped by the kernel alone, not with
/// the whole group it would share — the scene too. (A scene that does not use
/// its terminal lets go of it at the start, `leave_terminal`: there such a
/// program finds none, and fails at once.)
pub fn apart(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A process group of its own, not a session: `pre_exec` (what `setsid` needs) makes Rust fork the whole scene instead of
        // `posix_spawn`ing, and forking a scene that holds a gigabyte stalls every thread in it, the painting one too.
        command.process_group(0);
    }
    let _ = command;
}

/// What answers each line said to the scene. It returns the answer, if any;
/// one that goes on talking (`watch`) writes its lines with `out`, which says
/// `false` once whoever asked has gone. An empty line is not written: it only
/// asks whether they are still there, for one with nothing to say for a while.
pub type Commands = std::sync::Arc<dyn Fn(String, &mut dyn FnMut(&str) -> bool) -> Option<String> + Send + Sync>;

/// Commands from outside: a global compositor shortcut, a script, another
/// application. One line of text per command, through a socket named after the
/// scene. `pleamar --say "emit toggle"` is the other end.
///
/// On Linux and macOS, a Unix socket. On Windows it will be a named pipe.
#[cfg(unix)]
pub fn listen_for_commands(scene: &str, receive: Commands) {
    use std::io::{BufRead, BufReader, Write};
    let Some(mut path) = command_socket_path(scene) else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    // If there's already ANOTHER scene with this name listening, the socket is its.
    // It used to be deleted without looking: the newcomer took it, and when it left
    // it left the first one deaf forever —a ten-second trial run took the
    // keyboard shortcuts away from the everyday bar—. Only the socket of
    // someone no longer there is deleted; if they are there, this one answers to its name and its pid.
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        let own = format!("{scene}-{}", std::process::id());
        eprintln!("orders · another '{scene}' is already listening: this one answers to '{own}' (pleamar --say {own} …)");
        let Some(other) = command_socket_path(&own) else { return };
        path = other;
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("display"));
    let Ok(listener) = std::os::unix::net::UnixListener::bind(&path) else { return };
    *OWN_SOCKET.lock().unwrap() = Some(path.clone());
    note_display();
    std::thread::Builder::new()
        .name("commands".into())
        .spawn(move || {
            for mut c in listener.incoming().map_while(Result::ok) {
                let Ok(reader) = c.try_clone() else { continue };
                // Each one on its own: a `wait` or a `watch` that lasts does not
                // keep the next order waiting (an agent presses while it watches).
                let receive = receive.clone();
                let _ = std::thread::Builder::new().name("command".into()).spawn(move || {
                    for line in BufReader::new(reader).lines().map_while(Result::ok) {
                        let mut out = |l: &str| match l.is_empty() {
                            true => still_there(&c),
                            false => writeln!(c, "{l}").and_then(|_| c.flush()).is_ok(),
                        };
                        // A question (`get open`) is answered back the way it came.
                        if let Some(answer) = receive(line, &mut out) {
                            let _ = writeln!(c, "{}", answer.trim_end_matches('\n'));
                        }
                    }
                });
            }
        })
        .ok();
}

/// Whether the other end of a connection, or of a pipe, is still there,
/// without writing to it: one that has closed it says so at once (`POLLHUP`,
/// `POLLERR`). One that only said it has nothing more to say is still there.
#[cfg(unix)]
fn still_there(c: &impl std::os::fd::AsRawFd) -> bool {
    let mut p = libc::pollfd { fd: c.as_raw_fd(), events: 0, revents: 0 };
    // SAFETY: one descriptor of ours, looked at without waiting.
    let r = unsafe { libc::poll(&mut p, 1, 0) };
    r <= 0 || p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0
}

/// Where this scene listens, once it does, and the display it is shown on,
/// once it is connected: with both, the display is written beside the socket
/// (`notes.display`), for `--say` to tell a scene on the caller's display
/// from one of the same name on another —a nested compositor's copy of the
/// lock screen, and the real one (issue #67)—.
static OWN_SOCKET: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
static SHOWN_ON: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The display a program started from here talks to: `WAYLAND_DISPLAY`, a
/// name in `XDG_RUNTIME_DIR` or a path, as one path. Unknown with none (a
/// terminal over ssh, a scene drawn by pleamar-wm's own compositor).
pub fn display_here() -> Option<String> {
    let d = std::env::var("WAYLAND_DISPLAY").ok().filter(|d| !d.is_empty())?;
    if d.starts_with('/') {
        return Some(d);
    }
    let run = std::env::var("XDG_RUNTIME_DIR").ok().filter(|r| !r.is_empty())?;
    Some(format!("{}/{d}", run.trim_end_matches('/')))
}

/// The scene is connected to that display (the Wayland side says so).
pub fn shown_on(display: String) {
    *SHOWN_ON.lock().unwrap() = Some(display);
    note_display();
}

fn note_display() {
    #[cfg(unix)]
    if let (Some(sock), Some(d)) = (OWN_SOCKET.lock().unwrap().clone(), SHOWN_ON.lock().unwrap().clone()) {
        let _ = std::fs::write(sock.with_extension("display"), d);
    }
}

/// Which socket `--say NAME` reaches, and a word for the caller if that is not
/// obviously the one meant. Of the scenes called NAME —the one that has the
/// name and those that took `NAME-pid` because it was taken—, the one shown
/// on the caller's display; with none there, NAME as ever, and if it is shown
/// on another display, the caller is told which.
#[cfg(unix)]
fn reach(scene: &str) -> Result<(std::path::PathBuf, Option<String>), String> {
    let path = command_socket_path(scene).ok_or("I don't know where the sockets are")?;
    let Some(mine) = display_here() else { return Ok((path, None)) };
    let shown = |p: &std::path::Path| std::fs::read_to_string(p.with_extension("display")).ok().map(|d| d.trim().to_owned());
    let alive = |p: &std::path::Path| std::os::unix::net::UnixStream::connect(p).is_ok();
    if shown(&path).as_deref() == Some(mine.as_str()) {
        return Ok((path, None));
    }
    let dir = path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let prefix = format!("{scene}-");
    let mut others: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "sock"))
                .filter(|p| p.file_stem().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix(prefix.as_str())).is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())))
                .collect()
        })
        .unwrap_or_default();
    others.sort();
    if let Some(p) = others.into_iter().find(|p| shown(p).as_deref() == Some(mine.as_str()) && alive(p)) {
        return Ok((p, None));
    }
    let word = shown(&path).filter(|_| alive(&path)).map(|there| format!("note · the '{scene}' that answers is shown on {there}, not on this terminal's display ({mine}): none called '{scene}' is shown there"));
    Ok((path, word))
}

#[cfg(unix)]
/// Where a scene listens: `$XDG_RUNTIME_DIR/pleamar`, or `PLEAMAR_SOCKETS` —a
/// desktop of its own beside another of the same user (pleamar-wm's session
/// next to Hyprland) gives its programs another place, so that `marea
/// search` there reaches its own Marea and not the other desktop's—.
fn command_socket_path(scene: &str) -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(dir) = tests::SOCKETS.with(|d| d.borrow().clone()) {
        return Some(dir.join(format!("{scene}.sock")));
    }
    let dir = match std::env::var("PLEAMAR_SOCKETS").ok().filter(|d| !d.is_empty()) {
        Some(d) => std::path::PathBuf::from(d),
        None => std::path::Path::new(&std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned())).join("pleamar"),
    };
    Some(dir.join(format!("{scene}.sock")))
}

/// The scenes running where `--say` reaches: those whose socket answers.
#[cfg(unix)]
pub fn running_scenes() -> Vec<String> {
    let Some(dir) = command_socket_path("x").and_then(|r| r.parent().map(|p| p.to_owned())) else { return Vec::new() };
    let mut alive: Vec<String> = std::fs::read_dir(&dir)
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "sock") && std::os::unix::net::UnixStream::connect(p).is_ok()).filter_map(|p| p.file_stem().map(|n| n.to_string_lossy().into_owned())).collect())
        .unwrap_or_default();
    alive.sort();
    // pleamar-wm leaves its scene there under two names (`wm`, and its own):
    // one is a link to the other, and it would be measured twice.
    let real = |n: &str| std::fs::canonicalize(dir.join(format!("{n}.sock"))).ok();
    let mut seen = Vec::new();
    alive.retain(|n| {
        let r = real(n);
        if seen.contains(&r) {
            return false;
        }
        seen.push(r);
        true
    });
    alive
}
#[cfg(not(unix))]
pub fn running_scenes() -> Vec<String> {
    Vec::new()
}

/// Tell a running scene something and bring back what it answers.
#[cfg(unix)]
pub fn ask(scene: &str, command: &str, wait: std::time::Duration) -> Result<String, String> {
    use std::io::Write;
    let (path, word) = reach(scene)?;
    if let Some(w) = word {
        eprintln!("{w}");
    }
    let mut s = std::os::unix::net::UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(s, "{command}").map_err(|e| e.to_string())?;
    let _ = s.shutdown(std::net::Shutdown::Write);
    let _ = s.set_read_timeout(Some(wait));
    let mut answer = String::new();
    let _ = std::io::Read::read_to_string(&mut s, &mut answer);
    Ok(answer)
}
#[cfg(not(unix))]
pub fn ask(_: &str, _: &str, _: std::time::Duration) -> Result<String, String> {
    Err("this system has nowhere to receive commands yet".into())
}

/// Tell a running scene something: each command a line, on one connection, so
/// they are run in order. Without a name, to the only one there is.
#[cfg(unix)]
pub fn send(scene: Option<&str>, commands: &[String]) -> Result<(), String> {
    use std::io::Write;
    let path = match scene {
        Some(e) => {
            let (path, word) = reach(e)?;
            if let Some(w) = word {
                eprintln!("{w}");
            }
            path
        }
        None => {
            let dir = command_socket_path("x").and_then(|r| r.parent().map(|p| p.to_owned())).ok_or("I don't know where the sockets are")?;
            let alive: Vec<_> = std::fs::read_dir(&dir).map_err(|_| "there is no scene running")?.filter_map(Result::ok).map(|e| e.path()).filter(|p| std::os::unix::net::UnixStream::connect(p).is_ok()).collect();
            match alive.as_slice() {
                [one] => one.clone(),
                [] => return Err("there is no scene running".into()),
                several => return Err(format!("there are several scenes running; say which: {}", several.iter().filter_map(|p| p.file_stem()).map(|n| n.to_string_lossy()).collect::<Vec<_>>().join(", "))),
            }
        }
    };
    let mut s = std::os::unix::net::UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    for command in commands {
        writeln!(s, "{command}").map_err(|e| e.to_string())?;
    }
    // Everything has been said: if it was a question, now comes whatever they answer.
    let _ = s.shutdown(std::net::Shutdown::Write);
    // A `wait` waits up to a minute; a `watch` talks for as long as it was asked,
    // and every second with nothing said, it looks whether anyone still reads
    // what it prints (`… | head -1` has gone): if not, it goes too, and the scene
    // sees it and stops following.
    let watching = commands.iter().any(|c| c.starts_with("watch"));
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(if watching { 1 } else { 65 })));
    // Line by line, as it comes: a `watch` is read while it happens.
    let (mut from, mut line, mut to) = (std::io::BufReader::new(s), Vec::new(), std::io::stdout());
    loop {
        match std::io::BufRead::read_until(&mut from, b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {
                if !line.ends_with(b"\n") {
                    line.push(b'\n');
                }
                if to.write_all(&line).and_then(|_| to.flush()).is_err() {
                    break;
                }
                line.clear();
            }
            Err(e) if watching && matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {
                if !still_there(&to) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    Ok(())
}

/// One scene telling another one of `--say`'s commands: `sys.call("say",
/// "bar", "emit card_landed 3")`. The same socket and the same words, so the
/// other scene needs nothing new to be told. What it answers is not brought
/// back, except a `? …`, which is the error: so is a scene that is not there,
/// rather than a word lost on the way.
#[cfg(unix)]
pub fn say(scene: &str, command: &str) -> Result<(), String> {
    // The name becomes a path: it stays a name.
    if scene.is_empty() || scene.starts_with('.') || scene.contains(['/', '\0']) {
        return Err(format!("'{scene}' is not the name of a scene"));
    }
    // One call, one command: a second line would be a second one, unasked for.
    if command.contains(['\n', '\r']) {
        return Err("`say` takes one command, on one line: call it again for the next".into());
    }
    // A write this small goes as one buffer, whole or not at all (Linux's Unix
    // stream sockets): a line cut short by the deadline would be run as it stands.
    if command.len() >= SAY_LONGEST {
        return Err(format!("`say` takes a command of under {SAY_LONGEST} bytes"));
    }
    // Finding the scene connects to sockets too, and so blocks like the rest:
    // it goes on the exchange's thread, under its second.
    #[cfg(test)]
    let here = tests::SOCKETS.with(|d| d.borrow().clone());
    let name = scene.to_owned();
    let find = move || {
        #[cfg(test)]
        tests::SOCKETS.with(|d| *d.borrow_mut() = here);
        let (path, word) = reach(&name)?;
        // The word `--say` gives too: the scene told is shown on another display.
        if let Some(w) = word {
            eprintln!("{}", w.replace("this terminal's display", "the display of the scene telling it"));
        }
        Ok(path)
    };
    say_at(scene, command, find, running_scenes)
}
#[cfg(unix)]
const SAY_LONGEST: usize = 4096;
#[cfg(not(unix))]
pub fn say(_: &str, _: &str) -> Result<(), String> {
    Err("this system has nowhere to receive commands yet".into())
}

#[cfg(unix)]
fn say_at(scene: &str, command: &str, find: impl FnOnce() -> Result<std::path::PathBuf, String> + Send + 'static, others: fn() -> Vec<String>) -> Result<(), String> {
    use std::io::Write;
    use std::time::{Duration, Instant};
    // One second in all, for the whole exchange. Connecting to a scene that has
    // stopped taking calls (its queue full) or writing to one that stopped
    // reading blocks, and a native call cannot be cut off by the logic's
    // patience: so the exchange goes on a thread of its own, and the logic
    // waits for it no longer than that. A thread left blocked ends when that
    // scene takes the call or goes away, without writing: an error the logic
    // was given means the order was not delivered, and calling again does not
    // deliver it twice. Who goes first decides, under one lock: the thread
    // starting to write, or the logic giving up.
    const WAITING: u8 = 0;
    const WRITING: u8 = 1;
    const GIVEN_UP: u8 = 2;
    let until = Instant::now() + Duration::from_secs(1);
    let (tx, rx) = std::sync::mpsc::channel::<Result<bool, String>>();
    let phase = std::sync::Arc::new(std::sync::Mutex::new(WAITING));
    let (who, line, worker) = (scene.to_owned(), format!("{command}\n"), std::sync::Arc::clone(&phase));
    std::thread::spawn(move || {
        let path = match find() {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        // No socket, or one left behind by a scene that ended: nobody to tell.
        let Ok(mut s) = std::os::unix::net::UnixStream::connect(&path) else {
            let _ = tx.send(Err(match others() {
                o if o.is_empty() => format!("the scene '{who}' is not running"),
                o => format!("the scene '{who}' is not running (running: {})", o.join(", ")),
            }));
            return;
        };
        {
            let mut p = worker.lock().unwrap();
            if *p == GIVEN_UP || Instant::now() >= until {
                *p = GIVEN_UP;
                return;
            }
            *p = WRITING;
        }
        let _ = s.set_write_timeout(Some(until.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))));
        // One write: `writeln!` would make two, and the second could miss the deadline.
        if let Err(e) = s.write_all(line.as_bytes()) {
            let _ = tx.send(Err(format!("'{who}': {e}")));
            return;
        }
        let _ = s.shutdown(std::net::Shutdown::Write);
        let _ = tx.send(Ok(false));
        // An order answers nothing and the connection closes at once; a `? …`
        // comes as fast. Whatever takes longer (`wait`, `watch`) is not waited
        // for: the logic does not stand still for another scene. The deadline
        // holds across reads: a `watch` that keeps talking would otherwise hold
        // it for as long as it talks.
        let mut answer = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || s.set_read_timeout(Some(left)).is_err() {
                break;
            }
            match std::io::Read::read(&mut s, &mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => answer.extend_from_slice(&chunk[..n]),
            }
        }
        let _ = tx.send(match String::from_utf8_lossy(&answer).lines().find_map(|l| l.strip_prefix("? ").map(str::to_owned)) {
            Some(wrong) => Err(format!("'{who}' answers: {wrong}")),
            None => Ok(true),
        });
    });
    let left = || until.saturating_duration_since(Instant::now());
    // Not even taken within the second: the order did not get there.
    let delivered = match rx.recv_timeout(left()) {
        Err(_) => {
            let mut p = phase.lock().unwrap();
            // Given up by the worker itself (connected too late) is as sure a miss.
            if *p != WRITING {
                *p = GIVEN_UP;
                return Err(format!("the scene '{scene}' did not take the order within a second"));
            }
            drop(p);
            // The write began in time and has until the second to end: its outcome comes.
            rx.recv_timeout(Duration::from_millis(100)).map_err(|_| format!("'{scene}' was still being told when the second ran out: it may have the order"))?
        }
        Ok(r) => r,
    };
    delivered?;
    // Taken: only a `? …` is an error; one still waiting at the second is not.
    match rx.recv_timeout(left() + Duration::from_millis(50)) {
        Ok(Err(e)) => Err(e),
        _ => Ok(()),
    }
}

#[cfg(not(unix))]
pub fn listen_for_commands(_: &str, _: Commands) {}
#[cfg(not(unix))]
pub fn send(_: Option<&str>, _: &[String]) -> Result<(), String> {
    Err("this system has nowhere to receive commands yet: the named pipe is missing".into())
}

/// What became of the agent's cursor sent to a point (see `agent_cursor_to`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CursorTrip {
    Going,
    Arrived,
    /// The user stopped the agent (pleamar-wm's «Stop»): nothing more is done.
    Stopped,
    /// There is no agent's cursor here (no pleamar-wm, or `agent on` missing).
    Nowhere,
}

/// Where the agent's cursor is sent: this program's window, in the pixels of
/// its picture; or one of its surfaces that is no window (a panel), of that
/// size, in its units.
#[derive(Clone, Copy, Debug)]
pub enum CursorOn {
    Window,
    Panel(u32, u32),
}

/// The agent's own cursor of the session (pleamar-wm, `cua-inject v1`), glided
/// to (x, y) of this program's window or panel: whoever
/// watches sees it reach what it is about to press before it is pressed. Fast,
/// as a decided hand: 60 to 180 ms by how far it goes; `glide: false`, at once
/// (the steps of a drag, which already come one by one).
#[cfg(unix)]
pub fn agent_cursor_to(on: CursorOn, x: f32, y: f32, glide: bool) -> std::sync::Arc<std::sync::Mutex<CursorTrip>> {
    use std::io::{BufRead, BufReader, Write};
    let trip = std::sync::Arc::new(std::sync::Mutex::new(CursorTrip::Going));
    let set = trip.clone();
    let path = std::env::var("CUA_INJECT_SOCKET").ok().filter(|p| !p.is_empty());
    std::thread::spawn(move || {
        let end = |t| *set.lock().unwrap() = t;
        let Some(stream) = path.and_then(|p| std::os::unix::net::UnixStream::connect(p).ok()) else { return end(CursorTrip::Nowhere) };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(400)));
        let (Ok(mut w), mut r) = (stream.try_clone(), BufReader::new(stream)) else { return end(CursorTrip::Nowhere) };
        let mut say = |l: &str| -> Option<String> {
            writeln!(w, "{l}").ok()?;
            let mut reply = String::new();
            r.read_line(&mut reply).ok()?;
            Some(reply.trim_end().to_owned())
        };
        if say("cua-inject v1").is_none() {
            return end(CursorTrip::Nowhere);
        }
        let me = std::process::id();
        // The same hands either way: on a window by its process, on a panel by its size too.
        let to = |px: f32, py: f32| match on {
            CursorOn::Window => format!("m root:{me} 0 {px:.1} {py:.1}"),
            CursorOn::Panel(w, h) => format!("M {me} {w} {h} 0 {px:.1} {py:.1}"),
        };
        if !glide {
            let reply = say(&to(x, y));
            return end(if reply.as_deref() == Some("err stopped-by-user") { CursorTrip::Stopped } else { CursorTrip::Arrived });
        }
        let nums = |r: Option<String>, head: &str| -> Option<Vec<f32>> { r?.strip_prefix(head).map(|v| v.split_whitespace().filter_map(|n| n.parse().ok()).collect()) };
        let (at, rect) = match on {
            CursorOn::Window => (nums(say("p 0"), "at "), nums(say(&format!("r {me}")), "rect ")),
            CursorOn::Panel(..) => (None, None),
        };
        // From where it is if that is on this window; else from a little before.
        let from = match (at.filter(|a| a.len() >= 2), rect.filter(|r| r.len() >= 4)) {
            (Some(a), Some(r)) if a[0] >= r[0] && a[1] >= r[1] && a[0] < r[0] + r[2] && a[1] < r[1] + r[3] => (a[0] - r[0], a[1] - r[1]),
            _ => (x - 40.0, y - 26.0),
        };
        let far = (x - from.0).hypot(y - from.1);
        let total = (60.0 + far * 0.15).clamp(60.0, 180.0);
        let steps = if far < 2.0 { 1 } else { ((total / 12.0).round() as u32).max(3) };
        for k in 1..=steps {
            let t = k as f32 / steps as f32;
            let e = t * t * (3.0 - 2.0 * t);
            let reply = say(&to(from.0 + (x - from.0) * e, from.1 + (y - from.1) * e));
            if reply.as_deref() == Some("err stopped-by-user") {
                return end(CursorTrip::Stopped);
            }
            if k < steps {
                std::thread::sleep(std::time::Duration::from_millis((total / steps as f32) as u64));
            }
        }
        end(CursorTrip::Arrived)
    });
    trip
}

#[cfg(not(unix))]
pub fn agent_cursor_to(_: CursorOn, _: f32, _: f32, _: bool) -> std::sync::Arc<std::sync::Mutex<CursorTrip>> {
    std::sync::Arc::new(std::sync::Mutex::new(CursorTrip::Nowhere))
}

/// The system clipboard. `arboard` speaks it on all three systems; it lives
/// here so the core keeps knowing nothing about any of them.
pub fn clipboard_read() -> Option<String> {
    #[cfg(target_os = "linux")]
    if let Some(read) = wayland_clipboard_read() {
        return read;
    }
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// Wayland's own clipboard, asked first: arboard falls back to X11's
/// without saying so, and that one may hold something copied long before.
/// `None`: not asked (no Wayland, or it cannot be asked); `Some(None)`:
/// nothing to write —a picture copied, which comes with its HTML as text,
/// is a picture, not that HTML: the key goes on to the scene, which may take
/// it—.
#[cfg(target_os = "linux")]
fn wayland_clipboard_read() -> Option<Option<String>> {
    use std::io::Read;
    use wl_clipboard_rs::paste::{get_contents, get_mime_types_ordered, ClipboardType, MimeType, Seat};
    std::env::var_os("WAYLAND_DISPLAY")?;
    let types = match get_mime_types_ordered(ClipboardType::Regular, Seat::Unspecified) {
        Ok(t) => t,
        Err(wl_clipboard_rs::paste::Error::ClipboardEmpty | wl_clipboard_rs::paste::Error::NoMimeType) => return Some(None),
        Err(_) => return None,
    };
    let plain = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];
    // No plain text: a picture (with its HTML), or something else that is
    // not words to write.
    let Some(kind) = plain.iter().find(|p| types.iter().any(|t| t == *p)) else { return Some(None) };
    let (mut pipe, _) = get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Specific(kind)).ok()?;
    let mut text = String::new();
    pipe.read_to_string(&mut text).ok()?;
    Some(Some(text))
}

pub fn clipboard_write(text: &str) {
    // On Wayland whoever copies has to stay alive to serve what was copied: a
    // thread that holds on to it until someone else copies something else.
    let text = text.to_owned();
    std::thread::spawn(move || {
        if let Ok(mut c) = arboard::Clipboard::new() {
            #[cfg(target_os = "linux")]
            {
                use arboard::SetExtLinux;
                let _ = c.set().wait().text(text);
            }
            #[cfg(not(target_os = "linux"))]
            let _ = c.set_text(text);
        }
    });
}

/// Give back to the system the memory that was used to read a scene. Compiling
/// Marea (3 700 instructions) goes through 128 MB for a moment to settle at 9,
/// and glibc keeps what was freed in each thread's arenas: the process
/// stayed at 355 MB of RSS forever. It's done a while later, when the
/// workshop and the logic have already done their thing with it. On other systems, or with
/// another libc, it's not needed or we don't know how to ask: it does nothing.
pub fn release_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    std::thread::Builder::new()
        .name("release".into())
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(3));
            // It only gives back free pages: it touches nothing alive.
            unsafe { libc::malloc_trim(0) };
        })
        .ok();
}

/// What the render asks of a system window.
pub trait PlatformWindow: Send {
    /// Where the mouse gets in: only through these boxes, in logical pixels.
    fn update_input_region(&self, boxes: &[[i32; 4]]);
    /// Which cursor shows while the mouse is over it.
    fn cursor(&self, c: crate::scene::Cursor);
    /// Grab or release the keyboard with the scene running: a launcher wants it
    /// entirely while it's open, and not at all when it isn't.
    fn keyboard(&self, t: crate::scene::Keyboard);
    /// That the system report (`ToRender::Frame`) when it wants the frame after
    /// the one about to be presented. Where that's unknown, it doesn't report, and the clock sets the pace.
    fn request_frame(&self) {}
    /// What the system blurs behind the window: rectangles in logical
    /// pixels, those of its glass. Empty, nothing. Where it can't be asked for, it does nothing.
    fn update_blur_region(&self, _boxes: &[[i32; 4]]) {}
    /// Ask for what's shown on screen in that box of the window —x, y, width and
    /// height, logical—, **with the window on top**: it arrives as `ToRender::Backdrop`. It's
    /// what lets you see what's behind the glass: what the window itself painted
    /// gets subtracted from it. With `on_change`, it doesn't arrive until something changes in that
    /// box; without it, the next time the compositor paints the screen.
    /// `false` if it can't be asked for, or if there's already one on the way.
    fn capture_backdrop(&self, _bounds: [i32; 4], _on_change: bool) -> bool {
        false
    }
    /// Forget the capture that's on the way: it won't arrive anymore.
    fn cancel_backdrop(&self) {}
    /// On which monitor it is, by name, and where on it: its top left corner,
    /// logical. `None` where that is not known.
    fn desktop_place(&self) -> Option<(String, (i32, i32))> {
        None
    }
    /// Frames of our own to paint it in, where the platform can hand them to
    /// the compositor itself, with what changed in each: a swapchain tells it
    /// all changed. `None`, the swapchain.
    fn own_frames(&self) -> Option<Box<dyn crate::gpu::Frames>> {
        None
    }
}

/// A capture of what was seen on screen in a box of a window.
pub struct Backdrop {
    /// Which window it belongs to.
    pub sheet: u32,
    /// In real pixels, four bytes per pixel: blue, green, red and alpha.
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// With what opacity the compositor blended ours: 1 except in a
    /// window whose opacity is lowered (Hyprland: `decoration:active_opacity`).
    pub opacity: f32,
    /// The pixels, where the compositor left them, without copying them. `None` if the capture failed,
    /// or if the compositor wrote them straight on the card (`card`).
    pub data: Option<std::sync::Arc<dyn AsRef<[u8]> + Send + Sync>>,
    /// The capture is in this texture of the card, written by the compositor: no readback, no upload.
    pub card: Option<std::sync::Arc<CardBackdrop>>,
    /// The compositor can copy into a buffer on the card (fourcc, width, height) and none was ready:
    /// the render makes one (`CARD_BACKDROPS`) for the next capture.
    pub offer: Option<(u32, u32, u32)>,
}

/// Where the compositor leaves the captures of one sheet, on the card.
pub struct CardBackdrop {
    pub texture: wgpu::Texture,
    pub fd: std::os::fd::OwnedFd,
    pub modifier: u64,
    pub planes: Vec<(u32, u32)>,
    pub fourcc: u32,
    pub size: (u32, u32),
}

/// Each sheet's, by its id: made by the render when the compositor offers, taken by the platform thread.
pub static CARD_BACKDROPS: std::sync::Mutex<Option<std::collections::HashMap<u32, std::sync::Arc<CardBackdrop>>>> = std::sync::Mutex::new(None);
/// The sheets whose compositor did not take the card (a copy failed, or no layout): they go on in shared memory.
/// For half a minute: a copy fails when a private window is shown, or the pointer is in it, and that passes.
pub static CARD_REFUSED: std::sync::Mutex<Vec<(u32, std::time::Instant)>> = std::sync::Mutex::new(Vec::new());

/// Whether that sheet's captures go through shared memory for now.
pub fn card_refused(sheet: u32) -> bool {
    let mut all = CARD_REFUSED.lock().unwrap();
    all.retain(|(_, at)| at.elapsed() < std::time::Duration::from_secs(30));
    all.iter().any(|(s, _)| *s == sheet)
}

/// `PLEAMAR_BACKDROP_SHM=1` keeps the old way (the compositor writes to shared memory), to compare.
pub fn card_backdrops_allowed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("PLEAMAR_BACKDROP_SHM").is_none())
}

/// Whether the scene running wants to know where the mouse is on the whole
/// desktop (`cursor.x`): the render says so every time a scene arrives.
pub static CURSOR_WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Where the mouse is when it is not over us. Wayland does not tell a surface
/// that —on purpose—, so it is asked of whoever knows: Hyprland, through its
/// socket, about thirty times a second; pleamar-wm, through the `cursor.sock`
/// of its session, which says it as it moves. Only while a scene wants it. It
/// reaches the render as `ToRender::Cursor` when it moves. Elsewhere nothing
/// arrives, and `cursor.x` is only known over the scene, like `pointer.x`.
pub fn watch_cursor(to_render: std::sync::mpsc::Sender<crate::scene::ToRender>) {
    #[cfg(target_os = "linux")]
    if hyprland::is_present() && std::env::var_os("PLEAMAR_GENERIC").is_none() {
        hyprland::watch_cursor(to_render);
    } else if let Some(dir) = std::env::var("PLEAMAR_SOCKETS").ok().filter(|d| std::path::Path::new(&format!("{d}/cursor.sock")).exists()) {
        session_cursor(dir, to_render);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = to_render;
}

/// What the system says about motion and colour (the desktop portal's
/// `org.freedesktop.appearance`), as the facts `motion.reduced` and
/// `color.scheme`, at the start and each time it changes. Elsewhere, nothing.
pub fn watch_appearance(to_render: std::sync::mpsc::Sender<crate::scene::ToRender>) {
    #[cfg(target_os = "linux")]
    appearance::watch(to_render);
    #[cfg(not(target_os = "linux"))]
    let _ = to_render;
}

/// pleamar-wm's `cursor.sock`: a line, `x y` in units of the desktop, each
/// time the mouse moves. Connected only while a scene wants it, and let go of
/// when it no longer does; the monitors, to place it, from the `desktop` file
/// beside it (`monitor name width height x y mhz focused scale`, the size in
/// pixels), read again every two seconds.
#[cfg(target_os = "linux")]
fn session_cursor(dir: String, to_render: std::sync::mpsc::Sender<crate::scene::ToRender>) {
    use std::io::BufRead;
    use std::sync::atomic::Ordering;
    let monitors_of = |dir: &str| -> Vec<(String, [i32; 4])> {
        let text = std::fs::read_to_string(format!("{dir}/desktop")).unwrap_or_default();
        text.lines()
            .filter_map(|l| l.strip_prefix("monitor "))
            .filter_map(|l| {
                let f: Vec<&str> = l.split(' ').collect();
                let [name, w, h, x, y, _mhz, _focused, scale] = f[..] else { return None };
                let scale = scale.parse::<f64>().ok()?.max(0.1);
                Some((name.to_owned(), [x.parse().ok()?, y.parse().ok()?, (w.parse::<f64>().ok()? / scale) as i32, (h.parse::<f64>().ok()? / scale) as i32]))
            })
            .collect()
    };
    let _ = std::thread::Builder::new().name("session·cursor".into()).spawn(move || loop {
        if !CURSOR_WANTED.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(500));
            continue;
        }
        let Ok(stream) = std::os::unix::net::UnixStream::connect(format!("{dir}/cursor.sock")) else {
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        };
        let mut monitors = monitors_of(&dir);
        let mut asked = std::time::Instant::now();
        for line in std::io::BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if !CURSOR_WANTED.load(Ordering::Relaxed) {
                break;
            }
            let Some((x, y)) = line.split_once(' ').and_then(|(x, y)| Some((x.parse::<f32>().ok()?, y.parse::<f32>().ok()?))) else { continue };
            if asked.elapsed().as_secs() >= 2 {
                asked = std::time::Instant::now();
                monitors = monitors_of(&dir);
            }
            if to_render.send(crate::scene::ToRender::Cursor((x, y), monitors.clone())).is_err() {
                return;
            }
        }
        // The session restarted it, or it went: try again in a moment.
        std::thread::sleep(std::time::Duration::from_secs(1));
    });
}

/// Where the scene is shown and the input comes from. pleamar's own is a
/// client of the system's compositor (Wayland); whoever builds on pleamar can
/// hand over another —pleamar-wm drives the monitors itself— with
/// `provide_platform`, before `run`.
pub trait Platform: Send {
    /// Puts up the surfaces the scene asks for, hands them to the render as
    /// sheets (`ToRender::Sheet`), and tells it the input, until the end.
    fn run(self: Box<Self>, surfaces: Vec<Surface>, extra_height: u32, instance: wgpu::Instance, to_render: std::sync::mpsc::Sender<ToRender>);
}

static PLATFORM: std::sync::Mutex<Option<Box<dyn Platform>>> = std::sync::Mutex::new(None);

pub fn provide_platform(p: Box<dyn Platform>) {
    *PLATFORM.lock().unwrap() = Some(p);
}

/// The platform handed over, if there is one.
pub fn provided_platform() -> Option<Box<dyn Platform>> {
    PLATFORM.lock().unwrap().take()
}

/// The keyboard layout pleamar's own window was given, as xkb text.
static HOST_KEYMAP: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_host_keymap(k: String) {
    *HOST_KEYMAP.lock().unwrap() = Some(k);
}

pub fn host_keymap() -> Option<String> {
    HOST_KEYMAP.lock().unwrap().clone()
}

/// The compositor inside the scene (`windows`): where to tell it things.
pub type NestSender = Box<dyn Fn(crate::scene::ToNest) + Send>;

/// What starts one: given how many windows the scene holds and where to
/// report to the render, it answers where to talk to it —or nothing, if it
/// could not start—.
pub type NestStarter = fn(usize, std::sync::mpsc::Sender<crate::scene::ToRender>) -> Option<NestSender>;

static NEST: std::sync::OnceLock<NestStarter> = std::sync::OnceLock::new();

/// pleamar itself holds no compositor: whoever builds one on top of it
/// (pleamar-wm) hands it over here before `run()`, and the scenes' `windows`
/// fill from it.
pub fn provide_windows(start: NestStarter) {
    let _ = NEST.set(start);
}

pub fn start_nest(max: usize, to_render: std::sync::mpsc::Sender<crate::scene::ToRender>) -> Option<NestSender> {
    match NEST.get() {
        Some(start) => start(max, to_render),
        None => {
            eprintln!("windows · this pleamar holds no other programs' windows: open the scene with pleamar-wm");
            None
        }
    }
}

/// The sounds a scene plays itself: its mixer, and how files and tones are made.
pub mod sound;
#[cfg(target_os = "linux")]
mod gamepad;
/// How game controller number `n` (from 1) is right now: `{ name, lx, ly, a, b, … }`.
#[cfg(target_os = "linux")]
pub use gamepad::state as gamepad;
#[cfg(not(target_os = "linux"))]
pub fn gamepad(_: usize) -> Option<SysValue> {
    None
}
#[cfg(target_os = "linux")]
mod auth;
#[cfg(target_os = "linux")]
mod notifications;
#[cfg(target_os = "linux")]
mod polkit;
#[cfg(target_os = "linux")]
mod tray;
#[cfg(target_os = "linux")]
mod compositor;
#[cfg(target_os = "linux")]
mod thumbnails;
#[cfg(target_os = "linux")]
mod capture;
#[cfg(target_os = "linux")]
pub use thumbnails::Frame as ThumbnailFrame;
/// A frame kept in memory, by the name that `image … = from` was given: a
/// live window's last one (`thumbnails:3`, `thumbnails.live`) or a frozen
/// monitor (`capture:DP-1`, `capture.freeze`).
#[cfg(target_os = "linux")]
pub fn thumbnail_frame(name: &str) -> Option<std::sync::Arc<ThumbnailFrame>> {
    thumbnails::frame(name).or_else(|| capture::frame(name))
}
/// Live windows copied on the card: the buffers asked of the render, its
/// answers, and which of them are still in use.
#[cfg(target_os = "linux")]
pub use thumbnails::{checked as thumbnail_checked, checks as thumbnail_checks, on_check as thumbnail_on_check};
#[cfg(target_os = "linux")]
pub use thumbnails::{cards as thumbnail_cards, offer as thumbnail_offer, wants as thumbnail_wants, Offer as ThumbnailOffer};
#[cfg(not(target_os = "linux"))]
pub struct ThumbnailFrame {
    pub version: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub opaque: bool,
    /// Never on the card here.
    pub card: Option<()>,
}
#[cfg(not(target_os = "linux"))]
pub fn thumbnail_frame(_: &str) -> Option<std::sync::Arc<ThumbnailFrame>> {
    None
}
mod files;

/// JSON to what the logic sees, and back: `json.decode` and `json.encode`.
pub fn json_decode(text: &str) -> Result<SysValue, String> {
    serde_json::from_str(text).map(files::from_json).map_err(|e| e.to_string())
}

pub fn json_encode(v: &SysValue) -> String {
    files::to_json(v).to_string()
}
mod clock;
#[cfg(target_os = "linux")]
mod appearance;
#[cfg(target_os = "linux")]
mod desktop;
#[cfg(target_os = "linux")]
mod hyprland;
#[cfg(target_os = "linux")]
mod mpris;
#[cfg(target_os = "linux")]
mod networkmanager;
#[cfg(target_os = "linux")]
mod iwd;
#[cfg(target_os = "linux")]
mod bluez;
#[cfg(target_os = "linux")]
mod pulse;
#[cfg(target_os = "linux")]
mod system;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
pub use wayland::{backdrop_layouts, run_event_loop};

/// No platform yet: the core compiles —it's the guard that it stays
/// portable— but there's nowhere to paint.
#[cfg(not(target_os = "linux"))]
pub fn run_event_loop(_: Vec<Surface>, _: u32, _: wgpu::Instance, _: Sender<ToRender>) {
    eprintln!("pleamar cannot put windows on this system yet: its src/platform/ is missing");
    std::process::exit(1);
}

/// Where an icon's file is, by its name.
#[cfg(target_os = "linux")]
pub use wayland::icon;
#[cfg(not(target_os = "linux"))]
pub fn icon(_: &str) -> Option<std::path::PathBuf> {
    None
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::{say, say_at};
    use std::io::{BufRead, BufReader, Write};

    /// The socket found, without looking: a stand-in's.
    fn at(path: &std::path::Path) -> impl FnOnce() -> Result<std::path::PathBuf, String> + Send + 'static {
        let p = path.to_owned();
        move || Ok(p)
    }
    fn nobody() -> Vec<String> {
        Vec::new()
    }

    thread_local! {
        /// Where this test thread's scenes listen, instead of the real ones'.
        pub(crate) static SOCKETS: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_scene_that_takes_no_calls_holds_the_caller_one_second_in_all() {
        use std::os::fd::AsRawFd;
        let dir = std::env::temp_dir().join(format!("pleamar-say-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bar.sock");
        // A scene stuck: it never accepts, and its queue is as short as it gets.
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let (filler, stop) = (path.clone(), std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let stopped = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while !stopped.load(std::sync::atomic::Ordering::SeqCst) {
                let Ok(s) = std::os::unix::net::UnixStream::connect(&filler) else { break };
                held.push(s);
            }
            // Held, never closed: the one that closes is say's.
            std::thread::park();
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        let t = std::time::Instant::now();
        assert_eq!(say_at("bar", "emit x", at(&path), nobody), Err("the scene 'bar' did not take the order within a second".into()));
        let took = t.elapsed();
        assert!(took < std::time::Duration::from_millis(1500), "held for {took:?}");
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        // The scene comes back and takes its calls: say's connection is among
        // them (it is the one that closes), and it brings nothing.
        listener.set_nonblocking(true).unwrap();
        let back = std::time::Instant::now();
        let mut says = None;
        while says.is_none() {
            assert!(back.elapsed() < std::time::Duration::from_secs(5), "say's connection was never taken: the test proves nothing");
            match listener.accept() {
                Ok((mut c, _)) => {
                    c.set_nonblocking(false).unwrap();
                    c.set_read_timeout(Some(std::time::Duration::from_millis(50))).unwrap();
                    let mut heard = String::new();
                    if std::io::Read::read_to_string(&mut c, &mut heard).is_ok() {
                        says = Some(heard);
                    }
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        assert_eq!(says.as_deref(), Some(""), "delivered after the caller was told it was not");
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_watch_that_keeps_talking_holds_the_caller_one_second_in_all() {
        let dir = std::env::temp_dir().join(format!("pleamar-say-drip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bar.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        // A `watch` that answers a line every 200 ms, for ten seconds.
        std::thread::spawn(move || {
            if let Some(mut c) = listener.incoming().map_while(Result::ok).next() {
                for i in 0..50 {
                    if writeln!(c, "bar.level {i}").is_err() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        });
        let t = std::time::Instant::now();
        assert_eq!(say_at("bar", "watch 10s bar.level", at(&path), nobody), Ok(()));
        let took = t.elapsed();
        assert!(took < std::time::Duration::from_millis(1500), "held for {took:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_scene_tells_another_and_hears_only_what_went_wrong() {
        let dir = std::env::temp_dir().join(format!("pleamar-say-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A stand-in for a scene's listener: it answers as `--say`'s does, nothing to an order.
        let path = dir.join("bar.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (heard, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for c in listener.incoming().map_while(Result::ok) {
                let mut w = c.try_clone().unwrap();
                for line in BufReader::new(c).lines().map_while(Result::ok) {
                    if !line.starts_with("emit ") {
                        let _ = writeln!(w, "? I don't understand '{line}'");
                    }
                    heard.send(line).unwrap();
                }
            }
        });
        assert_eq!(say_at("bar", "emit card_landed 3", at(&path), nobody), Ok(()));
        assert_eq!(lines.recv().unwrap(), "emit card_landed 3");
        assert_eq!(say_at("bar", "emitt card_landed", at(&path), nobody), Err("'bar' answers: I don't understand 'emitt card_landed'".into()));

        // Not there: no socket, or one a scene that ended left behind.
        assert_eq!(say_at("toasts", "emit x", at(&dir.join("toasts.sock")), nobody), Err("the scene 'toasts' is not running".into()));
        let left = dir.join("osd.sock");
        drop(std::os::unix::net::UnixListener::bind(&left).unwrap());
        assert_eq!(say_at("osd", "emit x", at(&left), nobody), Err("the scene 'osd' is not running".into()));

        // A name stays a name, and one call is one command.
        assert!(say("../bar", "emit x").is_err());
        assert!(say("bar", "emit x\nquit").is_err());
        assert!(say("bar", &format!("emit {}", "x".repeat(4096))).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
