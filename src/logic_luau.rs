//! A scene's logic, in Luau. It lives on its own thread, in a sandbox: no
//! files and no system, with a memory cap and with its seconds counted. If it
//! gets stuck nobody notices —the render does not wait for it—, and if it stays
//! in a loop forever, it gets cut off.
//!
//! The only thing it can do is what crosses the frontier: say what is true,
//! what a text says, that something has happened; and hear what happens in the scene.
//!
//! ```lua
//! fact.open = true                         -- a fact
//! text["notice.title"] = "Meeting"         -- a live text
//! emit("confirmed")   play("joy")          -- an event, a gesture
//! on("view_event", function(n) … end)      -- an event from the scene, with its payload
//! on("press:view", …)  on("enter:orb", …)  on("layer:card", function(claim) … end)
//! on("fact:open", function(v) … end)       -- a rule changed a fact
//! local t = every(1000, function() … end)  after(500, …)  cancel(t)
//! run("date", {"+%H:%M"}, function(out, code) … end)   -- a system command
//! ```

use crate::scene::{intern, Event, Scene, ToRender};
use crate::logic::{Context, Script};
use crate::platform::SysValue;
use mlua::{Function, Lua, MultiValue, Table, Value, VmState};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Everything the logic has left running. When the program exits it has to be
/// stopped: a child does not die with its parent, and an orphaned `pactl subscribe`
/// would stay there forever.
static CHILDREN: Mutex<Vec<Arc<Mutex<Option<std::process::Child>>>>> = Mutex::new(Vec::new());

pub fn stop_children() {
    for h in CHILDREN.lock().unwrap().drain(..) {
        if let Some(mut h) = h.lock().unwrap().take() {
            let _ = h.kill();
            let _ = h.wait();
        }
    }
}

/// How long a handler may take before it gets cut off.
const PATIENCE: Duration = Duration::from_secs(2);
const MEMORY: usize = 64 << 20;

struct Timer {
    id: u32,
    when: Instant,
    every: Option<Duration>,
    f: Function,
}

#[derive(Default)]
struct Shared {
    /// In the logic of a scene brought in (G22): the names of the other scenes beside it,
    /// whose facts, texts, models and events it names whole (`Bar.volume`) and only reads.
    neighbors: Vec<String>,
    /// And the libraries the scenes import, whose boundary a scene brought in owns as one alone does.
    libraries: Vec<String>,
    handlers: HashMap<String, Vec<Function>>,
    timers: Vec<Timer>,
    processes: HashMap<u32, Function>,
    /// The commands still running: who gets told each line, and how to stop them.
    running: HashMap<u32, (Function, Arc<Mutex<Option<std::process::Child>>>)>,
    /// The input of those started with `{ stdin = "open" }`: `write(id, text)`
    /// talks to them while they run (an agent that is spoken to line by line).
    inputs: HashMap<u32, Writer>,
    watchers: HashMap<String, Vec<Function>>,
    next: u32,
    facts: HashMap<String, f64>,
    texts: HashMap<String, String>,
    permissions: crate::scene::Permissions,
    models: Vec<crate::scene::Model>,
    /// A model walked with `key` (`for m in said key m.id`): which record's key each
    /// copy shows, so a record keeps its copy while it is there (see `slotted`).
    slots: HashMap<String, Vec<Option<String>>>,
    /// Of the facts that are not plain numbers, what they are: the logic sees them as `true` or as `"critical"`.
    types: HashMap<String, crate::scene::FactType>,
    signals: std::collections::HashSet<String>,
    /// The scene's `translations`, for `tr`.
    translations: crate::scene::Translations,
    /// The services the scene asked for by name, and which fields it wants from each one.
    services: Vec<crate::scene::Service>,
    /// Which ones already got a thread: reloading the scene does not start them twice.
    subscribed: std::collections::HashSet<String>,
    /// What each one said last, by alias: spread again when the scene is
    /// reloaded, so a field or a list it asks for anew is filled at once (a
    /// service speaks only when something changes, and a reload changes nothing).
    last_reports: std::collections::HashMap<String, SysValue>,
    /// If it is a plugin's logic, what it is called: so that errors talk about it and not about the scene.
    plugin: Option<String>,
    /// It asks for permissions nobody has approved: it runs with none, and the errors say why.
    unapproved: bool,
    /// Until when whatever is running may run.
    deadline: Option<Instant>,
    /// It listens to every frame: counted in `FRAME_LISTENERS` until it lets go.
    frames: bool,
    /// The sounds it has started, to stop the ones still going when it is reloaded.
    sounds: Vec<u32>,
    /// The keys held down now, by their plain names: `held("Left")`.
    held: std::collections::HashSet<String>,
    /// Where the mouse is on the scene, if it is over it, and which of its
    /// buttons are down: what `mouse.x`, `mouse.left` say.
    mouse: (Option<(f32, f32)>, u8),
}

impl Shared {
    /// What a new logic starts from: what the scene is, and none of what the
    /// old one left running (its handlers, timers, processes and watchers).
    fn fresh(&self) -> Shared {
        Shared {
            next: self.next,
            facts: self.facts.clone(),
            texts: self.texts.clone(),
            permissions: self.permissions.clone(),
            models: self.models.clone(),
            types: self.types.clone(),
            signals: self.signals.clone(),
            translations: self.translations.clone(),
            services: self.services.clone(),
            subscribed: self.subscribed.clone(),
            slots: self.slots.clone(),
            plugin: self.plugin.clone(),
            unapproved: self.unapproved,
            neighbors: self.neighbors.clone(),
            libraries: self.libraries.clone(),
            ..Default::default()
        }
    }
}

/// Spreads a list of records over the texts and facts the scene has for it
/// —`rows.3.label`—, and the lists inside, the same way. Only what changes travels.
///
/// First it is read whole and then applied: if a record is wrong, the list that
/// was there stays as it was, not half-done.
fn spread_list(c: &mut Shared, tx: &Sender<ToRender>, prefix: &str, m: &crate::scene::Model, list: &Table) -> mlua::Result<()> {
    let mut changes = Vec::new();
    match &m.key {
        Some(key) => {
            let copies = slotted(c.slots.entry(prefix.to_owned()).or_default(), key, m.capacity, list)?;
            read_rows(&mut changes, prefix, m, list, Some(&copies))?;
            // Each copy: whether it shows a record, and at which place of the list.
            for (k, place) in copies.iter().enumerate() {
                changes.push(Change::Fact(format!("{prefix}.{k}.__there"), place.is_some() as u8 as f64));
                changes.push(Change::Fact(format!("{prefix}.{k}.__place"), place.map_or(-1.0, |p| p as f64)));
            }
        }
        None => read_rows(&mut changes, prefix, m, list, None)?,
    }
    for change in changes {
        match change {
            Change::Text(name, t) => if c.texts.get(&name) != Some(&t) {
                c.texts.insert(name.clone(), t.clone());
                let _ = tx.send(ToRender::Text(intern(&name), t));
            },
            Change::Fact(name, n) => if c.facts.get(&name) != Some(&n) {
                c.facts.insert(name.clone(), n);
                let _ = tx.send(ToRender::Fact(intern(&name), n as f32));
            },
        }
    }
    Ok(())
}

enum Change {
    Text(String, String),
    Fact(String, f64),
}

/// Which record each copy of a model walked with `key` shows: a key that was
/// there keeps its copy, a new one takes the first copy left free, and one
/// that went frees its own. `slots` is what each copy showed (kept between
/// writes); the answer, for each copy, the record it shows now: its place in
/// `list`. Records past the capacity, and a key said twice after the first,
/// are not shown.
fn slotted(slots: &mut Vec<Option<String>>, key: &str, capacity: usize, list: &Table) -> mlua::Result<Vec<Option<usize>>> {
    slots.resize(capacity, None);
    let total = list.raw_len().min(capacity);
    let mut keys: Vec<Option<String>> = Vec::with_capacity(total);
    for i in 0..total {
        let row: Value = list.raw_get(i + 1)?;
        let k = match row {
            Value::Table(t) => match t.get::<Value>(key)? {
                Value::Nil => None,
                Value::Integer(n) => Some(n.to_string()),
                Value::Number(x) => Some(x.to_string()),
                v => Some(v.to_string()?),
            },
            _ => None,
        };
        // A key said again is not a second record of the same identity.
        keys.push(k.filter(|k| !keys.iter().flatten().any(|o| o == k)));
    }
    let mut copies: Vec<Option<usize>> = vec![None; capacity];
    // The ones that were there keep their copy.
    for (s, had) in slots.iter_mut().enumerate() {
        match had.as_ref().and_then(|h| keys.iter().position(|k| k.as_ref() == Some(h))) {
            Some(i) => copies[s] = Some(i),
            None => *had = None,
        }
    }
    // The new ones (and those without a key), into the copies left free, in order.
    for (i, k) in keys.iter().enumerate() {
        if copies.contains(&Some(i)) {
            continue;
        }
        if let Some(s) = copies.iter().position(Option::is_none) {
            copies[s] = Some(i);
            slots[s] = k.clone();
        }
    }
    Ok(copies)
}

/// `copies`: with `key`, which record of `list` each copy shows (see `slotted`);
/// without it, copy k is record k.
fn read_rows(changes: &mut Vec<Change>, prefix: &str, m: &crate::scene::Model, list: &Table, copies: Option<&[Option<usize>]>) -> mlua::Result<()> {
    use crate::scene::{FactType, FieldType, FieldValue};
    let total = list.raw_len();
    let text = |c: &mut Vec<Change>, name: String, t: String| c.push(Change::Text(name, t));
    let fact = |c: &mut Vec<Change>, name: String, n: f64| c.push(Change::Fact(name, n));
    let c = changes;
    // Which copy each record goes to: its own place, or with `key` the copy it keeps.
    let order: Vec<(usize, usize)> = match copies {
        Some(cs) => cs.iter().enumerate().filter_map(|(k, i)| i.map(|i| (k, i))).collect(),
        None => (0..total.min(m.capacity)).map(|i| (i, i)).collect(),
    };
    for (copy, i) in order {
        let row: Value = list.raw_get(i + 1)?;
        let Value::Table(row) = row else {
            return Err(mlua::Error::runtime(format!("'{prefix}' is a list of records, and number {} is not a table", i + 1)));
        };
        for field in &m.fields {
            let name = format!("{prefix}.{copy}.{}", field.name);
            let v: Value = row.get(field.name.as_str())?;
            match (&field.kind, &field.fallback) {
                (FieldType::List(inner), _) => match v {
                    Value::Table(t) => read_rows(c, &name, inner, &t, None)?,
                    // No list means an empty one: don't leave behind the one from the record that was here before.
                    _ => {
                        fact(c, format!("{name}.count"), 0.0);
                        fact(c, format!("{name}.total"), 0.0);
                    }
                },
                (FieldType::Text | FieldType::Image(..), fallback) => {
                    let t = match &v {
                        Value::Nil => if let FieldValue::Text(t) = fallback { t.clone() } else { String::new() },
                        other => other.to_string()?,
                    };
                    text(c, name, t);
                }
                (kind, fallback) => {
                    let missing = if let FieldValue::Number(d) = fallback { *d as f64 } else { 0.0 };
                    let n = match (kind, &v) {
                        (_, Value::Nil) => missing,
                        // A list where a number is expected counts as how many it has.
                        (FieldType::Number, Value::Table(t)) => t.raw_len() as f64,
                        (FieldType::Bool, Value::Table(t)) => (t.raw_len() > 0) as u8 as f64,
                        (FieldType::Bool, _) => to_number(&name, Some(&FactType::Bool), &v)?,
                        (FieldType::Enum(names), _) => to_number(&name, Some(&FactType::Enum(names.clone())), &v)?,
                        _ => to_number(&name, None, &v)?,
                    };
                    fact(c, name, n);
                }
            }
        }
    }
    fact(c, format!("{prefix}.count"), total.min(m.capacity) as f64);
    fact(c, format!("{prefix}.total"), total as f64);
    Ok(())
}

/// What the logic writes into a fact, as the number the scene sees. A `bool`
/// wants `true` or `false`; an enumeration, one of its names; the rest, a number.
fn to_number(name: &str, kind: Option<&crate::scene::FactType>, v: &Value) -> mlua::Result<f64> {
    use crate::scene::FactType;
    let number = match v {
        Value::Boolean(b) => Some(*b as u8 as f64),
        Value::Integer(i) => Some(*i as f64),
        Value::Number(x) => Some(*x),
        _ => None,
    };
    match (kind, v) {
        (Some(FactType::Enum(names)), Value::String(t)) => {
            let t = t.to_string_lossy();
            match names.iter().position(|n| *n == t) {
                Some(k) => Ok(k as f64),
                None => Err(mlua::Error::runtime(format!("'{name}' cannot be '{t}'{}: it can be {}", did_you_mean(&t, names.iter()), names.join(", ")))),
            }
        }
        (Some(FactType::Enum(names)), _) => match number {
            Some(n) if n >= 0.0 && (n as usize) < names.len() && n.fract() == 0.0 => Ok(n),
            _ => Err(mlua::Error::runtime(format!("'{name}' is an enum: it can be {}", names.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")))),
        },
        (Some(FactType::Bool), _) => number.map(|n| (n != 0.0) as u8 as f64).ok_or_else(|| mlua::Error::runtime(format!("'{name}' is a yes or no: true or false, not a {}", v.type_name()))),
        (None, _) => number.ok_or_else(|| mlua::Error::runtime(format!("'{name}' is a number, not a {}", v.type_name()))),
    }
}

/// And the other way round: as the logic sees it.
fn from_number(lua: &Lua, kind: Option<&crate::scene::FactType>, v: f64) -> mlua::Result<Value> {
    use crate::scene::FactType;
    Ok(match kind {
        Some(FactType::Bool) => Value::Boolean(v > 0.5),
        Some(FactType::Enum(names)) => match names.get(v.round().max(0.0) as usize) {
            Some(n) => Value::String(lua.create_string(n)?),
            None => Value::Number(v),
        },
        None => Value::Number(v),
    })
}

fn describe(p: &crate::scene::Permissions) -> String {
    crate::permissions::describe(p)
}

/// Why not, told to the right party: the scene declares it; a plugin declares it and also has it approved.
fn denied(c: &Mutex<Shared>, what_for: &str, how: &str) -> mlua::Error {
    let c = c.lock().unwrap();
    mlua::Error::runtime(match &c.plugin {
        None => format!("the scene gives no permission to {what_for}. If it should be able to, declare it in the .plm: permissions {{ {how} }}"),
        Some(p) if c.unapproved => format!("plugin '{p}' wants to {what_for}, but nobody has approved its permissions: it runs with none. To see them and decide: pleamar --approve SCENE"),
        Some(p) => format!("plugin '{p}' has no permission to {what_for}. If it should be able to, declare it in its own .plm (the scene's do not count): permissions {{ {how} }}"),
    })
}

/// `{ env = { LC_ALL = "C" } }`: variables for that program only, on top of
/// the scene's own. A program that speaks the user's language is read in English.
/// Not any: one that decides which code runs (`PATH`, `LD_PRELOAD`, `NODE_OPTIONS`)
/// would make `run: "node tools/usage.js"` run something else, so it has to be
/// named in the permissions; and a plugin, approved by someone else, sets only
/// the harmless ones unless it names more (`crate::permissions::env_refused`).
fn environment(c: &Mutex<Shared>, how: Option<&mlua::Table>) -> mlua::Result<Vec<(String, String)>> {
    let vars: Vec<(String, String)> = how
        .and_then(|t| t.get::<Option<mlua::Table>>("env").ok().flatten())
        .map(|t| t.pairs::<String, String>().filter_map(Result::ok).collect())
        .unwrap_or_default();
    if vars.is_empty() {
        return Ok(vars);
    }
    let (declared, plugin) = {
        let c = c.lock().unwrap();
        (c.permissions.env.clone(), c.plugin.is_some())
    };
    for (name, _) in &vars {
        if let Some(why) = crate::permissions::env_refused(&declared, plugin, name) {
            return Err(denied(c, &format!("set {name} for a program ({why})"), &format!("env: \"{name}\"")));
        }
    }
    Ok(vars)
}

/// Writes `text` on the child's input and closes it, so it knows nothing more is
/// coming. On a thread of its own: a program that writes before reading could fill
/// its output while this waited to hand it the input.
/// The input of a program started with `{ stdin = "open" }`: what `write` hands
/// over goes to a thread of its own, which writes it while the logic goes on.
/// Written with the logic's lock held, a program that stopped reading filled
/// its pipe (64 KiB) and froze the logic (M24).
struct Writer {
    queue: std::sync::mpsc::Sender<Vec<u8>>,
    /// Bytes handed over and not yet written.
    waiting: Arc<std::sync::atomic::AtomicUsize>,
}

/// More than this waiting to be written, and `write` says false: the program is not reading.
const MOST_WAITING: usize = 8 << 20;

impl Writer {
    fn new(mut stdin: std::process::ChildStdin) -> Writer {
        let (queue, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let waiting = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let left = waiting.clone();
        std::thread::spawn(move || {
            use std::io::Write;
            // Until the logic drops its end (`write(id)`, `kill`, the program gone): then the
            // input closes, after what was waiting, and the program reads its end.
            for bytes in rx {
                let n = bytes.len();
                let ok = stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_ok();
                left.fetch_sub(n, std::sync::atomic::Ordering::Relaxed);
                if !ok {
                    break;
                }
            }
        });
        Writer { queue, waiting }
    }

    /// Hands it over; false if the program is gone or has too much waiting already.
    fn write(&self, bytes: Vec<u8>) -> bool {
        use std::sync::atomic::Ordering;
        if self.waiting.load(Ordering::Relaxed) + bytes.len() > MOST_WAITING {
            return false;
        }
        self.waiting.fetch_add(bytes.len(), Ordering::Relaxed);
        self.queue.send(bytes).is_ok()
    }
}

/// Where a program the logic starts runs: the scene's folder, or `{ cwd = "tools" }`
/// (relative to it, or absolute).
fn working_folder(scene: &std::path::Path, how: Option<&mlua::Table>) -> std::path::PathBuf {
    match how.and_then(|t| t.get::<Option<String>>("cwd").ok().flatten()) {
        Some(d) if std::path::Path::new(&d).is_absolute() => std::path::PathBuf::from(d),
        Some(d) => scene.join(d),
        None => scene.to_path_buf(),
    }
}

/// The longest line read from a program: past it, the rest of the line is left out.
const LONGEST_LINE: usize = 16 << 20;

/// A program's output, line by line, to `each` (which says whether to go on): a byte
/// that is not UTF-8 is a replacement character, not the end of the reading —it
/// stopped there in silence, the program alive and nothing more arriving (M25)—, and
/// a line longer than `LONGEST_LINE` keeps its beginning.
fn each_line(from: impl std::io::Read, mut each: impl FnMut(String) -> bool) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(from);
    let mut line: Vec<u8> = Vec::new();
    let mut warned = false;
    loop {
        line.clear();
        let mut cut = false;
        // Read up to the line's end, keeping no more than the longest.
        loop {
            let (done, used) = {
                let buf = match reader.fill_buf() {
                    Ok(b) => b,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return,
                };
                if buf.is_empty() {
                    (true, 0)
                } else {
                    let (take, done) = match buf.iter().position(|b| *b == b'\n') {
                        Some(k) => (k + 1, true),
                        None => (buf.len(), false),
                    };
                    let room = LONGEST_LINE.saturating_sub(line.len());
                    if room < take {
                        cut = true;
                    }
                    line.extend_from_slice(&buf[..take.min(room)]);
                    (done, take)
                }
            };
            if used == 0 {
                // The end of what it writes: its last line, if it had no line break.
                if !line.is_empty() {
                    each(String::from_utf8_lossy(&line).into_owned());
                }
                return;
            }
            reader.consume(used);
            if done {
                break;
            }
        }
        if cut && !std::mem::replace(&mut warned, true) {
            eprintln!("logic  · a program wrote a line longer than {} MiB: its rest is left out", LONGEST_LINE >> 20);
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if !each(String::from_utf8_lossy(&line).into_owned()) {
            return;
        }
    }
}

fn give_input(child: &mut std::process::Child, text: Option<String>) {
    let (Some(text), Some(mut stdin)) = (text, child.stdin.take()) else { return };
    std::thread::spawn(move || {
        use std::io::Write;
        let _ = stdin.write_all(text.as_bytes());
    });
}

/// Whether a declared command allows this one. A bare name (`"date"`) allows
/// the program with any arguments; with words after it (`"node tools/usage.js"`)
/// it allows the program only when its arguments begin with those words: an
/// interpreter, declared with its script, runs that script and nothing else.
pub(crate) fn command_allowed(declared: &[String], command: &str, args: &[String]) -> bool {
    declared.iter().any(|d| {
        let mut words = d.split_whitespace();
        if words.next() != Some(command) {
            return false;
        }
        let rest: Vec<&str> = words.collect();
        rest.len() <= args.len() && rest.iter().zip(args).all(|(w, a)| *w == a)
    })
}

fn check_command_permission(c: &Mutex<Shared>, command: &str, args: &[String]) -> mlua::Result<()> {
    let declared = c.lock().unwrap().permissions.commands.clone();
    if command_allowed(&declared, command, args) {
        return Ok(());
    }
    // Declared with other arguments: said, so the fix is plain.
    if let Some(d) = declared.iter().find(|d| d.split_whitespace().next() == Some(command)) {
        return Err(mlua::Error::runtime(format!("'{command}' is allowed only as `{d}…`, not with {:?}", args)));
    }
    Err(denied(c, &format!("run '{command}'"), &format!("run: \"{command}\"")))
}

/// The same for a service. **Listening is not commanding**: `services: "audio"` lets you know the
/// volume (`sys.watch`, `sys.ask`); changing it takes `"audio.volume"`, or `"audio.*"`.
fn check_service_permission(c: &Mutex<Shared>, name: &str, commands: bool) -> mlua::Result<()> {
    let service = name.split(['.', ':']).next().unwrap_or(name);
    let has = |what: &str| c.lock().unwrap().permissions.services.iter().any(|s| s == what);
    if commands {
        if has(name) || has(&format!("{service}.*")) {
            return Ok(());
        }
        // Saving the copied picture to a file is reading the clipboard, as
        // `sys.ask("clipboard.image")` is: the same permission covers both.
        if name == "clipboard.image.save" && has("clipboard") {
            return Ok(());
        }
        return Err(denied(c, &format!("ask the system for '{name}'"), &format!("services: \"{name}\"  (or \"{service}.*\" for everything of {service})")));
    }
    if has(service) || has(&format!("{service}.*")) {
        return Ok(());
    }
    Err(denied(c, &format!("use the '{service}' service"), &format!("services: \"{service}\"")))
}

/// May this logic tell that scene something? `services: "say:bar"` lets it
/// talk to `bar`, `"say:*"` to any. By the scene, not by the command: what a
/// scene does with an order is up to it, as it is with `--say`.
pub(crate) fn say_allowed(declared: &[String], scene: &str) -> bool {
    declared.iter().any(|d| d == "say:*" || d.strip_prefix("say:") == Some(scene))
}

fn check_say_permission(c: &Mutex<Shared>, scene: &str) -> mlua::Result<()> {
    if say_allowed(&c.lock().unwrap().permissions.services, scene) {
        return Ok(());
    }
    Err(denied(c, &format!("talk to the scene '{scene}'"), &format!("services: \"say:{scene}\"  (or \"say:*\" for any scene)")))
}

/// What the logic hands to the system: numbers, texts and tables. A table with
/// keys 1..n is a list; any other, a map. Up to six levels: deeper than
/// that, what is being passed is not data.
fn lua_to_value(v: &Value, depth: usize) -> SysValue {
    match v {
        Value::Boolean(b) => SysValue::Bool(*b),
        Value::Integer(i) => SysValue::Num(*i as f64),
        Value::Number(n) => SysValue::Num(*n),
        Value::String(s) => SysValue::Text(s.to_string_lossy()),
        Value::Table(t) if depth < 6 => {
            let length = t.raw_len();
            let list: Vec<SysValue> = (1..=length).map(|k| lua_to_value(&t.raw_get::<Value>(k).unwrap_or(Value::Nil), depth + 1)).collect();
            // With part list and part map, the map wins: nothing is lost.
            let mut map: Vec<(String, SysValue)> = Vec::new();
            for pair in t.clone().pairs::<Value, Value>().flatten() {
                if let Value::String(k) = pair.0 {
                    map.push((k.to_string_lossy(), lua_to_value(&pair.1, depth + 1)));
                }
            }
            if map.is_empty() { SysValue::List(list) } else { SysValue::Map(map) }
        }
        _ => SysValue::Null,
    }
}

/// A piece of system data, as Luau sees it: tables, numbers, texts.
fn value_to_lua(lua: &Lua, v: &SysValue) -> mlua::Result<Value> {
    Ok(match v {
        SysValue::Null => Value::Nil,
        SysValue::Bool(b) => Value::Boolean(*b),
        SysValue::Num(n) => Value::Number(*n),
        SysValue::Text(s) => Value::String(lua.create_string(s)?),
        SysValue::List(l) => {
            let t = lua.create_table()?;
            for (k, x) in l.iter().enumerate() {
                t.set(k + 1, value_to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
        SysValue::Map(m) => {
            let t = lua.create_table()?;
            for (k, x) in m {
                t.set(k.as_str(), value_to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
    })
}

fn did_you_mean<'a>(k: &str, known: impl Iterator<Item = &'a String>) -> String {
    crate::language::closest_match(k, known).map_or(String::new(), |p| format!(". Did you mean '{p}'?"))
}

pub struct LuauScript {
    scene: String,
    logic: String,
    tx: Sender<ToRender>,
    to_logic: Sender<Event>,
    blocked: Arc<AtomicBool>,
    lua: Option<Lua>,
    c: Arc<Mutex<Shared>>,
    /// If it is a plugin's logic: its name. Everything it names goes under it (`Clock.now`),
    /// so it cannot touch —or hear— anything that is not its own.
    prefix: Option<String>,
    /// Whether it listens to every frame now: the scene's logic reads its
    /// plugins' to know which of them to tell.
    wants_frames: Arc<AtomicBool>,
    /// The scene's logic carries its plugins' logic with it: each one, its own Luau
    /// state **and its own thread**. One that gets stuck does not hold up the others, or the scene.
    plugins: Vec<LivePlugin>,
    /// If it is a plugin: what the scene says about it (its logic, what it asks for).
    definition: Option<crate::scene::Plugin>,
    frontier: Frontier,
    /// A scene brought in's own mailbox (G22): what its services report comes straight to it.
    mailbox: Option<Sender<Event>>,
}

/// Who hears what a logic writes on a plugin's boundary. A plugin's logic, the
/// scene's (its owner); the scene's, the plugin whose name is in front.
#[derive(Clone, Default)]
struct Frontier {
    owner: Option<Sender<Event>>,
    /// In a scene brought in: its name, sent with what crosses (`Event::BoundaryFrom`).
    sender: Option<String>,
    plugins: Arc<Mutex<Vec<(String, Sender<Event>)>>>,
}

impl Frontier {
    fn cross(&self, name: &str, what: impl FnOnce() -> crate::scene::Crossing) {
        let to = match &self.owner {
            Some(owner) => Some(owner.clone()),
            None => {
                let plugins = self.plugins.lock().unwrap();
                name.split_once('.').and_then(|(p, _)| plugins.iter().find(|(n, _)| n == p).map(|(_, m)| m.clone()))
            }
        };
        if let Some(to) = to {
            let _ = to.send(match &self.sender {
                Some(s) => Event::BoundaryFrom(s.clone(), Box::new(what())),
                None => Event::Boundary(Box::new(what())),
            });
        }
    }
}

/// Where a logic keeps its models' lists, in its Luau registry.
const MODELS: &str = "pleamar.models";

/// A running plugin, seen from the scene's logic: how it is spoken to.
/// Dropping the mailbox stops it: its thread ends, and with it whatever it left running.
struct LivePlugin {
    definition: crate::scene::Plugin,
    mailbox: Sender<Event>,
    wants_frames: Arc<AtomicBool>,
}

/// Whether a name is one of the scenes' beside this one (G22): `Bar.volume` from Osd's logic.
fn neighbor_of<'a>(c: &'a Shared, k: &str) -> Option<&'a String> {
    c.neighbors.iter().find(|n| k.len() > n.len() && k.starts_with(n.as_str()) && k.as_bytes()[n.len()] == b'.')
}

/// Whether a name is a library's (`Palette.pal_primary_r`): in a scene brought in, as in a
/// scene alone, the libraries it imports are the scene's, and named whole.
fn library_of(c: &Shared, k: &str) -> bool {
    c.libraries.iter().any(|n| k.len() > n.len() && k.starts_with(n.as_str()) && k.as_bytes()[n.len()] == b'.')
}

/// The real name of what a logic names, knowing its neighbours: theirs are whole.
fn named(c: &Shared, prefix: &Option<String>, k: &str) -> String {
    match neighbor_of(c, k) {
        Some(_) => k.to_owned(),
        None if library_of(c, k) => k.to_owned(),
        None => qualified(prefix, k),
    }
}

/// A scene brought in reads the others' and does not write them (G22).
fn not_its_own(c: &Shared, prefix: &Option<String>, what: &str, k: &str) -> mlua::Result<()> {
    match neighbor_of(c, k) {
        Some(n) => Err(mlua::Error::runtime(format!("the {what} '{k}' is {n}'s: {} reads it, and tells {n} with an event (`emit(\"{n}.something\")`) to change it", prefix.as_deref().unwrap_or("this scene")))),
        None => Ok(()),
    }
}

/// The real name of what a logic names: in a plugin, under its name.
fn qualified(prefix: &Option<String>, k: &str) -> String {
    match prefix {
        Some(p) => format!("{p}.{k}"),
        None => k.to_owned(),
    }
}

/// "It does not exist", told to the right party: a plugin is told about its own things, with the name it uses.
fn not_found<'a>(prefix: &Option<String>, what: &str, full: &str, known: impl Iterator<Item = &'a String>) -> mlua::Error {
    match prefix {
        None => mlua::Error::runtime(format!("the scene has {what} called '{full}'{}", did_you_mean(full, known))),
        Some(p) => {
            let short = full.strip_prefix(&format!("{p}.")).unwrap_or(full);
            let theirs: Vec<String> = known.filter_map(|k| k.strip_prefix(&format!("{p}.")).map(str::to_owned)).collect();
            mlua::Error::runtime(format!("plugin '{p}' has {what} called '{short}'{}. A plugin only sees what its library declares", did_you_mean(short, theirs.iter())))
        }
    }
}

/// The same for what is listened to: `fact:ticking` is `fact:Clock.ticking`, and `tapped`, `Clock.tapped`.
/// That way a plugin does not hear the keyboard, or the mouse, or the scene's events: only its own.
fn qualified_listener(prefix: &Option<String>, what: &str) -> String {
    match (prefix, what.split_once(':')) {
        (None, _) => what.to_owned(),
        (Some(p), Some((class, name))) => format!("{class}:{p}.{name}"),
        (Some(p), None) => format!("{p}.{what}"),
    }
}

impl LuauScript {
    pub fn new(scene: &str, logic: &str, tx: Sender<ToRender>, to_logic: Sender<Event>, blocked: Arc<AtomicBool>) -> Self {
        LuauScript { scene: scene.to_owned(), logic: logic.to_owned(), tx, to_logic, blocked, lua: None, c: Arc::default(), prefix: None, wants_frames: Arc::default(), plugins: Vec::new(), definition: None, frontier: Frontier::default(), mailbox: None }
    }

    /// A plugin's logic. The number is so that its timers and processes are not
    /// named the same as another logic's: they all share the mailbox.
    fn for_plugin(&self, p: &crate::scene::Plugin, number: usize) -> Self {
        // A scene brought in is a scene, not a library's plugin: its permissions are a scene's.
        let c = Shared { next: (number as u32 + 1) * 1_000_000, plugin: (!p.brought).then(|| p.name.clone()), ..Default::default() };
        LuauScript { scene: self.scene.clone(), logic: p.logic.to_string_lossy().into_owned(), tx: self.tx.clone(), to_logic: self.to_logic.clone(), blocked: self.blocked.clone(), lua: None, c: Arc::new(Mutex::new(c)), prefix: Some(p.name.clone()), wants_frames: Arc::default(), plugins: Vec::new(), definition: Some(p.clone()), frontier: Frontier { owner: Some(self.to_logic.clone()), plugins: Arc::default(), sender: p.brought.then(|| p.name.clone()) }, mailbox: None }
    }

    /// Sets a plugin running on its thread. From there it tends its mailbox and its timers.
    fn start(mut plugin: LuauScript) -> LivePlugin {
        let (mailbox, letters) = std::sync::mpsc::channel::<Event>();
        plugin.mailbox = Some(mailbox.clone()).filter(|_| plugin.brought());
        let definition = plugin.definition.clone().expect("only a plugin's logic is started");
        let name = format!("plugin {}", definition.name);
        let wants_frames = plugin.wants_frames.clone();
        let _ = std::thread::Builder::new().name(name).spawn(move || {
            let mut ctx = Context::for_plugin(plugin.tx.clone(), plugin.blocked.clone());
            loop {
                let wait = plugin.next_deadline().map_or(Duration::from_secs(3600), |p| p.saturating_duration_since(crate::clock::now()));
                match letters.recv_timeout(wait) {
                    Ok(e) => plugin.on_event(e, &mut ctx),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if plugin.next_deadline().is_some_and(|p| p <= crate::clock::now()) {
                    plugin.tick(&mut ctx);
                }
            }
            plugin.release();
        });
        LivePlugin { definition, mailbox, wants_frames }
    }

    /// Stops what this logic left running: timers, handlers, processes.
    fn release(&self) {
        let mut c = self.c.lock().unwrap();
        if std::mem::take(&mut c.frames) {
            crate::scene::FRAME_LISTENERS.fetch_sub(1, Ordering::AcqRel);
            self.wants_frames.store(false, Ordering::Relaxed);
        }
        // What it left sounding —a tune that repeats— goes with it.
        for id in c.sounds.drain(..) {
            crate::platform::sound::stop(Some(id));
        }
        c.handlers.clear();
        c.timers.clear();
        c.processes.clear();
        c.watchers.clear();
        for (_, (_, child)) in c.running.drain() {
            if let Some(mut h) = child.lock().unwrap().take() {
                let _ = h.kill();
                let _ = h.wait();
            }
        }
    }

    /// What the scene has, seen from this logic. A plugin sees all of it on the inside
    /// —it is checked against it—, but can only name what starts with its name.
    /// Whose the files are: the scene's, by the name of its `.plm`; or the plugin's, by its own.
    fn owner(&self) -> String {
        match &self.prefix {
            // A scene brought in keeps the folder it has alone, its file's (`calima`).
            Some(_) if self.brought() => std::path::Path::new(&self.logic).file_stem().map_or_else(|| "scene".to_owned(), |n| n.to_string_lossy().into_owned()),
            Some(p) => p.clone(),
            None => std::path::Path::new(&self.scene)
                .file_stem()
                .map_or_else(|| "scene".to_owned(), |n| n.to_string_lossy().into_owned()),
        }
    }

    /// The logic of a scene brought into the one opened (G22), not a library's plugin.
    fn brought(&self) -> bool {
        self.definition.as_ref().is_some_and(|d| d.brought)
    }

    fn learn(&self, e: &Scene, permissions: crate::scene::Permissions) {
        let mut c = self.c.lock().unwrap();
        c.facts = e.facts.iter().map(|(n, v)| (n.to_string(), *v as f64)).collect();
        c.texts = e.texts.iter().map(|(n, v)| (n.to_string(), v.clone())).collect();
        c.permissions = crate::permissions::granted(permissions);
        c.models = e.models.clone();
        c.types = e.types.iter().cloned().collect();
        c.signals = e.signals.iter().map(|s| s.0.to_owned()).collect();
        c.services = e.services.clone();
        c.translations = e.translations.clone();
        c.neighbors = match &self.definition {
            Some(d) if d.brought => e.scenes.iter().filter(|n| **n != d.name).cloned().collect(),
            _ => Vec::new(),
        };
        c.libraries = if self.brought() { e.libraries.clone() } else { Vec::new() };
    }

    /// Whether this scene is asked what a Bluetooth pairing asks. Read again
    /// with each scene, so one that stops declaring it stops being asked.
    fn offer_pairing(&self) {
        if self.prefix.is_some() && !self.brought() {
            return;
        }
        let who = self.owner();
        // A pairing asks only a scene that says it shows what is asked —it
        // declares `pairing`— and may answer it. Watching Bluetooth is not
        // saying that: a bar's icon would turn a pairing that just works into a
        // question nobody sees. Only the scene's own declaration counts: a
        // plugin's `bluetooth` lands in the same list under its name, and
        // importing one must not turn the scene's pairing into questions.
        let plugins: Vec<String> = self.plugins.iter().map(|p| format!("{}.", p.definition.name)).collect();
        let pairing = {
            let c = self.c.lock().unwrap();
            let declared = c.services.iter().any(|s| {
                s.name == "bluetooth" && !plugins.iter().any(|p| s.alias.starts_with(p.as_str())) && s.fields.iter().any(|f| f.name == "pairing")
            });
            let has = |what: &str| c.permissions.services.iter().any(|x| x == what);
            let may = has("bluetooth.*") || ["bluetooth.answer", "bluetooth.confirm", "bluetooth.cancel"].iter().all(|w| has(w));
            if declared && !may {
                eprintln!("logic  · `pairing` is shown but cannot be answered: add `services: \"bluetooth.*\"` to this scene's `permissions`");
            }
            declared && may
        };
        crate::platform::bluetooth_pairing(&who, pairing);
    }

    /// The services the scene asked for with `service`: one per thread, and whatever they report
    /// is spread on its own over the facts and texts that carry their name in front.
    /// The permissions are the scene's: `services: "clock"` for `service clock`.
    fn subscribe_services(&mut self) {
        // A plugin does not set up the scene's services: whatever it declares goes with the rest,
        // and asking for them is up to whoever owns the frontier.
        // A scene brought in (G22) sets up its own, with its permissions and its name: the
        // scene that brings it in, the rest.
        if self.prefix.is_some() && !self.brought() {
            return;
        }
        let who = self.owner();
        self.offer_pairing();
        let mine = |alias: &str| match &self.prefix {
            Some(p) => alias.starts_with(&format!("{p}.")),
            None => !self.plugins.iter().any(|b| b.definition.brought && alias.starts_with(&format!("{}.", b.definition.name))),
        };
        let pending: Vec<crate::scene::Service> = {
            let c = self.c.lock().unwrap();
            c.services.iter().filter(|s| !c.subscribed.contains(&s.alias) && mine(&s.alias)).cloned().collect()
        };
        for s in pending {
            let root = s.name.split(['.', ':']).next().unwrap_or(&s.name).to_owned();
            let has = {
                let c = self.c.lock().unwrap();
                c.permissions.services.iter().any(|x| *x == root || *x == format!("{root}.*"))
            };
            if !has {
                eprintln!("logic  · `service {}` needs permission: add `services: \"{root}\"` to this scene's `permissions`", s.name);
                continue;
            }
            self.c.lock().unwrap().subscribed.insert(s.alias.clone());
            // What it reports goes to whoever asked: a scene brought in hears it in its own mailbox.
            let (to_logic, alias) = (Mutex::new(self.mailbox.clone().unwrap_or_else(|| self.to_logic.clone())), s.alias.clone());
            if !crate::platform::service(&who, &s.name, &format!("service:{}", s.alias), Box::new(move |v| {
                let _ = to_logic.lock().unwrap().send(Event::Data(format!("service:{alias}"), v));
            })) {
                eprintln!("logic  · the '{}' service is not available here: '{}' stays as the scene left it", s.name, s.alias);
            }
        }
    }

    /// What a service reports, spread over the fields the scene asked for. Whatever
    /// does not come stays as it was: a service may report only what changed.
    fn spread_service(&self, alias: &str, value: &SysValue) {
        // A service that is only a list (`tray`, `apps`, `notifications`) is its `list`.
        let whole;
        let value = match value {
            SysValue::List(_) => {
                whole = SysValue::Map(vec![("list".into(), value.clone())]);
                &whole
            }
            v => v,
        };
        let SysValue::Map(fields) = value else { return };
        let (theirs, bindings) = {
            let c = self.c.lock().unwrap();
            let Some(s) = c.services.iter().find(|s| s.alias == alias) else { return };
            (s.fields.clone(), s.models.clone())
        };
        // Its lists, into the models the scene declared for them: record by record,
        // by field name, as `model.x = …` would from the logic.
        for (field, model) in &bindings {
            let list = fields.iter().find(|(k, _)| k == field).map(|(_, v)| v.clone()).unwrap_or(SysValue::List(Vec::new()));
            let owned;
            let lua = match &self.lua {
                Some(l) => l,
                None => {
                    owned = Lua::new();
                    &owned
                }
            };
            let Ok(Value::Table(t)) = value_to_lua(lua, &list) else { continue };
            let mut c = self.c.lock().unwrap();
            let Some(m) = c.models.iter().find(|m| &m.name == model).cloned() else { continue };
            if let Err(e) = spread_list(&mut c, &self.tx, model, &m, &t) {
                eprintln!("logic  · '{alias}.{field}' into '{model}': {e}");
            }
        }
        for field in &theirs {
            let Some((_, v)) = fields.iter().find(|(k, _)| *k == field.name) else { continue };
            let full = format!("{alias}.{}", field.name);
            match &field.kind {
                crate::scene::FieldType::Text | crate::scene::FieldType::Image(..) => {
                    let t = match v {
                        SysValue::Text(t) => t.clone(),
                        SysValue::Num(n) => if n.fract() == 0.0 { format!("{n:.0}") } else { format!("{n}") },
                        SysValue::Bool(b) => b.to_string(),
                        _ => continue,
                    };
                    let mut c = self.c.lock().unwrap();
                    if c.texts.get(&full).is_some_and(|already| *already == t) {
                        continue;
                    }
                    c.texts.insert(full.clone(), t.clone());
                    drop(c);
                    let _ = self.tx.send(ToRender::Text(intern(&full), t));
                }
                kind => {
                    let n = match (v, kind) {
                        (SysValue::Num(n), _) => *n,
                        (SysValue::Bool(b), _) => *b as u8 as f64,
                        // An enumeration arrives by its name: `kind = "wifi"`.
                        (SysValue::Text(t), crate::scene::FieldType::Enum(names)) => match names.iter().position(|x| x == t) {
                            Some(k) => k as f64,
                            None => continue,
                        },
                        (SysValue::Text(t), _) => match t.parse() {
                            Ok(n) => n,
                            Err(_) => continue,
                        },
                        _ => continue,
                    };
                    let mut c = self.c.lock().unwrap();
                    if c.facts.get(&full).is_some_and(|already| *already == n) {
                        continue;
                    }
                    c.facts.insert(full.clone(), n);
                    drop(c);
                    let _ = self.tx.send(ToRender::Fact(intern(&full), n as f32));
                }
            }
        }
    }

    /// A fresh Luau state, with the frontier in place, and the file executed.
    ///
    /// All or nothing: the new one is set up on a state of its own, and only
    /// if it runs does it take the old one's place —and the old one stops—. If
    /// it fails halfway, what it had started (its timers, what it listens to,
    /// its processes) goes with it and the old one goes on as it was. Before,
    /// the old one was stopped first, and a failed reload left timers pointing
    /// at a Lua already gone: the next one to fire brought the thread down.
    fn load_script(&mut self) {
        // Said before loading: if the script trips over a permission it does not have, let it be known why.
        if let (Some(p), true) = (&self.prefix, self.c.lock().unwrap().unapproved) {
            println!("logic  · plugin '{p}' · ⚠ NOT APPROVED: runs unable to touch the system. To see what it asks for and decide: pleamar --approve {}", self.scene);
        }
        // A scene may have no logic of its own and yet have plugins that do.
        if self.prefix.is_none() && !std::path::Path::new(&self.logic).is_file() {
            self.release();
            return;
        }
        let source = match std::fs::read_to_string(&self.logic) {
            Ok(f) => f,
            Err(e) => return eprintln!("logic  · {}: {e}", self.logic),
        };
        let t0 = Instant::now();
        let old = self.c.clone();
        let fresh = old.lock().unwrap().fresh();
        self.c = Arc::new(Mutex::new(fresh));
        match self.prepare().and_then(|lua| {
            self.c.lock().unwrap().deadline = Some(Instant::now() + PATIENCE);
            lua.load(&source).set_name(format!("@{}", self.logic)).exec()?;
            Ok(lua)
        }) {
            Ok(lua) => {
                // The old one stops now, and this one takes its place.
                let new = std::mem::replace(&mut self.c, old);
                self.release();
                self.c = new;
                self.lua = Some(lua);
                let c = self.c.lock().unwrap();
                println!("logic  · {} running in {:.1} ms · {} handlers, {} timers", self.logic, t0.elapsed().as_secs_f32() * 1000.0, c.handlers.values().map(Vec::len).sum::<usize>(), c.timers.len());
                match &self.prefix {
                    Some(p) => println!("logic  · plugin '{p}' · permissions · {}", describe(&c.permissions)),
                    None => println!("logic  · permissions · {}", describe(&c.permissions)),
                }
            }
            Err(e) => {
                // What the failed one started goes with it; the old one, as it was.
                self.release();
                self.c = old;
                eprintln!("logic  · the previous one stays as it was:\n{e}");
            }
        }
        self.c.lock().unwrap().deadline = None;
    }

    fn prepare(&self) -> mlua::Result<Lua> {
        let lua = Lua::new();
        lua.set_memory_limit(MEMORY)?;
        // In the sandbox, Luau assumes globals do not change and reads
        // `fact.open` ONCE, when loading the script. These do change: it has to be told.
        lua.set_compiler(mlua::chunk::Compiler::new().set_mutable_globals(["fact", "text", "model", "sys", "mouse"]));
        let g = lua.globals();

        // A handler that never finishes cannot keep the thread forever.
        let c = self.c.clone();
        lua.set_interrupt(move |_| match c.lock().unwrap().deadline {
            Some(l) if Instant::now() > l => Err(mlua::Error::runtime(format!("a handler has been running for more than {} s: cut off", PATIENCE.as_secs()))),
            _ => Ok(VmState::Continue),
        });

        // fact.open = true · fact.open
        let (tx, c) = (self.tx.clone(), self.c.clone());
        let pre = self.prefix.clone();
        let frontier = self.frontier.clone();
        let set = lua.create_function(move |_, (_, k, v): (Table, String, Value)| {
            not_its_own(&c.lock().unwrap(), &pre, "fact", &k)?;
            let k = named(&c.lock().unwrap(), &pre, &k);
            // A misspelled name is an error here, with its line, and not a warning lost in the render.
            if !c.lock().unwrap().facts.contains_key(&k) {
                return Err(not_found(&pre, "no fact", &k, c.lock().unwrap().facts.keys()));
            }
            let kind = c.lock().unwrap().types.get(&k).cloned();
            let n = to_number(&k, kind.as_ref(), &v)?;
            c.lock().unwrap().facts.insert(k.clone(), n);
            let _ = tx.send(ToRender::Fact(intern(&k), n as f32));
            frontier.cross(&k, || crate::scene::Crossing::Fact(k.clone(), n));
            Ok(())
        })?;
        let c = self.c.clone();
        let pre = self.prefix.clone();
        let get = lua.create_function(move |lua, (_, k): (Table, String)| {
            let c = c.lock().unwrap();
            let k = named(&c, &pre, &k);
            match c.facts.get(&k) {
                Some(v) => from_number(lua, c.types.get(&k), *v),
                None => Ok(Value::Nil),
            }
        })?;
        g.set("fact", Self::live_table(&lua, get, set)?)?;

        // text["notice.title"] = "…"
        let (tx, c) = (self.tx.clone(), self.c.clone());
        let pre = self.prefix.clone();
        let frontier = self.frontier.clone();
        let set = lua.create_function(move |_, (_, k, v): (Table, String, String)| {
            not_its_own(&c.lock().unwrap(), &pre, "text", &k)?;
            let k = named(&c.lock().unwrap(), &pre, &k);
            if !c.lock().unwrap().texts.contains_key(&k) {
                return Err(not_found(&pre, "no text", &k, c.lock().unwrap().texts.keys()));
            }
            c.lock().unwrap().texts.insert(k.clone(), v.clone());
            frontier.cross(&k, || crate::scene::Crossing::Text(k.clone(), v.clone()));
            let _ = tx.send(ToRender::Text(intern(&k), v));
            Ok(())
        })?;
        let c = self.c.clone();
        let pre = self.prefix.clone();
        let get = lua.create_function(move |_, (_, k): (Table, String)| {
            let c = c.lock().unwrap();
            Ok(c.texts.get(&named(&c, &pre, &k)).cloned())
        })?;
        g.set("text", Self::live_table(&lua, get, set)?)?;

        // model.rows = { { label = "Open", enabled = true }, … }: a whole list, in one go.
        // Each field of each record is on the inside a text or a fact; here they are spread out,
        // and only what has changed travels.
        let stored = lua.create_table()?;
        // (Kept where a model that crosses a plugin's boundary is put: see `Event::Boundary`.)
        lua.set_named_registry_value(MODELS, stored.clone())?;
        let (tx, c, store) = (self.tx.clone(), self.c.clone(), stored.clone());
        let pre = self.prefix.clone();
        let frontier = self.frontier.clone();
        let set = lua.create_function(move |_, (_, k, list): (Table, String, Value)| {
            not_its_own(&c.lock().unwrap(), &pre, "model", &k)?;
            let k = named(&c.lock().unwrap(), &pre, &k);
            let Value::Table(list) = list else {
                return Err(mlua::Error::runtime(format!("'model.{k}' takes a list of records: model.{k} = {{ {{ … }}, {{ … }} }}")));
            };
            let mut c = c.lock().unwrap();
            let Some(m) = c.models.iter().find(|m| m.name == k).cloned() else {
                return Err(not_found(&pre, "no model", &k, c.models.iter().map(|m| &m.name)));
            };
            spread_list(&mut c, &tx, &k, &m, &list)?;
            drop(c);
            frontier.cross(&k, || crate::scene::Crossing::Model(k.clone(), lua_to_value(&Value::Table(list.clone()), 0)));
            store.raw_set(k, list)
        })?;
        let (pre, c) = (self.prefix.clone(), self.c.clone());
        let get = lua.create_function(move |_, (_, k): (Table, String)| {
            let k = named(&c.lock().unwrap(), &pre, &k);
            stored.raw_get::<Value>(k)
        })?;
        g.set("model", Self::live_table(&lua, get, set)?)?;

        let tx = self.tx.clone();
        let (pre, c) = (self.prefix.clone(), self.c.clone());
        let frontier = self.frontier.clone();
        // emit("tapped") · emit("Weather.ask", data): what it brings reaches a logic
        // across a plugin's boundary (the scene's rules hear the event, not the data).
        g.set("emit", lua.create_function(move |_, (n, data): (String, Value)| {
            // Another scene's event is emitted whole: telling it is how one changes what is its.
            let n = named(&c.lock().unwrap(), &pre, &n);
            // An event that does not exist used to be a warning lost in the render: here it is an error, with its line.
            if !c.lock().unwrap().signals.contains(&n) {
                let known: Vec<String> = c.lock().unwrap().signals.iter().cloned().collect();
                return Err(not_found(&pre, "no event", &n, known.iter()));
            }
            frontier.cross(&n, || crate::scene::Crossing::Signal(n.clone(), lua_to_value(&data, 0)));
            Ok(tx.send(ToRender::Signal(intern(&n))).is_ok())
        })?)?;
        // focus("query") puts the text cursor in a field; focus() removes it.
        let tx = self.tx.clone();
        let pre = self.prefix.clone();
        let brought = self.brought();
        g.set("focus", lua.create_function(move |_, n: Option<String>| {
            if pre.is_some() && !brought {
                return Err(mlua::Error::runtime("a plugin does not move the text cursor: that belongs to the scene"));
            }
            // A scene brought in names its own fields: `focus("query")` is `Calima.query`.
            Ok(tx.send(ToRender::FocusField(n.map(|n| intern(&qualified(&pre, &n))))).is_ok())
        })?)?;
        let tx = self.tx.clone();
        let pre = self.prefix.clone();
        let brought = self.brought();
        g.set("play", lua.create_function(move |_, n: String| {
            if brought {
                return Ok(tx.send(ToRender::Gesture(intern(&qualified(&pre, &n)))).is_ok());
            }
            if pre.is_some() {
                return Err(mlua::Error::runtime("a plugin does not ask for gestures: gestures belong to the scene. Let it emit an event of its own, and the scene will decide"));
            }
            Ok(tx.send(ToRender::Gesture(intern(&n))).is_ok())
        })?)?;

        let c = self.c.clone();
        let pre = self.prefix.clone();
        let (tx, wants_frames) = (self.tx.clone(), self.wants_frames.clone());
        let brought = self.brought();
        g.set("on", lua.create_function(move |_, (what, f): (String, Function)| {
            let frame = what == "frame";
            let mut c = c.lock().unwrap();
            // Another scene's are heard whole: `on("Bar.rang", …)`, `on("fact:Bar.volume", …)`.
            let theirs = what.split_once(':').map_or(what.as_str(), |(_, n)| n);
            // A scene brought in hears the keyboard and the rest as it would alone: they are nobody's.
            let input = brought && matches!(what.as_str(), "key" | "key_down" | "key_up" | "focus" | "blur" | "link" | "copy" | "demo");
            let what = if input || neighbor_of(&c, theirs).is_some() || library_of(&c, theirs) { what.clone() } else { qualified_listener(&pre, &what) };
            c.handlers.entry(what).or_default().push(f);
            // Listening to every frame is what keeps them coming: the render
            // is told, and woken if it was at rest.
            if frame && !std::mem::replace(&mut c.frames, true) {
                crate::scene::FRAME_LISTENERS.fetch_add(1, Ordering::AcqRel);
                wants_frames.store(true, Ordering::Relaxed);
                let _ = tx.send(ToRender::Wake);
            }
            Ok(())
        })?)?;

        // For a logic that plays. `held("Left")`: whether that key is down now,
        // by the name the rules give it (a letter, in lower case). And the
        // mouse: `mouse.x`, `mouse.y` on the scene —nil when it is not over
        // it—, `mouse.over`, and `mouse.left`, `mouse.right`, `mouse.middle`.
        // A plugin has neither: the keyboard and the mouse are the scene's.
        let c = self.c.clone();
        g.set("held", lua.create_function(move |_, name: String| Ok(c.lock().unwrap().held.contains(&name)))?)?;
        let c = self.c.clone();
        let get = lua.create_function(move |_, (_, k): (Table, String)| {
            let (at, buttons) = c.lock().unwrap().mouse;
            Ok(match k.as_str() {
                "x" => at.map_or(Value::Nil, |p| Value::Number(p.0 as f64)),
                "y" => at.map_or(Value::Nil, |p| Value::Number(p.1 as f64)),
                "over" => Value::Boolean(at.is_some()),
                "left" => Value::Boolean(buttons & 1 != 0),
                "right" => Value::Boolean(buttons & 2 != 0),
                "middle" => Value::Boolean(buttons & 4 != 0),
                _ => return Err(mlua::Error::runtime(format!("the mouse has no '{k}': it has x, y, over, left, right and middle"))),
            })
        })?;
        let set = lua.create_function(|_, (_, k, _): (Table, String, Value)| -> mlua::Result<()> { Err(mlua::Error::runtime(format!("'mouse.{k}' is read, not written: the mouse is the user's"))) })?;
        g.set("mouse", Self::live_table(&lua, get, set)?)?;
        g.set("hit", hit_table(&lua)?)?;

        // Sound of its own, with `services: "sound"`. A file from this logic's
        // folder —a WAV or an Ogg Vorbis— or a tone made on the spot:
        //     sound.play("coin.wav", { volume = 0.8, pitch = 1.2, pan = -0.5, loop = false })
        //     sound.tone({ wave = "square", freq = 440, to = 880, ms = 120, volume = 0.5 })
        // Both answer with a number, for `sound.stop(n)`; `sound.stop()` is everything.
        let sound = lua.create_table()?;
        let folder = std::path::Path::new(&self.logic).parent().map(std::path::Path::to_owned).unwrap_or_default();
        let file = move |name: &str| -> mlua::Result<std::path::PathBuf> {
            let clean = !name.is_empty() && name.split('/').all(|t| !t.is_empty() && t != ".." && t != ".");
            if !clean || name.starts_with('/') {
                return Err(mlua::Error::runtime(format!("sound: \"{name}\" has to be a file in this logic's folder, or in one inside it")));
            }
            Ok(folder.join(name))
        };
        fn how(t: Option<&Table>) -> mlua::Result<crate::platform::sound::How> {
            let mut h = crate::platform::sound::How::default();
            if let Some(t) = t {
                let n = |k: &str, or: f32| t.get::<Option<f32>>(k).map(|v| v.unwrap_or(or));
                (h.volume, h.pitch, h.pan) = (n("volume", 1.0)?, n("pitch", 1.0)?, n("pan", 0.0)?);
                h.repeats = t.get::<Option<bool>>("loop")?.unwrap_or(false);
                h.delay = n("delay", 0.0)? / 1000.0;
            }
            Ok(h)
        }
        // What it started, for `sound.stop()` and for when it is reloaded. Only
        // the last ones are remembered: what does not repeat ends by itself.
        fn remember(c: &Arc<Mutex<Shared>>, id: u32) {
            let mut c = c.lock().unwrap();
            if c.sounds.len() >= 1024 {
                c.sounds.remove(0);
            }
            c.sounds.push(id);
        }
        let (c, path) = (self.c.clone(), file.clone());
        sound.set("play", lua.create_function(move |_, (name, t): (String, Option<Table>)| {
            check_service_permission(&c, "sound", false)?;
            let id = crate::platform::sound::play(&path(&name)?, how(t.as_ref())?).map_err(mlua::Error::runtime)?;
            remember(&c, id);
            Ok(id)
        })?)?;
        let (c, path) = (self.c.clone(), file);
        sound.set("load", lua.create_function(move |_, name: String| {
            check_service_permission(&c, "sound", false)?;
            crate::platform::sound::load(&path(&name)?).map_err(mlua::Error::runtime)
        })?)?;
        let c = self.c.clone();
        sound.set("tone", lua.create_function(move |_, t: Table| {
            use crate::platform::sound::{Tone, Wave};
            check_service_permission(&c, "sound", false)?;
            let n = |k: &str| t.get::<Option<f32>>(k);
            let wave = match t.get::<Option<String>>("wave")?.as_deref() {
                None | Some("square") => Wave::Square,
                Some("sine") => Wave::Sine,
                Some("saw") => Wave::Saw,
                Some("triangle") => Wave::Triangle,
                Some("noise") => Wave::Noise,
                Some(other) => return Err(mlua::Error::runtime(format!("sound.tone: there is no wave '{other}': sine, square, saw, triangle or noise"))),
            };
            let from = n("freq")?.unwrap_or(440.0);
            let seconds = n("ms")?.unwrap_or(120.0) / 1000.0;
            let tone = Tone {
                wave,
                from,
                to: n("to")?.unwrap_or(from),
                seconds,
                attack: n("attack")?.unwrap_or(3.0) / 1000.0,
                release: n("release")?.map_or((seconds * 0.5).min(0.08), |ms| ms / 1000.0),
                duty: n("duty")?.unwrap_or(0.5),
            };
            let id = crate::platform::sound::tone(&tone, how(Some(&t))?);
            remember(&c, id);
            Ok(id)
        })?)?;
        let c = self.c.clone();
        sound.set("stop", lua.create_function(move |_, id: Option<u32>| {
            match id {
                Some(id) => crate::platform::sound::stop(Some(id)),
                // Everything of ITS: another logic's tune is not this one's to stop.
                None => c.lock().unwrap().sounds.drain(..).for_each(|id| crate::platform::sound::stop(Some(id))),
            }
            Ok(())
        })?)?;
        let c = self.c.clone();
        sound.set("volume", lua.create_function(move |_, v: f32| {
            check_service_permission(&c, "sound", false)?;
            crate::platform::sound::volume(v);
            Ok(())
        })?)?;
        g.set("sound", sound)?;

        // A game controller, with `services: "gamepad"`: `pad(1)` is how the
        // first one is right now —`{ name, lx, ly, rx, ry, lt, rt, a, b, x, y,
        // lb, rb, back, start, up, down, left, right, … }`—, or nil if there is none.
        // (A press on its own is heard with `sys.watch("gamepad", …)`.)
        let c = self.c.clone();
        g.set("pad", lua.create_function(move |lua, n: Option<usize>| {
            check_service_permission(&c, "gamepad", false)?;
            match crate::platform::gamepad(n.unwrap_or(1)) {
                Some(v) => value_to_lua(lua, &v),
                None => Ok(Value::Nil),
            }
        })?)?;

        let schedule = |c: &Arc<Mutex<Shared>>, ms: f64, f: Function, repeats: bool| {
            let mut c = c.lock().unwrap();
            c.next += 1;
            let d = Duration::from_secs_f64(ms.max(1.0) / 1000.0);
            let id = c.next;
            c.timers.push(Timer { id, when: crate::clock::now() + d, every: repeats.then_some(d), f });
            id
        };
        let c = self.c.clone();
        g.set("after", lua.create_function(move |_, (ms, f): (f64, Function)| Ok(schedule(&c, ms, f, false)))?)?;
        let c = self.c.clone();
        g.set("every", lua.create_function(move |_, (ms, f): (f64, Function)| Ok(schedule(&c, ms, f, true)))?)?;
        let c = self.c.clone();
        g.set("cancel", lua.create_function(move |_, id: u32| {
            c.lock().unwrap().timers.retain(|t| t.id != id);
            Ok(())
        })?)?;

        // Fake work, to see that the render does not care.
        let (c, blocked) = (self.c.clone(), self.blocked.clone());
        g.set("busy", lua.create_function(move |_, ms: f64| {
            let d = Duration::from_secs_f64(ms.max(0.0) / 1000.0);
            if let Some(l) = &mut c.lock().unwrap().deadline {
                *l += d;
            }
            blocked.store(true, Ordering::Relaxed);
            let end = Instant::now() + d;
            while Instant::now() < end {
                std::hint::spin_loop();
            }
            blocked.store(false, Ordering::Relaxed);
            Ok(())
        })?)?;

        // The scene's folder: where its programs run (M23), and what a relative `cwd` starts from.
        // They ran wherever pleamar was started from, so `python3 tools/x.py` worked only from there.
        let folder: std::path::PathBuf = std::path::Path::new(&self.logic).parent().filter(|p| !p.as_os_str().is_empty()).map_or_else(|| std::path::PathBuf::from("."), std::path::Path::to_path_buf);
        // A system command: it runs on another thread and answers when it finishes.
        let (c, to_logic) = (self.c.clone(), self.to_logic.clone());
        let run_folder = folder.clone();
        // With a fourth argument, how: `{ stdin = "/path" }` gives it that file as
        // its input —what in a terminal is `command < file`, which is how an image
        // is copied to the clipboard— and `{ output = false }` does not collect what
        // it writes. The latter is not a whim: a program that stays in the background
        // (`wl-copy` does, to keep serving what was copied) inherits the output
        // pipe, and waiting for it to close it is waiting forever.
        // `{ input = "text" }` writes that text on its input instead, and closes it:
        // what has to reach it without passing through a file, its arguments or its
        // environment, which other programs can read (`sudo -S` and a password).
        // And `{ env = { LC_ALL = "C" } }`, variables of its own.
        g.set("run", lua.create_function(move |_, (command, args, f, how): (String, Option<Vec<String>>, Option<Function>, Option<mlua::Table>)| {
            check_command_permission(&c, &command, args.as_deref().unwrap_or(&[]))?;
            let input: Option<String> = how.as_ref().and_then(|t| t.get("stdin").ok());
            let text: Option<String> = how.as_ref().and_then(|t| t.get("input").ok());
            let vars = environment(&c, how.as_ref())?;
            let collect: bool = how.as_ref().and_then(|t| t.get::<Option<bool>>("output").ok().flatten()).unwrap_or(true);
            let errors: bool = how.as_ref().and_then(|t| t.get::<Option<bool>>("errors").ok().flatten()).unwrap_or(false);
            let cwd = working_folder(&run_folder, how.as_ref());
            let id = {
                let mut c = c.lock().unwrap();
                c.next += 1;
                let id = c.next;
                if let Some(f) = f {
                    c.processes.insert(id, f);
                }
                id
            };
            let to_logic = to_logic.clone();
            std::thread::spawn(move || {
                let mut launch = std::process::Command::new(&command);
                crate::child_env(&mut launch);
                launch.args(args.unwrap_or_default()).envs(vars).current_dir(&cwd);
                crate::platform::apart(&mut launch);
                // No input unless it is given one: it got the scene's, and in
                // pleamar-wm's session that is its terminal. ffmpeg read it
                // from behind and the system stopped it (SIGTTIN): a recording
                // in Bahía never ended.
                launch.stdin(std::process::Stdio::null());
                if let Some(path) = &input {
                    match std::fs::File::open(path) {
                        Ok(f) => {
                            launch.stdin(f);
                        }
                        Err(e) => {
                            let _ = to_logic.send(Event::Process(id, format!("{path}: {e}"), -1));
                            return;
                        }
                    }
                }
                if text.is_some() {
                    launch.stdin(std::process::Stdio::piped());
                }
                let (output, code) = if collect {
                    let out = launch.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().and_then(|mut child| {
                        give_input(&mut child, text);
                        child.wait_with_output()
                    });
                    match out {
                        // `{ errors = true }`: what it said on its error output too, after the rest.
                        Ok(o) if errors && !o.stderr.is_empty() => {
                            let said = format!("{}\n{}", String::from_utf8_lossy(&o.stdout).trim_end(), String::from_utf8_lossy(&o.stderr).trim_end());
                            (said.trim().to_owned(), o.status.code().unwrap_or(-1))
                        }
                        Ok(o) => (String::from_utf8_lossy(&o.stdout).trim_end().to_owned(), o.status.code().unwrap_or(-1)),
                        Err(e) => (e.to_string(), -1),
                    }
                } else if text.is_some() {
                    match launch.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().and_then(|mut child| {
                        give_input(&mut child, text);
                        child.wait()
                    }) {
                        Ok(s) => (String::new(), s.code().unwrap_or(-1)),
                        Err(e) => (e.to_string(), -1),
                    }
                } else {
                    match launch.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status() {
                        Ok(s) => (String::new(), s.code().unwrap_or(-1)),
                        Err(e) => (e.to_string(), -1),
                    }
                };
                let _ = to_logic.send(Event::Process(id, output, code));
            });
            Ok(())
        })?)?;

        // A command that never finishes —`pactl subscribe`, `playerctl --follow`—: one
        // call for every line it writes, and `kill(id)` to stop it. With a
        // fourth function, that one is called when the process HAS FINISHED, with its
        // code: asking something to stop and it having stopped are not the same, and
        // that difference is where a recording gets lost.
        // And a fifth, how: `{ input = "text" }` and `env` as with `run`, and `{ errors = true }`
        // gives what it writes on its error output too, line by line with the rest:
        // a package manager says there what went wrong.
        let (c, to_logic) = (self.c.clone(), self.to_logic.clone());
        let spawn_folder = folder.clone();
        g.set("spawn", lua.create_function(move |_, (command, args, f, on_exit, how): (String, Option<Vec<String>>, Function, Option<Function>, Option<mlua::Table>)| {
            check_command_permission(&c, &command, args.as_deref().unwrap_or(&[]))?;
            let text: Option<String> = how.as_ref().and_then(|t| t.get("input").ok());
            let errors: bool = how.as_ref().and_then(|t| t.get::<Option<bool>>("errors").ok().flatten()).unwrap_or(false);
            let open: bool = how.as_ref().and_then(|t| t.get::<Option<String>>("stdin").ok().flatten()).is_some_and(|v| v == "open");
            let vars = environment(&c, how.as_ref())?;
            let mut launch = std::process::Command::new(&command);
            crate::child_env(&mut launch);
            launch.args(args.unwrap_or_default()).envs(vars).current_dir(working_folder(&spawn_folder, how.as_ref())).stdout(std::process::Stdio::piped());
            launch.stderr(if errors { std::process::Stdio::piped() } else { std::process::Stdio::null() });
            // No input unless it is given one, as with `run`.
            launch.stdin(std::process::Stdio::null());
            if text.is_some() || open {
                launch.stdin(std::process::Stdio::piped());
            }
            // If the program gets killed, this goes with it.
            crate::platform::die_with_parent(&mut launch);
            crate::platform::apart(&mut launch);
            let mut child = launch
                .spawn()
                .map_err(|e| mlua::Error::runtime(format!("cannot run '{command}': {e}")))?;
            // Kept open: the first text goes now, the rest with `write`.
            let kept = if open {
                // Its own writer: neither this first text nor any later one waits for it to read.
                let writer = child.stdin.take().map(Writer::new);
                if let (Some(t), Some(w)) = (&text, writer.as_ref()) {
                    w.write(t.as_bytes().to_vec());
                }
                writer
            } else {
                give_input(&mut child, text);
                None
            };
            let output = child.stdout.take();
            let complaints = child.stderr.take();
            let child = Arc::new(Mutex::new(Some(child)));
            CHILDREN.lock().unwrap().push(child.clone());
            let id = {
                let mut c = c.lock().unwrap();
                c.next += 1;
                let id = c.next;
                c.running.insert(id, (f, child.clone()));
                if let Some(writer) = kept {
                    c.inputs.insert(id, writer);
                }
                if let Some(end) = on_exit {
                    c.processes.insert(id, end);
                }
                id
            };
            let to_logic = to_logic.clone();
            // Its error output on a thread of its own: read after the other, a
            // program that fills that pipe while the other is still open blocks.
            let complained = complaints.map(|s| {
                let to_logic = to_logic.clone();
                std::thread::spawn(move || each_line(s, |line| to_logic.send(Event::Line(id, line)).is_ok()))
            });
            std::thread::spawn(move || {
                if let Some(s) = output {
                    each_line(s, |line| to_logic.send(Event::Line(id, line)).is_ok());
                }
                // Every line before the end.
                if let Some(t) = complained {
                    let _ = t.join();
                }
                let code = child.lock().unwrap().take().and_then(|mut h| h.wait().ok()).and_then(|s| s.code()).unwrap_or(-1);
                let _ = to_logic.send(Event::Process(id, String::new(), code));
            });
            Ok(id)
        })?)?;
        // `write(id, "line\n")`: to the input of one started with `{ stdin = "open" }`.
        // `write(id)` closes it (the program reads its end). Whether it got there.
        let c = self.c.clone();
        // It never waits for the program to read: what it says goes to the program's own
        // writer (see `Writer`), and false is gone, or more than 8 MiB already waiting.
        g.set("write", lua.create_function(move |_, (id, text): (u32, Option<mlua::LuaString>)| {
            let mut c = c.lock().unwrap();
            let Some(text) = text else {
                return Ok(c.inputs.remove(&id).is_some());
            };
            let Some(writer) = c.inputs.get(&id) else { return Ok(false) };
            let ok = writer.write(text.as_bytes().to_vec());
            if !ok && writer.queue.send(Vec::new()).is_err() {
                c.inputs.remove(&id);
            }
            Ok(ok)
        })?)?;
        // `kill(id)` kills it and forgets about it. `kill(id, "int")` —or `"term"`— ASKS
        // it: it sends it the signal and keeps track of it until it leaves
        // on its own. A recorder that gets killed leaves an MP4 with no index, which
        // nobody can open; with SIGINT it writes it and exits.
        let c = self.c.clone();
        g.set("kill", lua.create_function(move |_, (id, how): (u32, Option<String>)| {
            let signal = match how.as_deref() {
                None => None,
                Some("int") => Some(libc::SIGINT),
                Some("term") => Some(libc::SIGTERM),
                Some(other) => return Err(mlua::Error::runtime(format!("kill(id, \"{other}\"): it is \"int\" or \"term\" to ask, or nothing to kill"))),
            };
            let mut c = c.lock().unwrap();
            match signal {
                Some(signal) => {
                    if let Some((_, child)) = c.running.get(&id) {
                        if let Some(h) = child.lock().unwrap().as_ref() {
                            #[cfg(unix)]
                            unsafe {
                                libc::kill(h.id() as libc::pid_t, signal);
                            }
                            let _ = (h, signal);
                        }
                    }
                }
                None => {
                    c.processes.remove(&id);
                    c.inputs.remove(&id);
                    if let Some((_, child)) = c.running.remove(&id) {
                        if let Some(mut h) = child.lock().unwrap().take() {
                            // Reaped here: its reader thread finds nothing
                            // left to wait for, and unwaited it stayed a zombie.
                            let _ = h.kill();
                            let _ = h.wait();
                        }
                    }
                }
            }
            Ok(())
        })?)?;

        // What happens in the system, by a name that is the same everywhere.
        let sys = lua.create_table()?;
        let who = self.owner();
        let (c, to_logic, mine) = (self.c.clone(), self.to_logic.clone(), who.clone());
        sys.set("watch", lua.create_function(move |_, (name, f): (String, Function)| {
            check_service_permission(&c, &name, false)?;
            let first = {
                let mut c = c.lock().unwrap();
                let v = c.watchers.entry(name.clone()).or_default();
                v.push(f);
                v.len() == 1
            };
            if !first {
                return Ok(true);
            }
            let (to_logic, n) = (Mutex::new(to_logic.clone()), name.clone());
            Ok(crate::platform::service(&mine, &name, "watch", Box::new(move |v| {
                let _ = to_logic.lock().unwrap().send(Event::Data(n.clone(), v));
            })))
        })?)?;
        let (c, mine) = (self.c.clone(), who.clone());
        let tx = self.tx.clone();
        sys.set("call", lua.create_function(move |_, (name, args): (String, mlua::Variadic<Value>)| {
            // `sys.call("image.data", "me", bytes)`: a picture of the scene's own, from
            // memory (`image me = data "me", 88, 88`). Its bytes as they are, or base64.
            if name == "image.data" {
                let (Some(Value::String(key)), Some(Value::String(bytes))) = (args.first(), args.get(1)) else {
                    return Err(mlua::Error::runtime("`image.data` takes the image's name and its bytes: sys.call(\"image.data\", \"me\", png)"));
                };
                let key = key.to_string_lossy().to_string();
                let version = crate::text::put_data_image(&key, crate::text::picture_bytes(&bytes.as_bytes()));
                let _ = tx.send(ToRender::Text(intern(&format!("·data.{key}")), format!("data:{key}?{version}")));
                return Ok(());
            }
            // Permission first, before anything the logic passed is converted.
            // (The scene's own terminals need no permission: they are its own.)
            if name != "say" {
                if !name.starts_with("terminal.") {
                    check_service_permission(&c, &name, true)?;
                }
                let args: Vec<SysValue> = args.iter().map(|v| lua_to_value(v, 0)).collect();
                return crate::platform::command(&mine, &name, &args).map_err(mlua::Error::runtime);
            }
            match args.first() {
                Some(Value::String(scene)) => check_say_permission(&c, &scene.to_string_lossy())?,
                // Not a scene's name: `say`'s own error says how it is called.
                _ => return crate::platform::command(&mine, &name, &[]).map_err(mlua::Error::runtime),
            }
            let args: Vec<SysValue> = args.iter().map(|v| lua_to_value(v, 0)).collect();
            // Waiting for the other scene does not count against the handler's
            // patience, as with `sys.ask`.
            let t0 = Instant::now();
            let said = crate::platform::command(&mine, &name, &args);
            if let Some(l) = &mut c.lock().unwrap().deadline {
                *l += t0.elapsed();
            }
            said.map_err(mlua::Error::runtime)
        })?)?;
        // The same, but it answers: `sys.ask("tray.menu", key)` returns the menu.
        let (c, mine) = (self.c.clone(), who.clone());
        sys.set("ask", lua.create_function(move |lua, (name, args): (String, mlua::Variadic<Value>)| {
            if !name.starts_with("terminal.") {
                check_service_permission(&c, &name, false)?;
            }
            let args: Vec<SysValue> = args.iter().map(|v| lua_to_value(v, 0)).collect();
            // However long asking the system takes does NOT count against the
            // handler's patience: patience is there to cut off a loop
            // that has run wild, and waiting is not running wild. PAM charges two
            // seconds for a bad password, which is exactly the limit: the
            // handler got cut off halfway through, without getting to say "that
            // wasn't it", and the lock screen stayed "checking"
            // forever. It is the same thing `busy` already did.
            let t0 = Instant::now();
            let answer = crate::platform::query(&mine, &name, &args);
            if let Some(l) = &mut c.lock().unwrap().deadline {
                *l += t0.elapsed();
            }
            value_to_lua(lua, &answer.map_err(mlua::Error::runtime)?)
        })?)?;
        g.set("sys", sys)?;

        // require("util"): another file from the same folder as this logic, and from no other.
        // It is loaded once; whatever it returns is the module.
        let folder = std::path::Path::new(&self.logic).parent().map(std::path::Path::to_owned).unwrap_or_default();
        let loaded = lua.create_table()?;
        g.set("require", lua.create_function(move |lua, name: String| {
            let clean = !name.is_empty() && name.split('/').all(|t| !t.is_empty() && t != ".." && t != "." && t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'));
            if !clean {
                return Err(mlua::Error::runtime(format!("require(\"{name}\"): a module is a file in this logic's folder, or in one inside it. No `..`, no full paths and no extension")));
            }
            if let Ok(Value::Table(t)) = loaded.raw_get::<Value>(name.as_str()) {
                return t.raw_get::<Value>("value");
            }
            let path = folder.join(format!("{name}.luau"));
            {
                // Watched from now on: saving it reloads the logic, like the main file.
                let mut r = REQUIRED.lock().unwrap();
                if !r.contains(&path) {
                    r.push(path.clone());
                }
            }
            let source = std::fs::read_to_string(&path).map_err(|e| mlua::Error::runtime(format!("require(\"{name}\"): {}: {e}", path.display())))?;
            let value: Value = lua.load(&source).set_name(format!("@{}", path.display())).eval()?;
            let slot = lua.create_table()?;
            slot.raw_set("value", value.clone())?;
            loaded.raw_set(name, slot)?;
            Ok(value)
        })?)?;
        // tr("Control center"): the scene's text in the language `locale` says, for what
        // the logic writes itself. Without a translation, as written.
        let c = self.c.clone();
        g.set("tr", lua.create_function(move |_, s: String| {
            let c = c.lock().unwrap();
            let k = c.facts.get("locale").map_or(0, |v| v.round().max(0.0) as usize);
            Ok(match k.checked_sub(1).and_then(|k| c.translations.get(k)).and_then(|(_, table)| table.get(&s)) {
                Some(t) => t.clone(),
                None => s,
            })
        })?)?;
        let who = self.prefix.as_ref().map_or(String::new(), |p| format!("[{p}] "));
        g.set("log", lua.create_function(move |_, v: MultiValue| {
            let pieces: Vec<String> = v.iter().map(|x| x.to_string().unwrap_or_else(|_| format!("{x:?}"))).collect();
            println!("luau   · {who}{}", pieces.join(" "));
            Ok(())
        })?)?;

        // `json.decode(text)` and `json.encode(table)`: what a program prints
        // (`run`'s output) read as tables, and tables written for one.
        let json = lua.create_table()?;
        json.set("decode", lua.create_function(|lua, text: String| match crate::platform::json_decode(&text) {
            Ok(v) => value_to_lua(lua, &v),
            Err(e) => Err(mlua::Error::RuntimeError(format!("json.decode: {e}"))),
        })?)?;
        json.set("encode", lua.create_function(|_, v: Value| Ok(crate::platform::json_encode(&lua_to_value(&v, 0))))?)?;
        g.set("json", json)?;

        // The sandbox, last of all: from here on the globals are not touched.
        lua.sandbox(true)?;
        Ok(lua)
    }

    /// A table that stores nothing: reading and writing it are calls.
    fn live_table(lua: &Lua, get: Function, set: Function) -> mlua::Result<Table> {
        let (t, meta) = (lua.create_table()?, lua.create_table()?);
        meta.set("__index", get)?;
        meta.set("__newindex", set)?;
        t.set_metatable(Some(meta))?;
        Ok(t)
    }

    fn call_handler(&self, f: &Function, args: impl mlua::IntoLuaMulti) {
        self.c.lock().unwrap().deadline = Some(Instant::now() + PATIENCE);
        if let Err(e) = f.call::<()>(args) {
            // Let it be known whose error it is: with plugins, "the logic" is several.
            match &self.prefix {
                Some(p) => eprintln!("logic  · plugin '{p}' · {e}"),
                None => eprintln!("logic  · {e}"),
            }
        }
        self.c.lock().unwrap().deadline = None;
    }

    /// The scene's logic: which plugin hears what is written under its name.
    fn tell_frontier(&self) {
        *self.frontier.plugins.lock().unwrap() = self.plugins.iter().map(|p| (p.definition.name.clone(), p.mailbox.clone())).collect();
    }

    fn dispatch_text(&self, what: &str, text: String) {
        let listeners: Vec<Function> = self.c.lock().unwrap().handlers.get(what).cloned().unwrap_or_default();
        for f in listeners {
            self.call_handler(&f, text.clone());
        }
    }

    fn dispatch(&self, what: &str, arg: Value) {
        let listeners: Vec<Function> = self.c.lock().unwrap().handlers.get(what).cloned().unwrap_or_default();
        for f in listeners {
            self.call_handler(&f, arg.clone());
        }
    }
}

impl Script for LuauScript {
    fn scene(&mut self) -> Scene {
        let e = super::scenes::from_file::read(&self.scene).unwrap_or_else(|m| {
            eprintln!("{m}");
            std::process::exit(1)
        });
        // What the scene takes as true at birth is what the logic believes until someone says otherwise.
        self.learn(&e, e.permissions.clone());
        self.plugins = e.plugins.iter().enumerate().map(|(k, p)| {
            let plugin = self.for_plugin(p, k);
            plugin.learn(&e, crate::permissions::effective(p));
            plugin.c.lock().unwrap().unapproved = !crate::permissions::is_approved(p);
            Self::start(plugin)
        }).collect();
        self.tell_frontier();
        e
    }

    fn on_event(&mut self, e: Event, ctx: &mut Context) {
        // Every plugin receives the same, but only has handlers with its name in front:
        // it does not find out about what is not its own.
        // What crosses from a scene brought in, the other scenes brought in hear too (G22):
        // its logic wrote `volume`, or emitted `rang`. Not the one it came from.
        // Who sent it is said with it (issue 145): the name in front only says whose it is.
        // What a library's plugin writes (no sender) the scenes brought in hear too: their
        // logic names the libraries whole.
        let e = match e {
            Event::BoundaryFrom(from, b) => {
                if self.prefix.is_none() {
                    for p in self.plugins.iter().filter(|p| p.definition.brought && p.definition.name != from) {
                        let _ = p.mailbox.send(Event::Boundary(b.clone()));
                    }
                }
                Event::Boundary(b)
            }
            Event::Boundary(b) => {
                if self.prefix.is_none() {
                    for p in self.plugins.iter().filter(|p| p.definition.brought) {
                        let _ = p.mailbox.send(Event::Boundary(b.clone()));
                    }
                }
                Event::Boundary(b)
            }
            other => other,
        };
        if !matches!(e, Event::NewScene(..) | Event::Boundary(_)) {
            // A frame, only to the ones that listen to them: the rest would be
            // woken sixty times a second for nothing.
            let frame = matches!(e, Event::Frame(..));
            for p in self.plugins.iter().filter(|p| !frame || p.wants_frames.load(Ordering::Relaxed)) {
                let _ = p.mailbox.send(e.clone());
            }
        }
        let _ = &ctx;
        let e_kind = match &e {
            Event::KeyDown(_) => 1,
            _ => 0,
        };
        let number = |v: f32| Value::Number(v as f64);
        match e {
            // The file is executed right on the logic's thread, with the scene delivered.
            Event::Alarm("start") | Event::ReloadLogic => {
                // Before the file runs: a `bluetooth.connect` at its top level
                // would otherwise pair as if nobody could be asked.
                self.offer_pairing();
                self.load_script();
                self.subscribe_services();
            }
            Event::Signal(n, payload) => self.dispatch(n, payload.map_or(Value::Nil, number)),
            // `emit moved(x: a, y: b)`: a table, `{x = …, y = …}`; unnamed, `{…, …}` by place.
            Event::SignalTable(n, values) => {
                let Some(lua) = &self.lua else { return };
                let Ok(t) = lua.create_table() else { return };
                let mut place = 0;
                for (name, v) in values {
                    let _ = match name {
                        Some(k) => t.set(k, number(v)),
                        None => {
                            place += 1;
                            t.set(place, number(v))
                        }
                    };
                }
                self.dispatch(n, Value::Table(t));
            }
            Event::Enter(z) => self.dispatch(&format!("enter:{z}"), Value::Nil),
            Event::Leave(z) => self.dispatch(&format!("leave:{z}"), Value::Nil),
            Event::Press(z) => self.dispatch(&format!("press:{z}"), Value::Nil),
            Event::Release(z) => self.dispatch(&format!("release:{z}"), Value::Nil),
            Event::Wheel(z, d) => self.dispatch(&format!("scroll:{z}"), number(d)),
            // What is typed into a field: the logic knows it key by key, and has not
            // had to do anything for it to show.
            Event::Text(n, value) => {
                self.c.lock().unwrap().texts.insert(n.to_owned(), value.clone());
                self.dispatch_text(&format!("text:{n}"), value);
            }
            Event::Submit(n, value) => self.dispatch_text(&format!("submit:{n}"), value),
            // `on("paste:draft", function(p) … end)`: `p.text` (nil if it holds no
            // text), `p.image` (true with a picture), `p.uris` (the files or
            // addresses copied, a list, maybe empty).
            Event::Paste(n, pasted) => {
                let Some(lua) = &self.lua else { return };
                let Ok(t) = lua.create_table() else { return };
                if let Some(text) = &pasted.text {
                    let _ = t.set("text", text.as_str());
                }
                let _ = t.set("image", pasted.image);
                if let Ok(uris) = lua.create_sequence_from(pasted.uris.iter().map(String::as_str)) {
                    let _ = t.set("uris", uris);
                }
                self.dispatch(&format!("paste:{n}"), Value::Table(t));
            }
            Event::Link(href) => self.dispatch_text("link", href),
            Event::Copied(code) => self.dispatch_text("copy", code),
            Event::Focus(yes) => self.dispatch(if yes { "focus" } else { "blur" }, Value::Nil),
            Event::Received(zone, kind, data) => {
                // By its name as written: a zone in a copy per monitor is
                // `pool#screen1` inside, and `on("drop:pool", …)` outside.
                let zone = zone.split('#').next().unwrap_or(zone);
                let listeners = self.c.lock().unwrap().handlers.get(&format!("drop:{zone}")).cloned().unwrap_or_default();
                listeners.iter().for_each(|f| self.call_handler(f, (data.clone(), kind.clone())));
            }
            Event::Key(name, typed) => {
                let Some(lua) = &self.lua else { return };
                let listeners = self.c.lock().unwrap().handlers.get("key").cloned().unwrap_or_default();
                if let Ok(n) = lua.create_string(&name) {
                    listeners.iter().for_each(|f| self.call_handler(f, (n.clone(), typed.clone())));
                }
            }
            Event::KeyDown(name) | Event::KeyUp(name) if self.prefix.is_none() || self.brought() => {
                let down = matches!(e_kind, 1);
                {
                    let mut c = self.c.lock().unwrap();
                    if down { c.held.insert(name.clone()) } else { c.held.remove(&name) };
                }
                self.dispatch_text(if down { "key_down" } else { "key_up" }, name);
            }
            Event::Frame(dt, at, buttons) => {
                if self.prefix.is_none() {
                    self.c.lock().unwrap().mouse = (at, buttons);
                }
                let what = qualified_listener(&self.prefix, "frame");
                let listeners: Vec<Function> = self.c.lock().unwrap().handlers.get(&what).cloned().unwrap_or_default();
                let mut failed = false;
                for f in listeners {
                    self.c.lock().unwrap().deadline = Some(Instant::now() + PATIENCE);
                    if let Err(e) = f.call::<()>(dt as f64) {
                        failed = true;
                        match &self.prefix {
                            Some(p) => eprintln!("logic  · plugin '{p}' · {e}"),
                            None => eprintln!("logic  · {e}"),
                        }
                    }
                    self.c.lock().unwrap().deadline = None;
                }
                // The same mistake sixty times a second is said once: it
                // stops hearing frames until the logic is saved again.
                if failed {
                    eprintln!("logic  · it no longer hears the frames: save the logic again once that is mended");
                    let mut c = self.c.lock().unwrap();
                    c.handlers.remove(&what);
                    if std::mem::take(&mut c.frames) {
                        crate::scene::FRAME_LISTENERS.fetch_sub(1, Ordering::AcqRel);
                        self.wants_frames.store(false, Ordering::Relaxed);
                    }
                }
                // The scene's logic is the one the render waits for.
                if self.prefix.is_none() {
                    crate::scene::FRAME_OWED.store(false, Ordering::Release);
                    let _ = self.tx.send(ToRender::FrameSeen);
                }
            }
            Event::Demo => self.dispatch("demo", Value::Nil),
            // From the other side of a plugin's boundary: as said by whoever said it,
            // and heard as a change from outside (the side that wrote it does not hear itself).
            Event::Boundary(b) => match *b {
                crate::scene::Crossing::Text(n, v) => {
                    self.c.lock().unwrap().texts.insert(n.clone(), v.clone());
                    self.dispatch_text(&format!("text:{n}"), v);
                }
                crate::scene::Crossing::Fact(n, v) => {
                    self.c.lock().unwrap().facts.insert(n.clone(), v);
                    let kind = self.c.lock().unwrap().types.get(&n).cloned();
                    let value = match &self.lua {
                        Some(lua) => from_number(lua, kind.as_ref(), v).unwrap_or(Value::Number(v)),
                        None => Value::Number(v),
                    };
                    self.dispatch(&format!("fact:{n}"), value);
                }
                crate::scene::Crossing::Model(n, v) => {
                    let Some(lua) = &self.lua else { return };
                    let Ok(list) = value_to_lua(lua, &v) else { return };
                    if let Ok(stored) = lua.named_registry_value::<Table>(MODELS) {
                        let _ = stored.raw_set(n.as_str(), list.clone());
                    }
                    self.dispatch(&format!("model:{n}"), list);
                }
                crate::scene::Crossing::Signal(n, v) => {
                    let Some(lua) = &self.lua else { return };
                    let value = value_to_lua(lua, &v).unwrap_or(Value::Nil);
                    self.dispatch(&n, value);
                }
            },
            Event::Fact(n, v) => {
                self.c.lock().unwrap().facts.insert(n.to_owned(), v as f64);
                // Whoever listens gets it the way they read it: `true`, `"critical"`, or a number.
                let kind = self.c.lock().unwrap().types.get(n).cloned();
                let value = match &self.lua {
                    Some(lua) => from_number(lua, kind.as_ref(), v as f64).unwrap_or(number(v)),
                    None => number(v),
                };
                self.dispatch(&format!("fact:{n}"), value);
            }
            Event::Layer(layer, winner) => {
                let Some(lua) = &self.lua else { return };
                if let Ok(s) = lua.create_string(winner) {
                    self.dispatch(&format!("layer:{layer}"), Value::String(s));
                }
            }
            // The scene was reloaded: whatever it has that is new can now be named; whatever
            // was already known, is still known.
            Event::NewScene(facts, texts, permissions, models, types, plugins, signals, services, translations) => {
                // A plugin receives the new scene with its own definition: that is where it gets its permissions.
                if let (Some(_), Some(def)) = (&self.prefix, plugins.first()) {
                    let mut c = self.c.lock().unwrap();
                    c.unapproved = !crate::permissions::is_approved(def);
                    self.definition = Some(def.clone());
                }
                // The scene: the plugins that stay, with what is new; the ones that arrive, start; the ones no longer there, go.
                if self.prefix.is_none() {
                    let mut staying: Vec<LivePlugin> = Vec::new();
                    for (k, p) in plugins.iter().enumerate() {
                        let fresh = Event::NewScene(facts.clone(), texts.clone(), crate::permissions::effective(p), models.clone(), types.clone(), vec![p.clone()], signals.clone(), services.clone(), translations.clone());
                        let live = match self.plugins.iter().position(|x| x.definition.name == p.name && x.definition.logic == p.logic) {
                            Some(i) => {
                                let mut v = self.plugins.remove(i);
                                v.definition = p.clone();
                                v
                            }
                            None => {
                                let v = Self::start(self.for_plugin(p, k + 100));
                                println!("logic  · plugin '{}' arrives", p.name);
                                let _ = v.mailbox.send(fresh.clone());
                                let _ = v.mailbox.send(Event::ReloadLogic);
                                v
                            }
                        };
                        let _ = live.mailbox.send(fresh);
                        staying.push(live);
                    }
                    for gone in std::mem::replace(&mut self.plugins, staying) {
                        // Dropping its mailbox, its thread ends and stops what it left running.
                        println!("logic  · plugin '{}' is gone", gone.definition.name);
                    }
                    self.tell_frontier();
                }
                let mut c = self.c.lock().unwrap();
                c.services = services;
                c.models = models;
                c.types = types.into_iter().collect();
                c.signals = signals.iter().map(|s| (*s).to_owned()).collect();
                c.translations = translations;
                // Permissions are indeed replaced: removing one from the scene removes it right away.
                let permissions = crate::permissions::granted(permissions);
                if c.permissions != permissions {
                    println!("logic  · permissions now: {}", describe(&permissions));
                    c.permissions = permissions;
                }
                for (n, v) in facts {
                    c.facts.entry(n.to_owned()).or_insert(v as f64);
                }
                for (n, v) in texts {
                    c.texts.entry(n.to_owned()).or_insert(v);
                }
                // If the scene now asks for a service it did not ask for before, it is set up here.
                drop(c);
                self.subscribe_services();
                // And what the ones already running said last goes again into
                // the scene as it is now: Bahía's Settings asked the network for
                // its list on a reload, and it stayed empty until the list changed.
                let last: Vec<(String, SysValue)> = self.c.lock().unwrap().last_reports.iter().map(|(a, v)| (a.clone(), v.clone())).collect();
                for (alias, value) in last {
                    self.spread_service(&alias, &value);
                }
            }
            Event::Line(id, line) => {
                let f = self.c.lock().unwrap().running.get(&id).map(|x| x.0.clone());
                if let Some(f) = f {
                    self.call_handler(&f, line);
                }
            }
            Event::Data(name, value) => {
                // `service clock as now`: it arrives without anyone asking for it from Luau,
                // and it is spread even if the scene has no logic at all.
                if let Some(alias) = name.strip_prefix("service:") {
                    self.c.lock().unwrap().last_reports.insert(alias.to_owned(), value.clone());
                    return self.spread_service(alias, &value);
                }
                let Some(lua) = &self.lua else { return };
                let listeners = self.c.lock().unwrap().watchers.get(&name).cloned().unwrap_or_default();
                match value_to_lua(lua, &value) {
                    Ok(v) => listeners.iter().for_each(|f| self.call_handler(f, v.clone())),
                    Err(e) => eprintln!("logic  · {e}"),
                }
            }
            Event::Process(id, output, code) => {
                self.c.lock().unwrap().running.remove(&id);
                self.c.lock().unwrap().inputs.remove(&id);
                let f = self.c.lock().unwrap().processes.remove(&id);
                if let Some(f) = f {
                    self.call_handler(&f, (output, code));
                }
            }
            _ => {}
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.c.lock().unwrap().timers.iter().map(|t| t.when).min()
    }

    fn tick(&mut self, _: &mut Context) {
        let now = crate::clock::now();
        let due: Vec<Function> = {
            let mut c = self.c.lock().unwrap();
            let f = c.timers.iter().filter(|t| t.when <= now).map(|t| t.f.clone()).collect();
            for t in &mut c.timers {
                if t.when <= now {
                    if let Some(d) = t.every {
                        t.when = now + d;
                    }
                }
            }
            c.timers.retain(|t| t.when > now);
            f
        };
        for f in due {
            self.call_handler(&f, ());
        }
    }
}

/// `hit`: whether two things touch, for a logic that plays. Boxes are said as
/// the scene says them —centre and size—, circles by centre and radius.
///
/// ```text
/// hit.boxes(ax, ay, aw, ah, bx, by, bw, bh)     hit.circles(ax, ay, ar, bx, by, br)
/// hit.circle_box(cx, cy, r, bx, by, bw, bh)     hit.point(px, py, bx, by, bw, bh)
/// hit.pairs(these, those)   every pair that touches, as { { i, j }, … }
/// hit.first(x, y, r, those) the first of `those` a circle touches, or nil
/// ```
///
/// In the lists each record has `x`, `y` and either `r` or `w` and `h`: the
/// pairs are found here, in one go, and not in a loop of the logic's.
fn hit_table(lua: &Lua) -> mlua::Result<Table> {
    #[derive(Clone, Copy)]
    enum Body {
        Circle(f64, f64, f64),
        Box(f64, f64, f64, f64),
    }
    fn touch(a: Body, b: Body) -> bool {
        match (a, b) {
            (Body::Circle(ax, ay, ar), Body::Circle(bx, by, br)) => (ax - bx).powi(2) + (ay - by).powi(2) <= (ar + br).powi(2),
            (Body::Box(ax, ay, aw, ah), Body::Box(bx, by, bw, bh)) => (ax - bx).abs() * 2.0 <= aw + bw && (ay - by).abs() * 2.0 <= ah + bh,
            (Body::Circle(cx, cy, r), Body::Box(bx, by, bw, bh)) | (Body::Box(bx, by, bw, bh), Body::Circle(cx, cy, r)) => {
                let (dx, dy) = (((cx - bx).abs() - bw / 2.0).max(0.0), ((cy - by).abs() - bh / 2.0).max(0.0));
                dx * dx + dy * dy <= r * r
            }
        }
    }
    fn bodies(what: &str, list: &Table) -> mlua::Result<Vec<Option<Body>>> {
        let mut out = Vec::with_capacity(list.raw_len());
        for (k, v) in list.sequence_values::<Value>().enumerate() {
            let Value::Table(t) = v? else {
                out.push(None);
                continue;
            };
            let n = |f: &str| t.get::<Option<f64>>(f);
            let (Some(x), Some(y)) = (n("x")?, n("y")?) else {
                return Err(mlua::Error::runtime(format!("hit.{what}: record {} has no x and y", k + 1)));
            };
            out.push(Some(match (n("r")?, n("w")?, n("h")?) {
                (Some(r), _, _) => Body::Circle(x, y, r),
                (None, Some(w), Some(h)) => Body::Box(x, y, w, h),
                _ => return Err(mlua::Error::runtime(format!("hit.{what}: record {} has neither r nor w and h", k + 1))),
            }));
        }
        Ok(out)
    }
    let t = lua.create_table()?;
    t.set("boxes", lua.create_function(|_, (ax, ay, aw, ah, bx, by, bw, bh): (f64, f64, f64, f64, f64, f64, f64, f64)| Ok(touch(Body::Box(ax, ay, aw, ah), Body::Box(bx, by, bw, bh))))?)?;
    t.set("circles", lua.create_function(|_, (ax, ay, ar, bx, by, br): (f64, f64, f64, f64, f64, f64)| Ok(touch(Body::Circle(ax, ay, ar), Body::Circle(bx, by, br))))?)?;
    t.set("circle_box", lua.create_function(|_, (cx, cy, r, bx, by, bw, bh): (f64, f64, f64, f64, f64, f64, f64)| Ok(touch(Body::Circle(cx, cy, r), Body::Box(bx, by, bw, bh))))?)?;
    t.set("point", lua.create_function(|_, (px, py, bx, by, bw, bh): (f64, f64, f64, f64, f64, f64)| Ok(touch(Body::Circle(px, py, 0.0), Body::Box(bx, by, bw, bh))))?)?;
    t.set("pairs", lua.create_function(|lua, (these, those): (Table, Table)| {
        let (a, b) = (bodies("pairs", &these)?, bodies("pairs", &those)?);
        let out = lua.create_table()?;
        for (i, x) in a.iter().enumerate() {
            for (j, y) in b.iter().enumerate() {
                if let (Some(x), Some(y)) = (x, y) {
                    if touch(*x, *y) {
                        out.raw_push(lua.create_sequence_from([i + 1, j + 1])?)?;
                    }
                }
            }
        }
        Ok(out)
    })?)?;
    t.set("first", lua.create_function(|_, (x, y, r, those): (f64, f64, f64, Table)| {
        Ok(bodies("first", &those)?.iter().position(|b| b.is_some_and(|b| touch(Body::Circle(x, y, r), b))).map(|k| k + 1))
    })?)?;
    Ok(t)
}

/// The modules the logic has loaded with `require`: the reload watches them
/// too, so saving one of them reloads the logic like saving the main file.
pub static REQUIRED: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

/// For `--check`: the scene's logic and each plugin's, read as Luau (a mistake of
/// writing is an error, with its file and line) and searched for the names of the
/// boundary they write as they are (`text.city`, `fact["Weather.degrees"]`,
/// `on("text:out", …)`, `emit("ask")`): one that is not there is said with its line,
/// as a warning (a name made up while running, `"rows." .. k`, is not looked at).
/// What it says: (errors, warnings).
pub fn check_logic(scene: &Scene, scene_path: &str) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let own = std::path::Path::new(scene_path).with_extension("luau");
    let mut files: Vec<(std::path::PathBuf, Option<&str>)> = Vec::new();
    if own.is_file() {
        files.push((own, None));
    }
    for p in &scene.plugins {
        if p.logic.is_file() {
            files.push((p.logic.clone(), Some(p.name.as_str())));
        }
    }
    // A scene brought in (G22) names its own under its name, and the other scenes' and the
    // libraries' whole, as its logic does when it runs (issue 145: `emit("Core.close")` from
    // P1 was said not to be in P1's boundary).
    let whole: Vec<String> = scene.libraries.iter().map(|l| format!("{l}.")).collect();
    let names = |all: &mut dyn Iterator<Item = String>, prefix: Option<&str>| -> Vec<String> {
        let brought = prefix.is_some_and(|p| scene.scenes.iter().any(|s| s == p));
        all.filter_map(|n| match prefix {
            Some(p) => n.strip_prefix(&format!("{p}.")).map(str::to_owned).or_else(|| {
                (brought && (scene.scenes.iter().any(|s| s != p && n.starts_with(&format!("{s}."))) || whole.iter().any(|w| n.starts_with(w.as_str())))).then(|| n.clone())
            }),
            None => Some(n),
        })
        .collect()
    };
    for (path, plugin) in files {
        let shown = path.display().to_string();
        let Ok(source) = std::fs::read_to_string(&path) else {
            errors.push(format!("{shown}: it cannot be read"));
            continue;
        };
        if let Err(e) = Lua::new().load(&source).set_name(format!("@{shown}")).into_function() {
            errors.push(e.to_string().trim_start_matches("syntax error: ").to_owned());
            continue;
        }
        let texts = names(&mut scene.texts.iter().map(|t| t.0.to_owned()), plugin);
        let facts = names(&mut scene.facts.iter().map(|f| f.0.to_owned()), plugin);
        let models = names(&mut scene.models.iter().map(|m| m.name.clone()), plugin);
        let signals = names(&mut scene.signals.iter().map(|x| x.0.to_owned()), plugin);
        let whose = match plugin {
            Some(p) => format!("the boundary of plugin '{p}'"),
            None => "the scene".to_owned(),
        };
        for (line, col, kind, name) in boundary_names(&source) {
            let (known, what) = match kind {
                "text" => (&texts, "text"),
                "fact" => (&facts, "fact"),
                "model" => (&models, "model"),
                _ => (&signals, "event"),
            };
            // A model's rows are read as facts and texts under its name (`rows.3.label`).
            let in_model = (kind == "text" || kind == "fact") && models.iter().any(|m| name.starts_with(&format!("{m}.")));
            if !known.contains(&name) && !in_model {
                // The name is there, but as something else: `on("fact:title")` for a text never fires.
                let other = [("text", &texts), ("fact", &facts), ("model", &models), ("event", &signals)].into_iter().find(|(k, l)| *k != what && l.contains(&name)).map(|(k, _)| k);
                warnings.push(match other {
                    Some(k) => format!("{shown}:{line}:{col}: warning: '{name}' is a {k} in {whose}, not a {what}"),
                    None => format!("{shown}:{line}:{col}: warning: there is no {what} '{name}' in {whose}{}", did_you_mean(&name, known.iter())),
                });
            }
        }
    }
    (errors, warnings)
}

/// The boundary's names a Luau source writes as they are: (line, column, text|fact|model|event, name).
fn boundary_names(source: &str) -> Vec<(usize, usize, &'static str, String)> {
    let mut found = Vec::new();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let quoted = |rest: &str| -> Option<String> {
        let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let end = rest[1..].find(q)?;
        Some(rest[1..1 + end].to_owned())
    };
    for (k, full) in source.lines().enumerate() {
        // Without its comment (a `--` inside a string cuts it short: then less is looked at).
        let line = full.split("--").next().unwrap_or(full);
        for (at, _) in line.char_indices() {
            let rest = &line[at..];
            let before = line[..at].chars().next_back();
            if before.is_some_and(|c| ident(c) || c == '.' || c == ':') {
                continue;
            }
            for kind in ["text", "fact", "model"] {
                let Some(after) = rest.strip_prefix(kind) else { continue };
                if let Some(name) = after.strip_prefix('.') {
                    let name: String = name.chars().take_while(|c| ident(*c)).collect();
                    // Calling a method of the table (`text.x(…)`) is not reading a name; nor is a
                    // name followed by `.` (a table inside: not one of the boundary's).
                    let next = after[1 + name.len()..].trim_start().chars().next();
                    if !name.is_empty() && next != Some('(') {
                        found.push((k + 1, at + 1, kind, name));
                    }
                } else if let Some(inner) = after.strip_prefix('[')
                    && let Some(name) = quoted(inner.trim_start())
                    && inner.trim_start()[name.len() + 2..].trim_start().starts_with(']')
                {
                    found.push((k + 1, at + 1, kind, name));
                }
            }
            for (call, class) in [("on(", None), ("emit(", Some("event"))] {
                let Some(after) = rest.strip_prefix(call) else { continue };
                let Some(name) = quoted(after.trim_start()) else { continue };
                // `on("fact:" .. k, …)`: made up while running, not looked at.
                if after.trim_start()[name.len() + 2..].trim_start().starts_with("..") {
                    continue;
                }
                match (class, name.split_once(':')) {
                    (Some(_), _) => found.push((k + 1, at + 1, "event", name)),
                    (None, Some((c @ ("text" | "fact" | "model"), n))) => found.push((k + 1, at + 1, if c == "text" { "text" } else if c == "fact" { "fact" } else { "model" }, n.to_owned())),
                    _ => {}
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program's output with a byte that is not UTF-8 goes on past it (M25), and a
    /// last line without its break still arrives.
    #[test]
    fn a_bad_byte_does_not_end_the_reading() {
        let bytes: &[u8] = b"one\ntw\xffo\r\nthree";
        let mut got = Vec::new();
        each_line(bytes, |l| {
            got.push(l);
            true
        });
        assert_eq!(got, vec!["one".to_owned(), "tw\u{fffd}o".to_owned(), "three".to_owned()]);
    }

    /// `write` to a program that does not read returns at once (M24): what does not fit
    /// in its pipe waits in its writer, and past 8 MiB `write` says false.
    #[cfg(unix)]
    #[test]
    fn writing_to_a_program_that_does_not_read_never_waits() {
        let mut child = std::process::Command::new("sleep").arg("5").stdin(std::process::Stdio::piped()).spawn().unwrap();
        let w = Writer::new(child.stdin.take().unwrap());
        let t0 = std::time::Instant::now();
        let chunk = vec![b'x'; 1 << 20];
        let accepted = (0..12).filter(|_| w.write(chunk.clone())).count();
        assert!(t0.elapsed() < std::time::Duration::from_secs(1), "it waited: {:?}", t0.elapsed());
        assert!((7..=8).contains(&accepted), "8 MiB at most waiting: {accepted}");
        let _ = child.kill();
        let _ = child.wait();
    }

    /// `for m in said key m.id`: taking the first of three away moves the other
    /// two up a place, each in the copy it already had, which is not written again.
    #[test]
    fn a_record_keeps_its_copy_by_its_key() {
        let lua = Lua::new();
        let list = |ids: &[i64]| {
            let t = lua.create_table().unwrap();
            for (i, id) in ids.iter().enumerate() {
                let r = lua.create_table().unwrap();
                r.set("id", *id).unwrap();
                r.set("text", format!("message {id}")).unwrap();
                t.raw_set(i + 1, r).unwrap();
            }
            t
        };
        let mut slots = Vec::new();
        assert_eq!(slotted(&mut slots, "id", 4, &list(&[10, 11, 12])).unwrap(), vec![Some(0), Some(1), Some(2), None]);
        // Without 10: 11 and 12 stay in copies 1 and 2, now at places 0 and 1; copy 0 is free.
        assert_eq!(slotted(&mut slots, "id", 4, &list(&[11, 12])).unwrap(), vec![None, Some(0), Some(1), None]);
        // A new one takes the first copy left free; the others do not move from theirs.
        assert_eq!(slotted(&mut slots, "id", 4, &list(&[13, 11, 12])).unwrap(), vec![Some(0), Some(1), Some(2), None]);
        assert_eq!(slots[0].as_deref(), Some("13"));
        // What is written: copy 1 keeps 11's text, so nothing of it travels again.
        let m = crate::scene::Model {
            name: "said".into(),
            capacity: 4,
            fields: vec![
                crate::scene::Field { name: "id".into(), kind: crate::scene::FieldType::Number, fallback: crate::scene::FieldValue::Number(0.0) },
                crate::scene::Field { name: "text".into(), kind: crate::scene::FieldType::Text, fallback: crate::scene::FieldValue::Text(String::new()) },
            ],
            key: Some("id".into()),
        };
        let mut changes = Vec::new();
        let copies = slotted(&mut Vec::new(), "id", 4, &list(&[11, 12])).unwrap();
        read_rows(&mut changes, "said", &m, &list(&[11, 12]), Some(&copies)).unwrap();
        let texts: Vec<(String, String)> = changes.iter().filter_map(|c| if let Change::Text(n, t) = c { Some((n.clone(), t.clone())) } else { None }).collect();
        assert_eq!(texts, vec![("said.0.text".to_owned(), "message 11".to_owned()), ("said.1.text".to_owned(), "message 12".to_owned())]);
    }
    use super::{command_allowed, say_allowed};

    #[test]
    fn a_command_declared_with_its_script_runs_only_that() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let declared = s(&["date", "node tools/usage.js"]);
        assert!(command_allowed(&declared, "date", &s(&["+%H:%M"])));
        assert!(command_allowed(&declared, "node", &s(&["tools/usage.js", "--lines"])));
        assert!(!command_allowed(&declared, "node", &s(&["-e", "require('fs').rmSync('/')"])));
        assert!(!command_allowed(&declared, "node", &s(&[])));
        assert!(!command_allowed(&declared, "sh", &s(&["-c", "true"])));
    }

    #[test]
    fn a_scene_is_told_only_by_name_or_by_say_star() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let bar = s(&["audio", "say:bar"]);
        assert!(say_allowed(&bar, "bar"));
        assert!(!say_allowed(&bar, "toasts"));
        assert!(!say_allowed(&bar, "bar2"));
        // Neither the service's bare name nor a dotted star is the permission.
        assert!(!say_allowed(&s(&["say", "say.*"]), "bar"));
        assert!(!say_allowed(&s(&[]), "bar"));
        assert!(say_allowed(&s(&["say:*"]), "toasts"));
    }

    #[test]
    #[cfg(unix)]
    fn say_from_the_logic_is_refused_or_errs_when_nobody_is_there() {
        use super::{LuauScript, Script};
        use crate::scene::{Event, ToRender};
        let dir = std::env::temp_dir().join(format!("pleamar-say-logic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The scenes this thread reaches listen here, not where the real ones do:
        // one stand-in that answers, `osd`, and none called `bar`.
        crate::platform::tests::SOCKETS.with(|d| *d.borrow_mut() = Some(dir.clone()));
        let _osd = std::os::unix::net::UnixListener::bind(dir.join("osd.sock")).unwrap();
        let absent = "bar";
        let (scene, logic) = (dir.join("teller.plm"), dir.join("teller.luau"));
        std::fs::write(&scene, format!("scene Teller {{\n    surface {{ size: 10, 10 }}\n    permissions {{ services: \"say:{absent}\" }}\n    text refused = \"\"\n    text absent = \"\"\n}}\n")).unwrap();
        std::fs::write(&logic, format!("local _, e = pcall(sys.call, \"say\", \"toasts\", \"emit x\")\ntext.refused = tostring(e)\nlocal _, f = pcall(sys.call, \"say\", \"{absent}\", \"emit x\")\ntext.absent = tostring(f)\n")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (to_logic, _) = std::sync::mpsc::channel();
        let blocked = std::sync::Arc::default();
        let mut s = LuauScript::new(&scene.to_string_lossy(), &logic.to_string_lossy(), tx.clone(), to_logic, std::sync::Arc::clone(&blocked));
        s.scene();
        s.on_event(Event::ReloadLogic, &mut crate::logic::Context::for_plugin(tx, blocked));
        let texts: std::collections::HashMap<&str, String> = rx.try_iter().filter_map(|m| if let ToRender::Text(k, v) = m { Some((k, v)) } else { None }).collect();
        assert!(texts["refused"].contains("no permission to talk to the scene 'toasts'"), "{}", texts["refused"]);
        assert!(texts["refused"].contains("services: \"say:toasts\""), "{}", texts["refused"]);
        assert!(texts["absent"].contains("the scene 'bar' is not running (running: osd)"), "{}", texts["absent"]);
        crate::platform::tests::SOCKETS.with(|d| *d.borrow_mut() = None);
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// A plugin's boundary, between its logic and the scene's: what one writes or emits the
    /// other hears and reads, with what an event brings; neither hears itself.
    #[test]
    fn the_scene_and_its_plugin_hear_each_other_across_the_boundary() {
        use super::{LuauScript, Script};
        use crate::scene::{Event, ToRender};
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("pleamar-boundary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/weather.plm"), "library Weather strict {\n    text out = \"\"\n    text got = \"\"\n    text heard = \"\"\n    fact degrees = 0\n    model hours max 3 { h: text }\n    event ask ->\n}\n").unwrap();
        std::fs::write(dir.join("lib/weather.luau"), concat!(
            "on(\"ask\", function(d) text.got = d.sky .. \" \" .. tostring(fact.degrees) .. \" \" .. #model.hours end)\n",
            "on(\"text:out\", function(v) text.heard = \"itself: \" .. v end)\n",
            "text.out = \"sunny\"\n",
            "fact.degrees = 21\n",
            "model.hours = { { h = \"10:00\" }, { h = \"11:00\" } }\n",
        )).unwrap();
        let (scene, logic) = (dir.join("owner.plm"), dir.join("owner.luau"));
        std::fs::write(&scene, "import \"lib/weather.plm\"\nscene Owner {\n    surface { size: 10, 10 }\n    text seen = \"\"\n}\n").unwrap();
        std::fs::write(&logic, concat!(
            "local said = {}\n",
            "local function tell() text.seen = table.concat(said, \" · \") end\n",
            "on(\"text:Weather.out\", function(v) table.insert(said, \"text \" .. v .. \"=\" .. text[\"Weather.out\"]); tell() end)\n",
            "on(\"fact:Weather.degrees\", function(v) table.insert(said, \"fact \" .. tostring(v)); tell() end)\n",
            "on(\"model:Weather.hours\", function(l) table.insert(said, \"model \" .. #l .. \"=\" .. #model[\"Weather.hours\"]); tell() end)\n",
            "fact[\"Weather.degrees\"] = 30\n",
            "model[\"Weather.hours\"] = { { h = \"12:00\" } }\n",
            "emit(\"Weather.ask\", { sky = \"clear\" })\n",
        )).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (to_logic, letters) = std::sync::mpsc::channel();
        let blocked = std::sync::Arc::default();
        let mut s = LuauScript::new(&scene.to_string_lossy(), &logic.to_string_lossy(), tx.clone(), to_logic, std::sync::Arc::clone(&blocked));
        s.scene();
        let mut ctx = crate::logic::Context::for_plugin(tx, blocked);
        s.on_event(Event::Alarm("start"), &mut ctx);
        // What the plugin says reaches the scene's mailbox: given to it as its loop would.
        let mut texts: std::collections::HashMap<&str, String> = Default::default();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < until && !(texts.get("seen").is_some_and(|v| v.contains("model")) && texts.contains_key("Weather.got")) {
            while let Ok(e) = letters.try_recv() {
                s.on_event(e, &mut ctx);
            }
            for m in rx.try_iter() {
                if let ToRender::Text(k, v) = m {
                    texts.insert(k, v);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(texts.get("seen").map(String::as_str), Some("text sunny=sunny · fact 21 · model 2=2"), "{texts:?}");
        // The scene set its degrees and its hours before emitting: the plugin had them when it heard.
        assert_eq!(texts.get("Weather.got").map(String::as_str), Some("clear 30 1"), "{texts:?}");
        assert!(!texts.contains_key("Weather.heard"), "the plugin heard its own write: {texts:?}");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scene brought in (G22) has its logic, under its name, with its own permissions,
    /// approved by opening the scene that brings it: it hears what is emitted to it.
    #[test]
    fn a_scene_brought_in_runs_its_logic_with_its_permissions() {
        use super::{LuauScript, Script};
        use crate::scene::{Event, ToRender};
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("pleamar-brought-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bar.plm"), "scene Bar {\n    surface { size: 10, 10 }\n    permissions { services: \"clipboard\" }\n    text said = \"\"\n    text perm = \"\"\n    event rang ->\n}\n").unwrap();
        std::fs::write(dir.join("bar.luau"), concat!(
            "on(\"rang\", function()\n",
            "    text.said = \"heard\"\n",
            "    local ok, e = pcall(sys.call, \"clipboard.image.save\", 42)\n",
            "    text.perm = tostring(e)\n",
            "end)\n",
        )).unwrap();
        let (scene, logic) = (dir.join("shell.plm"), dir.join("shell.luau"));
        std::fs::write(&scene, "import scene \"bar.plm\" as Bar\nscene Shell {\n    surface { size: 10, 10 }\n}\n").unwrap();
        std::fs::write(&logic, "emit(\"Bar.rang\")\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (to_logic, letters) = std::sync::mpsc::channel();
        let blocked = std::sync::Arc::default();
        let mut s = LuauScript::new(&scene.to_string_lossy(), &logic.to_string_lossy(), tx.clone(), to_logic, std::sync::Arc::clone(&blocked));
        s.scene();
        let mut ctx = crate::logic::Context::for_plugin(tx, blocked);
        s.on_event(Event::Alarm("start"), &mut ctx);
        let mut texts: std::collections::HashMap<&str, String> = Default::default();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < until && !texts.contains_key("Bar.perm") {
            while let Ok(e) = letters.try_recv() {
                s.on_event(e, &mut ctx);
            }
            for m in rx.try_iter() {
                if let ToRender::Text(k, v) = m {
                    texts.insert(k, v);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(texts.get("Bar.said").map(String::as_str), Some("heard"), "{texts:?}");
        // Past its permission, without anyone approving it: the argument is what is wrong.
        assert!(texts.get("Bar.perm").is_some_and(|e| e.contains("takes the absolute path")), "{texts:?}");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two scenes brought in, logic to logic (G22): one reads and hears the other's, once,
    /// and does not write it.
    #[test]
    fn the_logic_of_a_scene_brought_in_reads_and_hears_the_others() {
        use super::{LuauScript, Script};
        use crate::scene::{Event, ToRender};
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("pleamar-neighbors-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bar.plm"), "scene Bar {\n    surface { size: 10, 10 }\n    fact volume = 3\n    event go ->\n    event rang ->\n}\n").unwrap();
        std::fs::write(dir.join("bar.luau"), "on(\"go\", function() fact.volume = 7; emit(\"rang\") end)\n").unwrap();
        std::fs::write(dir.join("osd.plm"), "uses scene \"bar.plm\" as Bar\nscene Osd {\n    surface { size: 10, 10 }\n    text heard = \"\"\n    text refused = \"\"\n}\n").unwrap();
        std::fs::write(dir.join("osd.luau"), concat!(
            "local n = 0\n",
            "on(\"Bar.rang\", function()\n",
            "    n = n + 1\n",
            "    text.heard = tostring(n) .. \" \" .. tostring(fact[\"Bar.volume\"])\n",
            "    local ok, e = pcall(function() fact[\"Bar.volume\"] = 1 end)\n",
            "    text.refused = tostring(e)\n",
            "end)\n",
        )).unwrap();
        let (scene, logic) = (dir.join("shell.plm"), dir.join("shell.luau"));
        std::fs::write(&scene, "import scene \"bar.plm\" as Bar\nimport scene \"osd.plm\" as Osd\nscene Shell {\n    surface { size: 10, 10 }\n}\n").unwrap();
        std::fs::write(&logic, "emit(\"Bar.go\")\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (to_logic, letters) = std::sync::mpsc::channel();
        let blocked = std::sync::Arc::default();
        let mut s = LuauScript::new(&scene.to_string_lossy(), &logic.to_string_lossy(), tx.clone(), to_logic, std::sync::Arc::clone(&blocked));
        s.scene();
        let mut ctx = crate::logic::Context::for_plugin(tx, blocked);
        s.on_event(Event::Alarm("start"), &mut ctx);
        let mut texts: std::collections::HashMap<&str, String> = Default::default();
        // Past when it is heard, to see that it is heard once.
        let until = std::time::Instant::now() + Duration::from_millis(1500);
        while std::time::Instant::now() < until {
            while let Ok(e) = letters.try_recv() {
                s.on_event(e, &mut ctx);
            }
            for m in rx.try_iter() {
                if let ToRender::Text(k, v) = m {
                    texts.insert(k, v);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(texts.get("Osd.heard").map(String::as_str), Some("1 7"), "{texts:?}");
        assert!(texts.get("Osd.refused").is_some_and(|e| e.contains("is Bar's")), "{texts:?}");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue 145: one scene brought in emits another's event from its logic
    /// (`emit("Core.height", { v = 42 })`), and the other's logic hears it, with what it
    /// carries; the name in front says whose the event is, not who sent it.
    #[test]
    fn a_scene_brought_in_hears_its_events_emitted_by_another_scenes_logic() {
        use super::{LuauScript, Script};
        use crate::scene::{Event, ToRender};
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("pleamar-145-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("core.plm"), "scene Core {\n    fact got = 0\n    text said = \"\"\n    event height(v) ->\n    event close ->\n}\n").unwrap();
        std::fs::write(dir.join("core.luau"), concat!(
            "on(\"height\", function(v) fact.got = (type(v) == \"table\" and tonumber(v.v)) or -1 end)\n",
            "on(\"close\", function() fact.got = fact.got + 100; text.said = tostring(fact.got) end)\n",
        )).unwrap();
        std::fs::write(dir.join("p1.plm"), "uses scene \"core.plm\" as Core\nscene P1 {\n    surface s { size: 10, 10 }\n}\n").unwrap();
        std::fs::write(dir.join("p1.luau"), "after(200, function() emit(\"Core.height\", { v = 42 }); emit(\"Core.close\") end)\n").unwrap();
        let scene = dir.join("main.plm");
        std::fs::write(&scene, "import scene \"core.plm\" as Core\nimport scene \"p1.plm\" as P1\nscene Main {\n    surface { size: 10, 10 }\n}\n").unwrap();
        let logic = dir.join("main.luau");
        let (tx, rx) = std::sync::mpsc::channel();
        let (to_logic, letters) = std::sync::mpsc::channel();
        let blocked = std::sync::Arc::default();
        let mut s = LuauScript::new(&scene.to_string_lossy(), &logic.to_string_lossy(), tx.clone(), to_logic, std::sync::Arc::clone(&blocked));
        s.scene();
        let mut ctx = crate::logic::Context::for_plugin(tx, blocked);
        s.on_event(Event::Alarm("start"), &mut ctx);
        let mut said = None;
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < until && said.is_none() {
            while let Ok(e) = letters.try_recv() {
                s.on_event(e, &mut ctx);
            }
            for m in rx.try_iter() {
                if let ToRender::Text(k, v) = m {
                    if k == "Core.said" {
                        said = Some(v);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // 42 from `height`'s payload, then `close`'s 100: both heard by Core's logic, once.
        assert_eq!(said.as_deref(), Some("142"));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue 145: `--check` reads a scene brought in naming another's event whole as fine.
    #[test]
    fn check_reads_another_scenes_event_from_a_scene_brought_in_as_fine() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/brought/emit145/main.plm");
        let scene = crate::scenes::from_file::read(path).unwrap();
        let (errors, warnings) = super::check_logic(&scene, path);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn saving_the_copied_picture_asks_only_for_the_clipboard() {
        use super::{LuauScript, Script};
        use crate::scene::ToRender;
        let dir = std::env::temp_dir().join(format!("pleamar-clip-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A wrong argument, so the clipboard itself is never read: the error says
        // whether the call got past the permission.
        let run = |services: &str| {
            let (scene, logic) = (dir.join("paster.plm"), dir.join("paster.luau"));
            std::fs::write(&scene, format!("scene Paster {{\n    surface {{ size: 10, 10 }}\n    permissions {{ services: \"{services}\" }}\n    text said = \"\"\n}}\n")).unwrap();
            std::fs::write(&logic, "local ok, e = pcall(sys.call, \"clipboard.image.save\", 42)\ntext.said = tostring(ok) .. \" \" .. tostring(e)\n").unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let (to_logic, _) = std::sync::mpsc::channel();
            let blocked = std::sync::Arc::default();
            let mut s = LuauScript::new(&scene.to_string_lossy(), &logic.to_string_lossy(), tx.clone(), to_logic, std::sync::Arc::clone(&blocked));
            s.scene();
            s.on_event(crate::scene::Event::ReloadLogic, &mut crate::logic::Context::for_plugin(tx, blocked));
            rx.try_iter().find_map(|m| if let ToRender::Text(k, v) = m { (k == "said").then_some(v) } else { None }).unwrap()
        };
        let allowed = run("clipboard");
        assert!(allowed.starts_with("false") && allowed.contains("takes the absolute path"), "{allowed}");
        let refused = run("env");
        assert!(refused.starts_with("false") && refused.contains("no permission"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
