//! The thread that paints. It owns the springs and the clock: it receives intentions,
//! walks through them at the screen's cadence and, when everything is still, stops
//! painting altogether.

use crate::scene::*;
use crate::gpu::{DrawList, Gpu, Sheet, HUD_HEIGHT, N_UNIFORMS};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Options {
    pub hud: bool,
    pub naive: bool,
    /// Reduced motion: springs settle and gestures show their still face.
    pub reduced_motion: bool,
    /// Without waiting for the screen and without resting: to measure what painting costs.
    pub no_vsync: bool,
    /// What to record on every frame: properties, facts or texts, by name.
    /// It is how you check that an animation lasts as long as it says it lasts.
    pub trace: Vec<String>,
    pub start_time: Instant,
    /// Where the render's own messages arrive: what the compositor inside the
    /// scene (`windows`) is given to talk to it.
    pub to_self: std::sync::mpsc::Sender<ToRender>,
}

/// A window of the compositor inside the scene, as the render knows it: where
/// the window itself is in its main surface, its pieces, and the size it was
/// last told to have.
#[derive(Default)]
struct NestWindow {
    geometry: [i32; 4],
    pieces: Vec<NestPiece>,
    ask: Option<(i32, i32)>,
}

/// One surface of a window: its layer of the windows' texture, where it goes,
/// and what it shows —pixels kept here, or a buffer of the program's on the
/// card—, and whether that is already on its layer.
struct NestPiece {
    id: u64,
    layer: u32,
    at: (i32, i32),
    size: (u32, u32),
    /// Its pixels, and the part of them it shows (see `WindowPiece`).
    px: (u32, u32),
    src: [f32; 4],
    opaque: bool,
    pixels: Vec<u8>,
    buffer: Option<u64>,
    #[cfg(unix)]
    fresh: Option<crate::scene::DmabufPiece>,
    uploaded: bool,
}

/// The workspaces of the scene's windows: which one each window is on, which
/// one each monitor shows, and the window that last had the keyboard on each
/// (monitor, workspace), to give it back when that workspace is shown again.
/// The windows' workspaces: pools, a stack of them per monitor. A pool has
/// an identity that does not change; its number is its turn in its
/// monitor's stack. A monitor always has the one it shows; any other with no
/// window dries up, and the ones after it move up. Past the last one with
/// windows there is always one more, empty, to go to.
struct Desks {
    /// Each slot's pool (0: none yet).
    pool: Vec<u32>,
    /// The slots that hold a window.
    alive: Vec<bool>,
    /// Each monitor's pools, top to bottom.
    stack: [Vec<u32>; 4],
    /// The one each monitor shows.
    shown: [u32; 4],
    /// The one each monitor stopped showing because its windows went to
    /// another monitor (one unplugged, the phone's): when they come back, it
    /// is shown again, as it was left (0: none).
    left: [u32; 4],
    /// The window that last had the keyboard in each pool.
    last: std::collections::HashMap<u32, usize>,
    next: u32,
    // ── the dock ──
    /// Each slot's program (its app id, lowercase), its icon, whether it is
    /// put away, and when it opened (the dock's order).
    apps: Vec<String>,
    icons: Vec<String>,
    names: Vec<String>,
    execs: Vec<String>,
    dialog: Vec<bool>,
    minimized: Vec<bool>,
    /// Floating over the layout (`float`): out of its count, not of the order.
    floating: Vec<bool>,
    /// Opened under `placement manual` and not yet accepted: out of the layout and of the order.
    pending: Vec<bool>,
    born: Vec<u64>,
    stamp: u64,
    focus: Option<usize>,
    /// When each slot last had the keyboard (0: never): the one a program's
    /// dock item goes to.
    touched: Vec<u64>,
    /// `dock programs` (see `Nest::programs`): the docks are one, of every
    /// program with a window anywhere.
    programs: bool,
    /// `workspaces kept` (see `Nest::kept`): an empty one stays.
    kept: bool,
    /// The programs pinned, and each monitor's dock as last worked out.
    pins: Vec<crate::scene::DockPin>,
    items: [Vec<DockItem>; 4],
}

/// An item of a monitor's dock: a program pinned, or one with windows there.
#[derive(Clone, Default)]
struct DockItem {
    key: String,
    pin: Option<usize>,
    icon: String,
    exec: String,
    name: String,
}

impl Default for Desks {
    fn default() -> Self {
        Desks { pool: Vec::new(), alive: Vec::new(), stack: [vec![1], vec![2], vec![3], vec![4]], shown: [1, 2, 3, 4], left: [0; 4], last: Default::default(), next: 5, apps: Vec::new(), icons: Vec::new(), names: Vec::new(), execs: Vec::new(), dialog: Vec::new(), minimized: Vec::new(), floating: Vec::new(), pending: Vec::new(), born: Vec::new(), stamp: 0, focus: None, touched: Vec::new(), programs: false, kept: false, pins: Vec::new(), items: Default::default() }
    }
}

impl Desks {
    fn grow(&mut self, max: usize) {
        if self.pool.len() < max {
            self.pool.resize(max, 0);
            self.alive.resize(max, false);
            self.apps.resize(max, String::new());
            self.icons.resize(max, String::new());
            self.names.resize(max, String::new());
            self.execs.resize(max, String::new());
            self.dialog.resize(max, false);
            self.minimized.resize(max, false);
            self.floating.resize(max, false);
            self.pending.resize(max, false);
            self.born.resize(max, 0);
            self.touched.resize(max, 0);
        }
    }

    /// The windows a monitor's dock is made of and counts: the ones on the
    /// workspace it shows; with `dock programs`, every one there is, on any
    /// monitor and any workspace. As they opened.
    fn docked(&self, screen: usize, screens: &[usize]) -> Vec<usize> {
        if !self.programs {
            return self.here(screen, screens);
        }
        let mut v: Vec<usize> = (0..self.pool.len()).filter(|k| self.alive(*k) && !self.dialog[*k]).collect();
        v.sort_by_key(|k| self.born[*k]);
        v
    }

    /// Whether that slot's window belongs to that dock item.
    fn of_item(&self, slot: usize, item: &DockItem) -> bool {
        let key = &self.apps[slot];
        !key.is_empty() && match item.pin {
            Some(p) => self.pins.get(p).is_some_and(|pin| pin.keys.iter().any(|k| k == key)),
            None => item.key == *key,
        }
    }

    /// The windows on the workspace a monitor shows, as they opened.
    fn here(&self, screen: usize, screens: &[usize]) -> Vec<usize> {
        // (Not a dialog: a file chooser is its program's, and the portal's is nobody's to pin.)
        let mut v: Vec<usize> = (0..self.pool.len()).filter(|k| self.alive(*k) && !self.dialog[*k] && screens.get(*k).copied().unwrap_or(0).min(3) == screen && self.pool[*k] == self.shown[screen]).collect();
        v.sort_by_key(|k| self.born[*k]);
        v
    }

    /// How much is unread for an item: said by its program's name (any of the
    /// names its windows go by, or its own, or its first word: «Telegram Desktop»).
    fn badge(&self, item: &DockItem, badges: &[(String, f32)]) -> f32 {
        let first = |t: &str| t.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_owned();
        let name = item.name.to_lowercase();
        let keys: Vec<&String> = match item.pin {
            Some(p) => self.pins.get(p).map(|pin| pin.keys.iter().collect()).unwrap_or_default(),
            None => vec![&item.key],
        };
        badges.iter().filter(|(a, _)| keys.iter().any(|k| *k == a) || *a == name || (!first(a).is_empty() && first(a) == first(&name))).map(|(_, n)| *n).sum()
    }

    /// A monitor's dock: the programs pinned, then the others with windows
    /// on the workspace it shows, in the order they opened.
    fn work_dock(&mut self, screen: usize, screens: &[usize]) {
        let mut items: Vec<DockItem> = self.pins.iter().enumerate().map(|(k, p)| DockItem { key: p.keys.first().cloned().unwrap_or_default(), pin: Some(k), icon: p.icon.clone(), exec: p.exec.clone(), name: p.name.clone() }).collect();
        for slot in self.docked(screen, screens) {
            if self.apps[slot].is_empty() || items.iter().any(|i| self.of_item(slot, i)) {
                continue;
            }
            let icon = if self.icons[slot].is_empty() { self.apps[slot].clone() } else { self.icons[slot].clone() };
            let name = if self.names[slot].is_empty() { self.apps[slot].clone() } else { self.names[slot].clone() };
            items.push(DockItem { key: self.apps[slot].clone(), pin: None, icon, exec: self.execs[slot].clone(), name });
        }
        items.truncate(crate::scene::DOCK_ITEMS);
        self.items[screen] = items;
    }

    fn pool_of(&self, slot: usize) -> u32 {
        self.pool.get(slot).copied().unwrap_or(0)
    }

    fn alive(&self, slot: usize) -> bool {
        self.alive.get(slot).copied().unwrap_or(false)
    }

    /// Its turn in the monitor's stack, from 1 (0: not there).
    fn index(&self, screen: usize, id: u32) -> usize {
        self.stack[screen.min(3)].iter().position(|p| *p == id).map_or(0, |k| k + 1)
    }

    /// Whether that slot's window is on the pool its monitor shows.
    fn shown_here(&self, slot: usize, screens: &[usize]) -> bool {
        let screen = screens.get(slot).copied().unwrap_or(0).min(3);
        self.pool_of(slot) == self.shown[screen]
    }

    fn holds(&self, screen: usize, id: u32, screens: &[usize]) -> bool {
        (0..self.pool.len()).any(|k| self.alive(k) && screens.get(k).copied().unwrap_or(0).min(3) == screen && self.pool[k] == id)
    }

    /// How many of the monitor's pools hold windows.
    fn used(&self, screen: usize, screens: &[usize]) -> usize {
        self.stack[screen.min(3)].iter().filter(|id| self.holds(screen, **id, screens)).count()
    }

    /// The pool at turn `n` of a monitor: no further than one past the last
    /// that holds windows, which is a new, empty one. (Kept, one past the
    /// last there is: empty ones count.)
    fn at(&mut self, screen: usize, n: usize, screens: &[usize]) -> u32 {
        let s = screen.min(3);
        let most = if self.kept { self.stack[s].len() } else { self.used(s, screens) };
        let n = n.clamp(1, most + 1);
        match self.stack[s].get(n - 1) {
            Some(id) => *id,
            None => {
                let id = self.next;
                self.next += 1;
                self.stack[s].push(id);
                id
            }
        }
    }

    /// The pools with no window dry up, unless shown; the one shown is always there.
    fn compact(&mut self, screens: &[usize]) {
        for s in 0..4 {
            if !self.stack[s].contains(&self.shown[s]) {
                self.stack[s].push(self.shown[s]);
            }
            // (Kept, none dries up: they go when they are closed.)
            if self.kept {
                continue;
            }
            let stack = std::mem::take(&mut self.stack[s]);
            self.stack[s] = stack.into_iter().filter(|id| *id == self.shown[s] || self.holds(s, *id, screens)).collect();
        }
        let live: Vec<u32> = self.stack.iter().flatten().copied().collect();
        self.last.retain(|id, _| live.contains(id));
    }
}

/// Each window's turn in the layout of its own monitor and workspace, how
/// many share them, and the order of the ones shown. A workspace not shown
/// is laid out all the same: its windows are in their places when it slides in.
fn nest_places(scene: &Scene, facts: &mut [f32], texts: &mut [String], to_logic: &Sender<Event>, n: &crate::scene::Nest, order: &[usize], screens: &[usize], desks: &mut Desks) {
    desks.grow(n.max);
    desks.programs = n.programs;
    desks.kept = n.kept;
    desks.compact(screens);
    for s in 0..4 {
        desks.work_dock(s, screens);
    }
    let desks = &*desks;
    let name = &n.name;
    let screen_of = |slot: usize| screens.get(slot).copied().unwrap_or(0);
    let together = |a: usize, b: usize| screen_of(a) == screen_of(b) && desks.pool_of(a) == desks.pool_of(b);
    let shown = |slot: usize| desks.shown_here(slot, screens);
    // One waiting to be accepted (`placement manual`) is in none of it yet.
    let pending = |k: usize| desks.pending.get(k).copied().unwrap_or(false);
    let visible: Vec<usize> = order.iter().copied().filter(|k| shown(*k) && !pending(*k)).collect();
    // A floating window keeps its turn for the keyboard (`order`, `rank`,
    // `count`), but the layout is shared out among the others: as a dialog.
    let floats = |k: usize| desks.floating.get(k).copied().unwrap_or(false);
    let tiled: Vec<usize> = order.iter().copied().filter(|k| !floats(*k) && !pending(*k)).collect();
    for k in 0..n.max {
        let listed = tiled.contains(&k);
        let place = if listed { tiled.iter().take_while(|s| **s != k).filter(|s| together(**s, k)).count() as f32 } else { -1.0 };
        nest_fact(scene, facts, to_logic, &format!("{name}.{k}.place"), place);
        nest_fact(scene, facts, to_logic, &format!("{name}.{k}.among"), if listed { tiled.iter().filter(|s| together(**s, k)).count() as f32 } else { 0.0 });
        nest_fact(scene, facts, to_logic, &format!("{name}.{k}.rank"), visible.iter().position(|s| *s == k).map_or(-1.0, |p| p as f32));
        nest_fact(scene, facts, to_logic, &format!("{name}.{k}.workspace"), desks.index(screen_of(k), desks.pool_of(k)) as f32);
        nest_fact(scene, facts, to_logic, &format!("{name}.{k}.pool"), desks.pool_of(k) as f32);
        nest_fact(scene, facts, to_logic, &format!("{name}.{k}.pending"), pending(k) as u8 as f32);
    }
    for p in 0..n.max {
        nest_fact(scene, facts, to_logic, &format!("{name}.order.{p}"), visible.get(p).map_or(-1.0, |s| *s as f32));
    }
    for s in 0..4 {
        nest_fact(scene, facts, to_logic, &format!("{name}.on.{s}"), visible.iter().filter(|k| screen_of(**k) == s && !floats(**k)).count() as f32);
        nest_fact(scene, facts, to_logic, &format!("{name}.shown.{s}"), desks.index(s, desks.shown[s]) as f32);
        nest_fact(scene, facts, to_logic, &format!("{name}.pool.{s}"), desks.shown[s] as f32);
        nest_fact(scene, facts, to_logic, &format!("{name}.pools.{s}"), desks.stack[s].len() as f32);
        nest_fact(scene, facts, to_logic, &format!("{name}.used.{s}"), desks.used(s, screens) as f32);
    }
    nest_fact(scene, facts, to_logic, &format!("{name}.count"), visible.len() as f32);
    // The docks. What is unread, by program: `discord:3 org.telegram.desktop:1`.
    let badges: Vec<(String, f32)> = scene.texts.iter().position(|t| t.0 == format!("{name}.badges")).and_then(|k| texts.get(k)).map(|t| {
        t.split_whitespace().filter_map(|w| w.rsplit_once(':')).filter_map(|(a, n)| Some((a.to_lowercase(), n.parse::<f32>().ok()?))).collect()
    }).unwrap_or_default();
    for s in 0..4 {
        let items = &desks.items[s];
        let here = desks.here(s, screens);
        let docked = desks.docked(s, screens);
        nest_fact(scene, facts, to_logic, &format!("{name}.docks.{s}"), items.len() as f32);
        for k in 0..crate::scene::DOCK_ITEMS {
            let item = items.get(k);
            let mine: Vec<usize> = item.map(|i| docked.iter().copied().filter(|w| desks.of_item(*w, i)).collect()).unwrap_or_default();
            nest_text(scene, texts, to_logic, &format!("{name}.dock.{s}.{k}.icon"), item.map_or(String::new(), |i| i.icon.clone()));
            nest_text(scene, texts, to_logic, &format!("{name}.dock.{s}.{k}.name"), item.map_or(String::new(), |i| i.name.clone()));
            nest_fact(scene, facts, to_logic, &format!("{name}.dock.{s}.{k}.badge"), item.map_or(0.0, |i| desks.badge(i, &badges)));
            nest_fact(scene, facts, to_logic, &format!("{name}.dock.{s}.{k}.windows"), mine.len() as f32);
            nest_fact(scene, facts, to_logic, &format!("{name}.dock.{s}.{k}.focused"), mine.iter().any(|w| desks.focus == Some(*w)) as u8 as f32);
            nest_fact(scene, facts, to_logic, &format!("{name}.dock.{s}.{k}.pinned"), item.is_some_and(|i| i.pin.is_some()) as u8 as f32);
            nest_fact(scene, facts, to_logic, &format!("{name}.dock.{s}.{k}.away"), mine.iter().filter(|w| desks.minimized[**w]).count() as f32);
        }
        let away: Vec<usize> = here.iter().copied().filter(|w| desks.minimized[*w]).collect();
        nest_fact(scene, facts, to_logic, &format!("{name}.mins.{s}"), away.len() as f32);
    }
    for k in 0..n.max {
        let s = screen_of(k).min(3);
        let here = desks.shown_here(k, screens) && desks.alive(k);
        // (With `dock programs` every window has its item, shown or not.)
        let docked = if desks.programs { desks.alive(k) && !desks.dialog[k] } else { here };
        let at = if docked { desks.items[s].iter().position(|i| desks.of_item(k, i)).map_or(-1.0, |p| p as f32) } else { -1.0 };
        nest_fact(scene, facts, to_logic, &format!("{name}.{k}.dockat"), at);
        let minat = if here && desks.minimized[k] { desks.here(s, screens).iter().filter(|w| desks.minimized[**w]).position(|w| *w == k).map_or(-1.0, |p| p as f32) } else { -1.0 };
        nest_fact(scene, facts, to_logic, &format!("{name}.{k}.minat"), minat);
        // Its turn among its item's windows there (for a row of their previews).
        let turn = if docked && at >= 0.0 { desks.docked(s, screens).iter().filter(|w| desks.items[s].get(at as usize).is_some_and(|i| desks.of_item(**w, i))).position(|w| *w == k).map_or(-1.0, |p| p as f32) } else { -1.0 };
        nest_fact(scene, facts, to_logic, &format!("{name}.{k}.dockturn"), turn);
    }
}

/// `%20` and the rest of a file URI, back to the bytes they stand for.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut k = 0;
    while k < b.len() {
        if b[k] == b'%' && k + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[k + 1..k + 3], 16) {
                out.push(v);
                k += 3;
                continue;
            }
        }
        out.push(b[k]);
        k += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A fact of the scene's windows, by name: set, and told to the logic.
fn nest_fact(scene: &Scene, facts: &mut [f32], to_logic: &Sender<Event>, name: &str, v: f32) {
    if let Some(k) = scene.facts.iter().position(|h| h.0 == name) {
        if (facts[k] - v).abs() > f32::EPSILON {
            facts[k] = v;
            let _ = to_logic.send(Event::Fact(scene.facts[k].0, v));
        }
    }
}

fn nest_text(scene: &Scene, texts: &mut [String], to_logic: &Sender<Event>, name: &str, v: String) {
    if let Some(k) = scene.texts.iter().position(|t| t.0 == name) {
        if k < texts.len() && texts[k] != v {
            texts[k] = v.clone();
            let _ = to_logic.send(Event::Text(scene.texts[k].0, v));
        }
    }
}

struct Rng(u64);
impl Rng {
    fn between(&mut self, a: f32, b: f32) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        a + (b - a) * ((self.0 >> 40) as f32 / (1u64 << 24) as f32)
    }
}

#[derive(Default)]
struct Cycle {
    dts: Vec<f32>,
    max_blocked: f32,
    frames_blocked: u32,
    /// What goes into READING the scene —evaluating its expressions and noting down what
    /// has to be painted—, against what goes into drawing it. It is always measured:
    /// it is two clocks per frame, and it is the only way for whoever writes a
    /// scene to find out they have made it too expensive.
    compose_ms: f32,
    slow_reported: bool,
}

impl Cycle {
    /// A scene that does not deliver the frames the screen asks for does not show in the
    /// log —they are isolated slow frames, one after another— and from outside it looks
    /// like the runtime has become slow. It is said ONCE, with the breakdown, as
    /// soon as there is enough to say it with: four seconds of lagging behind.
    fn watch(&mut self, period_ms: f32) {
        if self.slow_reported || self.dts.len() < 240 {
            return;
        }
        let mean = self.dts.iter().sum::<f32>() / self.dts.len() as f32;
        if mean <= period_ms * 1.35 {
            return;
        }
        self.slow_reported = true;
        let reading = self.compose_ms / self.dts.len() as f32;
        eprintln!(
            "render · this scene does not keep up: {mean:.1} ms a frame against the {period_ms:.1} the screen gives, and {reading:.1} of those go in reading the scene, not in drawing it. Something in it is too dear to work out sixty times a second"
        );
    }

    fn close(&mut self) {
        if self.dts.len() < 8 {
            self.dts.clear();
            return;
        }
        let mut o = self.dts.clone();
        o.sort_by(|a, b| a.total_cmp(b));
        let reading = self.compose_ms / o.len() as f32;
        let mean = o.iter().sum::<f32>() / o.len() as f32;
        let p99 = o[((o.len() as f32 * 0.99) as usize).min(o.len() - 1)];
        println!(
            "cycle  · {:>4} frames · mean {:>5.2} ms · p99 {:>6.2} ms · max {:>6.2} ms · reading the scene {:.2} ms · with the logic blocked: {} frames, max {:.2} ms",
            o.len(), mean, p99, o[o.len() - 1], reading, self.frames_blocked, self.max_blocked
        );
        let reported = self.slow_reported;
        *self = Cycle::default();
        self.slow_reported = reported;
    }
}

pub fn run(
    instance: wgpu::Instance,
    rx: Receiver<ToRender>,
    mut letters: crate::text::Texts,
    to_logic: Sender<Event>,
    logic_blocked: Arc<AtomicBool>,
    op: Options,
) {
    let mut gpu: Option<Gpu> = None;
    let mut sheets: Vec<Sheet> = Vec::new();
    let mut draw = DrawList::default();
    let mut previous = crate::gpu::PreviousFrame::default();
    // What the last list was made from (see `same_scene`).
    let mut compose_memo: Option<ComposeMemo> = None;
    // `PLEAMAR_CHECK_SHORTCUTS=1`: each shortcut a round takes (work skipped
    // because nothing it would look at changed) is checked against doing the
    // work, and a difference is said. To try them on a scene; slow.
    let check_shortcuts = std::env::var_os("PLEAMAR_CHECK_SHORTCUTS").is_some();
    // What the drawing read the last time it was made.
    let mut compose_seen = crate::scene::Seen::default();
    let mut zone_memo = ZoneMemo::default();
    let mut rule_index = RuleIndex::default();
    let mut awake = AwakeBehaviors::default();
    // Stretches of the drawing copied and put together (see `gpu::Kept`), for the timing report.
    let (mut stretches_copied, mut stretches_made) = (0u64, 0u64);
    let mut z_read = ReadMemo::default();
    // Who asked what the scene holds (`describe`): answered after the next
    // list is made, which is when where every text went is known.
    let mut describing: Vec<(bool, std::sync::mpsc::Sender<String>)> = Vec::new();
    // What agents asked to do by name (`press save`), one after the other, and
    // the one being done: its steps, frame by frame, as a hand would. While it
    // lasts the pointer is its hand; the user's comes back when it is done.
    let mut acts: std::collections::VecDeque<(crate::agent::Act, std::sync::mpsc::Sender<String>)> = std::collections::VecDeque::new();
    let mut acting: Option<Acting> = None;
    let mut real_pointer: Option<(f32, f32)> = None;
    // A press of an agent's seat that was not let through, by button: its release is not either.
    let mut agent_held_back = [false; 3];
    // Who waits for a condition (`wait`), and who watches what happens (`watch`).
    let mut waits: Vec<(crate::agent::Cond, Instant, Instant, std::sync::mpsc::Sender<String>)> = Vec::new();
    let mut watchers: Vec<Watcher> = Vec::new();
    // And who follows some names, for a program (`watch 0 lay.`).
    let mut tails: Vec<crate::agent::Follow> = Vec::new();
    // The zones that have already been said to have no words: once each.
    let mut unnamed_said: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    let mut changed: Vec<[f32; 4]> = Vec::new();
    let mut sheet_counts = (0u32, 0u32, 0u32);
    let no_lens = std::env::var_os("PLEAMAR_NO_LENS").is_some();
    // The compositor's notice that it wants another frame: from which sheet it is
    // expected, whether it has already arrived, how many times in a row it did not arrive, and what arrived
    // from the rest of the world while waiting (it is handled on the next round).
    let mut frame_requested: Option<u32> = None;
    let mut frame_ready = false;
    // The other sheets' frames still on their way to the screen, and since
    // when; the sheets that have ever answered (a platform that never does is
    // never waited for); and those that skipped a round waiting, to paint as
    // soon as theirs arrives. See where they are painted.
    let mut in_flight: Vec<(u32, Instant)> = Vec::new();
    // The surfaces that answered a frame since the last round (see `Sheet::unseen`).
    let mut seen_again: Vec<u32> = Vec::new();
    let mut answers: Vec<u32> = Vec::new();
    let mut owed: Vec<u32> = Vec::new();
    let mut missed_notices = 0u32;
    let mut held_back: Vec<ToRender> = Vec::new();
    let mut atlas_stale = false;
    let mut first_frame = true;
    let mut texts: Vec<String> = Vec::new();
    // The language the texts are shown in right now (`locale`, with `translations`).
    let mut shown_locale: Option<usize> = None;
    let mut key_repeat: Option<(u32, u32)> = Some((400, 33));
    // Each popup of the scene: whether it is open, where, and at what size.
    let mut popups: Vec<Option<[i32; 4]>> = Vec::new();
    // The lock surfaces that are engaged (or requested).
    let mut locks: Vec<usize> = Vec::new();
    let mut uniforms = [0f32; N_UNIFORMS];
    let mut size = (720.0f32, 224.0f32);
    // If `--record` was asked for, the header is written once.
    let mut trace_header_done = false;
    let mut region: Vec<[i32; 4]> = vec![[i32::MIN; 4]];

    // ── state ────────────────────────────────────────────────────
    let mut scene = Scene::default();
    let mut props: Vec<Animated> = Vec::new();
    let mut facts: Vec<f32> = Vec::new();
    let mut inside: Vec<bool> = Vec::new();
    // Whether each zone was alive last round: one that only moved under a
    // pointer that did not is not entered or left by it (see the zones, below).
    let mut was_live: Vec<bool> = Vec::new();
    let mut blinks: Vec<(Instant, Option<Instant>)> = Vec::new();
    // What each `follow` and `bind` last came to, and what it reads.
    let mut follows = Follows::default();
    // The facts as they were when what is bound was last computed (see the rules).
    let mut facts_at_binds: Vec<f32> = Vec::new();
    // Who reads what, to ask only those whose inputs changed.
    let mut readers = Readers::default();
    let mut pending: Vec<(Instant, Transition)> = Vec::new();
    let mut layers: Vec<LayerState> = Vec::new();
    let mut rules: Vec<RuleState> = Vec::new();
    let mut gesture: Option<Playback> = None;
    // What a keyframe emits is handled on the next frame.
    let mut late_signals: Vec<SignalId> = Vec::new();
    let mut pointer: Option<(f32, f32)> = None;
    // The light a click leaves on the glass: where, and how much is left. It lights up
    // on pressing and goes out by itself, slowly.
    let mut finger = (0.0f32, 0.0f32);
    let mut finger_light = 0.0f32;
    let mut finger_down = false;
    // The ring a press sends through the glass: where, and since when.
    let mut ripple: Option<((f32, f32), Instant)> = None;
    // The mouse on the whole desktop, and the monitors, when the system says.
    let mut cursor: Option<((f32, f32), Vec<(String, [i32; 4])>)> = None;
    // What is being dragged: which zone, and where the mouse was when pressed.
    let mut drag: Option<(usize, (f32, f32), Instant)> = None;
    // A window asked to be carried or stretched from its own frame (slot, 1
    // carried or 2 stretched, edges; 0 let go), and the handle pressed for it.
    let mut handle_asked: Option<(usize, u32, u32)> = None;
    let mut handle_held: Option<usize> = None;
    let mut cursor_set = Cursor::Normal;

    // The keyboard that has been asked of the compositor, whether it has the focus, and whether it has
    // been asked for exclusively (see `Effect::FocusField`).
    let mut keyboard_set: Option<Keyboard> = None;
    let mut have_keyboard = false;
    let mut keyboard_lent: Option<Instant> = None;
    // Since when the scene stopped wanting the keyboard while it was lent.
    let mut loan_grace: Option<Instant> = None;
    // The field being typed into, and the key that has been left held down.
    let mut editing: Option<Editing> = None;
    // The field that was being typed in when the keyboard went: it comes back
    // with it. Moving between two of the scene's surfaces (the finder and its
    // catcher, a copy on each monitor) the compositor says «gone» and «here»
    // in a row; forgetting the field there left it on another one.
    let mut parked: Option<Editing> = None;
    // What is selected of a text that can be (`selectable: true`), and the
    // clicks in a row on it: a second one takes a word, a third the whole text.
    let mut selection: Option<TextSelection> = None;
    let mut clicks: (Instant, (f32, f32), u32) = (Instant::now(), (0.0, 0.0), 0);
    // The zone a drag out was tried from, until the button is let go.
    let mut carry_tried: Option<usize> = None;
    let mut repeat: Option<(String, Option<String>, Mods, Instant)> = None;
    // For a logic that plays: which keys are held —by their code, since the
    // name a key has can change with Shift between going down and coming up—,
    // which buttons of the mouse, and how long since the last frame it heard of.
    let mut keys_held: Vec<(u32, String)> = Vec::new();
    let mut mouse_buttons = 0u8;
    let mut frame_told = Instant::now();
    let mut last_key = Instant::now();
    let mut last_pointer: Option<(f32, f32)> = None;
    let mut last_activity = Instant::now();
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let start = Instant::now();

    let mut history = [0f32; 120];
    let mut cycle = Cycle::default();
    let mut last = Instant::now();
    let mut resting = false;
    let mut next_appointment: Option<Instant> = None;
    let mut period_ms = 16.7f32;
    let mut last_presented = Instant::now();
    // Which edge each surface is attached to right now, so as not to ask for it twice.
    let mut levels_set: Vec<crate::scene::Level> = Vec::new();
    let mut reserves_set: Vec<i32> = Vec::new();
    // How many frames a second at most right now: `rate:`, or its lower one
    // while its `while` holds. 0 is the monitor's.
    let mut rate_now: u32 = 0;
    let mut anchors_set: Vec<crate::scene::SurfaceAnchor> = Vec::new();
    let mut layers_seen: u64 = 0;
    // The "this does not compile" banner, and the good scene with it on top.
    let mut warning: Option<Vec<Instr>> = None;
    let mut with_warning: Vec<Instr> = Vec::new();
    let mut next_frame = Instant::now();
    let mut frames_for_period = 0u32;
    // The compositor inside the scene (`windows`), once the scene asks for it;
    // it outlives reloads, and so do the programs in it.
    let mut nest: Option<crate::platform::NestSender> = None;
    let mut nest_windows: Vec<NestWindow> = Vec::new();
    // Which zone is which window's, the pointer as last told to it, the window
    // holding the buttons pressed on it, and the keys it was handed.
    let mut nest_zones: Vec<Option<usize>> = Vec::new();
    let mut nest_pointer: Option<(usize, f64, f64)> = None;
    let mut nest_grab: Option<usize> = None;
    let mut nest_dragging = false;
    let mut nest_buttons: Vec<u32> = Vec::new();
    let mut nest_keys: Vec<u32> = Vec::new();
    let mut nest_size: (i32, i32) = (0, 0);
    // A window drew something new: what it covers is painted again.
    // Which windows drew something new this round: what they cover is painted again.
    // A surface presented its first frame: the input regions are given again (see `Sheet::presented`).
    let mut regions_again = false;
    let mut nest_changed: Vec<usize> = Vec::new();
    // The windows someone is sharing: each is read back after it draws.
    let mut nest_watch: Vec<(usize, u64, std::sync::mpsc::Sender<crate::scene::WindowPicture>)> = Vec::new();
    let mut nest_watch_due: Vec<usize> = Vec::new();
    let mut nest_cursor = Cursor::Normal;
    // Which layers of the windows' texture are taken, and whether the
    // compositor already knows what the card can read straight from a program.
    let mut nest_layers: Vec<bool> = Vec::new();
    let mut nest_gpu_told = false;
    // The order of the windows, which monitor each one is on, and which one the pointer is on.
    let mut nest_order: Vec<usize> = Vec::new();
    let mut nest_screens: Vec<usize> = Vec::new();
    let mut nest_on_screen: Option<usize> = None;
    let mut desks = Desks::default();
    let mut over_later: Option<usize> = None;
    let mut pointer_later = false;
    // The last `z:` values and the arrangement they gave.
    let mut z_memo: Option<(Vec<f32>, Option<crate::scene::ZArrangement>)> = None;
    // Where the windows' instructions are, by how many instructions there were.
    let mut window_instrs: Option<(usize, Vec<usize>)> = None;
    // The buffers copied this round, and the copy to wait for before handing them back.
    let mut nest_copied: (Vec<u64>, bool) = (Vec::new(), false);
    // Programs' buffers the card is still copying, by the work they went in:
    // they go back as soon as it says it has finished, and nobody waits for it.
    let mut nest_lent: Vec<(crate::gpu::Sent, Vec<u64>)> = Vec::new();
    let mut last_card_ask = Instant::now();
    // Where each window was last told to be seen.
    let mut nest_shown: std::collections::HashMap<usize, (String, [i32; 4])> = Default::default();
    // Programs' buffers already destroyed that a window is still showing.
    #[cfg(target_os = "linux")]
    let mut nest_doomed: Vec<u64> = Vec::new();
    // Windows closed whose last image is kept while the scene shows them leaving.
    let mut nest_closed: Vec<usize> = Vec::new();

    // `PLEAMAR_TIMING=1`: where a frame's time goes, section by section, without the waits.
    // Measured too while `pleamar --report` asks for it (`probe`), but only said then.
    let timing = crate::gpu::timing_enabled();
    let mut profiling: bool;
    let mut probe: Option<crate::probe::Probe> = None;
    // What the pacing slept in a round: waiting for the screen, not painting.
    let mut sec_paced = 0f64;
    let mut prof = [0f64; 8];
    // The same, in the thread's own CPU time: what the wall clock counts includes waiting.
    let mut prof_cpu = [0f64; 8];
    let mut prof_c = thread_cpu_ms();
    let mut prof_frames = 0u32;
    let mut prof_t = Instant::now();
    let mut prof_since = Instant::now();
    let mut prof_window_frames = 0u32;
    let mut prof_painted = 0u32;
    let mut prof_awake = [0u32; 8];
    // The thread's CPU outside the sections: taking what has arrived, at the
    // start of a round, and waiting for its turn to paint.
    let mut prof_outside = [0f64; 2];
    let mut prof_arrived = [0u32; 10];
    let mut prof_named: std::collections::HashMap<&'static str, u32> = std::collections::HashMap::new();
    let mut prof_sheets: Vec<u32> = Vec::new();
    let mut prof_moving: Option<usize> = None;
    let mut prof_alive_by: Vec<String> = Vec::new();
    // This round's sections, and how long it waited for its turn to paint: a
    // slow frame says them, which an average of 300 does not.
    let mut sec = [0f64; 8];
    let mut sec_wait = 0f64;
    // Reduced motion: asked at the start (`--reduced-motion`), or said by the
    // system or from outside as the fact `motion.reduced` (where it is in the
    // scene, kept: looked for again only if the scene changes).
    let mut reduced_motion = op.reduced_motion;
    let mut reduced_at: Option<usize> = None;
    loop {
        // ── 1. what has arrived ─────────────────────────────────
        let mut block = None;
        // (which button, whether it goes down or up), the wheel notches and the keys of this frame.
        let mut buttons: Vec<(u8, bool)> = Vec::new();
        // Which of them come from an agent's own seat.
        let mut buttons_agent: Vec<bool> = Vec::new();
        let mut wheel = 0.0f32;
        // What fingers moved (not a wheel): for a window as it is, already
        // counted in `wheel` as notches for the zones.
        let mut fingers = (0.0f32, 0.0f32, 0.0f32);
        let mut fingers_up = false;
        let mut keys: Vec<String> = Vec::new();
        let mut key_presses: Vec<(String, Option<String>, Mods, u32)> = Vec::new();
        let mut key_releases: Vec<(String, u32)> = Vec::new();
        let mut focus_changes: Vec<bool> = Vec::new();
        let mut focus_asked = false;
        let mut drops: Vec<(String, String)> = Vec::new();
        // `drag.over` going back to 0 in the same round a drop arrives: after
        // it, so the zones only there while something is over receive it.
        if let Some(k) = over_later.take() {
            facts[k] = 0.0;
        }
        if std::mem::take(&mut pointer_later) {
            pointer = None;
            real_pointer = None;
            finger_down = false;
        }
        // (which one, whether it comes from the logic)
        let mut signals: Vec<(usize, bool, Option<f32>)> = late_signals.drain(..).map(|s| (s.0 as usize, false, None)).collect();
        let mut gestures_asked: Vec<usize> = Vec::new();
        // What a `watch` is told of this frame besides what changed: events, and presses.
        let mut heard_events: Vec<(&str, &'static str)> = Vec::new();
        let mut incoming: Vec<ToRender> = std::mem::take(&mut held_back);
        if resting {
            // Still: not a single frame. Only a message or the next appointment wakes it
            // up: a delay that expires, a blink, a claim that runs out.
            let until = next_appointment.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            // The compositor's notice that a frame was shown does not wake it:
            // resting, there is nothing to paint after it. It woke it once per
            // frame of a window in the scene —a round for nothing, every time—.
            while incoming.is_empty() {
                // A program's buffer still on loan is looked at every couple of
                // milliseconds, without a round: it goes back as soon as it is copied.
                if let (Some(g), Some(send)) = (&gpu, &nest) {
                    let ask = last_card_ask.elapsed() >= Duration::from_millis(5);
                    if ask && !nest_lent.is_empty() {
                        last_card_ask = Instant::now();
                    }
                    nest_lent.retain(|(index, buffers)| {
                        let done = index.finished() || (ask && g.is_done(index));
                        if done {
                            send(ToNest::Released(buffers.clone()));
                        }
                        !done
                    });
                }
                let wait = until.saturating_duration_since(Instant::now());
                let wait = if nest_lent.is_empty() { wait } else { wait.min(Duration::from_millis(2)) };
                match rx.recv_timeout(wait) {
                    // A sheet that skipped a round for it paints now: it wakes the render.
                    Ok(ToRender::Frame(id)) if owed.contains(&id) => incoming.push(ToRender::Frame(id)),
                    Ok(ToRender::Frame(id)) => {
                        frame_ready |= frame_requested == Some(id);
                        if !seen_again.contains(&id) {
                            seen_again.push(id);
                        }
                        in_flight.retain(|f| f.0 != id);
                        if !answers.contains(&id) {
                            answers.push(id);
                        }
                    }
                    Ok(ToRender::Thumbnails) => {
                        #[cfg(target_os = "linux")]
                        if let Some(g) = &mut gpu {
                            g.check_thumbnails();
                        }
                    }
                    Ok(m) => incoming.push(m),
                    Err(RecvTimeoutError::Timeout) if Instant::now() < until => {}
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            resting = false;
            last = Instant::now() - Duration::from_secs_f32(period_ms / 1000.0);
        }
        // A logic that plays hears every frame before it is made: how long since
        // the last one it was told of, and where the mouse is. It is given a
        // moment to answer, so that what it says goes into this very frame
        // —where the ball is now, not where it was—; if it takes longer the
        // frame is made without it, as ever, and what it says goes into the
        // next. One at a time: while it has not finished with one, the next
        // are not queued behind it.
        let plays = crate::scene::FRAME_LISTENERS.load(std::sync::atomic::Ordering::Relaxed) > 0;
        if plays && !crate::scene::FRAME_OWED.swap(true, std::sync::atomic::Ordering::AcqRel) {
            let now = Instant::now();
            let _ = to_logic.send(Event::Frame((now - frame_told).as_secs_f32().min(0.1), pointer, mouse_buttons));
            frame_told = now;
            let until = now + FRAME_PATIENCE;
            loop {
                match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
                    Ok(ToRender::FrameSeen) => break,
                    Ok(m) => incoming.push(m),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
        incoming.extend(rx.try_iter());
        if let (Some(g), Some(send)) = (&gpu, &nest) {
            // The card is asked at most every 5 ms from here: its notices are set
            // by whoever asks, and each monitor already does, every frame.
            let ask = last_card_ask.elapsed() >= Duration::from_millis(5);
            if ask && !nest_lent.is_empty() {
                last_card_ask = Instant::now();
            }
            nest_lent.retain(|(index, buffers)| {
                let done = index.finished() || (ask && g.is_done(index));
                if done {
                    send(ToNest::Released(buffers.clone()));
                }
                !done
            });
        }
        // A press, and the pointer moving on in the same round (a finger that
        // presses and goes, a remote's hands that send both at once): the
        // press is the zone's under where it was pressed, the moving and what
        // follows it, the next round's.
        let mut pressed = false;
        if timing {
            for m in &incoming {
                let kind = match m {
                    ToRender::Frame(_) => 0,
                    ToRender::Cursor(..) => 1,
                    ToRender::Pointer(_) => 2,
                    ToRender::Fact(..) | ToRender::ExternalFact(..) => 3,
                    ToRender::Text(..) => 4,
                    ToRender::Command(_) => 5,
                    ToRender::Workshop(_) => 6,
                    ToRender::Backdrop(_) => 7,
                    ToRender::Nest(_) => 8,
                    _ => 9,
                };
                prof_arrived[kind] += 1;
                if let ToRender::Fact(name, _) | ToRender::ExternalFact(name, _) | ToRender::Text(name, _) = m {
                    *prof_named.entry(name).or_insert(0) += 1;
                }
            }
        }
        for m in incoming {
            if pressed && matches!(m, ToRender::Pointer(_) | ToRender::Button(..) | ToRender::AgentButton(..)) {
                held_back.push(m);
                continue;
            }
            pressed |= matches!(m, ToRender::Button(_, true) | ToRender::AgentButton(_, true));
            match m {
                // A reload that does not go through is shown on the surface, not only on
                // a console that nobody may be looking at. It is composed
                // once, on arrival: while it lasts, the good scene is painted with the
                // banner on top.
                ToRender::ReloadError(what) => {
                    // `size: full` does not tell its width until the compositor
                    // configures it: then it is the screen's, which is `screen.width`.
                    let width = match scene.surface().width {
                        0 => facts.first().copied().unwrap_or(900.0),
                        w => w as f32,
                    };
                    warning = what.map(|m| error_banner(&m, width));
                    with_warning.clear();
                    if let Some(banner) = &warning {
                        with_warning.extend(scene.instrs.iter().cloned());
                        with_warning.extend(banner.iter().cloned());
                    }
                }
                ToRender::Scene(fresh) => {
                    crate::platform::release_memory();
                    crate::platform::CURSOR_WANTED.store(fresh.wants_cursor, std::sync::atomic::Ordering::Relaxed);
                    // The properties with the same name survive the change.
                    let old: Vec<(&str, Animated)> =
                        scene.props.iter().map(|p| p.0).zip(props.iter().copied()).collect();
                    props = fresh
                        .props
                        .iter()
                        .map(|(name, initial, spring)| {
                            old.iter().find(|(n, _)| n == name).map(|(_, a)| *a).unwrap_or(Animated::at(*initial, *spring))
                        })
                        .collect();
                    // What was true and what the texts said also survives: on a
                    // hot reload, the scene carries on where it was.
                    let hot = !scene.props.is_empty();
                    // Whoever follows names finds them again in the new one.
                    tails.iter_mut().for_each(crate::agent::Follow::lost);
                    facts = fresh.facts.iter().map(|(n, initial)| scene.facts.iter().position(|h| h.0 == *n).map_or(*initial, |k| facts[k])).collect();
                    inside = vec![false; fresh.zones.len()];
                    was_live = vec![false; fresh.zones.len()];
                    follows = Follows::default();
                    warning = None;
                    with_warning.clear();
                    anchors_set = fresh.surfaces.iter().map(|s| s.anchor).collect();
                    blinks = fresh
                        .behaviors
                        .iter()
                        .filter_map(|c| match c {
                            Behavior::Blink { every, .. } => {
                                Some((Instant::now() + Duration::from_secs_f32(rng.between(every.0, every.1) * 0.6), None))
                            }
                            _ => None,
                        })
                        .collect();
                    layers = fresh.layers.iter().map(|c| LayerState::new(c.claims.len(), hot)).collect();
                    rules = fresh
                        .rules
                        .iter()
                        .map(|r| {
                            let mut e = RuleState::default();
                            if let Trigger::Every { between, .. } = &r.when {
                                e.next = Some(Instant::now() + Duration::from_secs_f32(rng.between(between.0, between.1)));
                            }
                            e
                        })
                        .collect();
                    gesture = None;
                    pending.clear();
                    // What it measures. `full` (0) is what its monitor measures, which
                    // the surface already knows: it is kept, or a reload left the scene
                    // 0 × 0 and everything drawn from its corner —a wallpaper— went
                    // unpainted.
                    let hud = if op.hud { HUD_HEIGHT } else { 0.0 };
                    size = (
                        if fresh.surface().width == 0 { size.0 } else { fresh.surface().width as f32 },
                        if fresh.surface().height == 0 { size.1 } else { fresh.surface().height as f32 + hud },
                    );
                    texts = fresh.texts.iter().map(|(n, initial)| scene.texts.iter().position(|t| t.0 == *n).map_or_else(|| initial.clone(), |k| texts[k].clone())).collect();
                    shown_locale = None;
                    atlas_stale = true;
                    // A surface saved at another size, edge, margin, room or level
                    // changes in place: it kept its first ones until the scene
                    // started again, and what grew past them was cut off. One that
                    // became another kind of thing or went to other monitors is
                    // only made again when the scene starts again: it says so.
                    let shape = |s: &crate::scene::Surface| (s.width, s.height, s.anchor, s.margin, s.exclusive_zone, s.level);
                    for (k, (was, now)) in scene.surfaces.iter().zip(&fresh.surfaces).enumerate() {
                        if shape(was) != shape(now) {
                            crate::platform::reshape(k, now);
                        }
                        if was.window != now.window || was.lock_screen != now.lock_screen || was.screens != now.screens || was.keyboard != now.keyboard {
                            println!("reload · surface {k} is now another kind of surface, on other monitors or with another keyboard: that is applied when the scene starts again");
                        }
                    }
                    if !scene.surfaces.is_empty() && scene.surfaces.len() != fresh.surfaces.len() {
                        println!("reload · the scene now has {} surfaces instead of {}: the new ones appear when it starts again", fresh.surfaces.len(), scene.surfaces.len());
                    }
                    println!(
                        "render · scene: {} properties, {} instructions, {} facts, {} layers, {} gestures, {} rules, {} zones",
                        fresh.props.len(), fresh.instrs.len(), fresh.facts.len(), fresh.layers.len(),
                        fresh.gestures.len(), fresh.rules.len(), fresh.zones.len()
                    );
                    scene = fresh;
                    z_memo = None;
                    window_instrs = None;
                    draw.hidden.clear();
                    draw.new_scene();
                    zone_memo = ZoneMemo::default();
                    if let Some(n) = &scene.nest {
                        if nest.is_none() {
                            nest = crate::platform::start_nest(n.max, op.to_self.clone());
                        }
                        nest_windows.resize_with(nest_windows.len().max(n.max), NestWindow::default);
                        for w in &mut nest_windows {
                            for p in &mut w.pieces {
                                p.uploaded = false;
                            }
                            w.ask = None;
                        }
                        nest_size = (0, 0);
                    }
                    // A window's zone is named like it: `win.3`, with the copy's mark if it has one.
                    nest_zones = scene
                        .zones
                        .iter()
                        .map(|z| {
                            let n = scene.nest.as_ref()?;
                            let bare = z.id.split('#').next().unwrap_or(z.id);
                            bare.strip_prefix(&format!("{}.", n.name))?.parse::<usize>().ok().filter(|k| *k < n.max)
                        })
                        .collect();
                    // Its own shaders: the pipeline is only remade if they changed.
                    if let Some(g) = gpu.as_mut() {
                        g.set_lens_image(scene.lens_image.as_deref());
                        g.set_shaders(&scene.shaders);
                        g.set_blur(scene.blur.as_ref());
                    }
                }
                ToRender::Sheet(n) => {
                    let g = gpu.get_or_insert_with(|| {
                        Gpu::new(&instance, match &n.target {
                            crate::gpu::Target::Surface(s) => Some(s),
                            crate::gpu::Target::Frames(_) => None,
                        })
                    });
                    g.set_lens_image(scene.lens_image.as_deref());
                    g.set_shaders(&scene.shaders);
                    g.set_blur(scene.blur.as_ref());
                    letters.allow_atlas(g.atlas_most());
                    // A scene that asks for "the full width" measures whatever its monitor measures, and it
                    // can know it: `screen.width`.
                    // A window measures whatever the compositor has given it, and that can change.
                    let is_window = scene.surface().window.is_some();
                    let its_own = n.view.surface == 0 && n.view.popup.is_none();
                    // A named surface publishes what it measures (see `SheetSize`). A
                    // lock one has a copy per monitor, and they all share the name: the
                    // largest of them, so that a background drawn that big covers each
                    // —the last one alone gave a portrait monitor a landscape picture—.
                    let measures = if scene.surfaces.get(n.view.surface).is_some_and(|s| s.lock_screen) {
                        sheets.iter().filter(|l| l.view.surface == n.view.surface && l.view.popup.is_none()).fold((n.size.0 as f32, n.size.1 as f32), |(w, h), l| (w.max(l.view.size.0), h.max(l.view.size.1)))
                    } else {
                        (n.size.0 as f32, n.size.1 as f32)
                    };
                    if let Some((w, h)) = scene.surfaces.get(n.view.surface).and_then(|s| s.size_props).filter(|_| n.view.popup.is_none()) {
                        for (p, v) in [(w, measures.0), (h, measures.1)] {
                            props[p.0 as usize] = Animated { x: v, v: 0.0, target: v, spring: props[p.0 as usize].spring };
                        }
                    }
                    if (scene.surface().width == 0 || is_window) && its_own {
                        size.0 = n.size.0 as f32;
                    }
                    // And the whole height too: `size: full, full` measures its monitor
                    // both ways. It used to take only the width, and a scene laid out
                    // with `screen.height` —a whole desktop— got 0 for it.
                    if (scene.surface().height == 0 || is_window) && its_own {
                        size.1 = n.size.1 as f32;
                    }
                    // Only the scene's own surface says what the screen measures: a
                    // named one —a corner of 120 px— said it was the screen's height.
                    if its_own {
                        let height = if is_window || scene.surface().height == 0 { n.size.1 as f32 } else { scene.surface().height as f32 };
                        for (k, (name, _)) in scene.facts.iter().enumerate() {
                            match *name {
                                "screen.width" => facts[k] = size.0,
                                "screen.height" => facts[k] = height,
                                _ => {}
                            }
                        }
                    }
                    // With `screens: each`, each copy knows which monitor it belongs to: its name and
                    // what it measures. It is what lets it show its own things and not the other one's.
                    // Only the copies of the main one: a surface with a name —the
                    // recording corner, the lock— also has instance 0, and when it
                    // arrived later it TRAMPLED `screen.0.name` with its monitor. With the
                    // corner on DP-3, both copies said "DP-3" and the logic did not
                    // know which was which.
                    // A scene whose surfaces all have a name —a shell: a background, a bar,
                    // panels— has no main one: then the copies of its FIRST surface say it,
                    // and only those (the others would trample them as above). Without
                    // them `screen.width` was 0 inside every one of its surfaces.
                    let no_main = !scene.surfaces.iter().any(|s| s.name.is_empty());
                    let first = scene.surfaces.first().map(|s| s.name.as_str()).unwrap_or("");
                    if let Some(its_own) = scene.surfaces.get(n.view.surface).filter(|s| s.name.is_empty() || (no_main && s.name == first)) {
                        let k = its_own.instance;
                        if let Some(i) = scene.texts.iter().position(|t| t.0 == format!("screen.{k}.name")) {
                            texts.resize(scene.texts.len().max(texts.len()), String::new());
                            texts[i] = n.name.clone();
                            let _ = to_logic.send(Event::Text(scene.texts[i].0, n.name.clone()));
                        }
                        for (part, v) in [("width", n.size.0 as f32), ("height", n.size.1 as f32)] {
                            if let Some(i) = scene.facts.iter().position(|h| h.0 == format!("screen.{k}.{part}")) {
                                facts[i] = v;
                            }
                        }
                    }
                    {
                        // Which surface of the scene it is, not just the sheet number: with
                        // `screens: each` there are several alike and it is worth knowing which landed where.
                        let which = scene.surfaces.get(n.view.surface).map_or(String::new(), |s| match (s.name.as_str(), s.instance) {
                            ("", 0) => String::new(),
                            ("", k) => format!(" · copy {k}"),
                            (name, 0) => format!(" · {name}"),
                            (name, k) => format!(" · {name} copy {k}"),
                        });
                        println!("render · surface {} on {} · {}×{} · scale {} · {:.0} Hz{which}", n.id, n.name, n.size.0, n.size.1, n.scale, n.mhz as f32 / 1000.0);
                    }
                    sheets.push(g.sheet(*n, size));
                    if let Some(i) = scene.facts.iter().position(|h| h.0 == "screens.count") {
                        facts[i] = screens_showing(&sheets, &scene.surfaces) as f32;
                    }
                    assign_pace(g, &mut sheets, size, op.no_vsync);
                    region = vec![[i32::MIN; 4]];
                }
                ToRender::SheetGone(id) => {
                    sheets.retain(|l| l.id != id);
                    crate::platform::sheet_released(id);
                    println!("render · surface {id} gone; {} left", sheets.len());
                    if let Some(i) = scene.facts.iter().position(|h| h.0 == "screens.count") {
                        facts[i] = screens_showing(&sheets, &scene.surfaces) as f32;
                    }
                    if let Some(g) = &gpu {
                        assign_pace(g, &mut sheets, size, op.no_vsync);
                    }
                }
                // A text laid out, an image loaded: the list is made again with
                // it. Kept as it was, a scene that does not move painted its old
                // list —made before the text was there— and never another: after
                // a reload its texts stayed unwritten.
                ToRender::Workshop(p) => {
                    letters.receive(*p);
                    compose_memo = None;
                    draw.forget_kept();
                }
                // A window someone stretches: the sheet changes, and with it what the
                // scene reads in `screen.width` and `screen.height`.
                // A capture of what is behind a glass: the background is unmixed, and if
                // something has changed, that sheet is painted again. And the next one is
                // asked for, which will not arrive until something changes on screen.
                ToRender::Backdrop(d) => {
                    if let Some(g) = &gpu {
                        handle_backdrop(g, &mut sheets, d);
                    }
                }
                ToRender::Frame(id) => {
                    if frame_requested == Some(id) {
                        frame_ready = true;
                    }
                    if !seen_again.contains(&id) {
                        seen_again.push(id);
                    }
                    in_flight.retain(|f| f.0 != id);
                    if !answers.contains(&id) {
                        answers.push(id);
                    }
                }
                ToRender::SheetSize(id, new_size) => {
                    if let (Some(g), Some(l)) = (&gpu, sheets.iter_mut().find(|l| l.id == id)) {
                        // A named surface publishes what it measures.
                        if let Some((w, h)) = scene.surfaces.get(l.view.surface).and_then(|s| s.size_props) {
                            for (p, v) in [(w, new_size.0), (h, new_size.1)] {
                                props[p.0 as usize] = Animated { x: v, v: 0.0, target: v, spring: props[p.0 as usize].spring };
                            }
                        }
                        if l.view.size != new_size {
                            l.view.size = new_size;
                            g.reconfigure(l, size);
                            if l.view.surface == 0 && l.view.popup.is_none() {
                                // What is painted outside the surface does not exist: if the
                                // window grows, the frame has to grow with it. The same for a
                                // surface that asked for the whole monitor (`size: full`) when
                                // its monitor changes mode: it measures what the monitor does now.
                                let is_window = scene.surface().window.is_some();
                                if is_window || scene.surface().width == 0 {
                                    size.0 = new_size.0;
                                }
                                if is_window || scene.surface().height == 0 {
                                    size.1 = new_size.1;
                                }
                                let height = if is_window || scene.surface().height == 0 { new_size.1 } else { scene.surface().height as f32 };
                                for (k, (name, _)) in scene.facts.iter().enumerate() {
                                    match *name {
                                        "screen.width" => facts[k] = size.0,
                                        "screen.height" => facts[k] = height,
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                ToRender::Scale(id, e) => {
                    if let (Some(g), Some(l)) = (&gpu, sheets.iter_mut().find(|l| l.id == id)) {
                        if (l.scale - e).abs() > 0.001 {
                            println!("render · surface {id}: scale {} → {e}", l.scale);
                            l.scale = e;
                            g.reconfigure(l, size);
                        }
                    }
                }
                ToRender::Command(Command::Animate(t)) => pending.push((Instant::now() + t.delay, t)),
                ToRender::Command(Command::Impulse { prop, velocity }) => props[prop.0 as usize].v += velocity,
                ToRender::Command(Command::Block(d)) => block = Some(d),
                // What is unread, told from outside (`--say wm "text win.badges discord:2"`): the docks show it.
                ToRender::Text(name, value) if scene.nest.as_ref().is_some_and(|n| name == format!("{}.badges", n.name)) => {
                    if let Some(k) = scene.texts.iter().position(|t| t.0 == name) {
                        texts[k] = value;
                    }
                    if let Some(n) = scene.nest.clone() {
                        nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                    }
                }
                ToRender::Text(name, value) => match scene.texts.iter().position(|t| t.0 == name) {
                    Some(i) => texts[i] = value,
                    None => eprintln!("render · I don't know the text '{name}'"),
                },
                ToRender::Fact(name, v) => match scene.facts.iter().position(|h| h.0 == name) {
                    Some(i) if name == "drag.over" && v == 0.0 && !drops.is_empty() => over_later = Some(i),
                    Some(i) => facts[i] = v,
                    None => eprintln!("render · I don't know the fact '{name}'"),
                },
                ToRender::LockScreen(held) => {
                    if !held {
                        // It has not granted it, or it has ended it: what was being painted is released
                        // and then the surfaces. It stays noted as requested, so as
                        // not to try again until the scene releases it itself.
                        for &k in &locks {
                            sheets.retain(|l| !(l.view.surface == k && l.view.popup.is_none()));
                            crate::platform::lock_screen(k, None);
                        }
                        if let Some(g) = &gpu {
                            assign_pace(g, &mut sheets, size, op.no_vsync);
                        }
                    }
                    if let Some(k) = scene.facts.iter().position(|(n, _)| *n == "lock.held") {
                        facts[k] = if held { 1.0 } else { 0.0 };
                    }
                }
                ToRender::PopupClosed(k) => {
                    // They have clicked outside: first what was painting in it is released, then it.
                    sheets.retain(|l| l.view.popup != Some(k));
                    crate::platform::popup(k, None);
                    if let Some(g) = &gpu {
                        assign_pace(g, &mut sheets, size, op.no_vsync);
                    }
                    if let Some(open) = popups.get_mut(k) {
                        *open = None;
                    }
                    if let Some(em) = scene.popups.get(k) {
                        let h = em.open.0 as usize;
                        facts[h] = 0.0;
                        let _ = to_logic.send(Event::Fact(scene.facts[h].0, 0.0));
                    }
                }
                ToRender::ExternalFact(name, written) => match scene.facts.iter().position(|h| h.0 == name) {
                    Some(i) => {
                        let kind = scene.types.iter().find(|(n, _)| n == name).map(|(_, t)| t);
                        let v = kind.and_then(|t| t.from_text(&written)).or_else(|| written.parse().ok()).or(match written.as_str() { "true" => Some(1.0), "false" => Some(0.0), _ => None });
                        match v {
                            Some(v) => {
                                facts[i] = v;
                                // The logic did not set it: let it know.
                                let _ = to_logic.send(Event::Fact(name, v));
                            }
                            None => eprintln!("render · '{name}' cannot be '{written}'"),
                        }
                    }
                    None => eprintln!("render · I don't know the fact '{name}'"),
                },
                ToRender::Probe(None) => probe = Some(crate::probe::Probe::new()),
                ToRender::Probe(Some(reply_to)) => {
                    let r = match probe.take() {
                        Some(p) => p.report(&crate::probe::Context {
                            card: gpu.as_ref().map_or_else(|| "none yet".into(), Gpu::describe),
                            card_memory: gpu.as_ref().and_then(|g| g.device.generate_allocator_report()).map(|r| r.total_allocated_bytes as f64 / 1e6),
                            monitors: sheets
                                .iter()
                                .map(|l| {
                                    let (w, h) = l.px();
                                    let hz = if l.mhz > 0 { format!("{:.0} Hz", l.mhz as f32 / 1000.0) } else { "refresh not told".into() };
                                    format!("{} · {w}×{h} · {hz} · scale {}{}{}", if l.name.is_empty() { "surface" } else { &l.name }, l.scale, if l.drives_pace { " · sets the pace" } else { "" }, if l.open { "" } else { " · closed" })
                                })
                                .collect(),
                            windows: nest_windows.iter().filter(|w| !w.pieces.is_empty()).count(),
                            scene: format!("{} instructions, {} properties, {} rules, {} zones", scene.instrs.len(), scene.props.len(), scene.rules.len(), scene.zones.len()),
                            rate: rate_now,
                        }),
                        None => "? it was not measuring: `probe start` first".into(),
                    };
                    let _ = reply_to.send(r);
                }
                ToRender::Act(act, reply_to) => acts.push_back((act, reply_to)),
                ToRender::Wait(src, reply_to) => match crate::agent::Cond::parse(&scene, &src) {
                    Ok((cond, timeout)) => {
                        let now = Instant::now();
                        waits.push((cond, now, now + timeout, reply_to));
                    }
                    Err(m) => {
                        let _ = reply_to.send(format!("? {m}\n"));
                    }
                },
                ToRender::Watch(lines, until) => {
                    let c = Ctx { props: &props, facts: &facts, seen: None };
                    let _ = lines.send(format!("watching for {:.0} s", until.saturating_duration_since(Instant::now()).as_secs_f32()));
                    watchers.push(Watcher { lines, until, start: Instant::now(), before: crate::agent::before(&scene, c, &texts, open_surfaces(&scene, &sheets)) });
                }
                ToRender::Follow(mut f) => {
                    f.start(&scene, &facts, &texts);
                    tails.push(*f);
                }
                ToRender::Describe(json, reply_to) => {
                    describing.push((json, reply_to));
                    draw.collect_texts = true;
                    compose_memo = None;
                }
                ToRender::Query(name, reply_to) => {
                    let number = |v: f32| if v.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v}") };
                    let r = if let Some(i) = scene.facts.iter().position(|h| h.0 == name) {
                        // As it would be written: `true`, `critical`, or the number.
                        match scene.types.iter().find(|(n, _)| n == name) {
                            Some((_, t)) => t.to_text(facts[i]),
                            None => number(facts[i]),
                        }
                    } else if let Some(i) = scene.texts.iter().position(|t| t.0 == name) {
                        texts.get(i).cloned().unwrap_or_default()
                    } else if let Some(i) = scene.props.iter().position(|p| p.0 == name) {
                        number(props[i].x)
                    } else {
                        format!("? I don't know '{name}'")
                    };
                    let _ = reply_to.send(r);
                }
                ToRender::Signal(name) => match scene.signals.iter().position(|s| s.0 == name) {
                    Some(i) => signals.push((i, true, None)),
                    None => eprintln!("render · I don't know the event '{name}'"),
                },
                ToRender::ExternalSignal(name, payload) => match scene.signals.iter().position(|s| s.0 == name) {
                    Some(i) => signals.push((i, false, payload)),
                    None => eprintln!("render · I don't know the event '{name}'"),
                },
                ToRender::Gesture(name) => match scene.gestures.iter().position(|g| g.name == name) {
                    Some(i) => gestures_asked.push(i),
                    None => eprintln!("render · I don't know the gesture '{name}'"),
                },
                // The mouse moving anywhere is someone there: for `idle`, it counts
                // as much as moving it over the scene.
                ToRender::Cursor(at, monitors) => {
                    if cursor.as_ref().is_some_and(|c| c.0 != at) {
                        last_activity = Instant::now();
                    }
                    cursor = Some((at, monitors));
                }
                // Gone in the same round something is let go: after it, so the
                // zone it was let go on receives it.
                ToRender::Pointer(None) if !drops.is_empty() => pointer_later = true,
                ToRender::Pointer(p) => {
                    pointer = p;
                    real_pointer = p;
                    last_activity = Instant::now();
                    // Gone with the button down, its release goes elsewhere: a
                    // press that changed the input region under it (a panel that
                    // opens on a click) never heard it, and its light stayed on
                    // for good —the scene drew every frame of the monitor, at rest—.
                    if p.is_none() {
                        finger_down = false;
                    }
                }
                ToRender::Button(b, down) | ToRender::AgentButton(b, down) => {
                    buttons_agent.resize(buttons.len(), false);
                    buttons_agent.push(matches!(m, ToRender::AgentButton(..)));
                    buttons.push((b, down));
                    if b < 3 {
                        mouse_buttons = if down { mouse_buttons | 1 << b } else { mouse_buttons & !(1 << b) };
                    }
                    if b == 0 {
                        finger_down = down;
                        if let (true, Some(p)) = (down, pointer) {
                            finger = p;
                            finger_light = 1.0;
                            if !reduced_motion {
                                ripple = Some((p, Instant::now()));
                            }
                        }
                    }
                    last_activity = Instant::now();
                }
                ToRender::Wheel(d) => {
                    wheel += d;
                    last_activity = Instant::now();
                }
                ToRender::Scroll { x, y, notches } => {
                    wheel += notches;
                    fingers = (fingers.0 + x, fingers.1 + y, fingers.2 + notches);
                    last_activity = Instant::now();
                }
                ToRender::ScrollEnd => fingers_up = true,
                ToRender::Wake | ToRender::FrameSeen => {}
                #[cfg(target_os = "linux")]
                ToRender::Thumbnails => {
                    if let Some(g) = &mut gpu {
                        g.check_thumbnails();
                    }
                }
                #[cfg(not(target_os = "linux"))]
                ToRender::Thumbnails => {}
                ToRender::KeyRepeat(r) => key_repeat = r,
                ToRender::Key(name, typed, mods, code) => {
                    last_activity = Instant::now();
                    // If it is held down, it repeats, however this user has it set up.
                    repeat = key_repeat.map(|(delay, _)| (name.clone(), typed.clone(), mods, Instant::now() + Duration::from_millis(delay as u64)));
                    key_presses.push((name, typed, mods, code));
                }
                ToRender::KeyReleased(name, code) => {
                    if repeat.as_ref().is_some_and(|r| r.0 == name) {
                        repeat = None;
                    }
                    // A key handed to a window is released in it too: after the
                    // presses of this round, which may include its own.
                    key_releases.push((name, code));
                }
                ToRender::KeyboardFocus(yes) => {
                    focus_changes.push(yes);
                    if let Some(send) = &nest {
                        // What the windows had held is let go before they lose the
                        // keyboard: forgotten, it stayed held for the next one.
                        if !yes {
                            for code in nest_keys.drain(..) {
                                send(ToNest::Key { code, down: false });
                            }
                        }
                        send(ToNest::HostFocus(yes));
                    }
                }
                ToRender::WatchWindow(slot, who, to) => {
                    nest_watch.retain(|(s, w, _)| !(*s == slot && *w == who));
                    if let Some(to) = to {
                        nest_watch.push((slot, who, to));
                        nest_watch_due.push(slot);
                    }
                }
                ToRender::Nest(e) => {
                    let Some(n) = scene.nest.clone() else { continue };
                    let name = &n.name;
                    match e {
                        NestEvent::Socket(socket) => nest_text(&scene, &mut texts, &to_logic, &format!("{name}.socket"), socket),
                        NestEvent::Opened { slot, title, app, screen } => {
                            if nest_screens.len() < n.max {
                                nest_screens.resize(n.max, 0);
                            }
                            nest_screens[slot] = screen;
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.screen"), screen as f32);
                            // It opens on the pool its monitor shows.
                            desks.grow(n.max);
                            desks.pool[slot] = desks.shown[screen.min(3)];
                            desks.alive[slot] = true;
                            desks.apps[slot] = app.to_lowercase();
                            desks.icons[slot] = String::new();
                            desks.names[slot] = String::new();
                            desks.execs[slot] = String::new();
                            desks.dialog[slot] = false;
                            desks.minimized[slot] = false;
                            desks.floating[slot] = false;
                            desks.pending[slot] = n.manual;
                            // Said with `open`, not at the next order: a rule that waits for
                            // `open` reads it as it is.
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.pending"), n.manual as u8 as f32);
                            desks.stamp += 1;
                            desks.born[slot] = desks.stamp;
                            // What the slot showed before —a window closing, still fading— is no longer it.
                            nest_closed.retain(|s| *s != slot);
                            if let Some(w) = nest_windows.get_mut(slot) {
                                let mut gone = Vec::new();
                                for p in w.pieces.drain(..) {
                                    nest_layers[p.layer as usize] = false;
                                    gone.extend(p.buffer);
                                }
                                w.geometry = [0; 4];
                                #[cfg(target_os = "linux")]
                                if let Some(g) = gpu.as_mut() {
                                    nest_doomed.retain(|b| !gone.contains(b));
                                    g.forget_dmabufs(&gone);
                                }
                            }
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.open"), 1.0);
                            nest_text(&scene, &mut texts, &to_logic, &format!("{name}.{slot}.title"), title);
                            nest_text(&scene, &mut texts, &to_logic, &format!("{name}.{slot}.app"), app);
                        }
                        NestEvent::Title(slot, t) => nest_text(&scene, &mut texts, &to_logic, &format!("{name}.{slot}.title"), t),
                        NestEvent::App(slot, t) => {
                            desks.grow(n.max);
                            if slot < n.max {
                                desks.apps[slot] = t.to_lowercase();
                            }
                            nest_text(&scene, &mut texts, &to_logic, &format!("{name}.{slot}.app"), t);
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                        }
                        NestEvent::Program { slot, icon, name: program, exec } => {
                            desks.grow(n.max);
                            if slot < n.max {
                                desks.icons[slot] = icon.clone();
                                desks.names[slot] = program;
                                desks.execs[slot] = exec;
                            }
                            nest_text(&scene, &mut texts, &to_logic, &format!("{name}.{slot}.icon"), icon);
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                        }
                        NestEvent::Dock(pins) => {
                            desks.pins = pins;
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                        }
                        NestEvent::Fullscreen(slot, yes) => nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.fullscreen"), yes as u8 as f32),
                        NestEvent::Framed(slot, yes) => nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.framed"), yes as u8 as f32),
                        NestEvent::Urgent(slot, yes) => nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.urgent"), yes as u8 as f32),
                        NestEvent::Held { slot, how, edges } => {
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.edges"), edges as f32);
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.held"), how as f32);
                            handle_asked = Some((slot, how, edges));
                        }
                        NestEvent::Minimized(slot, yes) => {
                            // Brought back, it comes to the workspace its monitor shows:
                            // left on the one it was put away from, it had the keyboard
                            // and could not be seen.
                            if !yes && slot < n.max {
                                desks.grow(n.max);
                                let here = desks.shown[nest_screens.get(slot).copied().unwrap_or(0).min(3)];
                                if desks.pool[slot] != here {
                                    desks.pool[slot] = here;
                                    nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                                }
                            }
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.minimized"), yes as u8 as f32);
                            desks.grow(n.max);
                            if slot < n.max {
                                desks.minimized[slot] = yes;
                            }
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                        }
                        NestEvent::Floating(slot, yes) => {
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.floating"), yes as u8 as f32);
                            desks.grow(n.max);
                            if slot < n.max {
                                desks.floating[slot] = yes;
                            }
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                        }
                        NestEvent::Dialog(slot, yes) => {
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.dialog"), yes as u8 as f32);
                            desks.grow(n.max);
                            if slot < n.max {
                                desks.dialog[slot] = yes;
                            }
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                        }
                        NestEvent::Frame { slot, geometry, pieces } => {
                            prof_window_frames += 1;
                            if let Some(p) = &mut probe {
                                p.window_frames += 1;
                            }
                            // Buffers that were waiting to be copied and never will be.
                            #[allow(unused_mut)]
                            let mut unread: Vec<u64> = Vec::new();
                            if let Some(w) = nest_windows.get_mut(slot) {
                                let mut old = std::mem::take(&mut w.pieces);
                                for p in pieces {
                                    let kept = old.iter().position(|o| o.id == p.id).map(|k| old.remove(k));
                                    let mut piece = kept.unwrap_or_else(|| {
                                        // The first free layer, or one more.
                                        let layer = nest_layers.iter().position(|used| !used).unwrap_or(nest_layers.len());
                                        if layer == nest_layers.len() {
                                            nest_layers.push(true);
                                        }
                                        nest_layers[layer] = true;
                                        NestPiece { id: p.id, layer: layer as u32, at: p.at, size: p.size, px: p.px, src: p.src, opaque: false, pixels: Vec::new(), buffer: None, #[cfg(unix)] fresh: None, uploaded: false }
                                    });
                                    piece.at = p.at;
                                    piece.size = p.size;
                                    piece.px = p.px;
                                    piece.src = p.src;
                                    match p.content {
                                        PieceContent::Kept => {}
                                        PieceContent::Pixels(px) => {
                                            #[cfg(unix)]
                                            if let Some(before) = piece.fresh.take() {
                                                unread.push(before.buffer);
                                            }
                                            piece.pixels = px;
                                            piece.buffer = None;
                                            piece.uploaded = false;
                                        }
                                        #[cfg(unix)]
                                        PieceContent::Dmabuf(d) => {
                                            // (The card's buffers are Linux's: elsewhere nothing sends them.)
                                            #[cfg(target_os = "linux")]
                                            {
                                                piece.opaque = crate::dmabuf::opaque(d.fourcc);
                                            }
                                            // A frame on top of one not copied yet: that one goes back to
                                            // its program now. Kept, it was never handed back, and a
                                            // program with three buffers froze after losing them.
                                            if let Some(before) = piece.fresh.take() {
                                                if before.buffer != d.buffer {
                                                    unread.push(before.buffer);
                                                }
                                            }
                                            piece.pixels = Vec::new();
                                            piece.buffer = Some(d.buffer);
                                            piece.fresh = Some(d);
                                            piece.uploaded = false;
                                        }
                                    }
                                    w.pieces.push(piece);
                                }
                                for gone in old {
                                    nest_layers[gone.layer as usize] = false;
                                    #[cfg(unix)]
                                    if let Some(before) = gone.fresh {
                                        unread.push(before.buffer);
                                    }
                                }
                                if !unread.is_empty() {
                                    if let Some(send) = &nest {
                                        send(ToNest::Released(std::mem::take(&mut unread)));
                                    }
                                }
                                w.geometry = geometry;
                                nest_changed.push(slot);
                                if nest_watch.iter().any(|(s, _, _)| *s == slot) {
                                    nest_watch_due.push(slot);
                                }
                            }
                            // The buffers kept for a window that has now moved on go.
                            #[cfg(target_os = "linux")]
                            if !nest_doomed.is_empty() {
                                let shown: Vec<u64> = nest_windows.iter().flat_map(|w| w.pieces.iter().filter_map(|p| p.buffer)).collect();
                                let (still, gone): (Vec<u64>, Vec<u64>) = std::mem::take(&mut nest_doomed).into_iter().partition(|b| shown.contains(b));
                                nest_doomed = still;
                                if let Some(g) = gpu.as_mut() {
                                    g.forget_dmabufs(&gone);
                                }
                            }
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.width"), geometry[2] as f32);
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.height"), geometry[3] as f32);
                        }
                        NestEvent::Closed(slot) => {
                            // Its pool may dry up.
                            desks.grow(n.max);
                            if slot < n.max {
                                desks.alive[slot] = false;
                                nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                            }
                            // Whoever watched it (a share, a thumbnail) stops here: its slot
                            // will be another window's, and a share went on showing that one.
                            nest_watch.retain(|(s, _, _)| *s != slot);
                            // Its last image stays: the scene may want to see it leave.
                            // It goes once the scene no longer draws it.
                            if !nest_closed.contains(&slot) {
                                nest_closed.push(slot);
                            }
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.open"), 0.0);
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.focused"), 0.0);
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.fullscreen"), 0.0);
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.dialog"), 0.0);
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.minimized"), 0.0);
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.floating"), 0.0);
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.framed"), 0.0);
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.held"), 0.0);
                            if slot < desks.floating.len() {
                                desks.floating[slot] = false;
                                desks.pending[slot] = false;
                            }
                            if nest_grab == Some(slot) {
                                nest_grab = None;
                            }
                        }
                        NestEvent::Workspace(slot, ws) => {
                            desks.grow(n.max);
                            if slot < n.max {
                                let screen = nest_screens.get(slot).copied().unwrap_or(0);
                                desks.pool[slot] = desks.at(screen, ws.max(1), &nest_screens);
                                nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                                // Sent where it is not seen (a rule: `workspace 3`) with the
                                // keyboard: the keyboard stays where it is seen —the one that
                                // last had it on that workspace, or the first there—.
                                let screen = nest_screens.get(slot).copied().unwrap_or(0).min(3);
                                let focused = facts.iter().zip(&scene.facts).find(|(_, h)| h.0 == format!("{}.focus", n.name)).map_or(-1.0, |(v, _)| *v);
                                if focused == slot as f32 && desks.pool_of(slot) != desks.shown[screen] {
                                    if let Some(send) = &nest {
                                        let shown = desks.shown[screen];
                                        let here = desks.last.get(&shown).copied().filter(|k| *k != slot && nest_order.contains(k) && desks.pool_of(*k) == shown && nest_screens.get(*k) == Some(&screen));
                                        let first = || nest_order.iter().copied().find(|k| *k != slot && desks.pool_of(*k) == shown && nest_screens.get(*k) == Some(&screen));
                                        send(match here.or_else(first) {
                                            Some(k) => ToNest::Focus(k),
                                            None => ToNest::Blur,
                                        });
                                    }
                                }
                            }
                        }
                        NestEvent::Focused(which) => {
                            if let Some(k) = which {
                                desks.last.insert(desks.pool_of(k), k);
                                desks.grow(n.max.max(k + 1));
                                desks.stamp += 1;
                                desks.touched[k] = desks.stamp;
                            }
                            desks.focus = which;
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                            for k in 0..n.max {
                                nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{k}.focused"), (which == Some(k)) as u8 as f32);
                            }
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.focus"), which.map_or(-1.0, |k| k as f32));
                        }
                        NestEvent::Cursor(kind) => nest_cursor = kind,
                        // A program's drag takes the button over: the compositor
                        // drops it when it is let go, and that release never
                        // comes this way. Kept held, the window the drag began
                        // on kept the pointer for good —no other window got it,
                        // a terminal could not select any more—.
                        NestEvent::Dragging(yes) => {
                            nest_dragging = yes;
                            nest_buttons.clear();
                            nest_grab = None;
                        }
                        NestEvent::Pick(what) => nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.picking"), what as f32),
                        // A buffer a window is still showing is kept until it shows
                        // another: a program that resizes destroys the old one before
                        // the new frame arrives, and a copy asked for in between —the
                        // texture grew, the scene reloaded— found nothing to copy.
                        #[cfg(target_os = "linux")]
                        NestEvent::Forget(buffers) => {
                            let shown: Vec<u64> = nest_windows.iter().flat_map(|w| w.pieces.iter().filter_map(|p| p.buffer)).collect();
                            let (kept, gone): (Vec<u64>, Vec<u64>) = buffers.into_iter().partition(|b| shown.contains(b));
                            nest_doomed.extend(kept);
                            if let Some(g) = gpu.as_mut() {
                                g.forget_dmabufs(&gone);
                            }
                        }
                        #[cfg(not(target_os = "linux"))]
                        NestEvent::Forget(_) => {}
                        NestEvent::Reserved(screen, edges) => {
                            for (edge, v) in ["top", "right", "bottom", "left"].iter().zip(edges) {
                                nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.reserved.{screen}.{edge}"), v);
                            }
                        }
                        NestEvent::Screen(slot, screen) => {
                            if nest_screens.len() < n.max {
                                nest_screens.resize(n.max, 0);
                            }
                            nest_screens[slot] = screen;
                            // Carried there alone, it joins the pool shown there; its
                            // whole pool carried there (`send workspace`), it stays in it.
                            desks.grow(n.max);
                            if slot < n.max && desks.index(screen, desks.pool[slot]) == 0 {
                                desks.pool[slot] = desks.shown[screen.min(3)];
                            }
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.screen"), screen as f32);
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                        }
                        NestEvent::Moved(slot, screen) => {
                            if nest_screens.len() < n.max {
                                nest_screens.resize(n.max, 0);
                            }
                            desks.grow(n.max);
                            if slot < n.max {
                                let from = nest_screens[slot].min(3);
                                let to = screen.min(3);
                                nest_screens[slot] = screen;
                                let id = desks.pool[slot];
                                if id != 0 && desks.index(to, id) == 0 {
                                    desks.stack[to].push(id);
                                }
                                // Back where it was shown, and nothing shown there since:
                                // that pool is shown again.
                                if from != to && id != 0 && desks.left[to] == id && !desks.holds(to, desks.shown[to], &nest_screens) {
                                    desks.shown[to] = id;
                                    desks.left[to] = 0;
                                }
                                // Left behind with nothing in it, the one it showed there
                                // is not shown any more: the one beside it is.
                                if from != to && desks.shown[from] == id && !desks.holds(from, id, &nest_screens) {
                                    desks.left[from] = id;
                                    desks.stack[from].retain(|p| *p != id);
                                    desks.shown[from] = match desks.stack[from].first() {
                                        Some(p) => *p,
                                        None => {
                                            let p = desks.next;
                                            desks.next += 1;
                                            p
                                        }
                                    };
                                }
                            }
                            nest_fact(&scene, &mut facts, &to_logic, &format!("{name}.{slot}.screen"), screen as f32);
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                        }
                        NestEvent::Reveal(slot) => {
                            desks.grow(n.max);
                            if slot < n.max && desks.alive(slot) {
                                let screen = nest_screens.get(slot).copied().unwrap_or(0).min(3);
                                let id = desks.pool[slot];
                                if id != 0 && desks.shown[screen] != id {
                                    desks.shown[screen] = id;
                                    nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                                }
                            }
                        }
                        NestEvent::Order(order) => {
                            nest_order = order;
                            if nest_screens.len() < n.max {
                                nest_screens.resize(n.max, 0);
                            }
                            nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                        }
                    }
                }
                ToRender::FocusField(name) => {
                    parked = None;
                    editing = name.and_then(|n| scene.texts.iter().position(|t| t.0 == n)).map(|k| Editing { field: k, cursor: texts[k].len(), anchor: texts[k].len() });
                    last_key = Instant::now();
                    // Lent or not once this round's focus changes are in (below).
                    focus_asked = editing.is_some();
                }
                ToRender::Dropped(kind, data) => drops.push((kind, data)),
                ToRender::Repaint => {
                    for l in &mut sheets {
                        l.painted = None;
                    }
                }
                ToRender::Quit => {
                    cycle.close();
                    return;
                }
            }
        }
        // ── an agent's hand ─────────────────────────────────────
        if acting.is_some() || !acts.is_empty() {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            let absent: Vec<std::ops::Range<usize>> = scene.spans.iter().filter(|t| scene.surfaces.get(t.surface).is_some_and(|s| s.instance > 0) && !sheets.iter().any(|l| l.view.surface == t.surface)).map(|t| t.instrs.clone()).collect();
            let gone = |at: usize| draw.hidden.get(at).copied().unwrap_or(false) || absent.iter().any(|r| r.contains(&at));
            let rank = z_memo.as_ref().and_then(|(_, a)| a.as_ref()).map(|a| a.zone_rank.clone());
            let sight = crate::agent::Sight {
                shown: sheets.iter().filter(|s| s.open).map(|s| crate::agent::Shown { surface: s.view.surface, popup: s.view.popup, bounds: s.view.bounds(), scale: s.scale }).collect(),
                texts_seen: &[],
                gone: &gone,
                rank: rank.as_ref().map(|r| r.as_slice()),
            };
            let now = Instant::now();
            if acting.is_none()
                && let Some((act, reply)) = acts.pop_front()
            {
                match Acting::plan(&scene, c, &sight, act, reply, &texts, open_surfaces(&scene, &sheets)) {
                    Ok(a) => acting = Some(a),
                    Err((reply, m)) => {
                        let _ = reply.send(m + "\n");
                    }
                }
            }
            let mut failed = None;
            if let Some(a) = &mut acting {
                // One step a frame, as a hand: in, down, up, out. A wait lets frames go by.
                while let Some(step) = a.steps.front().cloned() {
                    match step {
                        Step::Cursor(trip, until) => match *trip.lock().unwrap() {
                            crate::platform::CursorTrip::Going if now < until => break,
                            crate::platform::CursorTrip::Stopped => {
                                failed = Some("? the user stopped the agent: stop here and tell them where you left it".to_owned());
                                break;
                            }
                            _ => {
                                a.hand = Some(a.from);
                                a.steps.pop_front();
                                break;
                            }
                        },
                        Step::Pause(d) => {
                            a.steps[0] = Step::Wait(now + d);
                            break;
                        }
                        Step::Wait(until) if now < until => break,
                        Step::Wait(_) => {}
                        Step::Scroll(deadline) => match crate::agent::reach_point(&scene, c, a.zone, &sight) {
                            Ok(None) if now < deadline => {
                                if !a.scrolled
                                    && let Some((prop, by)) = crate::agent::scroll_into_view(&scene, c, a.zone)
                                {
                                    let p = &mut props[prop.0 as usize];
                                    p.target = (p.target + by).max(0.0);
                                    a.scrolled = true;
                                }
                                break;
                            }
                            Ok(None) => {
                                failed = Some(format!("? {} could not be brought into sight", a.said_name));
                                break;
                            }
                            Ok(Some(_)) | Err(_) => {
                                if a.scrolled {
                                    a.said.insert_str(0, &format!("scrolled {} into sight\n", a.said_name));
                                }
                            }
                        },
                        Step::Point => match crate::agent::reach_point(&scene, c, a.zone, &sight) {
                            Ok(Some(p)) => {
                                a.from = p;
                                a.steps.pop_front();
                                // The agent's cursor there first, in the pixels of the window's
                                // picture; the hand comes in when it arrives, so what lights up
                                // lights up under it.
                                match sheets.iter().find(|s| s.open && s.view.popup.is_none() && crate::agent::inside_box(s.view.bounds(), p)) {
                                    Some(s) => {
                                        let (on, at) = cursor_place(&scene, s, p);
                                        a.steps.push_front(Step::Cursor(crate::platform::agent_cursor_to(on, at.0, at.1, true), now + Duration::from_millis(300)));
                                    }
                                    None => a.hand = Some(p),
                                }
                                break;
                            }
                            Ok(None) => {
                                failed = Some(format!("? {} is out of sight", a.said_name));
                                break;
                            }
                            Err(m) => {
                                failed = Some(m);
                                break;
                            }
                        },
                        Step::Move(dx, dy) => {
                            let p = (a.from.0 + dx, a.from.1 + dy);
                            a.hand = Some(p);
                            // The agent's cursor goes with the hand, step by step.
                            if let Some(s) = sheets.iter().find(|s| s.open && s.view.popup.is_none() && crate::agent::inside_box(s.view.bounds(), p)) {
                                let (on, at) = cursor_place(&scene, s, p);
                                crate::platform::agent_cursor_to(on, at.0, at.1, false);
                            }
                            a.steps.pop_front();
                            break;
                        }
                        Step::Down(b) => {
                            buttons.push((b, true));
                            if let (0, Some(p)) = (b, a.hand) {
                                finger_down = true;
                                finger = p;
                                finger_light = 1.0;
                                if !reduced_motion {
                                    ripple = Some((p, now));
                                }
                            }
                            a.steps.pop_front();
                            break;
                        }
                        Step::Up(b) => {
                            buttons.push((b, false));
                            if b == 0 {
                                finger_down = false;
                            }
                            a.steps.pop_front();
                            break;
                        }
                        Step::Wheel(n) => {
                            wheel += n;
                            a.steps.pop_front();
                            break;
                        }
                        Step::Leave => {
                            a.hand = None;
                            pointer = real_pointer;
                            a.steps.pop_front();
                            break;
                        }
                        Step::Type(k, text) => {
                            // As typing over what was there: all of it chosen, then the letters.
                            editing = Some(Editing { field: k, cursor: texts[k].len(), anchor: 0 });
                            last_key = now;
                            key_presses.push(if text.is_empty() { ("BackSpace".into(), None, Mods::default(), 0) } else { ("type".into(), Some(text), Mods::default(), 0) });
                            a.steps.pop_front();
                            break;
                        }
                        Step::Key(name, mods, typed) => {
                            key_presses.push((name, typed, mods, 0));
                            a.steps.pop_front();
                            break;
                        }
                    }
                    a.steps.pop_front();
                }
                if let Some(p) = a.hand {
                    pointer = Some(p);
                }
            }
            if let Some(m) = failed
                && let Some(a) = acting.take()
            {
                if a.hand.is_some() {
                    pointer = real_pointer;
                }
                let _ = a.reply.send(m + "\n");
            }
        }
        profiling = timing || probe.is_some();
        if profiling {
            prof_t = Instant::now();
            let c = thread_cpu_ms();
            prof_outside[0] += c - prof_c;
            prof_c = c;
        }
        // The key that is still held down counts again.
        if let Some((name, typed, mods, when)) = &mut repeat {
            if Instant::now() >= *when {
                // A repeat is made up here: a window repeats on its own, it is not handed one.
                key_presses.push((name.clone(), typed.clone(), *mods, 0));
                *when = Instant::now() + Duration::from_millis(key_repeat.map_or(33, |r| r.1) as u64);
            }
        }
        let mut submitted: Vec<usize> = Vec::new();
        for (name, typed, mods, code) in key_presses {
            // Ctrl+C with something of a text selected copies it, unless the
            // field being typed into has a selection of its own.
            if mods.ctrl && !mods.alt && !mods.logo && name == "c" && editing.as_ref().is_none_or(|e| e.cursor == e.anchor) {
                if let Some(s) = selection.as_ref().filter(|s| s.anchor != s.cursor) {
                    if let Some(t) = draw.selectable.iter().find(|t| t.at == s.at) {
                        let (a, b) = s.bytes(&t.text);
                        crate::platform::clipboard_write(&t.text[a..b]);
                    }
                    continue;
                }
            }
            // The field first: whatever is typing belongs to it. The rest —Escape, a
            // shortcut— goes on to the rules and to the logic. Only a field that is
            // drawn now: a hidden one (a search that is closed) takes nothing, or
            // in a window manager no window could be typed into.
            if let Some(ed) = editing.as_mut().filter(|e| draw.fields.iter().any(|f| f.text == e.field && !f.passive)) {
                let k = ed.field;
                match ed.handle_key(&mut texts[k], &name, typed.as_deref(), mods) {
                    KeyOutcome::Changed => {
                        last_key = Instant::now();
                        let _ = to_logic.send(Event::Text(scene.texts[k].0, texts[k].clone()));
                        continue;
                    }
                    KeyOutcome::Moved => {
                        last_key = Instant::now();
                        continue;
                    }
                    KeyOutcome::Submitted => {
                        submitted.push(k);
                        let _ = to_logic.send(Event::Submit(scene.texts[k].0, texts[k].clone()));
                        continue;
                    }
                    KeyOutcome::Unhandled => {}
                }
            }
            // Going down, once: a repeat is the same key still held.
            let plain = plain_key(&name);
            if !keys_held.iter().any(|(c, n)| (code != 0 && *c == code) || *n == plain) {
                keys_held.push((code, plain.clone()));
                let _ = to_logic.send(Event::KeyDown(plain));
            }
            let mut combo = String::new();
            for (on, prefix) in [(mods.ctrl, "Ctrl+"), (mods.alt, "Alt+"), (mods.logo, "Super+")] {
                if on {
                    combo.push_str(prefix);
                }
            }
            // Shift is said with another modifier or a key that writes nothing
            // (`Super+Shift+Left`, `Shift+F5`); alone with a letter it already
            // is in the name: `question`, `A`.
            if mods.shift && (mods.ctrl || mods.alt || mods.logo || typed.is_none()) && !name.starts_with("Shift") {
                combo.push_str("Shift+");
            }
            combo.push_str(&name);
            // `PLEAMAR_DEBUG_KEYS=1`: each key as the rules see it.
            if std::env::var_os("PLEAMAR_DEBUG_KEYS").is_some() {
                eprintln!("keys   · «{combo}» (typed {typed:?}, code {code})");
            }
            // A key the scene has no rule for belongs to the window that has
            // the keyboard. With a rule —`on key Super+Return`—, to the scene.
            // (A rule whose `while` does not hold now does not take it: `on key
            // Escape while overview` leaves Esc to the windows the rest of the time.)
            let the_scenes = scene.rules.iter().any(|r| matches!(&r.when, Trigger::Key(t) if *t == combo) && r.guard.as_ref().is_none_or(|g| g.is_true(Ctx { props: &props, facts: &facts, seen: None })));
            if let Some(send) = &nest {
                let focus = scene.nest.as_ref().and_then(|n| scene.facts.iter().position(|h| h.0 == format!("{}.focus", n.name))).map_or(-1.0, |k| facts[k]);
                if !the_scenes && code != 0 && focus >= 0.0 {
                    send(ToNest::Key { code, down: true });
                    nest_keys.push(code);
                }
            }
            let _ = to_logic.send(Event::Key(combo.clone(), typed));
            keys.push(combo);
        }
        for (name, code) in key_releases {
            let plain = plain_key(&name);
            if let Some(k) = keys_held.iter().position(|(c, n)| if code != 0 && *c != 0 { *c == code } else { *n == plain }) {
                let _ = to_logic.send(Event::KeyUp(keys_held.remove(k).1));
            }
            if let (Some(send), Some(k)) = (&nest, nest_keys.iter().position(|c| *c == code)) {
                nest_keys.remove(k);
                send(ToNest::Key { code, down: false });
            }
        }
        for gained in &focus_changes {
            have_keyboard = *gained;
            // Without the keyboard nothing is held: its coming up will not be heard.
            if !*gained {
                for (_, name) in keys_held.drain(..) {
                    let _ = to_logic.send(Event::KeyUp(name));
                }
            }
            let _ = to_logic.send(Event::Focus(*gained));
            if *gained {
                // On gaining the keyboard: the field it was in when it went; if
                // none, somewhere to type that is on show —the first one drawn—.
                // Not in a window manager: there the windows are what is typed
                // into, and a field only takes the keys when a rule focuses it.
                // If a surface asked for it exclusively, the field is one of its own or
                // none: a field on another surface (the bar's, behind a sheet with no
                // field) sent the keyboard back to "on demand" there (see `elsewhere`),
                // it was lost, the field went with it, the sheet asked again… forever.
                if editing.is_none() && scene.nest.is_none() {
                    let exclusive: Vec<[f32; 4]> = sheets
                        .iter()
                        .filter(|l| (l.open || l.view.popup.is_some()) && l.keyboard_mode == Some(Keyboard::Always))
                        .map(|l| l.view.bounds())
                        .collect();
                    let on_exclusive = |f: &&crate::gpu::PlacedField| {
                        exclusive.is_empty()
                            || scene.zones.iter().filter(|z| z.id == f.zone).filter_map(|z| z.bounds(Ctx { props: &props, facts: &facts, seen: None })).any(|b| exclusive.iter().any(|v| b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]))
                    };
                    // The one it was in, if it is there; if not, it stays parked for later.
                    let here = parked.as_ref().is_some_and(|e| exclusive.is_empty() || draw.fields.iter().filter(|f| f.text == e.field).any(|f| on_exclusive(&f)));
                    let parked_here = if here { parked.take() } else { None };
                    editing = parked_here.or_else(|| {
                        draw.fields
                            .iter()
                            .find(on_exclusive)
                            .map(|f| f.text)
                            .or_else(|| if exclusive.is_empty() { scene.instrs.iter().find_map(|i| if let Instr::Field { text, .. } = i { Some(text.0 as usize) } else { None }) } else { None })
                            .map(|k| Editing { field: k, cursor: texts[k].len(), anchor: texts[k].len() })
                    });
                }
            } else {
                parked = editing.take().or(parked.take());
                repeat = None;
            }
        }
        // A field focused from the logic (`focus("query")`) or from outside
        // (`--say … focus`) asks for the keyboard as a rule's `focus` does (see
        // `Effect::FocusField`): without the loan, the cursor showed and what was
        // typed went to another window. After the focus changes, as the rule
        // sees them: a gain arriving in the same round needs no loan.
        if focus_asked && !have_keyboard && scene.surface().keyboard == Keyboard::OnDemand {
            keyboard_lent = Some(last_key);
        }

        if let Some(d) = block {
            // Naive mode: the logic's work happens here, on the thread
            // that paints. It is what happens in QtQuick with a heavy handler.
            if op.naive {
                logic_blocked.store(true, Ordering::Relaxed);
                std::thread::sleep(d);
                logic_blocked.store(false, Ordering::Relaxed);
            }
        }

        if profiling {
            let n = Instant::now();
            { let d = (n - prof_t).as_secs_f64() * 1000.0; prof[0] += d; sec[0] += d; }
            prof_t = n;
            let c = thread_cpu_ms();
            prof_cpu[0] += c - prof_c;
            prof_c = c;
        }
        // ── 2. advance time ─────────────────────────────────────
        // Reduced motion from this frame on, if it was just asked for: every
        // spring settles where it was going, and from then on at once.
        if reduced_at.map_or(true, |k| scene.facts.get(k).map(|f| f.0) != Some("motion.reduced")) {
            reduced_at = scene.facts.iter().position(|f| f.0 == "motion.reduced");
        }
        let asked = op.reduced_motion || reduced_at.is_some_and(|k| facts.get(k).is_some_and(|v| *v > 0.5));
        if asked && !reduced_motion {
            props.iter_mut().for_each(Animated::settle);
        }
        reduced_motion = asked;
        let now = Instant::now();
        let dt = (now - last).as_secs_f32();
        last = now;
        let t_total = (now - start).as_secs_f32();
        let mut effects: Vec<Effect> = Vec::new();
        let mut ripple_target = None;
        let mut keyboard_changed = false;
        let mut appointments: Vec<Instant> = Vec::new();

        // The copies of a `screens: each` surface for monitors that are not
        // there (see `asleep`, below): their zones catch nothing.
        let absent_spans: Vec<&crate::scene::Span> = scene.spans.iter().filter(|t| scene.surfaces.get(t.surface).is_some_and(|s| s.instance > 0) && !sheets.iter().any(|l| l.view.surface == t.surface)).collect();
        let absent: Vec<std::ops::Range<usize>> = absent_spans.iter().map(|t| t.instrs.clone()).collect();
        // Nor do their `follow`s follow: nothing of theirs is seen. When the
        // monitor comes, they catch up with what changed meanwhile.
        let absent_behaviors: Vec<std::ops::Range<usize>> = absent_spans.iter().map(|t| t.behaviors.clone()).collect();
        awake.update(&scene.behaviors, &absent_behaviors);
        // Zones: who has the mouse over them.
        let mut edges: Vec<(bool, usize)> = Vec::new(); // (enters, zone)
        {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            zone_memo.update(&scene, &facts, &props, &draw.hidden, &absent, check_shortcuts);
            // A zone that only moved —alive before and now— under a pointer
            // that did nothing this round is not entered or left by it: the
            // person did not go anywhere. A row laid out again by a text
            // that changed width slid an island's edge back and forth under
            // a parked pointer, and every crossing was an `enter` or a
            // `leave` with its ease, long after the last real hover (issue
            // 110); and a panel that grows when hovered and moves its own
            // edge away from the pointer closed and opened again by itself.
            // It is worked out as it is the next time the pointer moves, a
            // button goes or the wheel turns —a press always lands on what
            // is under it—; a zone that appears or goes away under it is
            // entered or left at once.
            let pointer_news = pointer != last_pointer || !buttons.is_empty() || wheel != 0.0;
            was_live.resize(scene.zones.len(), false);
            for (k, (z, was_inside)) in scene.zones.iter().zip(inside.iter_mut()).enumerate() {
                // Its box first: the shape is only measured when the mouse is in it.
                let in_box = |x: f32, y: f32| zone_memo.bounds[k].is_some_and(|b| x >= b[0] - 1.0 && x <= b[2] + 1.0 && y >= b[1] - 1.0 && y <= b[3] + 1.0);
                let live = zone_memo.live[k];
                let moved_only = !pointer_news && live && was_live[k];
                was_live[k] = live;
                if moved_only {
                    continue;
                }
                let is_inside = live && pointer.is_some_and(|(x, y)| in_box(x, y) && z.contains(c, x, y));
                if is_inside != *was_inside {
                    *was_inside = is_inside;
                    edges.push((is_inside, k));
                    let _ = to_logic.send(if is_inside { Event::Enter(z.id) } else { Event::Leave(z.id) });
                }
            }
        }
        // The topmost one is the last declared; or, with groups with `z:`, the
        // one drawn last.
        // Worked out again only when some `z:` changed: it walks the whole drawing.
        // And the `z:`s themselves only when something they read changed.
        let arrangement = {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            let z_read_held = z_memo.is_some() && z_read.holds(&facts, &props);
            if !z_read_held || check_shortcuts {
                let zs: Vec<f32> = {
                    let seen = z_read.start(facts.len(), props.len());
                    scene.zblocks.iter().map(|b| b.z.eval(Ctx { seen: Some(seen), ..c })).collect()
                };
                z_read.keep(&facts, &props);
                if check_shortcuts && z_memo.as_ref().is_some_and(|(before, _)| *before != zs) && z_read_held {
                    eprintln!("check  · the z: were taken as unchanged and are not");
                }
                if z_memo.as_ref().is_none_or(|(before, _)| *before != zs) {
                    z_memo = Some((zs, scene.z_arrange(c)));
                }
            }
            z_memo.as_ref().and_then(|(_, a)| a.clone())
        };
        let hovered = match &arrangement {
            Some(a) => inside.iter().enumerate().filter(|(_, d)| **d).max_by_key(|(k, _)| (a.zone_rank[*k], *k)).map(|(k, _)| k),
            None => inside.iter().rposition(|d| *d),
        };
        if std::env::var_os("PLEAMAR_DEBUG_ZONES").is_some() && !edges.is_empty() {
            let names: Vec<String> = inside
                .iter()
                .enumerate()
                .filter(|(_, d)| **d)
                .map(|(k, _)| match &arrangement {
                    Some(a) => format!("{} {:?}", scene.zones[k].id, a.zone_rank[k]),
                    None => scene.zones[k].id.to_owned(),
                })
                .collect();
            eprintln!("zones  · under the pointer: {names:?}");
        }
        // An agent's own hand, by its pixels, on what is kept for a person's
        // (`agent: no`): its press does not get through, nor the release after it.
        if buttons_agent.iter().any(|a| *a) {
            buttons_agent.resize(buttons.len(), false);
            let mut kept = Vec::new();
            for (&(b, down), &agent) in buttons.iter().zip(&buttons_agent) {
                let slot = (b as usize).min(2);
                if agent
                    && down
                    && let Some(z) = hovered.and_then(|k| scene.zones.get(k)).filter(|z| z.reach == crate::scene::Reach::Person)
                {
                    agent_held_back[slot] = true;
                    let said = z.id.split("#screen").next().unwrap_or(z.id);
                    eprintln!("agent  · a press on '{said}' from the agent's hand was not let through: it is for a person's (agent: no)");
                    heard_events.push(("kept", z.id));
                    continue;
                }
                if agent && !down && std::mem::take(&mut agent_held_back[slot]) {
                    continue;
                }
                kept.push((b, down));
            }
            buttons = kept;
        }
        let (mut pressed, mut pressed_with, mut released) = (None, None, None);
        for (button, down) in &buttons {
            match (*button, *down) {
                (0, true) => {
                    // On the letters of a text that can be selected, the press
                    // selects —unless a button, a field or something to carry
                    // out is on top of them: that one keeps its click—.
                    let keeps = hovered.and_then(|k| scene.zones.get(k)).is_some_and(|z| z.cursor_in(Ctx { props: &props, facts: &facts, seen: None }) == Cursor::Hand || z.carries.is_some() || draw.fields.iter().any(|f| f.zone == z.id));
                    let on_text = if keeps { None } else { pointer.and_then(|p| draw.selectable.iter().rev().find(|t| t.contains(p.0, p.1)).map(|t| (t, p))) };
                    if let Some((t, p)) = on_text {
                        let (lx, ly) = t.local(p.0, p.1);
                        let b = t.layout.byte_at(lx, ly).min(t.text.len());
                        let again = now.duration_since(clicks.0) < Duration::from_millis(450) && (p.0 - clicks.1 .0).hypot(p.1 - clicks.1 .1) < 5.0;
                        clicks = (now, p, if again { clicks.2 % 3 + 1 } else { 1 });
                        let (anchor, cursor) = match clicks.2 {
                            1 => (b, b),
                            2 => word_around(&t.text, b),
                            _ => (0, t.text.len()),
                        };
                        selection = Some(TextSelection { at: t.at, anchor, cursor, dragging: clicks.2 == 1 });
                        continue;
                    }
                    selection = None;
                    pressed = hovered;
                    if let Some(k) = hovered.filter(|_| !watchers.is_empty()) {
                        heard_events.push(("press", scene.zones[k].id));
                    }
                    if let (Some(k), Some(p)) = (hovered, pointer) {
                        // Clicking a field focuses it, with the cursor where the click landed.
                        let id = scene.zones[k].id;
                        if let Some(placed) = draw.fields.iter().find(|c| c.zone == id) {
                            let local = scene.zones[k].to_local(Ctx { props: &props, facts: &facts, seen: None }, p.0, p.1);
                            let b = placed.layout.as_ref().map_or(0, |m| m.byte_at_x(local.0 - placed.x0 + placed.scroll));
                            let b = if placed.secret { crate::gpu::unmasked_byte(&texts[placed.text], b) } else { b };
                            let b = b.min(texts[placed.text].len());
                            editing = Some(Editing { field: placed.text, cursor: b, anchor: b });
                            last_key = now;
                        }
                        drag = Some((k, p, now));
                        let _ = to_logic.send(Event::Press(scene.zones[k].id));
                    }
                }
                (0, false) => {
                    if let Some(s) = &mut selection {
                        s.dragging = false;
                    }
                    if let Some((k, _, _)) = drag.take() {
                        released = Some(k);
                        if let Some(z) = scene.zones.get(k) {
                            let _ = to_logic.send(Event::Release(z.id));
                        }
                    }
                }
                // (The side buttons —back, forward— are the windows' alone.)
                (b, true) if b <= 2 => pressed_with = hovered.map(|k| (k, b)),
                _ => {}
            }
        }
        // A window that draws its own frame (a GTK 4 header bar, a browser's
        // tabs) asks to be carried from it, or stretched by one of its edges:
        // the zone that is its handle (`handle:`) is pressed for it, on the
        // copy of the scene the pointer is over, as if pressed there —its
        // rules, its drag and its release are the scene's own—, and let go
        // when the button goes up.
        if let Some((slot, how, edges)) = handle_asked.take() {
            if how == 0 {
                if let Some(k) = handle_held.take()
                    && drag.is_some_and(|d| d.0 == k)
                {
                    drag = None;
                    released = Some(k);
                    let _ = to_logic.send(Event::Release(scene.zones[k].id));
                }
            } else if let Some(p) = pointer {
                let copy = sheets
                    .iter()
                    .filter(|l| l.view.popup.is_none())
                    .find(|l| {
                        let v = l.view.bounds();
                        p.0 >= v[0] && p.0 < v[2] && p.1 >= v[1] && p.1 < v[3]
                    })
                    .and_then(|l| scene.surfaces.get(l.view.surface))
                    .map(|s| format!("#screen{}", s.instance));
                let want = if how == 1 { 0 } else { edges };
                let c = Ctx { props: &props, facts: &facts, seen: None };
                // Its handle by that edge; else the one that shares most of it
                // (a top corner asked for, the side one).
                let found = scene
                    .zones
                    .iter()
                    .enumerate()
                    .filter_map(|(k, z)| {
                        let h = z.handle.as_ref()?;
                        let mine = (h.window.eval(c) - slot as f32).abs() < 0.5;
                        let here = !z.id.contains("#screen") || copy.as_ref().is_some_and(|m| z.id.ends_with(m.as_str()));
                        let fit = if h.edges == want { 3 } else if want != 0 && h.edges & want != 0 { (h.edges & want).count_ones() } else { 0 };
                        (mine && here && fit > 0 && z.active.is_true(c)).then_some((fit, k))
                    })
                    .max_by_key(|(fit, _)| *fit)
                    .map(|(_, k)| k);
                if let Some(k) = found {
                    pressed = Some(k);
                    drag = Some((k, p, now));
                    handle_held = Some(k);
                    let _ = to_logic.send(Event::Press(scene.zones[k].id));
                }
            }
        }
        // A prototype's emergency exit —the right button closes— must not
        // steal the button from a scene that uses it. What knows whether it has landed
        // on top of something is this, which is what looks at the zones: if there was none under the
        // pointer, it closes; if there was, the click belongs to the scene.
        // The platform already closes on its own when NO surface uses the
        // right button, and then this does not even run.
        // A zone that carries something (`carries:`), dragged far enough: out
        // to another program. From then on the compositor has the pointer, so
        // the scene's own drag is over.
        let mut carried_now: Option<usize> = None;
        if let (Some((k, from, _)), Some(p)) = (drag, pointer) {
            if let Some(content) = scene.zones.get(k).and_then(|z| z.carries.as_ref()) {
                if carry_tried != Some(k) && (p.0 - from.0).hypot(p.1 - from.1) > 8.0 {
                    carry_tried = Some(k);
                    let text = crate::gpu::content_text(content, Ctx { props: &props, facts: &facts, seen: None }, &texts).into_owned();
                    let icon = scene.zones.get(k).and_then(|z| z.carry_icon.as_ref()).map(|c| crate::gpu::content_text(c, Ctx { props: &props, facts: &facts, seen: None }, &texts).into_owned());
                    if crate::platform::start_drag(&text, icon.as_deref().filter(|p| !p.is_empty())) {
                        let _ = to_logic.send(Event::Release(scene.zones[k].id));
                        drag = None;
                        carried_now = Some(k);
                    }
                }
            }
        }
        if drag.is_none() {
            carry_tried = None;
        }
        // Selecting: the end follows the mouse, even off the text.
        if let (Some(s), Some(p)) = (selection.as_mut().filter(|s| s.dragging), pointer) {
            if let Some(t) = draw.selectable.iter().find(|t| t.at == s.at) {
                let (lx, ly) = t.local(p.0, p.1);
                s.cursor = t.layout.byte_at(lx, ly).min(t.text.len());
            }
        }
        let dragged = match (drag, pointer) {
            (Some((k, _, _)), Some(p)) if last_pointer != Some(p) => Some(k),
            _ => None,
        };
        last_pointer = pointer;
        // The wheel is not only for the topmost zone: it works for any zone it has
        // underneath, because a whole pill wants the wheel even if there is a button inside.
        if wheel != 0.0 {
            if let Some(k) = hovered {
                let _ = to_logic.send(Event::Wheel(scene.zones[k].id, wheel));
            }
        }

        // Each zone's own springs: over it, and pressed on it.
        for (z, hover, pressed) in &scene.zone_springs {
            let k = z.0 as usize;
            props[hover.0 as usize].target = if inside.get(k).copied().unwrap_or(false) { 1.0 } else { 0.0 };
            props[pressed.0 as usize].target = if drag.is_some_and(|d| d.0 == k) { 1.0 } else { 0.0 };
        }

        // The monitor the pointer is on —which copy of the scene it is over—:
        // new windows open there.
        if let (Some(send), Some((x, y))) = (&nest, pointer) {
            let screen = sheets
                .iter()
                .filter(|l| l.view.popup.is_none())
                .find(|l| {
                    let v = l.view.bounds();
                    x >= v[0] && x < v[2] && y >= v[1] && y < v[3]
                })
                .and_then(|l| scene.surfaces.get(l.view.surface))
                .map(|s| match s.screens {
                    Screens::Number(k) => k,
                    _ => 0,
                });
            if let Some(screen) = screen.filter(|s| nest_on_screen != Some(*s)) {
                nest_on_screen = Some(screen);
                send(ToNest::OnScreen(screen));
            }
        }
        // The windows of the scene's compositor: the one under the pointer —or
        // the one a button was pressed on, while it is held— gets the pointer,
        // in its own pixels, and the buttons and the wheel.
        if let Some(send) = &nest {
            // While a program drags something, whichever is under the pointer:
            // that is where it would be dropped.
            let hovered_window = hovered.and_then(|k| nest_zones.get(k).copied().flatten());
            let under = if nest_dragging { hovered_window } else { nest_grab.or(hovered_window) };
            // (And how many of its own pixels a unit of the scene is there: a
            // window drawn small moves further under the same fingers.)
            let mut local_scale = (1.0f32, 1.0f32);
            let local = under.zip(pointer).and_then(|(slot, (x, y))| {
                let (_, d, affine) = draw.windows_drawn.iter().rev().find(|w| w.0 == slot)?;
                let g = nest_windows.get(slot)?.geometry;
                let (lx, ly) = affine.inverse().apply(x, y);
                if d[2] > 0.0 && d[3] > 0.0 && g[2] > 0 && g[3] > 0 {
                    local_scale = (g[2] as f32 / d[2], g[3] as f32 / d[3]);
                }
                Some((slot, ((lx - d[0]) / d[2] * g[2] as f32) as f64, ((ly - d[1]) / d[3] * g[3] as f32) as f64))
            });
            match local {
                Some(p) if nest_pointer != Some(p) => {
                    send(ToNest::Pointer { slot: p.0, x: p.1, y: p.2 });
                    nest_pointer = Some(p);
                }
                None if nest_pointer.is_some() => {
                    send(ToNest::PointerOut);
                    nest_pointer = None;
                }
                _ => {}
            }
            for (button, down) in &buttons {
                // Left, right, middle, and the side ones: back and forward
                // (BTN_SIDE, BTN_EXTRA, BTN_FORWARD, BTN_BACK), for a browser.
                let code = match button {
                    0 => 0x110,
                    1 => 0x111,
                    2 => 0x112,
                    b => 0x110 + *b as u32,
                };
                // Pressed while still held for a window: its release was lost
                // on the way. The window is let go first, or it took every
                // move after for a drag (text selected, a window carried).
                if *down {
                    if let Some(k) = nest_buttons.iter().position(|c| *c == code) {
                        nest_buttons.remove(k);
                        send(ToNest::Button { code, down: false });
                    }
                }
                if *down {
                    if let Some((slot, _, _)) = local {
                        send(ToNest::Button { code, down: true });
                        nest_buttons.push(code);
                        nest_grab = Some(slot);
                    }
                } else if let Some(k) = nest_buttons.iter().position(|c| *c == code) {
                    nest_buttons.remove(k);
                    send(ToNest::Button { code, down: false });
                    if nest_buttons.is_empty() {
                        nest_grab = None;
                    }
                }
            }
            // The wheel in notches; what fingers moved, as far as they went.
            if local.is_some() {
                let turned = wheel - fingers.2;
                if turned.abs() > 1e-6 {
                    send(ToNest::Wheel(turned as f64));
                }
                if fingers.0 != 0.0 || fingers.1 != 0.0 {
                    send(ToNest::Scroll((fingers.0 * local_scale.0) as f64, (fingers.1 * local_scale.1) as f64));
                }
            }
            if fingers_up {
                send(ToNest::ScrollEnd);
            }
        }

        // What a rule can read from the mouse: where it is, where inside the zone
        // it is dealing with, how far it has dragged and how much the wheel has turned.
        {
            let focus = drag.map(|a| a.0).or(hovered);
            let (px, py) = pointer.unwrap_or((0.0, 0.0));
            // The mouse wherever it is, in the plane of the scene. It is placed
            // against a surface that is open —the one on the mouse's monitor if
            // there is one, else the first—: from there on it is just a sum, even
            // if it is on another monitor. Over the scene, the pointer itself.
            // Each open sheet placed on the desktop: which surface it is, whether
            // the mouse is on its monitor, and the mouse from its own corner.
            let placed: Vec<(usize, bool, (f32, f32))> = match (scene.wants_cursor, &cursor) {
                (true, Some(((gx, gy), monitors))) => {
                    let on = |m: &[i32; 4]| *gx >= m[0] as f32 && *gy >= m[1] as f32 && *gx < (m[0] + m[2]) as f32 && *gy < (m[1] + m[3]) as f32;
                    let under = monitors.iter().find(|m| on(&m.1)).map(|m| m.0.as_str());
                    sheets.iter().filter(|l| l.open && l.view.popup.is_none()).filter_map(|l| {
                        let (name, at) = l.desktop_place()?;
                        let m = monitors.iter().find(|m| m.0 == name)?.1;
                        let corner = ((m[0] + at.0) as f32, (m[1] + at.1) as f32);
                        Some((l.view.surface, under == Some(name.as_str()), (gx - corner.0, gy - corner.1)))
                    }).collect()
                }
                _ => Vec::new(),
            };
            // Of a set of sheets, the one on the mouse's monitor, or else the first.
            let pick = |of: &dyn Fn(usize) -> bool| placed.iter().filter(|p| of(p.0)).fold(None, |best: Option<&(usize, bool, (f32, f32))>, p| match best { Some(b) if b.1 || !p.1 => Some(b), _ => Some(p) });
            // `cursor.x` belongs to the scene's surface: its plane is the one the
            // loose drawing uses. The named ones have their own, below.
            let seen_cursor = match pointer {
                Some(p) => p,
                None => match pick(&|k| scene.surfaces.get(k).is_some_and(|s| s.name.is_empty())).or_else(|| pick(&|_| true)) {
                    // From its corner to the plane: plus where the sheet looks from.
                    Some(&(k, _, (x, y))) => {
                        let origin = sheets.iter().find(|l| l.view.surface == k && l.open).map_or((0.0, 0.0), |l| l.view.origin);
                        (origin.0 + x, origin.1 + y)
                    }
                    None => (f32::NAN, f32::NAN),
                },
            };
            // `panel.cursor.x`: from that surface's corner, which is what it draws from.
            for (k, s) in scene.surfaces.iter().enumerate() {
                let Some((px_, py_)) = s.cursor_props else { continue };
                if scene.surfaces.iter().position(|o| o.cursor_props == s.cursor_props) != Some(k) {
                    continue;
                }
                let same = |j: usize| scene.surfaces.get(j).is_some_and(|o| o.cursor_props == s.cursor_props);
                if let Some(p) = pick(&same) {
                    for (id, v) in [(px_, p.2 .0), (py_, p.2 .1)] {
                        props[id.0 as usize] = Animated { x: v, v: 0.0, target: v, spring: props[id.0 as usize].spring };
                    }
                }
            }
            // Unknown, it stays where it was.
            let seen_cursor = if seen_cursor.0.is_nan() {
                let k = |n: &str| scene.facts.iter().position(|f| f.0 == n).map_or(0.0, |k| facts[k]);
                (k("cursor.x"), k("cursor.y"))
            } else {
                seen_cursor
            };
            let local = focus.and_then(|k| scene.zones.get(k)).map_or((px, py), |z| z.to_local(Ctx { props: &props, facts: &facts, seen: None }, px, py));
            let (dx, dy) = drag.map_or((0.0, 0.0), |(_, o, _)| (px - o.0, py - o.1));
            for (k, (name, _)) in scene.facts.iter().enumerate() {
                match *name {
                    "pointer.x" => facts[k] = px,
                    "pointer.y" => facts[k] = py,
                    "cursor.x" => facts[k] = seen_cursor.0,
                    "cursor.y" => facts[k] = seen_cursor.1,
                    "local.x" => facts[k] = local.0,
                    "local.y" => facts[k] = local.1,
                    "drag.dx" => facts[k] = dx,
                    "drag.dy" => facts[k] = dy,
                    "wheel" => facts[k] = wheel,
                    _ => {}
                }
            }
        }

        for (kind, data) in &drops {
            if let Some(k) = hovered {
                let _ = to_logic.send(Event::Received(scene.zones[k].id, kind.clone(), data.clone()));
            }
        }

        // The keyboard, only while the scene wants it: a closed launcher cannot
        // keep it. And if it asked for the focus for a field without having it,
        // exclusively until it arrives (see `Effect::FocusField`).
        // Each surface by its own `keyboard: … while`, or the scene's without one.
        let wants = |s: &crate::scene::Surface| match s.keyboard_when.as_ref().or(scene.keyboard_while.as_ref()) {
            Some(when) => when.is_true(Ctx { props: &props, facts: &facts, seen: None }),
            None => true,
        };
        let any_condition = scene.keyboard_while.is_some() || scene.surfaces.iter().any(|s| s.keyboard_when.is_some());
        // The scene wants it while an open surface does (with no condition anywhere, always).
        let keyboard_wanted = !any_condition
            || sheets.iter().filter(|l| l.open || l.view.popup.is_some()).any(|l| scene.surfaces.get(l.view.surface).is_some_and(|s| s.keyboard != Keyboard::Never && wants(s)));
        // The loan lasts while the scene wants the keyboard: giving it back on
        // receiving the focus does not work, because on going back to "on demand" Hyprland
        // gives it again to the previous window. It is what any launcher does:
        // open, the keyboard is its own; Esc closes it and releases it.
        //
        // And it outlives a short gap: the finder closes, and 180 ms later the
        // card it opened comes out. Released in between, the keyboard went back
        // to the window below and the card was left without it —Esc did not
        // reach it until the mouse went over—. So on not being wanted the loan
        // is kept 300 ms more; if something wants it again by then, it goes on.
        if !keyboard_wanted {
            if keyboard_lent.is_some() {
                let since = *loan_grace.get_or_insert(now);
                if now.duration_since(since) >= Duration::from_millis(300) {
                    keyboard_lent = None;
                    loan_grace = None;
                } else {
                    appointments.push(since + Duration::from_millis(300));
                }
            }
        } else {
            loan_grace = None;
        }
        // What each surface asks for: lent, all of it; not wanted, none; else its own.
        let mode_of = |s: &crate::scene::Surface| {
            if keyboard_lent.is_some() {
                Keyboard::Always
            } else if !wants(s) {
                Keyboard::Never
            } else {
                s.keyboard
            }
        };
        let mode = mode_of(scene.surface());
        // With no condition and no loan, the keyboard is the one asked for when creating it.
        // Each surface gets it while it is OPEN, and no other: a closed one
        // never asks for the keyboard. Handed to all of them, a closed
        // full-screen catcher or the copy on the other monitor took it —an
        // exclusive keyboard asked for by the finder went to one of them, and
        // nothing could be typed in the finder—.
        // The keyboard asked for exclusively goes to the surface where the text
        // cursor is, and no other. Asked by every open one, Hyprland gave it to
        // the last —Marea's full-screen catcher— and with it the mouse: over
        // the calendar, which puts the cursor in its field as it opens, the
        // pointer stayed on the catcher and the first click closed everything.
        // With one copy per monitor the field is there once per copy, all of
        // them the same text: the cursor is in whichever of them falls on an
        // open surface. Taking the first one found —the copy on the other
        // monitor, closed— left the open one without the keyboard.
        let typing_at: Vec<[f32; 4]> = editing
            .as_ref()
            .map(|e| {
                draw.fields
                    .iter()
                    .filter(|f| f.text == e.field)
                    .flat_map(|f| scene.zones.iter().filter(move |z| z.id == f.zone))
                    .filter_map(|z| z.bounds(Ctx { props: &props, facts: &facts, seen: None }))
                    .collect()
            })
            .unwrap_or_default();
        if keyboard_set.is_some() || any_condition || keyboard_lent.is_some() || scene.surfaces.iter().any(|s| s.keyboard != scene.surface().keyboard) {
            keyboard_set = Some(mode);
            for l in &mut sheets {
                let v = l.view.bounds();
                let elsewhere = !typing_at.is_empty() && !typing_at.iter().any(|b| b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]);
                let own = scene.surfaces.get(l.view.surface).map_or(mode, &mode_of);
                let its = if !(l.open || l.view.popup.is_some()) {
                    Keyboard::Never
                } else if own == Keyboard::Always && elsewhere {
                    Keyboard::OnDemand
                } else {
                    own
                };
                if l.keyboard_mode != Some(its) {
                    l.keyboard(its);
                    l.keyboard_mode = Some(its);
                    // It is applied with the frame that gets presented: let there be one.
                    l.painted = None;
                    keyboard_changed = true;
                }
            }
        }

        // The cursor, the one of the zone it is over.
        let wanted = drag.map(|a| a.0).or(hovered).and_then(|k| scene.zones.get(k)).map_or(Cursor::Normal, |z| z.cursor_in(Ctx { props: &props, facts: &facts, seen: None }));
        // Over letters that can be selected, or selecting them, the text one.
        let wanted = if selection.as_ref().is_some_and(|s| s.dragging)
            || (wanted == Cursor::Normal && drag.is_none() && pointer.is_some_and(|p| draw.selectable.iter().any(|t| t.contains(p.0, p.1))))
        {
            Cursor::Text
        } else {
            wanted
        };
        // Over a window, the one the window asks for.
        let wanted = if nest_pointer.is_some() { nest_cursor } else { wanted };
        if wanted != cursor_set {
            cursor_set = wanted;
            for l in &sheets {
                l.cursor(wanted);
            }
        }

        // With `screens: each`, the things of a copy whose surface is closed are not
        // looked at: neither its rules nor its drawing. Its behaviors YES: a `follow`
        // of its own is what opens it —`open: nothing.here > 0.01` chases a fact—,
        // and asleep it would never come back. They are cheap; the expensive part is reading the drawing.
        // Without this two copies were twice the scene per frame even if one was not
        // visible.
        // And the copies of a `screens: each` surface for monitors that are not
        // there: `each max 4` with one monitor was four times the scene's rules
        // every frame, for three copies nobody sees. The first copy always runs.
        let asleep: Vec<&crate::scene::Span> = {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            let shown = |k: usize| sheets.iter().any(|l| l.view.surface == k);
            scene
                .spans
                .iter()
                .filter(|t| {
                    let Some(s) = scene.surfaces.get(t.surface) else { return false };
                    s.open.as_ref().is_some_and(|e| !e.is_true(c)) || (s.instance > 0 && !shown(t.surface))
                })
                .collect()
        };
        let rule_asleep = |k: usize| asleep.iter().any(|t| t.rules.contains(&k));

        // What is bound is brought up to date before the rules too, when a
        // fact has changed since it was computed. A big `let` is a property
        // computed with the behaviors, after the rules: a rule that followed a
        // fact read the `let`s made from it as they were before it changed.
        // pleamar-wm took the keyboard going to a window on the other monitor
        // for one going to a window on the same one, and a maximized window
        // went back to its tile.
        if facts != facts_at_binds {
            follows.begin(&scene.behaviors);
            readers.look(0, &scene.behaviors, &facts, &props, &absent_behaviors);
            for &k in &awake.binds {
                let k = k as usize;
                let behavior = &scene.behaviors[k];
                if !readers.dirty[k] {
                    continue;
                }
                if let Behavior::Bind { prop, to } = behavior {
                    if !readers.dirty[k] || absent_behaviors.iter().any(|r| r.contains(&k)) {
                        continue;
                    }
                    let v = follows.value(k, to, &props, &facts);
                    if props[prop.0 as usize].x.to_bits() != v.to_bits() {
                        readers.touch(*prop);
                    }
                    props[prop.0 as usize].set(v);
                } else if let Behavior::Sort { keys, ord, pos } = behavior {
                    if readers.dirty[k] && !absent_behaviors.iter().any(|r| r.contains(&k)) {
                        run_sort(keys, ord, pos, &mut props, &facts, |p| readers.touch(p));
                    }
                }
            }
            facts_at_binds.clone_from(&facts);
        }

        // Rules: all of this happens here, whatever state the logic is in.
        // Only the ones that can fire this round (see `RuleIndex`).
        if rule_index.of != (scene.rules.as_ptr() as usize, scene.rules.len()) {
            rule_index.build(&scene.rules);
        }
        rule_index.look(&facts, &props);
        let asleep_rules: Vec<std::ops::Range<usize>> = asleep.iter().map(|t| t.rules.clone()).collect();
        rule_index.wake(&asleep_rules, &scene.rules);
        let some_event = !edges.is_empty()
            || pressed.is_some()
            || pressed_with.is_some()
            || released.is_some()
            || !keys.is_empty()
            || !submitted.is_empty()
            || !focus_changes.is_empty()
            || wheel != 0.0
            || dragged.is_some()
            || !drops.is_empty()
            || carried_now.is_some();
        // Nothing changed, nothing woke, nothing happened: the `on change`s that read only facts and properties cannot fire.
        let quiet = !some_event && !rule_index.any_changed && !rule_index.woke;
        if quiet && check_shortcuts {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            for &k in &rule_index.awake_pure_changes {
                if let (Trigger::Change(x), Some(last)) = (&scene.rules[k as usize].when, rules[k as usize].last_value) {
                    let v = x.eval(c);
                    if v.to_bits() != last.to_bits() && (v - last).abs() > 0.001 {
                        eprintln!("check  · a quiet round skipped an `on change` that would fire: rule {k}, {last} → {v}");
                    }
                }
            }
        }
        let which = if some_event { 0 } else if quiet { 1 } else { 2 };
        let looked_at = std::mem::take(match which {
            0 => &mut rule_index.awake_with_events,
            1 => &mut rule_index.awake_quiet,
            _ => &mut rule_index.awake_always,
        });
        {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            let mut acted: Vec<usize> = Vec::new();
            for &k in &looked_at {
                let k = k as usize;
                let (Some(r), Some(e)) = (scene.rules.get(k), rules.get_mut(k)) else { continue };
                // (Asleep ones are not in the list: `RuleIndex::wake`.)
                debug_assert!(!rule_asleep(k));
                // Nothing it reads changed since it was worked out the round before: it is worth the same.
                let same = rule_index.unchanged(k) && e.last_value.is_some();
                rule_index.looked[k] = rule_index.round;
                if same && check_shortcuts {
                    let x = match &r.when {
                        Trigger::Change(x) => x,
                        Trigger::Still { value, .. } => value,
                        _ => unreachable!(),
                    };
                    let v = x.eval(c);
                    if e.last_value.is_some_and(|l| l.to_bits() != v.to_bits()) {
                        eprintln!("check  · an `on change` taken as unchanged is not: rule {k}, {:?} → {v}", e.last_value);
                    }
                }
                let fires = match &r.when {
                    Trigger::Enter(z) => edges.contains(&(true, z.0 as usize)),
                    Trigger::Leave(z) => edges.contains(&(false, z.0 as usize)),
                    Trigger::Press(z) => pressed == Some(z.0 as usize),
                    Trigger::PressWith(z, b) => pressed_with == Some((z.0 as usize, *b)),
                    Trigger::Release(z) => released == Some(z.0 as usize),
                    Trigger::Wheel(z) => wheel != 0.0 && inside[z.0 as usize],
                    // Dragging is not only for the topmost zone, like the wheel: it works
                    // for any zone that was underneath when it was pressed. That way a list
                    // can be dragged by grabbing one of its rows.
                    Trigger::Drag(z) => {
                        dragged.is_some()
                            && match (scene.zones.get(z.0 as usize), drag) {
                                // Only if it is on: a zone switched off —another monitor's
                                // copy of a window, one hidden— that happens to hold the
                                // point pressed did not ask to be dragged.
                                // The zone pressed keeps it until the button goes up, even
                                // if it has moved away from that point (a title bar that
                                // carries its window with it); others, while they hold it.
                                (Some(zone), Some((k, o, _))) => zone.active.is_true(c) && (k == z.0 as usize || zone.contains(c, o.0, o.1)),
                                _ => false,
                            }
                    }
                    Trigger::Hold { zone, duration } => {
                        let held = drag.is_some_and(|(k, _, _)| k == zone.0 as usize);
                        e.sustained(held, *duration, now, &mut appointments)
                    }
                    // `on change floor(list.scroll / 34) { … }`: when that changes.
                    Trigger::Change(x) => {
                        let now = if same { e.last_value.unwrap_or(0.0) } else { x.eval(c) };
                        let before = e.last_value.replace(now);
                        before.is_some_and(|v| (v - now).abs() > 0.001)
                    }
                    // `on still audio.volume for 1.1s { … }`: when that has been still for that
                    // long. Each change resets the clock to zero, so six
                    // taps in a row on the volume key are a single wait.
                    Trigger::Still { value, duration } => {
                        let value = if same { e.last_value.unwrap_or(0.0) } else { value.eval(c) };
                        let before = e.last_value.replace(value);
                        if before.is_some_and(|v| (v - value).abs() > 0.001) {
                            e.armed = true;
                            e.next = Some(now + *duration);
                        }
                        match e.next.filter(|_| e.armed) {
                            Some(p) if now >= p => {
                                e.armed = false;
                                true
                            }
                            Some(p) => {
                                appointments.push(p);
                                false
                            }
                            None => false,
                        }
                    }
                    Trigger::Key(t) => keys.iter().any(|x| x == t),
                    Trigger::Submit(t) => submitted.contains(&(t.0 as usize)),
                    Trigger::FocusGained => focus_changes.contains(&true),
                    Trigger::FocusLost => focus_changes.contains(&false),
                    Trigger::Receive(z) => !drops.is_empty() && inside[z.0 as usize],
                    Trigger::Carry(z) => carried_now == Some(z.0 as usize),
                    Trigger::Above { zone, also, duration } => {
                        let over = inside[zone.0 as usize] || also.iter().any(|z| inside[z.0 as usize]);
                        e.sustained(over, *duration, now, &mut appointments)
                    }
                    Trigger::Away { zone, also, duration } => {
                        let is_inside = inside[zone.0 as usize] || also.iter().any(|z| inside[z.0 as usize]);
                        e.armed |= is_inside;
                        let done = e.sustained(e.armed && !is_inside, *duration, now, &mut appointments);
                        if done {
                            e.armed = false;
                        }
                        done
                    }
                    Trigger::Idle { duration, during } => {
                        let left = (last_activity + *duration).saturating_duration_since(now);
                        let ready = during.is_true(c) && left.is_zero();
                        if during.is_true(c) && !left.is_zero() {
                            appointments.push(last_activity + *duration);
                        }
                        let done = ready && e.last_time != Some(last_activity);
                        if done {
                            e.last_time = Some(last_activity);
                        }
                        done
                    }
                    Trigger::Every { between, during } => {
                        // While the condition is not met, the wait does NOT count: the
                        // clock is set in full every frame, so it starts counting
                        // when it starts being true. Before, it ran on its own and
                        // `every 1s while counting` took its first step at 300 ms —whatever
                        // was left of the previous clock—, which in a countdown is
                        // a second that does not exist.
                        if !during.is_true(c) {
                            e.next = Some(now + Duration::from_secs_f32(rng.between(between.0, between.1)));
                            false
                        } else {
                            let due = e.next.is_some_and(|p| now >= p);
                            if due {
                                e.next = Some(now + Duration::from_secs_f32(rng.between(between.0, between.1)));
                            }
                            if let Some(p) = e.next {
                                appointments.push(p);
                            }
                            due
                        }
                    }
                    Trigger::On(_) => false, // handled with the signals, below
                };
                // Its twins in the other copies watched too —each one keeps its own
                // `on change`, its own `on still`—, but only the first awake acts.
                let twin = scene.twin_of.get(k).copied().unwrap_or(k);
                if fires && !acted.contains(&twin) && r.guard.as_ref().is_none_or(|guard| guard.is_true(c)) {
                    acted.push(twin);
                    effects.extend(r.effects.iter().cloned());
                }
            }
        }
        match which {
            0 => rule_index.awake_with_events = looked_at,
            1 => rule_index.awake_quiet = looked_at,
            _ => rule_index.awake_always = looked_at,
        }

        // Effects and signals, until there are none left (with a cap: a rule
        // that fires itself does not hang the render).
        //
        // And if something has happened, it does not go to sleep yet: the rules are all looked at
        // before applying anything, so an `on change` cannot see in the same
        // frame what another rule has just changed. It sees it on the next one, and without
        // this "the next one" could be a second later —the render asleep
        // until the next appointment—, which is how a countdown skipped its
        // end.
        let something_happened = !effects.is_empty() || !signals.is_empty() || !gestures_asked.is_empty();
        let mut cap = 8;
        while (!effects.is_empty() || !signals.is_empty() || !gestures_asked.is_empty()) && cap > 0 {
            cap -= 1;
            for ef in std::mem::take(&mut effects) {
                match ef {
                    Effect::Animate(t) => pending.push((now + t.delay, t)),
                    // A fact that a rule changes is told to the logic, which otherwise
                    // would go on believing what it said itself the last time.
                    Effect::Fact(h, v) => {
                        let v = v.eval(Ctx { props: &props, facts: &facts, seen: None });
                        facts[h.0 as usize] = v;
                        let _ = to_logic.send(Event::Fact(scene.facts[h.0 as usize].0, v));
                    }
                    Effect::FactAt(hs, i, v) => {
                        let c = Ctx { props: &props, facts: &facts, seen: None };
                        let (i, v) = (i.eval(c).round(), v.eval(c));
                        if let Some(h) = (i >= 0.0).then(|| hs.get(i as usize)).flatten() {
                            facts[h.0 as usize] = v;
                            let _ = to_logic.send(Event::Fact(scene.facts[h.0 as usize].0, v));
                        }
                    }
                    Effect::Toggle(h) => {
                        let v = if facts[h.0 as usize] > 0.5 { 0.0 } else { 1.0 };
                        facts[h.0 as usize] = v;
                        let _ = to_logic.send(Event::Fact(scene.facts[h.0 as usize].0, v));
                    }
                    Effect::Signal(s, payload) => {
                        let v = payload.as_ref().map(|e| e.eval(Ctx { props: &props, facts: &facts, seen: None }));
                        signals.push((s.0 as usize, false, v));
                    }
                    Effect::Impulse(p, v) => {
                        let v = v.eval(Ctx { props: &props, facts: &facts, seen: None });
                        props[p.0 as usize].v += v;
                    }
                    Effect::Ripple(z) => ripple_target = Some(z),
                    Effect::Gesture(g) => gestures_asked.push(g.0 as usize),
                    Effect::Window(action, which) => {
                        let slot = which.eval(Ctx { props: &props, facts: &facts, seen: None }).round();
                        if let (Some(send), true) = (&nest, slot >= 0.0) {
                            let slot = slot as usize;
                            // With `dock programs` a window given the keyboard is
                            // seen: the workspace it is on is shown on its monitor.
                            if let (WindowAction::Focus, Some(n)) = (action, scene.nest.clone().filter(|n| n.programs && slot < n.max)) {
                                desks.grow(n.max);
                                let screen = nest_screens.get(slot).copied().unwrap_or(0).min(3);
                                if desks.alive(slot) && desks.pool[slot] != 0 && desks.pool[slot] != desks.shown[screen] {
                                    desks.shown[screen] = desks.pool[slot];
                                    nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                                }
                            }
                            send(match action {
                                WindowAction::Focus => ToNest::Focus(slot),
                                WindowAction::Close => ToNest::Close(slot),
                                WindowAction::Promote => ToNest::Promote(slot),
                                WindowAction::Fullscreen => ToNest::Fullscreen(slot),
                                WindowAction::Minimize => ToNest::Minimize(slot, true),
                                WindowAction::Restore => ToNest::Minimize(slot, false),
                                WindowAction::Float => ToNest::Float(slot, true),
                                WindowAction::Tile => ToNest::Float(slot, false),
                            });
                        }
                    }
                    Effect::WindowTo(which, to) => {
                        let c = Ctx { props: &props, facts: &facts, seen: None };
                        let (slot, screen) = (which.eval(c).round(), to.eval(c).round());
                        if let (Some(send), true) = (&nest, slot >= 0.0 && screen >= 0.0) {
                            send(ToNest::Send(slot as usize, screen as usize));
                        }
                    }
                    // A workspace shown: its windows come, the others go, and the
                    // keyboard goes to the one that last had it there.
                    Effect::Workspace(ws, on) => {
                        let c = Ctx { props: &props, facts: &facts, seen: None };
                        let ws = ws.eval(c).round().max(1.0) as usize;
                        let screen = on.map_or(nest_on_screen.unwrap_or(0) as f32, |e| e.eval(c)).round().clamp(0.0, 3.0) as usize;
                        if let Some(n) = scene.nest.clone() {
                            desks.grow(n.max);
                            let id = desks.at(screen, ws, &nest_screens);
                            if desks.shown[screen] != id {
                                desks.shown[screen] = id;
                                nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                                if let Some(send) = &nest {
                                    let here = desks.last.get(&id).copied().filter(|k| nest_order.contains(k) && desks.pool_of(*k) == id && nest_screens.get(*k) == Some(&screen));
                                    let first = || nest_order.iter().copied().find(|k| desks.pool_of(*k) == id && nest_screens.get(*k) == Some(&screen));
                                    match here.or_else(first) {
                                        Some(k) => send(ToNest::Focus(k)),
                                        // Nobody there: the keyboard leaves the one that had it, now out of sight.
                                        None => {
                                            let focused = facts.iter().zip(&scene.facts).find(|(_, h)| h.0 == format!("{}.focus", n.name)).map_or(-1.0, |(v, _)| *v);
                                            if focused >= 0.0 && nest_screens.get(focused as usize) == Some(&screen) {
                                                send(ToNest::Blur);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Effect::WindowToWorkspace(which, to, on) => {
                        let c = Ctx { props: &props, facts: &facts, seen: None };
                        let (slot, ws) = (which.eval(c).round(), to.eval(c).round().max(1.0) as usize);
                        let on = on.as_ref().map(|e| e.eval(c).round().clamp(0.0, 3.0) as usize);
                        if let (Some(n), true) = (scene.nest.clone(), slot >= 0.0) {
                            let slot = slot as usize;
                            desks.grow(n.max);
                            if slot < n.max {
                                let screen = nest_screens.get(slot).copied().unwrap_or(0);
                                let target = on.unwrap_or(screen);
                                let id = desks.at(target, ws, &nest_screens);
                                if desks.pool[slot] != id {
                                    desks.pool[slot] = id;
                                    // To another monitor: it goes there, and there it stays in
                                    // this pool (it is already in that monitor's stack).
                                    if target != screen {
                                        if let Some(send) = &nest {
                                            send(ToNest::Send(slot, target));
                                        }
                                    }
                                    nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                                }
                            }
                        }
                    }
                    // A dock's item: its window —the next one, if one of them has the
                    // keyboard—, brought back if it was put away; or its program started.
                    Effect::Dock(k, on, what) => {
                        let what = what.clone();
                        let c = Ctx { props: &props, facts: &facts, seen: None };
                        let k = k.eval(c).round();
                        let screen = on.map_or(nest_on_screen.unwrap_or(0) as f32, |e| e.eval(c)).round().clamp(0.0, 3.0) as usize;
                        if let (Some(n), Some(send), true) = (scene.nest.clone(), &nest, k >= 0.0) {
                            desks.grow(n.max);
                            if let Some(item) = desks.items[screen].get(k as usize).cloned() {
                                let mine: Vec<usize> = desks.docked(screen, &nest_screens).into_iter().filter(|w| desks.of_item(*w, &item)).collect();
                                // Files dropped on it: opened with it (one `sh -c`, each path quoted).
                                if what == crate::scene::DockAction::OpenDrop {
                                    let paths: Vec<String> = drops.iter().flat_map(|(_, d)| d.lines().map(str::trim).filter_map(|l| l.strip_prefix("file://")).map(percent_decode).collect::<Vec<_>>()).collect();
                                    if !paths.is_empty() && !item.exec.is_empty() {
                                        let quoted: Vec<String> = paths.iter().map(|p| format!("'{}'", p.replace('\'', "'\\''"))).collect();
                                        send(ToNest::Launch(format!("{} {}", item.exec, quoted.join(" "))));
                                    }
                                } else if what == crate::scene::DockAction::Pin || what == crate::scene::DockAction::Unpin {
                                    send(ToNest::Pin(item.key.clone(), what == crate::scene::DockAction::Pin));
                                } else if what == crate::scene::DockAction::Close {
                                    for w in &mine {
                                        send(ToNest::Close(*w));
                                    }
                                } else if what == crate::scene::DockAction::Hide {
                                    for w in mine.iter().filter(|w| !desks.minimized[**w]) {
                                        send(ToNest::Minimize(*w, true));
                                    }
                                } else if what == crate::scene::DockAction::New || mine.is_empty() {
                                    if !item.exec.is_empty() {
                                        send(ToNest::Launch(item.exec.clone()));
                                    }
                                } else {
                                    let at = desks.focus.and_then(|f| mine.iter().position(|w| *w == f));
                                    let slot = match at {
                                        Some(p) => mine[(p + 1) % mine.len()],
                                        // A program's (`dock programs`): the one that had the
                                        // keyboard last, one that is out before one put away.
                                        None if desks.programs => mine.iter().copied().max_by_key(|w| (!desks.minimized[*w], desks.touched[*w])).unwrap_or(mine[0]),
                                        None => mine.iter().copied().find(|w| !desks.minimized[*w]).unwrap_or(mine[0]),
                                    };
                                    if desks.minimized[slot] {
                                        send(ToNest::Minimize(slot, false));
                                    }
                                    // And it is seen: on another workspace, that one is shown.
                                    let at_screen = nest_screens.get(slot).copied().unwrap_or(0).min(3);
                                    if desks.programs && desks.pool[slot] != 0 && desks.pool[slot] != desks.shown[at_screen] {
                                        desks.shown[at_screen] = desks.pool[slot];
                                        nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                                    }
                                    send(ToNest::Focus(slot));
                                }
                            }
                        }
                    }
                    // A whole pool to another monitor, with its windows: it goes to the
                    // end of that one's stack and is shown there; this one shows the
                    // pool beside it.
                    // A workspace closed: its windows go to the one before it (to the
                    // next, from the first), which is shown if this one was. The
                    // only one a monitor has stays.
                    Effect::WorkspaceClose(ws, on) => {
                        let c = Ctx { props: &props, facts: &facts, seen: None };
                        let ws = ws.eval(c).round().max(1.0) as usize;
                        let screen = on.map_or(nest_on_screen.unwrap_or(0) as f32, |e| e.eval(c)).round().clamp(0.0, 3.0) as usize;
                        if let Some(n) = scene.nest.clone() {
                            desks.grow(n.max);
                            if desks.stack[screen].len() > 1 && ws <= desks.stack[screen].len() {
                                let id = desks.stack[screen].remove(ws - 1);
                                let beside = desks.stack[screen][ws.saturating_sub(2).min(desks.stack[screen].len() - 1)];
                                for k in 0..desks.pool.len() {
                                    if desks.alive(k) && desks.pool[k] == id {
                                        desks.pool[k] = beside;
                                    }
                                }
                                if let Some(k) = desks.last.remove(&id) {
                                    desks.last.entry(beside).or_insert(k);
                                }
                                if desks.shown[screen] == id {
                                    desks.shown[screen] = beside;
                                }
                                nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                            }
                        }
                    }
                    Effect::WorkspaceTo(ws, on, to) => {
                        let c = Ctx { props: &props, facts: &facts, seen: None };
                        let ws = ws.eval(c).round().max(1.0) as usize;
                        let from = on.map_or(nest_on_screen.unwrap_or(0) as f32, |e| e.eval(c)).round().clamp(0.0, 3.0) as usize;
                        let to = to.eval(c).round().clamp(0.0, 3.0) as usize;
                        if let (Some(n), Some(send), true) = (scene.nest.clone(), &nest, from != to) {
                            desks.grow(n.max);
                            if let Some(id) = desks.stack[from].get(ws - 1).copied().filter(|id| desks.holds(from, *id, &nest_screens)) {
                                let at = ws - 1;
                                desks.stack[from].retain(|p| *p != id);
                                if desks.shown[from] == id {
                                    desks.shown[from] = match desks.stack[from].get(at.saturating_sub(1)).or(desks.stack[from].first()) {
                                        Some(p) => *p,
                                        None => {
                                            let p = desks.next;
                                            desks.next += 1;
                                            p
                                        }
                                    };
                                }
                                desks.stack[to].push(id);
                                desks.shown[to] = id;
                                for k in 0..n.max {
                                    if desks.alive(k) && desks.pool_of(k) == id && nest_screens.get(k) == Some(&from) {
                                        send(ToNest::Send(k, to));
                                    }
                                }
                                nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                            }
                        }
                    }
                    Effect::WindowSwap(a, b) => {
                        let c = Ctx { props: &props, facts: &facts, seen: None };
                        let (a, b) = (a.eval(c).round(), b.eval(c).round());
                        if let (Some(send), true) = (&nest, a >= 0.0 && b >= 0.0 && a != b) {
                            send(ToNest::Swap(a as usize, b as usize));
                        }
                    }
                    Effect::Accept(which) => {
                        let slot = which.eval(Ctx { props: &props, facts: &facts, seen: None }).round();
                        if let (Some(n), true) = (scene.nest.clone(), slot >= 0.0) {
                            let slot = slot as usize;
                            if desks.pending.get(slot).copied().unwrap_or(false) {
                                desks.pending[slot] = false;
                                nest_places(&scene, &mut facts, &mut texts, &to_logic, &n, &nest_order, &nest_screens, &mut desks);
                            }
                        }
                    }
                    Effect::Pick(what) => {
                        let c = Ctx { props: &props, facts: &facts, seen: None };
                        let picked = what.as_ref().and_then(|(window, e)| {
                            let k = e.eval(c).round();
                            (k >= 0.0).then(|| if *window { crate::scene::Picked::Window(k as usize) } else { crate::scene::Picked::Screen(k as usize) })
                        });
                        if let Some(send) = &nest {
                            send(ToNest::Picked(picked));
                        }
                    }
                    Effect::Launch(command) => {
                        if let Some(send) = &nest {
                            let c = Ctx { props: &props, facts: &facts, seen: None };
                            send(ToNest::Launch(crate::gpu::content_text(&command, c, &texts).into_owned()));
                        }
                    }
                    Effect::Text(t, value) => {
                        let k = t.0 as usize;
                        let v = match value {
                            crate::scene::TextValue::Words(w) => w.clone(),
                            crate::scene::TextValue::Of(o) => texts[o.0 as usize].clone(),
                        };
                        // A field being written in keeps its cursor within what it now holds.
                        if let Some(e) = editing.as_mut().filter(|e| e.field == k) {
                            e.cursor = e.cursor.min(v.len());
                            e.anchor = e.anchor.min(v.len());
                        }
                        let _ = to_logic.send(Event::Text(scene.texts[k].0, v.clone()));
                        texts[k] = v;
                    }
                    Effect::FocusField(t) => {
                        parked = None;
                        editing = t.map(|t| t.0 as usize).map(|k| Editing { field: k, cursor: texts[k].len(), anchor: texts[k].len() });
                        last_key = now;
                        // Focusing a field is wanting to type right away. With the keyboard "on
                        // demand", the compositor only gives it with a click, and a search box
                        // that opens with a shortcut never receives it: what is typed
                        // goes to another window and Esc does not close it. It is asked for exclusively
                        // while the scene keeps wanting the keyboard.
                        if t.is_some() && !have_keyboard && scene.surface().keyboard == Keyboard::OnDemand {
                            keyboard_lent = Some(now);
                        }
                    }
                }
            }
            for (s, from_logic, payload) in std::mem::take(&mut signals) {
                let id = SignalId(s as u16);
                if let Some(a) = &mut acting {
                    a.events.push(("event", scene.signals[s].0));
                }
                if !watchers.is_empty() {
                    heard_events.push(("event", scene.signals[s].0));
                }
                // When it last happened: what a `burst:` of particles starts from.
                if draw.signal_times.len() != scene.signals.len() {
                    draw.signal_times = vec![-1.0; scene.signals.len()];
                }
                draw.signal_times[s] = t_total;
                for (layer, st) in scene.layers.iter().zip(layers.iter_mut()) {
                    for (k, r) in layer.claims.iter().enumerate() {
                        match &r.when {
                            When::After { signals: ss, duration } if ss.contains(&id) => st.until[k] = Some(now + *duration),
                            When::FromUntil { from, until } => {
                                if from.contains(&id) {
                                    st.lit[k] = true;
                                } else if until.contains(&id) {
                                    st.lit[k] = false;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                for (k, r) in scene.rules.iter().enumerate() {
                    // One signal, one answer per rule: not one per monitor.
                    if scene.twin_of.get(k).is_some_and(|&t| t != k) {
                        continue;
                    }
                    if !matches!(&r.when, Trigger::On(x) if *x == id) {
                        continue;
                    }
                    // `on chosen(v)`: the value it arrived with, put in now —the
                    // effects are applied later, mixed with other events'—.
                    let v = payload.unwrap_or(0.0);
                    let guard_ok = r.guard.as_ref().is_none_or(|guard| guard.with_payload(v).is_true(Ctx { props: &props, facts: &facts, seen: None }));
                    if guard_ok {
                        effects.extend(r.effects.iter().map(|e| if payload.is_some() { e.with_payload(v) } else { e.clone() }));
                    }
                }
                let (name, goes_out) = scene.signals[s];
                if goes_out && !from_logic {
                    let _ = to_logic.send(Event::Signal(name, payload));
                }
            }
            // Gestures: one only cuts off another of its class or lower.
            for g in std::mem::take(&mut gestures_asked) {
                let requested = &scene.gestures[g];
                let current = gesture.as_ref().map(|r| scene.gestures[r.gesture].class);
                if current.is_some_and(|c| c > requested.class) {
                    let _ = to_logic.send(Event::GestureRejected(requested.name));
                    continue;
                }
                gesture = Some(Playback::start(g, requested, &scene.pose, Ctx { props: &props, facts: &facts, seen: None }, now, reduced_motion));
            }
        }

        pending.retain(|(when, t)| {
            if *when <= now {
                // The destination is computed now, not when it was declared.
                let target = t.to.eval(Ctx { props: &props, facts: &facts, seen: None });
                let a = &mut props[t.prop.0 as usize];
                a.target = target;
                // At once: there, still, and it keeps its own spring for what comes after.
                if t.spring.is_instant() {
                    a.settle();
                    return false;
                }
                a.spring = t.spring;
                if reduced_motion {
                    a.settle();
                }
                false
            } else {
                true
            }
        });
        appointments.extend(pending.iter().map(|p| p.0));

        // Layers: the first claim that holds wins.
        for (layer, st) in scene.layers.iter().zip(layers.iter_mut()) {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            let winner = layer
                .claims
                .iter()
                .enumerate()
                .position(|(k, r)| match &r.when {
                    When::Always => true,
                    When::While(e) => e.is_true(c),
                    When::After { .. } => st.until[k].is_some_and(|h| now < h),
                    When::FromUntil { .. } => st.lit[k],
                })
                .unwrap_or(layer.claims.len().saturating_sub(1));
            appointments.extend(st.until.iter().flatten().filter(|h| **h > now));
            if st.winner != Some(winner) {
                // Freshly loaded, a layer settles where it belongs. Hot reloaded,
                // no: if the file changed a destination, it goes towards it with its spring.
                let first_time = st.winner.is_none() && !st.hot;
                let silent = st.winner.is_none();
                let before = st.winner.map_or("—", |k| layer.claims[k].name);
                st.winner = Some(winner);
                let r = &layer.claims[winner];
                for (k, p) in layer.presences.iter().enumerate() {
                    let a = &mut props[p.0 as usize];
                    a.target = if k == winner { 1.0 } else { 0.0 };
                    if first_time || reduced_motion {
                        a.settle();
                    }
                }
                for t in &r.sets {
                    if first_time {
                        let v = t.to.eval(Ctx { props: &props, facts: &facts, seen: None });
                        props[t.prop.0 as usize].set(v);
                    } else {
                        pending.push((now + t.delay, t.clone()));
                    }
                }
                if !first_time && !silent {
                    println!("layer {} · {} → {}", layer.name, before, r.name);
                    let _ = to_logic.send(Event::Layer(layer.name, r.name));
                }
            }
        }

        // Behaviors: the scene's own life. Whatever a gesture is
        // moving right now they do not touch: a gesture rules over the ambient.
        let from_a_gesture: Vec<PropId> = gesture
            .as_ref()
            .map(|r| scene.gestures[r.gesture].keyframes.iter().flat_map(|f| f.values.iter().map(|(p, _)| *p)).collect())
            .unwrap_or_default();
        let mut alive = plays;
        // (`PLEAMAR_TIMING`) what keeps it alive, each by its name.
        let mut alive_by: Vec<String> = Vec::new();
        if timing && plays {
            alive_by.push("the logic hears every frame".to_owned());
        }
        let mut n_blink = 0;
        follows.begin(&scene.behaviors);
        readers.look(1, &scene.behaviors, &facts, &props, &absent_behaviors);
        for &k in &awake.all {
            let k = k as usize;
            let behavior = &scene.behaviors[k];
            // A `bind` nothing it reads changed for is what it was: its property
            // (a big `let`'s) is written by nobody else.
            if matches!(behavior, Behavior::Bind { .. }) && !readers.dirty[k] {
                if check_shortcuts {
                    if let Behavior::Bind { prop, to } = behavior {
                        let v = to.eval(Ctx { props: &props, facts: &facts, seen: None });
                        if props[prop.0 as usize].x.to_bits() != v.to_bits() {
                            eprintln!("check  · a bind taken as unchanged is not: {} {} → {v}", scene.props[prop.0 as usize].0, props[prop.0 as usize].x);
                        }
                    }
                }
                continue;
            }
            if let Behavior::Blink { prop, .. } | Behavior::Wave { prop, .. } | Behavior::Advance { prop, .. } = behavior {
                readers.touch(*prop);
            }
            match behavior {
                Behavior::Blink { prop, every, duration } => {
                    let (next, since) = &mut blinks[n_blink];
                    n_blink += 1;
                    // While a gesture is leading this same pose by the hand, the ambient
                    // keeps quiet: the gesture already says what the eyelids do. And its clock
                    // stops with it, so that when it ends it does not suddenly fire what was
                    // due halfway through the gesture.
                    if from_a_gesture.contains(prop) {
                        *next += Duration::from_secs_f32(dt);
                        *since = None;
                        continue;
                    }
                    // With reduced motion the ambient keeps quiet: a blink
                    // stays with the eye open. Springs settle and gestures
                    // show their still face; what goes on its own and in a loop is exactly
                    // what must not keep going round.
                    if reduced_motion {
                        *since = None;
                        props[prop.0 as usize].set(1.0);
                        continue;
                    }
                    if since.is_none() && now >= *next {
                        *since = Some(now);
                    }
                    if let Some(d) = *since {
                        let t = (now - d).as_secs_f32() / duration;
                        if t >= 1.0 {
                            *since = None;
                            // The period is counted from start to start: "every 5.2 s" is
                            // that, not 5.2 s **after** closing the eye.
                            let following = d + Duration::from_secs_f32(rng.between(every.0, every.1));
                            *next = following.max(now);
                            props[prop.0 as usize].set(1.0);
                        } else {
                            props[prop.0 as usize].set((2.0 * t - 1.0).abs().powf(1.6));
                            alive = true;
                            if timing {
                                alive_by.push(format!("blink {}", scene.props[prop.0 as usize].0));
                            }
                        }
                    }
                    appointments.push(*next);
                }
                Behavior::Wave { prop, frequency, amplitude } => {
                    if from_a_gesture.contains(prop) {
                        continue;
                    }
                    // And a wave stays at its rest —halfway through the journey—, which
                    // is where it was when it started.
                    if reduced_motion {
                        props[prop.0 as usize].set(0.0);
                        continue;
                    }
                    let a = amplitude.eval(Ctx { props: &props, facts: &facts, seen: None });
                    props[prop.0 as usize].set(a * (t_total * frequency).sin());
                    alive |= a.abs() > 0.01;
                    if timing && a.abs() > 0.01 {
                        alive_by.push(format!("wave {}", scene.props[prop.0 as usize].0));
                    }
                }
                Behavior::Sort { keys, ord, pos } => {
                    if readers.dirty[k] {
                        let mut moved = false;
                        run_sort(keys, ord, pos, &mut props, &facts, |p| {
                            readers.touch(p);
                            moved = true;
                        });
                        alive |= moved;
                        if timing && moved {
                            alive_by.push("a sort whose keys changed".to_owned());
                        }
                    }
                }
                Behavior::Follow { prop, to } => {
                    let v = follows.kept_or_value(readers.dirty[k], k, to, &props, &facts);
                    let t = &mut props[prop.0 as usize].target;
                    if t.to_bits() != v.to_bits() {
                        *t = v;
                    }
                }
                Behavior::Bind { prop, to } => {
                    let v = follows.kept_or_value(readers.dirty[k], k, to, &props, &facts);
                    if props[prop.0 as usize].x.to_bits() != v.to_bits() {
                        readers.touch(*prop);
                    }
                    let p = &mut props[prop.0 as usize];
                    alive |= (p.x - v).abs() > 1e-3;
                    if timing && (p.x - v).abs() > 1e-3 {
                        alive_by.push(format!("{} bound to what changes", scene.props[prop.0 as usize].0));
                    }
                    p.set(v);
                }
                Behavior::Advance { prop, per_second } => {
                    // A spin has no rest to return to: it stays where it is going.
                    if reduced_motion {
                        continue;
                    }
                    let v = per_second.eval(Ctx { props: &props, facts: &facts, seen: None });
                    // Stopped, it lets the property be: a rule can send it
                    // somewhere. Pinning it every frame at speed 0 undid every
                    // `x: 0` —a reading clock that was never reset—.
                    if v.abs() < 1e-9 {
                        continue;
                    }
                    // The clock only shaders read stands still while none of
                    // them is drawn: rain that is not falling kept a desktop
                    // going round at the monitor's pace, all day, for nothing.
                    if scene.shader_clock == Some(*prop) && !draw.timed {
                        continue;
                    }
                    // Turning, it carries the property and where it is going
                    // together: a rule that sends it back to 0 midway is obeyed,
                    // and it goes on turning from there.
                    let a = &mut props[prop.0 as usize];
                    a.x += v * dt;
                    a.target += v * dt;
                    alive |= v.abs() > 1e-4;
                    if timing && v.abs() > 1e-4 {
                        alive_by.push(format!("spin {}", scene.props[prop.0 as usize].0));
                    }
                }
                Behavior::Gaze { x, y, center, reach, distance, rest } => {
                    let c = Ctx { props: &props, facts: &facts, seen: None };
                    let (mx, my) = match pointer {
                        Some((px, py)) => {
                            let (dx, dy) = (px - center.0.eval(c), py - center.1.eval(c));
                            let far = dx.hypot(dy).max(1.0);
                            let pull = (far / distance).min(1.0);
                            (dx / far * reach.0 * pull, dy / far * reach.1 * pull)
                        }
                        None => (rest.0.eval(c), rest.1.eval(c)),
                    };
                    props[x.0 as usize].target = mx;
                    props[y.0 as usize].target = my;
                }
            }
        }

        // Postures: gestures that repeat on their own while something is true.
        if gesture.is_none() {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            if let Some((g, _)) = scene.postures.iter().find(|(_, e)| e.is_true(c)) {
                let k = g.0 as usize;
                gesture = Some(Playback::start(k, &scene.gestures[k], &scene.pose, Ctx { props: &props, facts: &facts, seen: None }, now, reduced_motion));
            }
        }

        // With reduced motion every spring is already where it goes: what a
        // `follow`, a hover or a `look` points it at too, not only what a rule
        // animates (those went on chasing with their springs, against what the
        // reference says).
        for a in &mut props {
            if reduced_motion {
                a.settle();
            } else {
                a.step(dt);
            }
        }

        // The gesture leads the pose's properties by the hand; when it ends
        // it lets them go and their springs bring them back to the base.
        let mut gesture_ended = false;
        if let Some(r) = &mut gesture {
            alive = true;
            if timing {
                alive_by.push("a gesture".to_owned());
            }
            let g = &scene.gestures[r.gesture];
            loop {
                let f = &g.keyframes[r.keyframe];
                let elapsed = (now - r.started_at).as_secs_f32() * 1000.0;
                if elapsed < (f.ms + f.hold) as f32 {
                    break;
                }
                // It has arrived: whatever comes next starts from where this one ends.
                for (k, p) in scene.pose.iter().enumerate() {
                    r.from[k] = r.target(r.keyframe, *p, &props);
                }
                r.started_at += Duration::from_millis((f.ms + f.hold) as u64);
                r.keyframe += 1;
                if r.keyframe >= g.keyframes.len() {
                    break;
                }
                if let Some(s) = g.keyframes[r.keyframe].emit {
                    late_signals.push(s);
                }
            }
            if r.keyframe >= g.keyframes.len() {
                for (k, p) in scene.pose.iter().enumerate() {
                    let a = &mut props[p.0 as usize];
                    a.x = if reduced_motion { a.target } else { r.from[k] };
                    a.v = 0.0;
                }
                gesture_ended = true;
            } else {
                let f = &g.keyframes[r.keyframe];
                let t = if f.ms == 0 { 1.0 } else { (now - r.started_at).as_secs_f32() * 1000.0 / f.ms as f32 };
                let progress = if r.still.is_some() { 1.0 } else { f.curve.apply(t) };
                let of = r.still.unwrap_or(r.keyframe);
                for (k, p) in scene.pose.iter().enumerate() {
                    let to = r.target(of, *p, &props);
                    let a = &mut props[p.0 as usize];
                    a.x = r.from[k] + (to - r.from[k]) * progress;
                    a.v = 0.0;
                }
            }
        }
        if gesture_ended {
            gesture = None;
        }

        // What is bound is computed again now that the springs have moved. Read
        // only before them, a sum kept in a property —a big `let`, where the
        // children of a layout fall— went one frame behind what the drawing
        // reads straight: a row that grows, and the one under it a frame late.
        // What it reads has seldom changed, and then it is only looked at. One
        // that did change asks for another round: whatever follows it, or is
        // decided by it, read the old one above.
        readers.look(2, &scene.behaviors, &facts, &props, &absent_behaviors);
        for &k in &awake.binds {
            let k = k as usize;
            let behavior = &scene.behaviors[k];
            if !readers.dirty[k] {
                continue;
            }
            if let Behavior::Bind { prop, to } = behavior {
                if absent_behaviors.iter().any(|r| r.contains(&k)) {
                    continue;
                }
                let v = follows.kept_or_value(readers.dirty[k], k, to, &props, &facts);
                if props[prop.0 as usize].x.to_bits() != v.to_bits() {
                    readers.touch(*prop);
                }
                let p = &mut props[prop.0 as usize];
                if (p.x - v).abs() > 1e-3 {
                    alive = true;
                    if timing {
                        alive_by.push(format!("{} bound to what moved", scene.props[prop.0 as usize].0));
                    }
                }
                p.set(v);
            } else if let Behavior::Sort { keys, ord, pos } = behavior {
                if readers.dirty[k] && !absent_behaviors.iter().any(|r| r.contains(&k)) {
                    let mut moved = false;
                    run_sort(keys, ord, pos, &mut props, &facts, |p| {
                        readers.touch(p);
                        moved = true;
                    });
                    if moved {
                        alive = true;
                        if timing {
                            alive_by.push("a sort whose keys moved".to_owned());
                        }
                    }
                }
            }
        }
        facts_at_binds.clone_from(&facts);

        // The popups: open while their fact is true, where and how their expressions
        // say. If they change place or size while open, they are remade.
        popups.resize(scene.popups.len(), None);
        // What is drawn exists if it falls on some live surface, or on some open
        // popup. A closed surface does not contribute its own: that way all of its things are discarded when
        // composing and its next frame comes out empty, which is what makes it disappear.
        let open = |k: usize| scene.surfaces.get(k).and_then(|s| s.open.as_ref()).is_none_or(|e| e.is_true(Ctx { props: &props, facts: &facts, seen: None }));
        // The lock ones are not put up: they are engaged when their `open:` becomes
        // true and removed when it stops being so. When removing them, first what was
        // being painted on them is released and then them, like a popup.
        let want: Vec<(usize, bool)> = scene.surfaces.iter().enumerate().filter(|(_, s)| s.lock_screen).map(|(k, _)| (k, open(k))).collect();
        for (k, wants) in want {
            let was = locks.contains(&k);
            if wants && !was {
                locks.push(k);
                let s = &scene.surfaces[k];
                crate::platform::lock_screen(k, Some(((s.width, s.height), s.origin, s.screens.clone())));
            } else if !wants && was {
                locks.retain(|x| *x != k);
                sheets.retain(|l| !(l.view.surface == k && l.view.popup.is_none()));
                crate::platform::lock_screen(k, None);
                if let Some(g) = &gpu {
                    assign_pace(g, &mut sheets, size, op.no_vsync);
                }
                if let Some(h) = scene.facts.iter().position(|(n, _)| *n == "lock.held") {
                    facts[h] = 0.0;
                }
            }
        }
        let open = |k: usize| scene.surfaces.get(k).and_then(|s| s.open.as_ref()).is_none_or(|e| e.is_true(Ctx { props: &props, facts: &facts, seen: None }));
        draw.views.clear();
        draw.views.extend(sheets.iter().filter(|l| open(l.view.surface)).map(|l| l.view.bounds()));
        for (k, em) in scene.popups.iter().enumerate() {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            let wants = (facts[em.open.0 as usize] > 0.5 && !sheets.is_empty()).then(|| {
                [em.at.0.eval(c).round() as i32, em.at.1.eval(c).round() as i32, em.size.0.eval(c).round().max(1.0) as i32, em.size.1.eval(c).round().max(1.0) as i32]
            });
            if wants != popups[k] {
                if popups[k].is_some() {
                    sheets.retain(|l| l.view.popup != Some(k));
                    crate::platform::popup(k, None);
                    if let Some(g) = &gpu {
                        assign_pace(g, &mut sheets, size, op.no_vsync);
                    }
                }
                if let Some(g) = wants {
                    crate::platform::popup(k, Some((g, em.origin)));
                }
                popups[k] = wants;
            }
            if let Some(g) = popups[k] {
                draw.views.push([em.origin.0, em.origin.1, em.origin.0 + g[2] as f32, em.origin.1 + g[3] as f32]);
            }
        }

        // The language changed (`locale`): the declared texts that still say one of their
        // versions say the new one. One the logic has written something else into is its own.
        if let Some(l) = scene.locale {
            let now = facts[l.0 as usize].round().max(0.0) as usize;
            if shown_locale != Some(now) {
                for (t, versions) in &scene.text_versions {
                    let k = t.0 as usize;
                    let Some(v) = versions.get(now) else { continue };
                    if texts.get(k).is_some_and(|x| versions.contains(x)) && texts[k] != *v {
                        texts[k] = v.clone();
                        let _ = to_logic.send(Event::Text(scene.texts[k].0, v.clone()));
                    }
                }
                shown_locale = Some(now);
            }
        }

        if profiling {
            let n = Instant::now();
            { let d = (n - prof_t).as_secs_f64() * 1000.0; prof[1] += d; sec[1] += d; }
            prof_t = n;
            let c = thread_cpu_ms();
            prof_cpu[1] += c - prof_c;
            prof_c = c;
        }
        // ── 3. paint ────────────────────────────────────────────
        let blocked = logic_blocked.load(Ordering::Relaxed);
        let c = Ctx { props: &props, facts: &facts, seen: None };
        // Text and images are painted at the scale of the finest sheet; the
        // others see them reduced, which shows much less than enlarged.
        let finest = sheets.iter().map(|l| l.scale).fold(1.0f32, f32::max);
        if atlas_stale || (finest - letters.scale()).abs() > 0.001 {
            letters.reset(finest, &scene.images, &scene.fonts);
            atlas_stale = false;
        }
        // It is composed even if there is nowhere to paint yet: that way the workshop gets on with
        // the texts and the images while the GPU and the windows start up.
        // The text cursor blinks: half a second on, half off, and always on right
        // after typing. Between blinks there is no need to paint.
        let view = editing.as_ref().map(|e| {
            let t = (now - last_key).as_secs_f32();
            appointments.push(now + Duration::from_secs_f32(0.53 - t % 0.53 + 0.001));
            crate::gpu::FieldView { text: e.field, cursor: e.cursor, anchor: e.anchor, visible: (t % 1.06) < 0.53 }
        });
        if let Some((_, _, _, when)) = &repeat {
            appointments.push(*when);
        }
        let to_paint: &[Instr] = if warning.is_some() { &with_warning } else { &scene.instrs };
        draw.set_attached_edges(scene.surface().anchor.attached_edges());
        let reading = Instant::now();
        let skip: Vec<std::ops::Range<usize>> = asleep.iter().map(|t| t.instrs.clone()).collect();
        draw.skip = skip;
        // A selection whose text was not painted last frame (closed, gone) is forgotten.
        if selection.as_ref().is_some_and(|s| !draw.selectable.iter().any(|t| t.at == s.at)) {
            selection = None;
        }
        draw.selected = selection.as_ref().map(|s| (s.at, s.anchor, s.cursor));
        // Which zone is on top of which stays for `describe`, after the list is made.
        let zone_rank = arrangement.as_ref().filter(|_| draw.collect_texts).map(|a| a.zone_rank.clone());
        draw.order = arrangement.map(|a| a.order);
        draw.clock = t_total;
        draw.reduced_motion = reduced_motion;
        if draw.signal_times.len() != scene.signals.len() {
            draw.signal_times = vec![-1.0; scene.signals.len()];
        }
        if profiling {
            let n = Instant::now();
            { let d = (n - prof_t).as_secs_f64() * 1000.0; prof[2] += d; sec[2] += d; }
            prof_t = n;
            let c = thread_cpu_ms();
            prof_cpu[2] += c - prof_c;
            prof_c = c;
        }
        if nest.is_some() {
            // What the windows drew, to the card; and where each one is in it.
            if let Some(g) = gpu.as_mut() {
                #[cfg(target_os = "linux")]
                if !nest_gpu_told {
                    nest_gpu_told = true;
                    if let (Some(send), Some((device, formats))) = (&nest, g.dmabuf_formats()) {
                        println!("windows · programs can hand over their frames on the card: {} layouts", formats.len());
                        send(ToNest::Gpu { device, formats });
                    }
                }
                let mut released: Vec<u64> = Vec::new();
                let mut copied_any = false;
                let mut again = true;
                while again {
                    again = false;
                    'windows: for w in nest_windows.iter_mut() {
                        for p in w.pieces.iter_mut() {
                            if p.uploaded {
                                continue;
                            }
                            p.uploaded = true;
                            let grew = if let Some(buffer) = p.buffer {
                                #[cfg(target_os = "linux")]
                                {
                                    let fresh = p.fresh.take();
                                    let was_fresh = fresh.is_some();
                                    match g.copy_dmabuf(p.layer, p.px, buffer, fresh) {
                                        Ok(grew) => {
                                            if was_fresh {
                                                released.push(buffer);
                                            }
                                            copied_any = true;
                                            grew
                                        }
                                        Err(e) => {
                                            eprintln!("windows · a frame on the card could not be read: {e}");
                                            if was_fresh {
                                                released.push(buffer);
                                            }
                                            false
                                        }
                                    }
                                }
                                #[cfg(not(target_os = "linux"))]
                                {
                                    let _ = buffer;
                                    false
                                }
                            } else if !p.pixels.is_empty() {
                                g.upload_window(p.layer, p.px, &p.pixels)
                            } else {
                                false
                            };
                            if grew {
                                // The texture grew and the others' images were lost with it.
                                again = true;
                                break 'windows;
                            }
                        }
                    }
                    if again {
                        for p in nest_windows.iter_mut().flat_map(|w| w.pieces.iter_mut()) {
                            p.uploaded = false;
                        }
                    }
                }
                // The programs' buffers go back once the card has copied them:
                // that is waited for after painting, when it is long done.
                nest_copied.0.extend(released);
                nest_copied.1 |= copied_any;
                let (dw, dh) = g.windows_dims();
                draw.window_tex = nest_windows
                    .iter()
                    .map(|w| {
                        let pieces: Vec<(u32, [f32; 4], [f32; 4], bool)> = w
                            .pieces
                            .iter()
                            .filter(|p| p.buffer.is_some() || !p.pixels.is_empty())
                            .map(|p| {
                                // Its pixels (the part it shows) into its size, in the window's units.
                                let (pw, ph) = (p.size.0 as f32, p.size.1 as f32);
                                let s = p.src;
                                (p.layer, [s[0] / dw as f32, s[1] / dh as f32, s[2] / dw as f32, s[3] / dh as f32], [p.at.0 as f32, p.at.1 as f32, pw, ph], p.opaque)
                            })
                            .collect();
                        (!pieces.is_empty()).then(|| {
                            let g = w.geometry;
                            crate::gpu::WindowTex { geometry: [g[0] as f32, g[1] as f32, g[2] as f32, g[3] as f32], pieces }
                        })
                    })
                    .collect();
            }
            if let Some(send) = &nest {
                // The size each window is told to have: only when it changes.
                // (Only the windows' own instructions, found once per scene; and not
                // those of the copies for monitors that are not there.)
                if window_instrs.as_ref().is_none_or(|(n, _)| *n != to_paint.len()) {
                    window_instrs = Some((to_paint.len(), to_paint.iter().enumerate().filter(|(_, i)| matches!(i, Instr::Window { .. })).map(|(k, _)| k).collect()));
                }
                let found = window_instrs.as_ref().map_or(&[][..], |(_, v)| v.as_slice());
                // Not inside a hidden group either (`show:` false —the copy of
                // another monitor—): it asked for another size too, and the
                // window was told two sizes over and over (Discord, black).
                for i in found.iter().filter(|k| !absent.iter().any(|r| r.contains(k)) && !draw.hidden.get(**k).copied().unwrap_or(false)).map(|k| &to_paint[*k]) {
                    let Instr::Window { slot, ask, alpha, .. } = i else { continue };
                    // Only where it is shown: with a copy of the scene per monitor,
                    // the copies where it is not would ask it for another size.
                    if alpha.eval(c) <= 0.001 {
                        continue;
                    }
                    // `ask: 0, 0`: the size it chooses (a dialog of its own size);
                    // `ask: -1, -1`: nothing, a picture of it (a dock's preview) that
                    // must not ask it for another size than the copy that lays it out.
                    let (aw, ah) = (ask.0.eval(c), ask.1.eval(c));
                    if aw < 0.0 || ah < 0.0 {
                        continue;
                    }
                    let wanted = (aw.round() as i32, ah.round() as i32);
                    if let Some(w) = nest_windows.get_mut(*slot) {
                        if w.ask != Some(wanted) {
                            w.ask = Some(wanted);
                            send(ToNest::Configure { slot: *slot, w: wanted.0, h: wanted.1 });
                        }
                    }
                }
                // And the monitor they believe they are on is the scene's surface.
                let own = (size.0 as i32, (size.1 - if op.hud { crate::gpu::HUD_HEIGHT } else { 0.0 }) as i32);
                if own != nest_size {
                    nest_size = own;
                    send(ToNest::Size(own.0, own.1));
                }
            }
        }
        if profiling {
            let n = Instant::now();
            { let d = (n - prof_t).as_secs_f64() * 1000.0; prof[7] += d; sec[7] += d; }
            prof_t = n;
            let c = thread_cpu_ms();
            prof_cpu[7] += c - prof_c;
            prof_c = c;
        }
        // Nothing the scene reads has changed —only a window drew something new
        // in its box, which is a texture and not the list—: the list is the one
        // already made, and already on the card. A browser at 165 frames a second
        // made it again every time for nothing.
        //
        // And of the facts and properties, only the ones the drawing read when
        // it was made: what it did not read cannot change it. The mouse going
        // over a desktop that draws nothing of it —no dock under it, nothing
        // carried— made the whole list again at the monitor's pace, to get the
        // same list.
        let same_scene = !draw.timed
            && !draw.particles_alive
            && draw.wake_at.is_none()
            && !draw.collect_texts
            && warning.is_none()
            && compose_memo.as_ref().is_some_and(|m: &ComposeMemo| {
                m.size == size
                    && m.view == view
                    && m.views == draw.views
                    && m.texts == texts
                    && m.window_tex == draw.window_tex
                    && m.skip == draw.skip
                    && m.order == draw.order
                    && m.selected == draw.selected
                    && m.signal_times == draw.signal_times
                    && m.facts.len() == facts.len()
                    && m.props.len() == props.len()
                    && m.seen.facts().all(|k| m.facts[k] == facts[k])
                    && m.seen.props().all(|k| m.props[k] == (props[k].x, props[k].v))
            });
        if !same_scene {
            compose_seen.reset(facts.len(), props.len());
            draw.compose(to_paint, Ctx { seen: Some(&compose_seen), ..c }, &texts, &mut letters, view, size, op.hud);
            stretches_copied += draw.copied.0 as u64;
            stretches_made += draw.copied.1 as u64;
            // With stretches copied: put together whole again, and compared.
            if check_shortcuts && draw.copied.0 > 0 {
                compose_seen.reset(facts.len(), props.len());
                if let Some(diff) = draw.check_copy(|d| d.compose(to_paint, Ctx { seen: Some(&compose_seen), ..c }, &texts, &mut letters, view, size, op.hud)) {
                    eprintln!("check  · stretches copied from the frame before are not what putting it together gives: {diff}");
                }
            }
            if draw.collect_texts {
                let gone = |at: usize| draw.hidden.get(at).copied().unwrap_or(false) || absent.iter().any(|r| r.contains(&at));
                let sight = crate::agent::Sight {
                    shown: sheets.iter().filter(|s| s.open).map(|s| crate::agent::Shown { surface: s.view.surface, popup: s.view.popup, bounds: s.view.bounds(), scale: s.scale }).collect(),
                    texts_seen: &draw.texts_seen,
                    gone: &gone,
                    rank: zone_rank.as_ref().map(|r| r.as_slice()),
                };
                let parts = crate::agent::describe(&scene, c, &texts, &sight);
                if std::env::var_os("PLEAMAR_DEBUG_DESCRIBE").is_some() {
                    let there = scene.zones.iter().filter(|z| !gone(z.at)).count();
                    let active = scene.zones.iter().filter(|z| !gone(z.at) && z.active.is_true(c)).count();
                    eprintln!("describe · {} zones, {there} there, {active} active · {} texts seen · shown {:?}", scene.zones.len(), draw.texts_seen.len(), sight.shown.iter().map(|s| (s.surface, s.bounds)).collect::<Vec<_>>());
                    for z in scene.zones.iter().filter(|z| !gone(z.at) && z.active.is_true(c)).take(12) {
                        eprintln!("describe ·   {} {:?}", z.id, z.bounds(c));
                    }
                }
                // By the name it was written with: `hit`, not each copy's `hit#r3`.
                for name in crate::agent::unnamed(&parts) {
                    let written = name.split('#').next().unwrap_or(name);
                    if unnamed_said.insert(written) {
                        eprintln!("agent  · '{written}' can be pressed, but nothing says what it is: give it a `label:`");
                    }
                }
                for (json, reply_to) in describing.drain(..) {
                    let _ = reply_to.send(if json { crate::agent::to_json(&parts) } else { crate::agent::to_text(&parts) });
                }
                draw.collect_texts = false;
                draw.texts_seen.clear();
            }
            // Written over the last one: a round composed is not a round of allocations.
            let m = compose_memo.get_or_insert_with(|| ComposeMemo {
                size,
                view,
                views: Vec::new(),
                facts: Vec::new(),
                texts: Vec::new(),
                window_tex: Vec::new(),
                props: Vec::new(),
                seen: Default::default(),
                skip: Vec::new(),
                order: None,
                selected: None,
                signal_times: Vec::new(),
            });
            m.size = size;
            m.view = view;
            m.views.clone_from(&draw.views);
            m.facts.clone_from(&facts);
            m.texts.clone_from(&texts);
            m.window_tex.clone_from(&draw.window_tex);
            m.props.clear();
            m.props.extend(props.iter().map(|a| (a.x, a.v)));
            m.seen.clone_from(&compose_seen);
            m.skip.clone_from(&draw.skip);
            m.order.clone_from(&draw.order);
            m.selected = draw.selected;
            m.signal_times.clone_from(&draw.signal_times);
        }
        // An agent's action: when its steps are done and the scene has stopped
        // answering —its rules, its logic—, it is told what happened.
        if let Some(a) = &mut acting {
            appointments.push(now + Duration::from_millis(15));
            if a.steps.is_empty() {
                let done = *a.done.get_or_insert(now);
                let open = open_surfaces(&scene, &sheets);
                let heard = crate::agent::what_happened(&scene, &a.before, c, &texts, &open, &a.events);
                if heard != a.heard {
                    a.heard = heard;
                    a.quiet = now;
                }
                let settled = now - done >= Duration::from_millis(150) && now - a.quiet >= Duration::from_millis(150);
                if settled || now - done >= Duration::from_millis(1200) {
                    let a = acting.take().unwrap();
                    let _ = a.reply.send(format!("{}{}", a.said, a.heard));
                }
            }
        }
        // Conditions waited for: said as soon as they hold.
        if !waits.is_empty() {
            waits.retain(|(cond, start, deadline, reply)| {
                if cond.holds(&scene, c, &texts) {
                    let _ = reply.send(format!("yes, after {} ms\n", (now - *start).as_millis()));
                    false
                } else if now >= *deadline {
                    let _ = reply.send(format!("? not after {:.1} s: {}\n", (*deadline - *start).as_secs_f32(), cond.now(&scene, c, &texts)));
                    false
                } else {
                    appointments.push(*deadline);
                    true
                }
            });
        }
        // And what is watched: a line for each change, with when it happened.
        if !watchers.is_empty() {
            let open = open_surfaces(&scene, &sheets);
            watchers.retain_mut(|w| {
                if now >= w.until {
                    let _ = w.lines.send("done watching".into());
                    return false;
                }
                appointments.push(w.until);
                let heard = crate::agent::what_happened(&scene, &w.before, c, &texts, &open, &heard_events);
                if heard.trim() == "nothing changed" {
                    return true;
                }
                w.before = crate::agent::before(&scene, c, &texts, open.clone());
                let at = (now - w.start).as_secs_f32();
                heard.lines().all(|l| w.lines.send(format!("+{at:.2}s{l}")).is_ok())
            });
        }
        // And who follows names: a line for each one worth something else. With
        // nobody following, this is all it costs: a look at an empty list.
        if !tails.is_empty() {
            tails.retain_mut(|f| {
                if let Some(u) = f.until() {
                    appointments.push(u);
                }
                f.tell(&scene, &facts, &texts, now)
            });
        }
        // Particles carry themselves: while one is alive, the scene does not rest.
        alive |= draw.particles_alive;
        // A shader that reads the clock has just been drawn: the clock goes
        // on from the next round (see `shader_clock`).
        if draw.timed && scene.shader_clock.is_some() {
            alive = true;
            if timing {
                alive_by.push("a shader that reads the time".to_owned());
            }
        }
        if timing && draw.particles_alive {
            alive_by.push("particles".to_owned());
        }
        // An image that moves: wake up when it changes frame.
        if let Some(t) = draw.wake_at {
            appointments.push(start + Duration::from_secs_f32(t.max(t_total + 0.001)));
        }
        cycle.compose_ms += reading.elapsed().as_secs_f32() * 1000.0;
        let Some(g) = &mut gpu else {
            // Nowhere yet: time runs all the same, but unhurried.
            std::thread::sleep(Duration::from_millis(8));
            continue;
        };
        // `lens = image ... while`: the picture while it holds, a capture of what is behind when it does not.
        if let (Some(path), Some(when)) = (scene.lens_image.as_deref(), &scene.lens_image_while) {
            let on = when.is_true(Ctx { props: &props, facts: &facts, seen: None });
            g.set_lens_image(on.then_some(path));
            g.set_shaders(&scene.shaders);
        }
        // `lens every` and `lens hold while`: how often what is behind a glass is captured, and whether at all.
        let capture_interval = crate::lens::capture_interval(scene.lens_every.as_ref().map(|e| e.eval(Ctx { props: &props, facts: &facts, seen: None })));
        let capture_held = scene.lens_hold_while.as_ref().is_some_and(|e| e.is_true(Ctx { props: &props, facts: &facts, seen: None }));
        let pace = |l: &Sheet| crate::lens::Pace { interval: capture_interval, held: capture_held, warming: l.lens.as_ref().is_some_and(|x| x.warming()), since: l.capture_taken.elapsed() };
        // The numbers the blur is given, worked out for this frame.
        g.blur_values = match &scene.blur {
            Some(_) => {
                let mut v = [0.0; 8];
                for (k, e) in scene.blur_values.iter().take(8).enumerate() {
                    v[k] = e.eval(Ctx { props: &props, facts: &facts, seen: None });
                }
                v
            }
            None => crate::blur::DEFAULT_VALUES,
        };
        // Buffers on the card for live windows, asked by the thumbnails service.
        #[cfg(target_os = "linux")]
        g.answer_thumbnail_wants();
        #[cfg(target_os = "linux")]
        g.check_thumbnails();
        // What was uploaded to the atlas may take the slot of something that is no longer there: with
        // a new atlas, any letter may have changed without changing its `uv`.
        // A picture of its own painted again keeps its number, and so its element.
        if !letters.pending_upload.is_empty() || !letters.pending_fit.is_empty() || !letters.pending_pictures.is_empty() {
            previous.forget();
            compose_memo = None;
            draw.forget_kept();
        }
        // (What is written to the card waits in memory for the next thing submitted to it.)
        let wrote = !letters.pending_upload.is_empty() || !letters.pending_pictures.is_empty() || !same_scene;
        g.fit_atlas(letters.atlas_height());
        g.upload_atlas(&mut letters.pending_upload);
        g.upload_pictures(&mut letters.pending_pictures, &mut letters.dropped_pictures);
        #[cfg(target_os = "linux")]
        g.fit_thumbnails(&mut letters.pending_fit);
        #[cfg(not(target_os = "linux"))]
        letters.pending_fit.clear();
        if !same_scene {
            g.upload(&draw);
        }
        // What has changed, and where. The frame graph always changes.
        // The light of a click changes the glass without changing the list: everything is painted.
        if profiling {
            let n = Instant::now();
            { let d = (n - prof_t).as_secs_f64() * 1000.0; prof[3] += d; sec[3] += d; }
            prof_t = n;
            let c = thread_cpu_ms();
            prof_cpu[3] += c - prof_c;
            prof_c = c;
        }
        // Resolve after layout: a tab may have appeared or moved in this very frame.
        if let Some(z) = ripple_target.filter(|_| !reduced_motion) {
            let c = Ctx { props: &props, facts: &facts, seen: None };
            if let Some(p) = ripple_origin(&scene, z, c, &draw.hidden)
                .filter(|p| sheets.iter().any(|s| s.open && crate::agent::inside_box(s.view.bounds(), *p)))
            {
                ripple = Some((p, Instant::now()));
            }
        }
        // A list not composed again is the one compared last time: comparing it
        // whole, and copying it, was work for nothing in every round a window drew.
        let known = if same_scene {
            let known = previous.unchanged(&mut changed);
            if check_shortcuts {
                let mut full = Vec::new();
                let full_known = previous.changed_rects(&draw, &mut full);
                if full_known != known || !full.is_empty() {
                    eprintln!("check  · the list was taken as unchanged and is not: known {known} → {full_known}, {} pieces changed", full.len());
                }
            }
            known
        } else {
            previous.changed_rects(&draw, &mut changed)
        };
        // A press's light and its ripple are seen only on the glass that catches
        // them: those boxes change, not the whole of every open sheet (issue
        // 109). With none, there is nothing to light and nothing to wait for.
        let lit = if finger_light > 0.0 || ripple.is_some() { draw.lit_by_a_press(ripple.is_some()) } else { Some(Vec::new()) };
        if lit.as_ref().is_some_and(Vec::is_empty) {
            finger_light = 0.0;
            ripple = None;
        }
        let all_changed = !known || op.hud || lit.is_none();
        changed.extend(lit.unwrap_or_default());
        // A window that drew something new changes what its box covers, and nothing else.
        for slot in std::mem::take(&mut nest_changed) {
            changed.extend(draw.windows_drawn.iter().filter(|w| w.0 == slot).map(|(_, d, affine)| {
                let b = affine.bounds([d[0], d[1], d[0] + d[2], d[1] + d[3]]);
                [b[0] - 1.0, b[1] - 1.0, b[2] + 1.0, b[3] + 1.0]
            }));
        }

        // A window closed that the scene no longer draws (its leaving is over):
        // its last image goes. Kept until its slot was used again, every slot
        // ended up holding a dead window's pixels, a monitor's worth each.
        if !nest_closed.is_empty() {
            nest_closed.retain(|slot| {
                if draw.windows_drawn.iter().any(|w| w.0 == *slot) {
                    return true;
                }
                if let Some(w) = nest_windows.get_mut(*slot) {
                    let mut gone = Vec::new();
                    for p in w.pieces.drain(..) {
                        nest_layers[p.layer as usize] = false;
                        gone.extend(p.buffer);
                    }
                    #[cfg(target_os = "linux")]
                    {
                        nest_doomed.retain(|b| !gone.contains(b));
                        g.forget_dmabufs(&gone);
                    }
                }
                false
            });
        }

        // Where each window is seen, for the compositor: on which monitor and its
        // box there (the last one drawn, which is the one on top).
        if let Some(send) = &nest {
            let mut seen: Vec<usize> = Vec::new();
            for (slot, d, affine) in draw.windows_drawn.iter().rev() {
                if seen.contains(slot) {
                    continue;
                }
                seen.push(*slot);
                let b = affine.bounds([d[0], d[1], d[0] + d[2], d[1] + d[3]]);
                let (cx, cy) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
                let Some(l) = sheets.iter().filter(|l| l.view.popup.is_none()).find(|l| {
                    let v = l.view.bounds();
                    cx >= v[0] && cx < v[2] && cy >= v[1] && cy < v[3]
                }) else {
                    continue;
                };
                let v = l.view.bounds();
                let rect = [((b[0] - v[0]) * l.scale).round() as i32, ((b[1] - v[1]) * l.scale).round() as i32, ((b[2] - b[0]) * l.scale).round() as i32, ((b[3] - b[1]) * l.scale).round() as i32];
                let now = (l.name.clone(), rect);
                if nest_shown.get(slot) != Some(&now) {
                    nest_shown.insert(*slot, now.clone());
                    send(ToNest::Shown { slot: *slot, monitor: now.0, rect });
                }
            }
            // And the ones no longer drawn (another workspace, put away): seen nowhere.
            let hidden: Vec<usize> = nest_shown.keys().filter(|k| !seen.contains(k)).copied().collect();
            for slot in hidden {
                nest_shown.remove(&slot);
                send(ToNest::Shown { slot, monitor: String::new(), rect: [0; 4] });
            }
        }

        // Where the mouse comes in: the active zones, and nothing else. The rest of
        // the surface is transparent for the click too.
        // Nothing can be clicked on a closed surface. Its piece of the plane is
        // the one its sheet really covers, not the size written in the scene:
        // `size: full, full` is written as 0 × 0, and a closed full-screen
        // surface kept its zones —a transparent wall over the whole desktop
        // that took every click, and with no cursor of its own, the mouse
        // vanished—.
        let closed: Vec<[f32; 4]> = sheets
            .iter()
            .filter(|l| l.view.popup.is_none())
            .filter(|l| scene.surfaces.get(l.view.surface).is_some_and(|s| s.open.as_ref().is_some_and(|e| !e.is_true(c))))
            .map(|l| l.view.bounds())
            .chain(scene.surfaces.iter().filter(|s| s.open.as_ref().is_some_and(|e| !e.is_true(c))).map(|s| [s.origin.0, s.origin.1, s.origin.0 + s.width.max(1) as f32, s.origin.1 + s.height.max(1) as f32]))
            .collect();
        // From the zones as they are now: the rules may have moved some since the mouse looked.
        zone_memo.update(&scene, &facts, &props, &draw.hidden, &absent, check_shortcuts);
        let boxes: Vec<[i32; 4]> = zone_memo
            .bounds
            .iter()
            .flatten()
            .copied()
            .filter(|b| !closed.iter().any(|v| b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]))
            .map(|b| [(b[0] - 3.0).floor() as i32, (b[1] - 3.0).floor() as i32, (b[2] + 3.0).ceil() as i32, (b[3] + 3.0).ceil() as i32])
            .collect();
        // (A surface that has just presented its first frame gets its region now.)
        let region_changes = boxes != region || std::mem::take(&mut regions_again);
        // `PLEAMAR_REGIONS=card`: and the zones whose name has that in it, with
        // where they are and whether a closed surface is hiding them.
        if region_changes {
            if let Some(which) = std::env::var_os("PLEAMAR_REGIONS").filter(|w| w.len() > 1) {
                let which = which.to_string_lossy();
                for z in scene.zones.iter().filter(|z| z.id.contains(which.as_ref())) {
                    let b = z.bounds(c);
                    let hidden = b.is_some_and(|b| closed.iter().any(|v| b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]));
                    eprintln!("regions · zone {} · active {} · {:?} · under a closed surface {hidden}", z.id, z.active.is_true(c), b);
                    if let Some(b) = b.filter(|_| hidden) {
                        for v in closed.iter().filter(|v| b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]) {
                            let who: Vec<String> = sheets.iter().filter(|l| l.view.bounds() == *v).map(|l| format!("sheet of surface {}", l.view.surface)).chain(scene.surfaces.iter().enumerate().filter(|(_, s)| s.origin.0 == v[0] && s.origin.1 == v[1]).map(|(k, s)| format!("surface {k} '{}' #{}", s.name, s.instance))).collect();
                            eprintln!("regions ·   hidden by {v:?}: {who:?}");
                        }
                    }
                }
            }
        }
        if region_changes {
            // Each surface gets the zones that fall on ITS piece of the plane, in its
            // coordinates. A popup is all its own, and carries no region.
            for l in sheets.iter_mut().filter(|l| l.view.popup.is_none()) {
                let v = l.view.bounds();
                let (dx, dy) = (l.view.origin.0 as i32, l.view.origin.1 as i32);
                let its_own: Vec<[i32; 4]> = boxes
                    .iter()
                    .filter(|b| (b[0] as f32) < v[2] && (b[2] as f32) > v[0] && (b[1] as f32) < v[3] && (b[3] as f32) > v[1])
                    .map(|b| [b[0] - dx, b[1] - dy, b[2] - dx, b[3] - dy])
                    .collect();
                // `PLEAMAR_REGIONS=1`: where each surface takes the mouse, when it
                // changes. What answers «why does this not click» —or «why does
                // everything click here»—.
                // Only if it changed for THIS surface, and then with a frame: the
                // region is committed with the next frame presented, and a
                // surface that draws nothing —a full-screen catcher— may never
                // present another, leaving its new region pending for ever.
                if l.input_region != its_own && l.presented {
                    if std::env::var_os("PLEAMAR_REGIONS").is_some() {
                        eprintln!("regions · surface {} ({}): {:?}", l.view.surface, scene.surfaces.get(l.view.surface).map_or("", |s| s.name.as_str()), its_own);
                    }
                    l.update_input_region(&its_own);
                    l.input_region = its_own;
                    l.painted = None;
                }
            }
            region = boxes;
        }
        // And behind the glass, what the compositor blurs: the strips that
        // fall on each surface's piece, in its coordinates. Only when they change.
        for l in sheets.iter_mut() {
            let v = l.view.bounds();
            let local = |b: &[f32; 4]| [(b[0].max(v[0]) - v[0]) as i32, (b[1].max(v[1]) - v[1]) as i32, (b[2].min(v[2]) - v[0]).ceil() as i32, (b[3].min(v[3]) - v[1]).ceil() as i32];
            // The ones of the glass that bends the light (`lens`), and those of the one that does not.
            // `PLEAMAR_NO_LENS=1`: none bends it, and the compositor blurs it.
            let (mut with_lens, mut without) = (Vec::new(), Vec::new());
            // Closed, it shows no glass: nothing behind it is blurred.
            // (A shader that reads what is behind is captured too, but never blurred instead.)
            let mut read = Vec::new();
            for (b, how) in draw.glass_regions.iter().filter(|_| l.open).filter(|(b, _)| b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]) {
                match how {
                    crate::gpu::Behind::Lens if !no_lens => with_lens.push(local(b)),
                    crate::gpu::Behind::Read => read.push(local(b)),
                    _ => without.push(local(b)),
                }
            }
            l.wants_lens = !with_lens.is_empty() || !read.is_empty();
            // Only what is needed is captured: the glass's box, with a
            // margin for frosting, rounded to 16 px so as not to remake
            // textures over one pixel. In Marea, the little ball and not the 820 × 680.
            l.glass_box = with_lens.iter().chain(&read).copied().reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]).map(|b| {
                let m = crate::lens::margin().ceil() as i32;
                let (w, h) = (l.view.size.0 as i32, l.view.size.1 as i32);
                let x0 = ((b[0] - m).max(0) / 16) * 16;
                let y0 = ((b[1] - m).max(0) / 16) * 16;
                let x1 = ((b[2] + m + 15) / 16 * 16).min(w);
                let y1 = ((b[3] + m + 15) / 16 * 16).min(h);
                [x0, y0, x1 - x0, y1 - y0]
            });
            // What the compositor blurs: the glass without a lens, and the one that has
            // it while there is still no background to show.
            // A lens that bends a picture (`lens = image`) still lets some of what is really behind through:
            // that is what the compositor blurs.
            if !l.lens.as_ref().is_some_and(|x| x.ready && x.fixed.is_none()) {
                without.extend(with_lens);
            }
            if without != l.blur_rects {
                l.update_blur_region(&without);
                l.blur_rects = without;
            }
        }

        // The 4 and the 7 are the view's origin: zero on the main one; each popup sets its own.
        uniforms[..8].copy_from_slice(&[size.0, size.1, t_total, 1.0, 0.0, period_ms, if blocked { 1.0 } else { 0.0 }, 0.0]);
        uniforms[8..128].copy_from_slice(&history);
        // While pressed, the light follows the finger; on release, it goes out in ~0.4 s.
        if finger_down {
            if let Some(p) = pointer {
                finger = p;
            }
        } else {
            finger_light *= (-dt / 0.4).exp();
        }
        let light_alive = finger_light > 0.01;
        if !light_alive {
            finger_light = 0.0;
        }
        uniforms[129..132].copy_from_slice(&[finger.0, finger.1, finger_light]);
        // It lasts 0.9 s, and while it lasts there are frames.
        if ripple.is_some_and(|(_, t)| t.elapsed().as_secs_f32() > 0.9) {
            ripple = None;
        }
        uniforms[132..136].copy_from_slice(&match ripple {
            Some(((x, y), t)) => [x, y, t.elapsed().as_secs_f32(), 1.0],
            None => [0.0; 4],
        });
        // The one that sets the pace goes last: it is the one that waits for the screen.
        sheets.sort_by_key(|l| l.drives_pace);
        // Has any surface decided to attach itself to another edge? The corner that
        // Marea's recording face chooses is a fact, and layer-shell lets it be
        // changed without creating anything again.
        // A monitor that came back has its layers new, with the scene's defaults:
        // what was changed is sent to them again.
        let placed_now = crate::platform::layers_placed();
        if placed_now != layers_seen {
            layers_seen = placed_now;
            anchors_set.clear();
            levels_set.clear();
            reserves_set.clear();
        }
        if anchors_set.len() != scene.surfaces.len() {
            anchors_set = scene.surfaces.iter().map(|s| s.anchor).collect();
        }
        // And a level of its own while something holds: above the rest while
        // it has something open, where it belongs the rest of the time.
        if levels_set.len() != scene.surfaces.len() {
            levels_set = scene.surfaces.iter().map(|s| s.level).collect();
        }
        for (k, sup) in scene.surfaces.iter().enumerate() {
            let Some((raised, when)) = &sup.level_while else { continue };
            let wants = if when.is_true(Ctx { props: &props, facts: &facts, seen: None }) { *raised } else { sup.level };
            if levels_set[k] != wants {
                levels_set[k] = wants;
                crate::platform::relayer(k, wants);
            }
        }
        // The first lower rate whose condition holds, in the order they were
        // written: `rate: 60, 20 while asleep, 5 while away` goes from the busiest
        // down, so the one that rules is the first that is true.
        // Each surface has its own pace (`rate:`), and the scene's step is that of
        // its fastest open one: 0 —the monitor's— if one of them says none.
        let paces: Vec<u32> = sheets
            .iter()
            .filter(|l| l.open && l.view.popup.is_none())
            .filter_map(|l| scene.surfaces.get(l.view.surface))
            .map(|s| surface_rate(s, Ctx { props: &props, facts: &facts, seen: None }))
            .collect();
        rate_now = if paces.is_empty() {
            surface_rate(scene.surface(), Ctx { props: &props, facts: &facts, seen: None })
        } else if paces.contains(&0) {
            0
        } else {
            paces.iter().copied().max().unwrap_or(0)
        };
        // And the room it keeps, while something holds (`reserve: n while …`).
        if reserves_set.len() != scene.surfaces.len() {
            reserves_set = scene.surfaces.iter().map(|s| if s.reserve_while.is_some() { -1 } else { s.exclusive_zone }).collect();
        }
        for (k, sup) in scene.surfaces.iter().enumerate() {
            let Some(when) = &sup.reserve_while else { continue };
            // A copy that is closed (`open:`) keeps none: Marea lives on one
            // monitor, and the other's copy left an empty strip there.
            let c = Ctx { props: &props, facts: &facts, seen: None };
            let open_here = sup.open.as_ref().is_none_or(|e| e.is_true(c));
            let wants = if when.is_true(c) && open_here { sup.exclusive_zone } else { 0 };
            if reserves_set[k] != wants {
                reserves_set[k] = wants;
                crate::platform::rezone(k, wants);
            }
        }
        for (k, sup) in scene.surfaces.iter().enumerate() {
            let Some((fact, anchors)) = &sup.anchor_from else { continue };
            let wants = anchors.get(facts[fact.0 as usize].round().max(0.0) as usize).copied();
            if let Some(a) = wants.filter(|a| anchors_set[k] != *a) {
                anchors_set[k] = a;
                crate::platform::reanchor(k, a);
            }
        }

        // Which ones are open now. If that changes, the pace is handed out again: a
        // closed surface that set it would wait with vsync for a frame that the
        // compositor is not going to give it —what is not seen is not given any—, and with it
        // everything stopped: 300 ms in the middle of an animation of another window.
        let mut open_changed = false;
        for l in &mut sheets {
            let is_open = l.view.popup.is_some() || open(l.view.surface);
            open_changed |= is_open != l.open;
            if is_open {
                l.cleared = false;
            } else if l.lens.is_some() {
                // Closed it shows no glass: its canvas and its background, out.
                l.cancel_backdrop();
                l.lens = None;
            }
            l.open = is_open;
        }
        // A surface the compositor shows again (it answered a frame) may set
        // the pace again.
        for id in seen_again.drain(..) {
            if let Some(l) = sheets.iter_mut().find(|l| l.id == id && l.unseen) {
                l.unseen = false;
                open_changed = true;
            }
        }
        if open_changed {
            assign_pace(g, &mut sheets, size, op.no_vsync);
            sheets.sort_by_key(|l| l.drives_pace);
        }
        if profiling {
            let n = Instant::now();
            { let d = (n - prof_t).as_secs_f64() * 1000.0; prof[4] += d; sec[4] += d; }
            prof_t = n;
            let c = thread_cpu_ms();
            prof_cpu[4] += c - prof_c;
            prof_c = c;
        }
        // The step. With mailbox the render sets it: an absolute deadline per period of the
        // monitor that sets the pace, so that the error of each wait does not
        // accumulate; if it arrives late —a rest, a long frame— it starts again
        // from now instead of rushing to catch up. With queued vsync the screen
        // sets it, but only when its queue is full: on waking up it
        // accepted two or three frames without waiting and the loop presented them in two
        // milliseconds; there, never more than one frame per period.
        if !op.no_vsync && !op.naive {
            let right_now = Instant::now();
            if g.uses_mailbox() {
                let mhz = sheets.iter().find(|l| l.drives_pace).map_or(60_000, |l| l.pace_mhz());
                // With `rate:`, the scene decides the step and not the monitor: a bar
                // that breathes does not need 165 frames per second, and painting them is what
                // costs. Never faster than the screen, which would be no use.
                let mhz = match rate_now {
                    0 => mhz,
                    r => mhz.min(r as i32 * 1000),
                };
                let period = Duration::from_secs_f64(1000.0 / mhz as f64);
                // The clock gives the step: one deadline per period. The compositor's
                // notice is no use as a metronome —Hyprland notifies when it composes,
                // and if a frame reaches it right after, it composes again and notifies
                // straight away: two frames per refresh—, but it does say something else: whether
                // anyone is watching.
                if next_frame > right_now {
                    // While waiting, the capture that a sheet with a lens
                    // needed in order to paint may arrive: then it is painted right away, without waiting
                    // for the clock, or its step and the compositor's drift out of phase and
                    // one out of every two gets painted.
                    let awaits_capture = sheets.iter().any(|l| l.capture == crate::gpu::BackdropCapture::AfterPresent);
                    if awaits_capture {
                        loop {
                            let left = next_frame.saturating_duration_since(Instant::now());
                            if left.is_zero() {
                                next_frame += period;
                                break;
                            }
                            match rx.recv_timeout(left) {
                                Ok(ToRender::Backdrop(d)) => {
                                    if handle_backdrop(g, &mut sheets, d) {
                                        next_frame = Instant::now() + period;
                                        break;
                                    }
                                }
                                // The frame notice is waited for right after: kept for the
                                // next round, that wait would not see it and would run out.
                                Ok(ToRender::Frame(k)) => {
                                    frame_ready |= frame_requested == Some(k);
                                    if !seen_again.contains(&k) {
                                        seen_again.push(k);
                                    }
                                    in_flight.retain(|f| f.0 != k);
                                }
                                Ok(m) => held_back.push(m),
                                Err(RecvTimeoutError::Timeout) => {}
                                Err(RecvTimeoutError::Disconnected) => return,
                            }
                        }
                    } else {
                        std::thread::sleep(next_frame - right_now);
                        next_frame += period;
                    }
                } else {
                    next_frame = right_now + period;
                }
                // What it is not showing —a monitor that is off, a window on another
                // workspace— it does not notify. After three waits in vain, it paints once
                // a second instead of 60 or 165 times for nobody.
                if let Some(id) = frame_requested {
                    let patience = if missed_notices >= 3 { Duration::from_secs(1) } else { (period * 3).max(Duration::from_millis(50)) };
                    let limit = Instant::now() + patience;
                    while !frame_ready {
                        let left = limit.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            missed_notices += 1;
                            if missed_notices == 3 {
                                // Another open surface still shown takes the pace;
                                // only with none is the scene painted once a second.
                                if let Some(l) = sheets.iter_mut().find(|l| l.id == id) {
                                    l.unseen = true;
                                }
                                assign_pace(g, &mut sheets, size, op.no_vsync);
                                sheets.sort_by_key(|l| l.drives_pace);
                                // (Only to one that is shown: with none, the one at a
                                // second, not each in turn.)
                                if sheets.iter().any(|l| l.view.popup.is_none() && l.open && !l.unseen) {
                                    println!("render · the compositor is not showing a surface: another one sets the pace until it shows it again");
                                    missed_notices = 0;
                                } else {
                                    println!("render · the compositor is not showing this surface: painting once a second until it shows it again");
                                }
                            }
                            break;
                        }
                        match rx.recv_timeout(left) {
                            Ok(ToRender::Frame(k)) => {
                                frame_ready |= k == id;
                                if !seen_again.contains(&k) {
                                    seen_again.push(k);
                                }
                                in_flight.retain(|f| f.0 != k);
                            }
                            Ok(m) => held_back.push(m),
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    if frame_ready {
                        if missed_notices >= 3 {
                            println!("render · the compositor is showing it again");
                        }
                        missed_notices = 0;
                    }
                    if Instant::now() > next_frame {
                        next_frame = Instant::now() + period;
                    }
                }
            } else {
                let minimum = Duration::from_secs_f32(period_ms * 0.8 / 1000.0);
                let since = right_now - last_presented;
                if since < minimum {
                    std::thread::sleep(minimum - since);
                }
            }
        }
        if profiling {
            let n = Instant::now();
            sec_wait = (n - prof_t).as_secs_f64() * 1000.0;
            prof_t = n;
            let c = thread_cpu_ms();
            prof_outside[1] += c - prof_c;
            prof_c = c;
        }
        let before_painting = last_presented;
        last_presented = Instant::now();
        let mut painted = 0;
        let mut up_to_date = 0;
        let mut waiting_for_screen = false;
        // If the one that sets the pace does not paint this time, there is no notice to wait for:
        // the clock sets the next round.
        frame_requested = None;
        frame_ready = false;
        // Painting only where something changed needs to know where: not with
        // particles, a shader that reads the time or an image that moves.
        let knows_where = !all_changed && !draw.timed && !draw.particles_alive && draw.particle_marks.is_empty() && draw.wake_at.is_none();
        // A surface found not shown while painting: the pace is given again after.
        let mut pace_again = false;
        for l in &mut sheets {
            // Closed, it is painted once, empty, and that is it: that, yes, always, whether something changes or not.
            if !l.open && std::mem::replace(&mut l.cleared, true) {
                continue;
            }
            // And what the compositor blurs behind it goes with that empty
            // frame: a region takes effect with the next frame presented, and
            // after this one there is none (a pill faded out left its blurred
            // patch on the screen until it opened again, #39).
            if !l.open && !l.blur_rects.is_empty() {
                l.update_blur_region(&[]);
                l.blur_rects.clear();
            }
            // What it shows is already up to date: nothing of what has changed falls on its piece of the plane.
            let where_ = (l.view.bounds(), l.scale);
            let v = where_.0;
            // A lens still warming up (see `lens::WARM_UP`) whose capture has been
            // lost: the compositor answers a capture the next time it paints the
            // monitor, and with nothing moving there it does not paint. Presenting
            // again is what makes it paint. Without this, a glass or a shader
            // that stays still from its first frame never saw its background.
            // The same for a capture it owed and asked without presenting (`capture_forced`).
            let unseen = (l.capture_forced || l.lens.as_ref().is_some_and(|x| x.warming())) && l.capture == crate::gpu::BackdropCapture::AfterPresent;
            if unseen && l.capture_asked.elapsed() < Duration::from_millis(100) {
                appointments.push(l.capture_asked + Duration::from_millis(101));
            }
            let nudge = unseen && l.capture_asked.elapsed() >= Duration::from_millis(100);
            let its_turn = !l.open || all_changed || nudge || l.painted != Some(where_) || !l.owed_damage.is_empty() || changed.iter().any(|b| b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]);
            // A round it skips: what changed in it is owed, if it is known
            // where; if not, it is painted whole when its turn comes.
            let skip = |l: &mut Sheet| {
                if knows_where && l.painted == Some(where_) {
                    owe(l, changed.iter().copied().filter(|b| b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]));
                } else {
                    l.painted = None;
                }
            };
            if !its_turn {
                up_to_date += 1;
                continue;
            }
            // Its own `rate:`, slower than the scene's step (another surface goes
            // faster): it skips the rounds in between —what changed is owed, as in
            // any skipped round— and asks to be woken when its turn comes.
            if l.open && l.view.popup.is_none() {
                let r = scene.surfaces.get(l.view.surface).map_or(0, |s| surface_rate(s, Ctx { props: &props, facts: &facts, seen: None }));
                if r > 0 && (rate_now == 0 || r < rate_now) {
                    let period = Duration::from_secs_f64(1.0 / r as f64);
                    if let Some(last) = l.rate_last.filter(|t| t.elapsed() + Duration::from_millis(2) < period) {
                        skip(l);
                        appointments.push(last + period);
                        up_to_date += 1;
                        continue;
                    }
                }
            }
            // With a lens: if the capture of the last presented frame has not arrived yet, it
            // waits for it —it arrives in less than a refresh— and paints afterwards.
            // If it was only watching, that capture is forgotten and it paints.
            if l.lens.is_some() {
                match l.capture {
                    // It arrives in less than a refresh. If it takes longer, it has been lost.
                    crate::gpu::BackdropCapture::AfterPresent if l.capture_asked.elapsed() < Duration::from_millis(100) => {
                        skip(l);
                        up_to_date += 1;
                        continue;
                    }
                    crate::gpu::BackdropCapture::AfterPresent => l.cancel_backdrop(),
                    crate::gpu::BackdropCapture::Watching => l.cancel_backdrop(),
                    crate::gpu::BackdropCapture::Idle => {}
                }
            }
            // Waiting for its screen (FIFO), it is handed a frame only once the
            // compositor has answered the last one: the driver holds the
            // present until then, and one that never answers —an output not
            // shown, a headless one— held the whole render there, painting
            // nothing and answering no `--say` (issue #69). It waits for that
            // answer as long as a present would, a few refreshes; past that,
            // the round goes on without it, and after three in a row it is
            // taken as not shown (`unseen`): another surface sets the pace, or
            // with none the clock does. Unseen, it does not wait at all: the
            // answer that shows it again makes it seen.
            let fifo = g.blocks_on_screen(l) && !op.no_vsync && !op.naive;
            if fifo && l.open && in_flight.iter().any(|f| f.0 == l.id) {
                if !l.unseen {
                    let period = Duration::from_secs_f64(1000.0 / l.pace_mhz() as f64);
                    let limit = Instant::now() + (period * 3).max(Duration::from_millis(50));
                    while in_flight.iter().any(|f| f.0 == l.id) {
                        let left = limit.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            break;
                        }
                        match rx.recv_timeout(left) {
                            Ok(ToRender::Frame(k)) => {
                                frame_ready |= frame_requested == Some(k);
                                if !seen_again.contains(&k) {
                                    seen_again.push(k);
                                }
                                in_flight.retain(|f| f.0 != k);
                            }
                            Ok(m) => held_back.push(m),
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                }
                if in_flight.iter().any(|f| f.0 == l.id) {
                    if !l.unseen {
                        l.missed += 1;
                        if l.missed >= 3 {
                            l.unseen = true;
                            pace_again = true;
                            println!("render · the compositor is not showing surface {}: it is not handed frames until it shows it again", l.id);
                        }
                    }
                    skip(l);
                    up_to_date += 1;
                    continue;
                }
            }
            if fifo {
                l.missed = 0;
            }
            // The one that sets the pace asks the compositor to notify when it wants another.
            let asks = l.drives_pace && g.uses_mailbox() && !op.no_vsync && !op.naive;
            // The others ask too, and while their last frame has not been
            // shown they skip the round instead of painting: on a slower
            // monitor, painting again waited until the compositor let go of a
            // buffer, and that held the whole render to that monitor's pace —a
            // scene on a 165 Hz monitor and a 60 Hz one went at 60 on both—.
            // Not beyond a while: what is not shown is never answered.
            let others = !l.drives_pace && g.uses_mailbox() && !op.no_vsync && !op.naive;
            if others && l.open && in_flight.iter().any(|f| f.0 == l.id && f.1.elapsed() < Duration::from_millis(100)) {
                if !owed.contains(&l.id) {
                    owed.push(l.id);
                }
                // What changed meanwhile is not lost: it is painted when its turn comes.
                skip(l);
                up_to_date += 1;
                continue;
            }
            owed.retain(|k| *k != l.id);
            // And what it owes.
            let with_owed: Option<Vec<[f32; 4]>> = (!l.owed_damage.is_empty()).then(|| changed.iter().chain(&l.owed_damage).copied().collect());
            l.owed_damage.clear();
            let was_painted = g.paint(l, &draw, &uniforms, asks || others || fifo, (knows_where && l.painted == Some(where_)).then(|| with_owed.as_deref().unwrap_or(&changed[..])));
            if was_painted {
                l.rate_last = Some(Instant::now());
            }
            if was_painted && ((others && answers.contains(&l.id)) || (fifo && l.open)) {
                in_flight.retain(|f| f.0 != l.id);
                in_flight.push((l.id, Instant::now()));
            }
            // With a lens, what was presented is captured to keep track of what is behind.
            // Not on every frame: for Hyprland each capture costs reading the screen
            // back from the card, and at 60 per second that was 14 points of a core.
            // One every `CAPTURE_INTERVAL` (or the scene's `lens every`) at most,
            // counting the watching ones too; and when it stops painting, it
            // watches (see further down). What it paints without capturing is owed.
            if was_painted && l.lens.is_some() {
                if pace(l).after_paint(nudge) {
                    l.request_backdrop(false);
                    l.capture_owed = false;
                } else {
                    l.capture_owed = true;
                }
            }
            l.painted_now = was_painted;
            if was_painted && !l.presented {
                l.presented = true;
                regions_again = true;
            }
            if asks {
                frame_requested = was_painted.then_some(l.id);
                frame_ready = false;
            }
            // Cleared, it does not show the scene's things: on reopening, it is painted no matter what.
            l.painted = (was_painted && l.open).then_some(where_);
            waiting_for_screen |= was_painted && l.drives_pace && !g.uses_mailbox();
            painted += was_painted as u32;
            if timing && was_painted {
                if prof_sheets.len() <= l.view.surface {
                    prof_sheets.resize(l.view.surface + 1, 0);
                }
                prof_sheets[l.view.surface] += 1;
            }
        }
        if pace_again {
            assign_pace(g, &mut sheets, size, op.no_vsync);
            sheets.sort_by_key(|l| l.drives_pace);
        }
        // A sheet with a lens that has not painted this round has stayed
        // still: what is behind is watched. Watching while painting is no use,
        // because our own frame already counts as a change and the capture would arrive
        // on every refresh; now it arrives only if something changes behind, and it brings the
        // last thing that was presented.
        // Watching keeps to the interval as well: a capture that arrives
        // watching counts as one, and the next waits out the interval (an
        // appointment wakes the render for it). Without that, a paint and a
        // watch each cycle made two captures per interval (issue 106).
        for l in &mut sheets {
            if l.lens.is_none() || std::mem::take(&mut l.painted_now) {
                continue;
            }
            let pace = pace(l);
            // Held: not even watching; and once it is let go, one capture straight away.
            if pace.held && !pace.warming {
                if l.capture == crate::gpu::BackdropCapture::Watching {
                    l.cancel_backdrop();
                }
                l.capture_owed = true;
                continue;
            }
            if l.capture != crate::gpu::BackdropCapture::Idle {
                continue;
            }
            match pace.still(l.capture_owed) {
                crate::lens::Still::Nothing => {}
                crate::lens::Still::Wait(left) => appointments.push(Instant::now() + left),
                // What changed behind while it could not capture is not
                // waited for: it may already have happened.
                crate::lens::Still::Capture => {
                    l.request_backdrop(false);
                    if l.capture == crate::gpu::BackdropCapture::AfterPresent {
                        l.capture_owed = false;
                        l.capture_forced = true;
                    }
                }
                crate::lens::Still::Watch => l.request_backdrop(true),
            }
        }
        // Nothing was painted —what changed is not seen on any sheet—: what was
        // written for it is submitted on its own. Without this it piled up, and a
        // scene whose logic moves something hidden every frame —a game put away
        // into its corner— grew by a gigabyte an hour.
        if wrote && painted == 0 {
            g.flush();
        }
        // With queued vsync, the step was given by the wait of the one that sets the pace.
        // If it was not its turn to paint this time, the clock gives the step: a whole period.
        if painted + up_to_date > 0 && !waiting_for_screen && !g.uses_mailbox() && !op.no_vsync && !op.naive {
            let period = Duration::from_secs_f32(period_ms / 1000.0);
            let since = before_painting.elapsed();
            if since < period {
                std::thread::sleep(period - since);
                sec_paced = (period - since).as_secs_f64() * 1000.0;
            }
            last_presented = Instant::now();
        }
        if crate::gpu::timing_enabled() && painted + up_to_date > 0 {
            sheet_counts.0 += painted;
            sheet_counts.1 += up_to_date;
            sheet_counts.2 += 1;
            if sheet_counts.2 >= 300 {
                // Which ones: a surface painted every round while nothing seems to move is what to look at.
                let which: Vec<String> = prof_sheets.iter().enumerate().filter(|(_, n)| **n > 0).map(|(k, n)| format!("{} {n}", scene.surfaces.get(k).map_or("?", |s| if s.name.is_empty() { "the scene's own" } else { s.name.as_str() }))).collect();
                println!("timing · surfaces per frame: {:.2} painted, {:.2} up to date{}", sheet_counts.0 as f32 / 300.0, sheet_counts.1 as f32 / 300.0, if which.is_empty() { String::new() } else { format!(" · painted, of 300 rounds: {}", which.join(", ")) });
                prof_sheets.clear();
                sheet_counts = (0, 0, 0);
                // What is kept for the compositor's windows: over a long
                // session, none of these should only grow.
                if nest.is_some() {
                    let pieces: usize = nest_windows.iter().map(|w| w.pieces.len()).sum();
                    let pixels: usize = nest_windows.iter().flat_map(|w| &w.pieces).map(|p| p.pixels.capacity()).sum();
                    println!(
                        "timing · kept: {} window slots, {pieces} pieces, {:.1} MB of their pixels, {} buffers to drop · {}",
                        nest_windows.len(),
                        pixels as f64 / 1e6,
                        {
                            #[cfg(target_os = "linux")]
                            let doomed = nest_doomed.len();
                            #[cfg(not(target_os = "linux"))]
                            let doomed = 0;
                            doomed
                        },
                        gpu.as_ref().map_or(String::new(), |g| g.kept())
                    );
                }
            }
        }
        if profiling {
            let n = Instant::now();
            { let d = (n - prof_t).as_secs_f64() * 1000.0; prof[5] += d; sec[5] += d; }
            prof_t = n;
            let c = thread_cpu_ms();
            prof_cpu[5] += c - prof_c;
            prof_c = c;
        }
        prof_painted += painted;
        if let (true, Some(g)) = (std::mem::take(&mut nest_copied.1), &gpu) {
            // The copies went with the painting; if nothing was painted, on their own.
            g.flush_copies();
            let done = std::mem::take(&mut nest_copied.0);
            if let (Some(index), false) = (g.last_submission(), done.is_empty()) {
                nest_lent.push((index, done));
            }
        }
        // The windows being shared that drew something: read back, now that
        // their layers have it, and sent.
        if !nest_watch_due.is_empty() {
            if let Some(g) = &gpu {
                nest_watch_due.sort_unstable();
                nest_watch_due.dedup();
                for slot in std::mem::take(&mut nest_watch_due) {
                    let Some(w) = nest_windows.get(slot) else { continue };
                    // Its pixels per unit, from its main surface; its box, the window
                    // itself (without the shadow around it), or the whole surface.
                    let Some(main) = w.pieces.iter().find(|p| p.size.0 > 0) else { continue };
                    let s = main.px.0 as f32 / main.size.0 as f32;
                    let g_box = if w.geometry[2] > 0 && w.geometry[3] > 0 { w.geometry } else { [0, 0, main.size.0 as i32, main.size.1 as i32] };
                    let size = (((g_box[2] as f32) * s).round() as u32, ((g_box[3] as f32) * s).round() as u32);
                    // Each piece the part of its pixels it shows (`src`), stretched to
                    // its size at this density: a piece drawn at another scale than
                    // the main surface (a browser's page) fills its place.
                    let pieces: Vec<(u32, (i32, i32), [u32; 4], (u32, u32), bool)> = w
                        .pieces
                        .iter()
                        .map(|p| {
                            let src = [p.src[0].max(0.0) as u32, p.src[1].max(0.0) as u32, p.src[2].round().max(0.0) as u32, p.src[3].round().max(0.0) as u32];
                            let to = ((p.size.0 as f32 * s).round() as u32, (p.size.1 as f32 * s).round() as u32);
                            (p.layer, (((p.at.0 - g_box[0]) as f32 * s).round() as i32, ((p.at.1 - g_box[1]) as f32 * s).round() as i32), src, to, p.opaque)
                        })
                        .collect();
                    if let Some(pixels) = g.read_window(&pieces, size) {
                        // Each one watching it gets it; whoever stopped listening, out.
                        let pixels = std::sync::Arc::new(pixels);
                        nest_watch.retain(|(k, _, to)| *k != slot || to.send(crate::scene::WindowPicture { size, pixels: pixels.clone() }).is_ok());
                    }
                }
            }
        }
        // What the windows drew has been shown: they may draw the next one.
        if painted > 0 {
            if let Some(send) = &nest {
                send(ToNest::FrameDone);
            }
        }
        if painted + up_to_date == 0 {
            // Nothing could be painted —the screen has not handed a buffer back
            // yet—: try again soon. Eight milliseconds were more than a whole
            // frame at 165 Hz, and every one of these naps lost one.
            std::thread::sleep(Duration::from_secs_f32((period_ms / 4.0).clamp(1.0, 8.0) / 1000.0));
        } else if painted > 0 && first_frame {
            first_frame = false;
            println!("render · first frame {} ms after starting", op.start_time.elapsed().as_millis());
        }

        // What the texts measured goes into their properties: the coming frame,
        // the box that depends on them already knows it.
        let mut measure_changed = false;
        for (p, v) in &draw.measurements {
            let a = &mut props[p.0 as usize];
            if (a.x - v).abs() > 0.01 {
                a.set(*v);
                measure_changed = true;
            }
        }

        // ── 4. measure ──────────────────────────────────────────
        //  What was asked to be recorded, frame by frame: it is how you check that an
        //  animation lasts as long as its contract says it lasts.
        if !op.trace.is_empty() {
            if !trace_header_done {
                trace_header_done = true;
                // A name that does not exist was recorded as "?" for the whole
                // measurement, and there was no way of knowing whether it was misspelled or
                // the scene just did not move it. Now it is said, once, with the
                // ones that resemble it: inside a copy the names carry their
                // mark (`px#Hat1`), and nobody could guess that.
                for n in &op.trace {
                    let n = n.as_str();
                    let exists = scene.props.iter().any(|p| p.0 == n) || scene.facts.iter().any(|h| h.0 == n) || scene.texts.iter().any(|t| t.0 == n);
                    if !exists {
                        let mut close: Vec<&str> = scene
                            .props
                            .iter()
                            .map(|p| p.0)
                            .chain(scene.facts.iter().map(|h| h.0))
                            .chain(scene.texts.iter().map(|t| t.0))
                            .filter(|c| c.contains(n) || n.contains(*c) || c.split('#').next() == Some(n))
                            .collect();
                        close.sort_unstable();
                        close.dedup();
                        close.truncate(6);
                        let hint = if close.is_empty() { String::new() } else { format!(" · there is {}", close.join(", ")) };
                        println!("record · there is nothing called '{n}'{hint}");
                    }
                }
                println!("ms\t{}", op.trace.join("\t"));
            }
            let values: Vec<String> = op
                .trace
                .iter()
                .map(|n| {
                    let n = n.as_str();
                    if let Some(i) = scene.props.iter().position(|p| p.0 == n) {
                        format!("{:.4}", props[i].x)
                    } else if let Some(i) = scene.facts.iter().position(|h| h.0 == n) {
                        format!("{:.4}", facts[i])
                    } else if let Some(i) = scene.texts.iter().position(|t| t.0 == n) {
                        texts.get(i).cloned().unwrap_or_default()
                    } else {
                        "?".into()
                    }
                })
                .collect();
            println!("{:.1}\t{}", t_total * 1000.0, values.join("\t"));
        }
        if profiling {
            let n = Instant::now();
            { let d = (n - prof_t).as_secs_f64() * 1000.0; prof[6] += d; sec[6] += d; }
            prof_t = n;
            let c = thread_cpu_ms();
            prof_cpu[6] += c - prof_c;
            prof_c = c;
        }
        if timing {
            prof_frames += 1;
            // Why this round does not end in rest: each reason, counted.
            let moving = props.iter().position(|a| !a.at_rest());
            for (k, yes) in [alive, something_happened, finger_light != 0.0 || ripple.is_some(), blocked, region_changes, measure_changed, keyboard_changed, moving.is_some()].into_iter().enumerate() {
                prof_awake[k] += yes as u32;
            }
            if let Some(k) = moving {
                prof_moving = Some(k);
            }
            for w in alive_by.drain(..) {
                if prof_alive_by.len() < 8 && !prof_alive_by.contains(&w) {
                    prof_alive_by.push(w);
                }
            }
            if prof_frames >= 300 {
                let f = prof_frames as f64;
                let why: Vec<String> = ["something alive", "something happened", "a light", "the logic busy", "regions", "a measure", "the keyboard", "a property moving"].iter().zip(prof_awake).filter(|(_, n)| *n > 0).map(|(w, n)| format!("{w} {n}")).collect();
                if !why.is_empty() {
                    println!(
                        "timing · awake, of 300 rounds: {}{}{}",
                        why.join(" · "),
                        prof_moving.take().map_or(String::new(), |k| format!(" ({})", scene.props[k].0)),
                        if prof_alive_by.is_empty() { String::new() } else { format!(" · alive: {}", std::mem::take(&mut prof_alive_by).join(", ")) }
                    );
                }
                prof_awake = [0; 8];
                println!(
                    "timing · {:.0} rounds, {:.0} windows' frames and {:.0} paintings a second · per round: input {:.2} · rules and springs {:.2} · windows' frames to the card {:.2} · compose {:.2} · regions {:.2} · paint {:.2} · rest {:.2} ms",
                    f / prof_since.elapsed().as_secs_f64(), prof_window_frames as f64 / prof_since.elapsed().as_secs_f64(), prof_painted as f64 / prof_since.elapsed().as_secs_f64(), prof[0] / f, (prof[1] + prof[2]) / f, prof[7] / f, prof[3] / f, prof[4] / f, prof[5] / f, prof[6] / f
                );
                println!(
                    "timing · CPU per round: input {:.2} · rules and springs {:.2} · windows' frames to the card {:.2} · compose {:.2} · regions {:.2} · paint {:.2} · rest {:.2} ms",
                    prof_cpu[0] / f, (prof_cpu[1] + prof_cpu[2]) / f, prof_cpu[7] / f, prof_cpu[3] / f, prof_cpu[4] / f, prof_cpu[5] / f, prof_cpu[6] / f
                );
                // How much of the drawing was copied from the frame before instead of being put together.
                if stretches_copied + stretches_made > 0 {
                    println!(
                        "timing · stretches of the drawing, in 300 rounds: {stretches_copied} copied from the frame before, {stretches_made} put together ({:.0} % copied)",
                        100.0 * stretches_copied as f64 / (stretches_copied + stretches_made) as f64
                    );
                    (stretches_copied, stretches_made) = (0, 0);
                }
                // What the sections above leave out: a round that looks cheap and is not.
                if prof_outside[0] + prof_outside[1] > 0.3 * f {
                    let arrived: Vec<String> = ["frames shown", "the mouse elsewhere", "the mouse", "facts", "texts", "the logic's orders", "texts shaped", "pictures of what is behind", "windows", "other"].iter().zip(prof_arrived).filter(|(_, n)| *n > 0).map(|(w, n)| format!("{w} {n}")).collect();
                    println!("timing · CPU per round outside those: what has arrived {:.2} · waiting its turn {:.2} ms · arrived, in 300 rounds: {}", prof_outside[0] / f, prof_outside[1] / f, arrived.join(", "));
                }
                // The facts and texts that arrive most: a service that says the same thing
                // a hundred times a second is found by its name.
                let mut most: Vec<(&str, u32)> = prof_named.drain().filter(|(_, n)| *n >= 30).collect();
                most.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
                most.truncate(8);
                if !most.is_empty() {
                    println!("timing · told most, in 300 rounds: {}", most.iter().map(|(w, n)| format!("{w} {n}")).collect::<Vec<_>>().join(", "));
                }
                prof_arrived = [0; 10];
                prof_outside = [0.0; 2];
                prof_since = Instant::now();
                prof_window_frames = 0;
                prof_painted = 0;
                prof = [0.0; 8];
                prof_cpu = [0.0; 8];
                prof_frames = 0;
            }
        }
        let ms = dt * 1000.0;
        // A permanent telltale: any frame that goes over two periods, with its time.
        if ms > period_ms * 2.4 && !first_frame && cycle.dts.len() > 1 && !op.no_vsync && !op.naive {
            if profiling {
                println!(
                    "render · slow frame: {ms:.0} ms at {t_total:.2} s · input {:.1} · rules {:.1} · windows' frames {:.1} · compose {:.1} · regions {:.1} · waiting its turn {sec_wait:.1} · paint {:.1} · rest {:.1} ms",
                    sec[0], sec[1] + sec[2], sec[7], sec[3], sec[4], sec[5], sec[6]
                );
            } else {
                println!("render · slow frame: {ms:.0} ms at {t_total:.2} s");
            }
        }
        if let Some(p) = &mut probe {
            // What the screen gives: the monitor that sets the pace, if it says.
            let pace = sheets.iter().find(|l| l.drives_pace && l.open && l.mhz > 0).map_or(period_ms, |l| 1_000_000.0 / l.mhz as f32);
            let mut s = [0f32; crate::probe::SECTIONS];
            for (k, v) in sec.iter().enumerate() {
                s[k] = *v as f32;
            }
            s[5] = (sec[5] - sec_paced).max(0.0) as f32;
            s[8] = (sec_wait + sec_paced) as f32;
            p.round(!first_frame && !cycle.dts.is_empty() && !op.no_vsync, ms, pace, s, blocked);
        }
        sec = [0.0; 8];
        sec_wait = 0.0;
        sec_paced = 0.0;
        history.copy_within(1.., 0);
        history[119] = if blocked { -ms } else { ms };
        cycle.dts.push(ms);
        if !op.no_vsync && !op.naive && !blocked {
            // The LEARNT period is no good here: a scene that is always
            // late teaches it that the screen gives 28 ms and then it is never
            // late. It is compared with the monitor's real refresh.
            let mut real = sheets.iter().find(|l| l.drives_pace).map_or(16.7, |l| 1_000_000.0 / l.pace_mhz() as f32);
            if rate_now > 0 {
                real = real.max(1000.0 / rate_now as f32);
            }
            cycle.watch(real);
        }
        if blocked {
            cycle.frames_blocked += 1;
            cycle.max_blocked = cycle.max_blocked.max(ms);
        }
        // The screen's period, learnt from the first good frames.
        if frames_for_period < 40 && ms > 3.0 && ms < 40.0 {
            frames_for_period += 1;
            if frames_for_period == 40 {
                let mut o: Vec<f32> = cycle.dts.iter().copied().filter(|m| *m > 3.0 && *m < 40.0).collect();
                o.sort_by(|a, b| a.total_cmp(b));
                if !o.is_empty() {
                    period_ms = o[o.len() / 2];
                }
            }
        }

        // ── 5. is anything still moving? ────────────────────────
        if op.no_vsync && cycle.dts.len() >= 600 {
            cycle.close();
        }
        if !op.no_vsync && !alive && !something_happened && finger_light == 0.0 && ripple.is_none() && !blocked && !region_changes && !measure_changed && !keyboard_changed && props.iter().all(Animated::at_rest) {
            for a in &mut props {
                a.settle();
            }
            cycle.close();
            next_appointment = appointments.iter().min().copied();
            resting = true;
            if let Some(p) = &mut probe {
                p.rest();
            }
            // Still now: if something overflowed, this is what stays watching.
            draw.report_pending();
        }
    }
}

/// How long a frame waits for a logic that plays to say where things are now.
const FRAME_PATIENCE: Duration = Duration::from_millis(3);

/// A key by its plain name, for whoever asks what is held: a letter is the
/// same key with Shift and without it.
fn plain_key(name: &str) -> String {
    let mut letters = name.chars();
    match (letters.next(), letters.next()) {
        (Some(c), None) => c.to_lowercase().collect(),
        _ => name.to_owned(),
    }
}

/// A capture of what is behind a sheet arrives: the background is unmixed with the
/// canvas that was presented. If something has changed, it has to be painted again (and
/// after painting it another will be asked for); if not, it watches until something changes behind.
/// Returns whether it has to paint.
/// What changed in a sheet's piece that it has not painted yet (`Sheet::owed_damage`).
/// Past a few dozen boxes, one round them all.
fn owe(l: &mut Sheet, rects: impl Iterator<Item = [f32; 4]>) {
    l.owed_damage.extend(rects);
    if l.owed_damage.len() > 32 {
        let u = l.owed_damage.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |u, b| [u[0].min(b[0]), u[1].min(b[1]), u[2].max(b[2]), u[3].max(b[3])]);
        l.owed_damage = vec![u];
    }
}

fn handle_backdrop(g: &Gpu, sheets: &mut [Sheet], d: Box<crate::platform::Backdrop>) -> bool {
    let Some(l) = sheets.iter_mut().find(|l| l.id == d.sheet) else { return false };
    // The compositor can write its captures straight on the card: a buffer for the next one.
    #[cfg(target_os = "linux")]
    if let Some((fourcc, w, h)) = d.offer {
        make_card_backdrop(g, d.sheet, fourcc, (w, h));
    }
    // One that was watching counts from when it arrives: it was read back now.
    if l.capture == crate::gpu::BackdropCapture::Watching {
        l.capture_taken = Instant::now();
    }
    l.capture = crate::gpu::BackdropCapture::Idle;
    l.capture_forced = false;
    if l.lens.is_none() {
        return false;
    }
    if g.receive_backdrop(l, *d) {
        // Only the glass sees it, and the glass is inside the box captured.
        let (o, b) = (l.view.origin, l.asked_box);
        let r = [o.0 + b[0] as f32, o.1 + b[1] as f32, o.0 + (b[0] + b[2]) as f32, o.1 + (b[1] + b[3]) as f32];
        owe(l, [r].into_iter());
        true
    } else {
        // Nothing new: the round watches again, once the interval allows.
        false
    }
}

/// A buffer on the card of that size where the compositor leaves the captures of a sheet (see `CardBackdrop`).
/// If the card or the compositor will not, that sheet stays on shared memory.
#[cfg(target_os = "linux")]
fn make_card_backdrop(g: &Gpu, sheet: u32, fourcc: u32, size: (u32, u32)) {
    use crate::platform::{CardBackdrop, CARD_BACKDROPS, CARD_REFUSED};
    if size.0 == 0 || size.1 == 0 || crate::platform::card_refused(sheet) {
        return;
    }
    if CARD_BACKDROPS.lock().unwrap().as_ref().and_then(|m| m.get(&sheet)).is_some_and(|c| c.size == size && c.fourcc == fourcc) {
        return;
    }
    let refuse = |why: String| {
        println!("render · captures stay in shared memory ({why})");
        CARD_REFUSED.lock().unwrap().push((sheet, Instant::now()));
    };
    if !g.device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF) {
        return refuse("the card does not share its memory with the compositor".into());
    }
    let layouts = crate::platform::backdrop_layouts(fourcc);
    match crate::dmabuf::export_to_capture(&g.device, size, &layouts) {
        Ok(e) => {
            println!("render · captures of a glass go straight onto the card ({}×{}), the processor never reads them", size.0, size.1);
            let card = std::sync::Arc::new(CardBackdrop { texture: e.texture, fd: e.fd, modifier: e.modifier, planes: e.planes, fourcc, size });
            CARD_BACKDROPS.lock().unwrap().get_or_insert_with(Default::default).insert(sheet, card);
        }
        Err(why) => refuse(why),
    }
}

/// The banner that says that what you have just saved does not compile, with the file, the
/// line and the message. It goes at the top of the main surface, which is where you are
/// looking, and goes away by itself when the scene is fine again. Underneath, the last good
/// scene keeps running: this stops nothing, it only tells.
fn error_banner(message: &str, width: f32) -> Vec<Instr> {
    let width = if width > 0.0 { width } else { 900.0 };
    // The first line is the one that says what is going on; the rest is the finger pointing.
    let said = message.lines().next().unwrap_or(message).trim().to_string();
    let height = 52.0;
    let rect = |x: f32, w: f32, r: f32| Shape::Rect {
        center: ((x + w * 0.5).into(), (height * 0.5).into()),
        half_size: ((w * 0.5).into(), (height * 0.5).into()),
        radius: r.into(),
    };
    vec![
        Instr::Group { shadow: Some(Shadow { offset: (0.0.into(), 6.0.into()), blur: 18.0.into(), alpha: 0.45.into(), color: None }) },
        Instr::Shape { shape: rect(8.0, width - 16.0, 10.0), fusion: 0.0.into() },
        Instr::Fill { paint: color(0.18, 0.05, 0.06).into(), alpha: 1.0.into(), rim: 0.05.into(), light: None, border: None, glass_spec: None },
        // A tab in the colour of errors, on the left: it is read before the text.
        Instr::Group { shadow: None },
        Instr::Shape { shape: rect(8.0, 5.0, 2.5), fusion: 0.0.into() },
        Instr::Fill { paint: color(0.99, 0.41, 0.33).into(), alpha: 1.0.into(), rim: 0.05.into(), light: None, border: None, glass_spec: None },
        Instr::Text {
            content: Content::Literal("this does not compile — the last good scene is still running".into()),
            at: (26.0.into(), 17.0.into()),
            anchor: (0.0, 0.5),
            width: Some((width - 52.0).into()),
            style: Style::new(10.5, color(0.99, 0.41, 0.33)).weight(600).lines(1),
            alpha: 1.0.into(),
            measure: None,
        },
        Instr::Text {
            content: Content::Literal(said),
            at: (26.0.into(), 34.0.into()),
            anchor: (0.0, 0.5),
            width: Some((width - 52.0).into()),
            style: Style::new(12.5, color(0.96, 0.96, 0.96)).lines(1),
            alpha: 1.0.into(),
            measure: None,
        },
    ]
}

/// How many monitors are showing something right now: one for each monitor a sheet is on,
/// not one for each surface: the scene's named ones sit on the same monitor as its main
/// one and are not another monitor. The lock's are left out: they go without a
/// `SheetGone`, and their name is the monitor's with " (lock)" after it.
fn screens_showing(sheets: &[Sheet], surfaces: &[Surface]) -> usize {
    sheets
        .iter()
        .filter(|l| l.view.popup.is_none() && !surfaces.get(l.view.surface).is_some_and(|s| s.lock_screen))
        .map(|l| l.name.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// A single sheet waits for its screen —the one of the fastest monitor— and the others
/// present without blocking. If they all waited, a monitor at 60 Hz would hold back
/// another at 165.
fn assign_pace(g: &Gpu, sheets: &mut [Sheet], size: (f32, f32), no_vsync: bool) {
    // A popup never sets the pace: it comes and goes, and it may be covered.
    // Nor a closed one. And between equals, the first: `max_by_key` keeps the
    // last, which with two surfaces at 60 Hz was the corner one, closed.
    // Nor one the compositor stopped showing (`unseen`), while another open
    // one is shown: waiting on its frames, the whole scene went at one a
    // second, the one on screen too.
    let shown = sheets.iter().any(|l| l.view.popup.is_none() && l.open && !l.unseen);
    let mut fastest: Option<(i32, _)> = None;
    for l in sheets.iter().filter(|l| l.view.popup.is_none() && l.open && (!l.unseen || !shown)) {
        if fastest.is_none_or(|(mhz, _)| l.mhz > mhz) {
            fastest = Some((l.mhz, l.id));
        }
    }
    let fastest = fastest.map(|(_, id)| id);
    for l in sheets.iter_mut() {
        let drives = !no_vsync && Some(l.id) == fastest;
        if l.drives_pace != drives {
            l.drives_pace = drives;
            g.reconfigure(l, size);
        }
    }
}

/// What is selected of a text that can be: its instruction, and the bytes
/// where it was started and where it goes now. While the button is held, the
/// second follows the mouse.
struct TextSelection {
    at: usize,
    anchor: usize,
    cursor: usize,
    dragging: bool,
}

impl TextSelection {
    /// From and to, in order and on letter boundaries of the text as it is now.
    fn bytes(&self, t: &str) -> (usize, usize) {
        let fit = |b: usize| {
            let mut b = b.min(t.len());
            while !t.is_char_boundary(b) {
                b -= 1;
            }
            b
        };
        (fit(self.anchor.min(self.cursor)), fit(self.anchor.max(self.cursor)))
    }
}

/// The word around a byte: letters, digits and what joins them. On anything
/// else, just that character.
fn word_around(t: &str, b: usize) -> (usize, usize) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || c == '’';
    let b = (0..=b.min(t.len())).rev().find(|k| t.is_char_boundary(*k)).unwrap_or(0);
    let Some(here) = t[b..].chars().next() else { return (b, b) };
    if !is_word(here) {
        return (b, b + here.len_utf8());
    }
    let start = t[..b].char_indices().rev().take_while(|(_, c)| is_word(*c)).last().map_or(b, |(k, _)| k);
    let end = t[b..].char_indices().find(|(_, c)| !is_word(*c)).map_or(t.len(), |(k, _)| b + k);
    (start, end)
}

/// The field being typed into: which text, where the cursor is and from where it was
/// selected. In bytes, always on the boundary of a letter.
struct Editing {
    field: usize,
    cursor: usize,
    anchor: usize,
}

enum KeyOutcome {
    Changed,
    Moved,
    Submitted,
    Unhandled,
}

impl Editing {
    fn selection(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    fn delete_selection(&mut self, t: &mut String) -> bool {
        let (a, b) = self.selection();
        t.replace_range(a..b, "");
        self.cursor = a;
        self.anchor = a;
        b > a
    }

    fn handle_key(&mut self, t: &mut String, name: &str, typed: Option<&str>, m: Mods) -> KeyOutcome {
        self.cursor = self.cursor.min(t.len());
        self.anchor = self.anchor.min(t.len());
        let before = |t: &str, b: usize| t[..b].char_indices().next_back().map_or(0, |c| c.0);
        let after = |t: &str, b: usize| t[b..].chars().next().map_or(t.len(), |c| b + c.len_utf8());
        let move_to = |me: &mut Self, a: usize| {
            me.cursor = a;
            if !m.shift {
                me.anchor = a;
            }
            KeyOutcome::Moved
        };
        match name {
            "a" if m.ctrl => {
                self.anchor = 0;
                self.cursor = t.len();
                KeyOutcome::Moved
            }
            "c" | "x" if m.ctrl => {
                let (a, b) = self.selection();
                if b > a {
                    crate::platform::clipboard_write(&t[a..b]);
                }
                if name == "x" && self.delete_selection(t) { KeyOutcome::Changed } else { KeyOutcome::Moved }
            }
            "v" if m.ctrl => {
                // Nothing to write (a picture copied): the key goes on to the
                // scene's rules and logic, which may take the picture.
                let Some(pasted) = crate::platform::clipboard_read().filter(|p| !p.is_empty()) else { return KeyOutcome::Unhandled };
                // A field is single-line: whatever comes with line breaks, without them.
                let pasted: String = pasted.chars().filter(|c| !c.is_control()).collect();
                self.delete_selection(t);
                t.insert_str(self.cursor, &pasted);
                self.cursor += pasted.len();
                self.anchor = self.cursor;
                KeyOutcome::Changed
            }
            "BackSpace" | "Delete" => {
                if !self.delete_selection(t) {
                    let (a, b) = if name == "BackSpace" { (before(t, self.cursor), self.cursor) } else { (self.cursor, after(t, self.cursor)) };
                    t.replace_range(a..b, "");
                    self.cursor = a;
                    self.anchor = a;
                }
                KeyOutcome::Changed
            }
            "Left" => {
                let a = before(t, self.cursor);
                move_to(self, a)
            }
            "Right" => {
                let a = after(t, self.cursor);
                move_to(self, a)
            }
            "Home" => move_to(self, 0),
            "End" => {
                let a = t.len();
                move_to(self, a)
            }
            "Return" | "KP_Enter" => KeyOutcome::Submitted,
            _ => match typed {
                Some(letters) if !letters.is_empty() && !m.ctrl && !m.alt && !m.logo => {
                    self.delete_selection(t);
                    t.insert_str(self.cursor, letters);
                    self.cursor += letters.len();
                    self.anchor = self.cursor;
                    KeyOutcome::Changed
                }
                _ => KeyOutcome::Unhandled,
            },
        }
    }
}

struct LayerState {
    hot: bool,
    winner: Option<usize>,
    until: Vec<Option<Instant>>,
    lit: Vec<bool>,
}

impl LayerState {
    fn new(n: usize, hot: bool) -> Self {
        LayerState { hot, winner: None, until: vec![None; n], lit: vec![false; n] }
    }
}

/// The behaviors a round goes through: all but the `follow`s, `bind`s and
/// `sort`s of the copies for monitors that are not there (those are not even
/// looked at), and of those, the `bind`s and `sort`s alone (the passes before
/// the rules and after the springs). Made again when what is absent changes.
/// With `screens: each max 4` and one monitor, three quarters of every pass
/// were spent finding behaviors to skip.
#[derive(Default)]
struct AwakeBehaviors {
    of: (usize, usize),
    absent: Vec<std::ops::Range<usize>>,
    all: Vec<u32>,
    binds: Vec<u32>,
}

impl AwakeBehaviors {
    fn update(&mut self, behaviors: &[Behavior], absent: &[std::ops::Range<usize>]) {
        let of = (behaviors.as_ptr() as usize, behaviors.len());
        if self.of == of && self.absent == absent {
            return;
        }
        self.of = of;
        self.absent = absent.to_vec();
        self.all.clear();
        self.binds.clear();
        for (k, b) in behaviors.iter().enumerate() {
            let owned = matches!(b, Behavior::Follow { .. } | Behavior::Bind { .. } | Behavior::Sort { .. });
            if owned && absent.iter().any(|r| r.contains(&k)) {
                continue;
            }
            self.all.push(k as u32);
            if matches!(b, Behavior::Bind { .. } | Behavior::Sort { .. }) {
                self.binds.push(k as u32);
            }
        }
    }
}

/// The rules by when they can fire, and what each `on change` and `on still`
/// reads: a round looks at the event rules —a press, a key, a zone entered—
/// only when that kind of thing happened, and works out an `on change` again
/// only when something it reads changed since it was last worked out. A window
/// manager's scene has 12 600 rules, 4 180 of them `on change`, and a round
/// went through every one of them and evaluated every `on change` to find, most
/// rounds, that nothing had happened.
#[derive(Default)]
struct RuleIndex {
    of: (usize, usize),
    /// Rules that can fire in any round (watching a value or a clock), in order.
    always: Vec<u32>,
    /// Rules that fire only with some event, in order; and all but the signals', in order.
    with_events: Vec<u32>,
    /// What an `on change` / `on still` reads, when that is all it depends on.
    inputs: Vec<Option<(Vec<u16>, Vec<u16>)>>,
    /// The round each one was last looked at (to know whether "unchanged since last round" holds).
    looked: Vec<u64>,
    round: u64,
    /// Facts and properties at the rules of the round before, and what changed since.
    facts: Vec<f32>,
    props: Vec<(f32, f32)>,
    changed_facts: Vec<bool>,
    changed_props: Vec<bool>,
    /// Of `always` and `with_events`, those of the copies awake, for these asleep:
    /// the rules of a copy that is asleep are not even looked at.
    asleep: Vec<std::ops::Range<usize>>,
    awake_always: Vec<u32>,
    awake_with_events: Vec<u32>,
    /// Of `awake_always`, all but the `on change`s that depend only on facts and
    /// properties: in a round where none of those changed —a window drawing, a
    /// clock waiting— none of them can fire, and they are not looked at at all.
    awake_quiet: Vec<u32>,
    /// Those `on change`s, for the checks.
    awake_pure_changes: Vec<u32>,
    /// Something changed this round (`look`), or what is awake did (`wake`).
    any_changed: bool,
    woke: bool,
    /// The awake lists are still to be made for the rules `build` read.
    stale: bool,
}

impl RuleIndex {
    fn wake(&mut self, asleep: &[std::ops::Range<usize>], rules: &[crate::scene::Rule]) {
        self.woke = false;
        if !self.stale && self.asleep == asleep {
            return;
        }
        self.stale = false;
        self.woke = true;
        self.asleep = asleep.to_vec();
        let awake = |k: &&u32| !asleep.iter().any(|r| r.contains(&(**k as usize)));
        self.awake_always = self.always.iter().filter(awake).copied().collect();
        self.awake_with_events = self.with_events.iter().filter(awake).copied().collect();
        let pure_change = |k: &u32| matches!(rules[*k as usize].when, Trigger::Change(_)) && self.inputs[*k as usize].is_some();
        self.awake_quiet = self.awake_always.iter().copied().filter(|k| !pure_change(k)).collect();
        self.awake_pure_changes = self.awake_always.iter().copied().filter(|k| pure_change(k)).collect();
    }

    fn build(&mut self, rules: &[crate::scene::Rule]) {
        self.of = (rules.as_ptr() as usize, rules.len());
        self.always.clear();
        self.with_events.clear();
        self.inputs.clear();
        for (k, r) in rules.iter().enumerate() {
            let every_round = matches!(
                r.when,
                Trigger::Change(_) | Trigger::Still { .. } | Trigger::Hold { .. } | Trigger::Above { .. } | Trigger::Away { .. } | Trigger::Idle { .. } | Trigger::Every { .. }
            );
            if every_round {
                self.always.push(k as u32);
            }
            if !matches!(r.when, Trigger::On(_)) {
                self.with_events.push(k as u32);
            }
            let watched = match &r.when {
                Trigger::Change(x) => Some(x),
                Trigger::Still { value, .. } => Some(value),
                _ => None,
            };
            self.inputs.push(watched.filter(|x| x.only_inputs()).map(|x| {
                let (mut props, mut facts) = (Vec::new(), Vec::new());
                x.inputs(&mut props, &mut facts);
                (facts, props)
            }));
        }
        self.looked = vec![0; rules.len()];
        self.asleep.clear();
        self.awake_always.clear();
        self.awake_with_events.clear();
        self.stale = true;
        self.facts.clear();
        self.props.clear();
    }

    /// What changed since the rules of the round before.
    fn look(&mut self, facts: &[f32], props: &[crate::scene::Animated]) {
        self.round += 1;
        let fresh = self.facts.len() != facts.len() || self.props.len() != props.len();
        if fresh {
            self.facts = facts.to_vec();
            self.props = props.iter().map(|a| (a.x, a.v)).collect();
            self.changed_facts = vec![true; facts.len()];
            self.changed_props = vec![true; props.len()];
            self.any_changed = true;
            return;
        }
        let mut any = false;
        for ((before, now), changed) in self.facts.iter_mut().zip(facts).zip(self.changed_facts.iter_mut()) {
            *changed = before.to_bits() != now.to_bits();
            if *changed {
                *before = *now;
                any = true;
            }
        }
        for ((before, now), changed) in self.props.iter_mut().zip(props).zip(self.changed_props.iter_mut()) {
            *changed = before.0.to_bits() != now.x.to_bits() || before.1.to_bits() != now.v.to_bits();
            if *changed {
                *before = (now.x, now.v);
                any = true;
            }
        }
        self.any_changed = any;
    }

    /// An `on change` / `on still` looked at the round before, and none of what it reads changed since.
    fn unchanged(&self, k: usize) -> bool {
        self.looked.get(k).is_some_and(|r| *r + 1 == self.round)
            && self.inputs.get(k).and_then(Option::as_ref).is_some_and(|(facts, props)| {
                facts.iter().all(|h| !self.changed_facts.get(*h as usize).copied().unwrap_or(true))
                    && props.iter().all(|p| !self.changed_props.get(*p as usize).copied().unwrap_or(true))
            })
    }
}

#[derive(Default)]
struct RuleState {
    /// What the expression of an `on change` was worth the last time.
    last_value: Option<f32>,
    since: Option<Instant>,
    fired: bool,
    armed: bool,
    next: Option<Instant>,
    last_time: Option<Instant>,
}

impl RuleState {
    /// "It has been true for this long": fires once per episode.
    fn sustained(&mut self, true_now: bool, duration: Duration, now: Instant, appointments: &mut Vec<Instant>) -> bool {
        if !true_now {
            self.since = None;
            self.fired = false;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        if self.fired {
            return false;
        }
        if now - since >= duration {
            self.fired = true;
            return true;
        }
        appointments.push(since + duration);
        false
    }
}

struct Playback {
    gesture: usize,
    keyframe: usize,
    started_at: Instant,
    from: Vec<f32>,
    /// With reduced motion: the keyframe that is shown, still, the whole time.
    still: Option<usize>,
    values: Vec<Vec<(PropId, f32)>>,
}

impl Playback {
    fn start(k: usize, g: &Gesture, pose: &[PropId], c: Ctx, now: Instant, reduced_motion: bool) -> Self {
        let props = c.props;
        // The values of each keyframe are fixed at the start.
        let values: Vec<Vec<(PropId, f32)>> = g.keyframes.iter().map(|f| f.values.iter().map(|(p, e)| (*p, e.eval(c))).collect()).collect();
        // The still face of a gesture is the keyframe that strays furthest from the base.
        let still = reduced_motion.then(|| {
            let score = |f: &Vec<(PropId, f32)>| -> f32 { f.iter().map(|(p, v)| (v - props[p.0 as usize].target).abs() / props[p.0 as usize].target.abs().max(1.0)).sum() };
            (0..values.len()).max_by(|a, b| score(&values[*a]).total_cmp(&score(&values[*b]))).unwrap_or(0)
        });
        Playback { gesture: k, keyframe: 0, started_at: now, from: pose.iter().map(|p| props[p.0 as usize].x).collect(), still, values }
    }

    /// Where a keyframe takes a property: to what it says, or to its base.
    fn target(&self, keyframe: usize, p: PropId, props: &[Animated]) -> f32 {
        self.values[keyframe].iter().find(|(q, _)| *q == p).map_or(props[p.0 as usize].target, |(_, v)| *v)
    }
}

/// The CPU this thread has used, in milliseconds (the wall clock also counts
/// what it spends waiting).
fn thread_cpu_ms() -> f64 {
    #[cfg(target_os = "linux")]
    {
        let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a valid clock and a timespec of our own to fill.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) } == 0 {
            return t.tv_sec as f64 * 1000.0 + t.tv_nsec as f64 / 1e6;
        }
    }
    0.0
}

/// The `follow`s and `bind`s of a scene, remembered: each one is worked out
/// again only if something it reads —a fact, or a property's value or speed—
/// is not what it was when it was last worked out. A scene of windows has
/// hundreds, and a terminal's frame, which changes none of them, had them all
/// worked out again.
///
/// What it reads is what it read the last time it was worked out, not all it
/// names: `if(dock.up, … mouse.x …, 0)` with no dock up reads nothing of the
/// mouse, and the mouse moving leaves it alone. By what they name, a desktop's
/// 266 follows that name the mouse were all worked out again on every move
/// of it, for the same values.
#[derive(Default)]
struct Follows {
    /// Which scene they belong to: another one, and all of it is forgotten.
    of: usize,
    /// For each: the facts and properties it read, what they were, and what it came to.
    each: Vec<Option<Remembered>>,
    /// Where what one reads is noted while it is worked out, and for how many
    /// facts and properties it has room.
    reading: crate::scene::Seen,
    room: (usize, usize),
}

struct Remembered {
    facts: Vec<u16>,
    props: Vec<u16>,
    seen: Vec<f32>,
    value: Option<f32>,
}

impl Follows {
    fn begin(&mut self, behaviors: &[Behavior]) {
        let id = behaviors.as_ptr() as usize ^ behaviors.len();
        if self.of != id {
            self.of = id;
            self.each = behaviors
                .iter()
                .map(|b| match b {
                    Behavior::Follow { .. } | Behavior::Bind { .. } => Some(Remembered { facts: Vec::new(), props: Vec::new(), seen: Vec::new(), value: None }),
                    _ => None,
                })
                .collect();
        }
    }

    fn value(&mut self, k: usize, to: &Expr, props: &[Animated], facts: &[f32]) -> f32 {
        let Some(r) = self.each.get_mut(k).and_then(Option::as_mut) else { return to.eval(Ctx { props, facts, seen: None }) };
        if let Some(v) = r.value {
            let (was_facts, was_props) = r.seen.split_at(r.facts.len());
            let same = r.facts.iter().zip(was_facts).all(|(h, w)| facts[*h as usize].to_bits() == w.to_bits())
                && r.props.iter().zip(was_props.chunks_exact(2)).all(|(p, w)| {
                    let a = &props[*p as usize];
                    a.x.to_bits() == w[0].to_bits() && a.v.to_bits() == w[1].to_bits()
                });
            if same {
                return v;
            }
        }
        if self.room != (facts.len(), props.len()) {
            self.room = (facts.len(), props.len());
            self.reading.reset(facts.len(), props.len());
        }
        let v = to.eval(Ctx { props, facts, seen: Some(&self.reading) });
        self.reading.take(&mut r.facts, &mut r.props);
        r.seen.clear();
        r.seen.extend(r.facts.iter().map(|h| facts[*h as usize]).chain(r.props.iter().flat_map(|p| [props[*p as usize].x, props[*p as usize].v])));
        r.value = Some(v);
        v
    }
}

impl Follows {
    /// What it came to last time, if what it reads has not changed since
    /// (`changed` false, see `Readers`): not even looked at. Otherwise, worked out.
    fn kept_or_value(&mut self, changed: bool, k: usize, to: &Expr, props: &[Animated], facts: &[f32]) -> f32 {
        if !changed
            && let Some(v) = self.each.get(k).and_then(Option::as_ref).and_then(|r| r.value)
        {
            return v;
        }
        self.value(k, to, props, facts)
    }
}

/// Which `follow`s and `bind`s read each fact and each property: directly, or
/// through the `bind`s they read (the properties big `let`s are computed in).
/// A round asks again only those that read something that changed. Asking
/// each one —thousands in pleamar-wm's scene— whether what it read had
/// changed was most of a round while the mouse moved, and the mouse only
/// moves `mouse.x` and `mouse.y`.
#[derive(Default)]
struct Readers {
    /// Which scene they belong to: another one, and they are found again.
    of: usize,
    facts: Vec<Vec<u32>>,
    props: Vec<Vec<u32>>,
    /// What each pass (before the rules, the behaviors, after the springs)
    /// saw last time: the facts, each property's value and speed, and the
    /// parts asleep.
    seen: [Seen3; 3],
    /// This pass: whether each behavior reads something that changed.
    dirty: Vec<bool>,
}

#[derive(Default)]
struct Seen3 {
    facts: Vec<f32>,
    props: Vec<(f32, f32)>,
    absent: Vec<std::ops::Range<usize>>,
}

impl Readers {
    fn build(&mut self, behaviors: &[Behavior], n_facts: usize, n_props: usize) {
        let bind_of: std::collections::HashMap<u16, usize> = behaviors
            .iter()
            .enumerate()
            .flat_map(|(k, b)| match b {
                Behavior::Bind { prop, .. } => vec![(prop.0, k)],
                Behavior::Sort { ord, pos, .. } => ord.iter().chain(pos).map(|p| (p.0, k)).collect(),
                _ => Vec::new(),
            })
            .collect();
        let direct: Vec<Option<(Vec<u16>, Vec<u16>)>> = behaviors
            .iter()
            .map(|b| match b {
                Behavior::Follow { to, .. } | Behavior::Bind { to, .. } => {
                    let (mut props, mut facts) = (Vec::new(), Vec::new());
                    to.inputs(&mut props, &mut facts);
                    Some((facts, props))
                }
                Behavior::Sort { keys, .. } => {
                    let (mut props, mut facts) = (Vec::new(), Vec::new());
                    for e in keys.iter().flatten() {
                        e.inputs(&mut props, &mut facts);
                    }
                    Some((facts, props))
                }
                _ => None,
            })
            .collect();
        // Through the binds: what each one reads in the end. A bind reads only
        // the ones declared before it (a `let` names what is above it); one
        // that names itself, or a later one, is taken as it is.
        let mut deep: Vec<Option<(Vec<u16>, Vec<u16>)>> = vec![None; behaviors.len()];
        for k in 0..behaviors.len() {
            let Some((f, p)) = &direct[k] else { continue };
            let (mut facts, mut props) = (f.clone(), p.clone());
            for q in p {
                if let Some(&j) = bind_of.get(q).filter(|j| **j < k)
                    && let Some((jf, jp)) = &deep[j]
                {
                    facts.extend_from_slice(jf);
                    props.extend_from_slice(jp);
                }
            }
            facts.sort_unstable();
            facts.dedup();
            props.sort_unstable();
            props.dedup();
            deep[k] = Some((facts, props));
        }
        self.facts = vec![Vec::new(); n_facts];
        self.props = vec![Vec::new(); n_props];
        for (k, d) in deep.iter().enumerate() {
            let Some((f, p)) = d else { continue };
            for h in f {
                if let Some(r) = self.facts.get_mut(*h as usize) {
                    r.push(k as u32);
                }
            }
            for q in p {
                if let Some(r) = self.props.get_mut(*q as usize) {
                    r.push(k as u32);
                }
            }
        }
    }

    /// Marks, for this pass, the behaviors that read something that changed
    /// since it last looked: all of them the first time, with a new scene, or
    /// when what is asleep is not what it was (what wakes has to catch up).
    fn look(&mut self, pass: usize, behaviors: &[Behavior], facts: &[f32], props: &[Animated], absent: &[std::ops::Range<usize>]) {
        let id = behaviors.as_ptr() as usize ^ behaviors.len();
        if self.of != id || self.facts.len() != facts.len() || self.props.len() != props.len() {
            self.build(behaviors, facts.len(), props.len());
            self.of = id;
            self.seen = Default::default();
        }
        let Readers { facts: by_fact, props: by_prop, seen, dirty, .. } = self;
        let s = &mut seen[pass];
        let all = s.facts.len() != facts.len() || s.props.len() != props.len() || s.absent != absent;
        dirty.clear();
        dirty.resize(behaviors.len(), all);
        if all {
            s.facts.clear();
            s.facts.extend_from_slice(facts);
            s.props.clear();
            s.props.extend(props.iter().map(|a| (a.x, a.v)));
        } else {
            // Compared and brought up to date in one go: only what changed is written.
            for (i, (a, b)) in facts.iter().zip(s.facts.iter_mut()).enumerate() {
                if a.to_bits() != b.to_bits() {
                    *b = *a;
                    for &k in &by_fact[i] {
                        dirty[k as usize] = true;
                    }
                }
            }
            for (i, (a, b)) in props.iter().zip(s.props.iter_mut()).enumerate() {
                if a.x.to_bits() != b.0.to_bits() || a.v.to_bits() != b.1.to_bits() {
                    *b = (a.x, a.v);
                    for &k in &by_prop[i] {
                        dirty[k as usize] = true;
                    }
                }
            }
        }
        if s.absent != absent {
            s.absent = absent.to_vec();
        }
    }

    /// That property is changing in the middle of the pass: whoever reads it,
    /// further on in it, is asked again (a blink, a wave or a spin moves its
    /// property as the behaviors go by, and Marea's eyes, made from them, were
    /// left a frame behind).
    fn touch(&mut self, prop: PropId) {
        if let Some(r) = self.props.get(prop.0 as usize) {
            for &k in r {
                if let Some(d) = self.dirty.get_mut(k as usize) {
                    *d = true;
                }
            }
        }
    }
}

/// What a draw list was made from: while all of it holds, the list holds.
/// What a result read —facts and properties, by `scene::Seen`— and what they
/// were: the result holds for as long as they do.
#[derive(Default)]
struct ReadMemo {
    valid: bool,
    seen: crate::scene::Seen,
    read_facts: Vec<u16>,
    read_props: Vec<u16>,
    facts: Vec<f32>,
    props: Vec<(f32, f32)>,
}

impl ReadMemo {
    fn holds(&self, facts: &[f32], props: &[crate::scene::Animated]) -> bool {
        self.valid
            && self.read_facts.iter().zip(&self.facts).all(|(k, v)| facts.get(*k as usize).is_some_and(|f| f.to_bits() == v.to_bits()))
            && self.read_props.iter().zip(&self.props).all(|(k, v)| props.get(*k as usize).is_some_and(|p| p.x.to_bits() == v.0.to_bits() && p.v.to_bits() == v.1.to_bits()))
    }
    /// Where to note what is read while the result is worked out.
    fn start(&mut self, facts: usize, props: usize) -> &crate::scene::Seen {
        self.seen.reset(facts, props);
        &self.seen
    }
    /// What was read, kept with what it was now.
    fn keep(&mut self, facts: &[f32], props: &[crate::scene::Animated]) {
        self.seen.take(&mut self.read_facts, &mut self.read_props);
        self.facts = self.read_facts.iter().map(|k| facts[*k as usize]).collect();
        self.props = self.read_props.iter().map(|k| (props[*k as usize].x, props[*k as usize].v)).collect();
        self.valid = true;
    }
}

/// Each zone as it was last worked out —alive (not hidden, not on a copy for a
/// monitor that is not there, `active:`) and its box—, with what that read: it
/// is worked out again only when some of that changed. A window drawing, an
/// effect turning, the mouse moving: none of them moves a zone, and every round
/// went through all of them (2600 in pleamar-wm's scene) to find the same.
#[derive(Default)]
struct ZoneMemo {
    valid: bool,
    seen: crate::scene::Seen,
    read_facts: Vec<u16>,
    read_props: Vec<u16>,
    facts: Vec<f32>,
    props: Vec<(f32, f32)>,
    hidden: Vec<bool>,
    absent: Vec<std::ops::Range<usize>>,
    live: Vec<bool>,
    bounds: Vec<Option<[f32; 4]>>,
    /// Each zone's own: whether it could be alive (not hidden, not absent),
    /// what its `active:` and its box read, and what that was. When something
    /// changed, only the zones that read it are worked out again: a panel
    /// sliding in moves its own zones, not the other two thousand.
    each: Vec<OneZone>,
}

#[derive(Default)]
struct OneZone {
    done: bool,
    could: bool,
    reads: Vec<u32>,
    values: Vec<(f32, f32)>,
}

impl ZoneMemo {
    /// Whether what was worked out still holds.
    fn holds(&self, facts: &[f32], props: &[crate::scene::Animated], hidden: &[bool], absent: &[std::ops::Range<usize>]) -> bool {
        self.valid
            && self.hidden == hidden
            && self.absent == absent
            && self.read_facts.iter().zip(&self.facts).all(|(k, v)| facts.get(*k as usize).is_some_and(|f| f.to_bits() == v.to_bits()))
            && self.read_props.iter().zip(&self.props).all(|(k, v)| props.get(*k as usize).is_some_and(|p| p.x.to_bits() == v.0.to_bits() && p.v.to_bits() == v.1.to_bits()))
    }

    /// Each zone alive or not, and its box, as they are now.
    fn fresh(scene: &Scene, c: Ctx, hidden: &[bool], absent: &[std::ops::Range<usize>]) -> (Vec<bool>, Vec<Option<[f32; 4]>>) {
        let live: Vec<bool> = scene.zones.iter().map(|z| !hidden.get(z.at).copied().unwrap_or(false) && !absent.iter().any(|r| r.contains(&z.at)) && z.active.is_true(c)).collect();
        let bounds = scene.zones.iter().zip(&live).map(|(z, l)| if *l { z.bounds(c) } else { None }).collect();
        (live, bounds)
    }

    /// Worked out again, if something it read changed. `check`: when it holds, it
    /// is worked out anyway and a difference is said.
    fn update(&mut self, scene: &Scene, facts: &[f32], props: &[crate::scene::Animated], hidden: &[bool], absent: &[std::ops::Range<usize>], check: bool) {
        if self.holds(facts, props, hidden, absent) {
            if check {
                let (live, bounds) = Self::fresh(scene, Ctx { props, facts, seen: None }, hidden, absent);
                if live != self.live || bounds != self.bounds {
                    let k = (0..live.len()).find(|k| live[*k] != self.live[*k] || bounds[*k] != self.bounds[*k]).unwrap_or(0);
                    eprintln!("check  · the zones were taken as unchanged and are not: '{}' alive {} → {}, {:?} → {:?}", scene.zones[k].id, self.live[k], live[k], self.bounds[k], bounds[k]);
                }
            }
            return;
        }
        self.seen.reset(facts.len(), props.len());
        let c = Ctx { props, facts, seen: Some(&self.seen) };
        let n = scene.zones.len();
        if self.each.len() != n || self.live.len() != n || self.bounds.len() != n {
            self.each.clear();
            self.each.resize_with(n, OneZone::default);
            self.live = vec![false; n];
            self.bounds = vec![None; n];
        }
        for (k, z) in scene.zones.iter().enumerate() {
            let could = !hidden.get(z.at).copied().unwrap_or(false) && !absent.iter().any(|r| r.contains(&z.at));
            let one = &mut self.each[k];
            if !could {
                (one.done, one.could) = (true, false);
                one.reads.clear();
                one.values.clear();
                (self.live[k], self.bounds[k]) = (false, None);
                continue;
            }
            let holds = one.done
                && one.could
                && one.reads.iter().zip(&one.values).all(|(r, v)| {
                    if r & crate::scene::SEEN_FACT != 0 {
                        facts.get((r & !crate::scene::SEEN_FACT) as usize).is_some_and(|f| f.to_bits() == v.0.to_bits())
                    } else {
                        props.get(*r as usize).is_some_and(|p| p.x.to_bits() == v.0.to_bits() && p.v.to_bits() == v.1.to_bits())
                    }
                });
            if holds {
                for r in &one.reads {
                    if r & crate::scene::SEEN_FACT != 0 {
                        self.seen.fact((r & !crate::scene::SEEN_FACT) as usize);
                    } else {
                        self.seen.prop(*r as usize);
                    }
                }
                continue;
            }
            self.seen.log_from_here();
            let live = z.active.is_true(c);
            self.bounds[k] = if live { z.bounds(c) } else { None };
            self.live[k] = live;
            self.seen.log_into(&mut one.reads);
            one.values.clear();
            one.values.extend(one.reads.iter().map(|r| {
                if r & crate::scene::SEEN_FACT != 0 {
                    (facts.get((r & !crate::scene::SEEN_FACT) as usize).copied().unwrap_or(f32::NAN), 0.0)
                } else {
                    props.get(*r as usize).map_or((f32::NAN, 0.0), |p| (p.x, p.v))
                }
            }));
            (one.done, one.could) = (true, true);
        }
        if check {
            let (live, bounds) = Self::fresh(scene, Ctx { props, facts, seen: None }, hidden, absent);
            if live != self.live || bounds != self.bounds {
                let k = (0..live.len()).find(|k| live[*k] != self.live[*k] || bounds[*k] != self.bounds[*k]).unwrap_or(0);
                eprintln!("check  · a zone was taken as unchanged and is not: '{}' alive {} → {}, {:?} → {:?}", scene.zones[k].id, self.live[k], live[k], self.bounds[k], bounds[k]);
            }
        }
        self.seen.take(&mut self.read_facts, &mut self.read_props);
        self.facts = self.read_facts.iter().map(|k| facts[*k as usize]).collect();
        self.props = self.read_props.iter().map(|k| (props[*k as usize].x, props[*k as usize].v)).collect();
        self.hidden = hidden.to_vec();
        self.absent = absent.to_vec();
        self.valid = true;
    }
}

struct ComposeMemo {
    size: (f32, f32),
    view: Option<crate::gpu::FieldView>,
    /// What falls on no sheet is left out of the list: a sheet that comes
    /// (a lock screen, after its `open:` had already been read) makes it again.
    views: Vec<[f32; 4]>,
    /// The facts, and each property's value and speed, as they were; and
    /// which of them the drawing read: only those have to hold.
    facts: Vec<f32>,
    props: Vec<(f32, f32)>,
    seen: crate::scene::Seen,
    texts: Vec<String>,
    window_tex: Vec<Option<crate::gpu::WindowTex>>,
    /// What else the list is made from: the parts asleep, the order of what
    /// has a `z:`, the text selected, and when each event last happened.
    skip: Vec<std::ops::Range<usize>>,
    order: Option<std::sync::Arc<Vec<usize>>>,
    selected: Option<(usize, usize, usize)>,
    signal_times: Vec<f32>,
}

fn ripple_origin(scene: &Scene, target: ZoneId, c: Ctx, hidden: &[bool]) -> Option<(f32, f32)> {
    let zone = &scene.zones[target.0 as usize];
    if hidden.get(zone.at).copied().unwrap_or(false) || !zone.active.is_true(c) {
        return None;
    }
    let [x1, y1, x2, y2] = zone.bounds(c)?;
    let p = ((x1 + x2) * 0.5, (y1 + y2) * 0.5);
    let mut within = zone.within;
    while let Some(id) = within {
        let viewport = &scene.zones[id.0 as usize];
        if !viewport.contains(c, p.0, p.1) { return None; }
        within = viewport.within;
    }
    (p.0.is_finite() && p.1.is_finite()).then_some(p)
}

#[cfg(test)]
mod ripple_tests {
    use super::*;

    #[test]
    fn feedback_targets_each_component_in_its_current_layout() {
        let (mut scene, _) = crate::language::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ripple.plm")).unwrap();
        let mut props: Vec<Animated> = scene.props.iter().map(|(_, x, s)| Animated::at(*x, *s)).collect();
        let facts: Vec<f32> = scene.facts.iter().map(|(_, x)| *x).collect();
        for b in &scene.behaviors {
            if let Behavior::Bind { prop, to } = b {
                props[prop.0 as usize].x = to.eval(Ctx { props: &props, facts: &facts, seen: None });
            }
        }
        let targets: Vec<ZoneId> = scene.rules.iter().filter_map(|r| match r.effects.first() {
            Some(Effect::Ripple(z)) => Some(*z),
            _ => None,
        }).collect();
        // Two rules in each copy target its own zone, including one with a payload.
        assert_eq!(targets.len(), 6);
        for (i, pair) in targets.chunks(2).enumerate() {
            assert_eq!(pair[0], pair[1]);
            let c = Ctx { props: &props, facts: &facts, seen: None };
            assert_eq!(ripple_origin(&scene, pair[0], c, &[]), Some((130.0 + i as f32 * 184.0, 120.0)));
        }
        let shift = scene.props.iter().position(|p| p.0 == "shift").unwrap();
        props[shift].x = 30.0;
        let c = Ctx { props: &props, facts: &facts, seen: None };
        let target = targets[0];
        assert_eq!(ripple_origin(&scene, target, c, &[]), Some((150.0, 120.0)));
        let mut hidden = vec![false; scene.instrs.len() + 1];
        hidden[scene.zones[target.0 as usize].at] = true;
        assert_eq!(ripple_origin(&scene, target, c, &hidden), None);
        scene.zones[target.0 as usize].within = Some(targets[2]);
        assert_eq!(ripple_origin(&scene, target, c, &[]), None);
        scene.zones[target.0 as usize].within = None;
        scene.zones[target.0 as usize].active = Expr::K(0.0);
        assert_eq!(ripple_origin(&scene, target, c, &[]), None);
    }
}

#[cfg(test)]
mod dock_tests {
    use super::*;

    #[test]
    fn a_dock_of_programs_counts_windows_on_any_monitor_and_workspace() {
        // Three windows: a terminal on the first monitor's workspace, a chat on
        // the second monitor, a browser on a workspace the first does not show.
        let mut desks = Desks::default();
        desks.grow(3);
        for (k, (app, pool)) in [("kitty", 1), ("telegram", 2), ("zen", 5)].into_iter().enumerate() {
            desks.alive[k] = true;
            desks.apps[k] = app.to_owned();
            desks.pool[k] = pool;
            desks.born[k] = k as u64 + 1;
        }
        desks.stack[0] = vec![1, 5];
        let screens = [0, 1, 0];
        let names = |d: &Desks| d.items[0].iter().map(|i| i.key.clone()).collect::<Vec<_>>();
        // A shore of its own monitor: only what is on the workspace it shows.
        desks.work_dock(0, &screens);
        assert_eq!(names(&desks), ["kitty"]);
        // A dock of programs: all three, as they opened, on every monitor's list.
        desks.programs = true;
        desks.work_dock(0, &screens);
        desks.work_dock(1, &screens);
        assert_eq!(names(&desks), ["kitty", "telegram", "zen"]);
        assert_eq!(desks.items[1].len(), 3);
        assert_eq!(desks.docked(0, &screens), [0, 1, 2]);
    }

    #[test]
    fn kept_workspaces_stay_while_empty_and_tide_pools_dry_up() {
        // One window on the first monitor's first workspace; a second, empty one after it.
        let fresh = |kept: bool| {
            let mut desks = Desks::default();
            desks.grow(1);
            desks.kept = kept;
            desks.alive[0] = true;
            desks.pool[0] = 1;
            desks.stack[0] = vec![1, 9];
            desks
        };
        let screens = [0];
        // A tide pool: empty and not shown, it dries up; and no further than one past the last with windows.
        let mut pools = fresh(false);
        pools.compact(&screens);
        assert_eq!(pools.stack[0], [1]);
        let second = pools.at(0, 7, &screens);
        assert_eq!(pools.index(0, second), 2);
        // Kept: it stays, and one past the last there is makes another.
        let mut kept = fresh(true);
        kept.compact(&screens);
        assert_eq!(kept.stack[0], [1, 9]);
        assert_eq!(kept.at(0, 2, &screens), 9);
        let third = kept.at(0, 9, &screens);
        assert_eq!((kept.index(0, third), kept.stack[0].len()), (3, 3));
    }
}

#[cfg(test)]
mod follows_tests {
    use super::*;
    use crate::scene::{FactId, PropId, Spring};

    #[test]
    fn a_follow_is_worked_out_again_only_for_what_it_read() {
        // follow p = if(fact 0, fact 1, 7): the dock's place under the mouse, with no dock up.
        let to = Expr::Mix(Box::new(Expr::K(7.0)), Box::new(Expr::H(FactId(1))), Box::new(Expr::H(FactId(0))));
        let behaviors = vec![Behavior::Follow { prop: PropId(0), to: to.clone() }];
        let props = vec![Animated::at(0.0, Spring::LIVELY)];
        let mut follows = Follows::default();
        follows.begin(&behaviors);
        assert_eq!(follows.value(0, &to, &props, &[0.0, 5.0]), 7.0);
        let read = |f: &Follows| f.each[0].as_ref().unwrap().facts.clone();
        assert_eq!(read(&follows), [0]);
        // The mouse moves (fact 1): not read, so it is not worked out again…
        follows.each[0].as_mut().unwrap().value = Some(-1.0);
        assert_eq!(follows.value(0, &to, &props, &[0.0, 9.0]), -1.0);
        // …until the dock is up (fact 0), and then it reads the mouse and follows it.
        assert_eq!(follows.value(0, &to, &props, &[1.0, 9.0]), 9.0);
        assert_eq!(read(&follows), [0, 1]);
        assert_eq!(follows.value(0, &to, &props, &[1.0, 12.0]), 12.0);
        assert_eq!(follows.value(0, &to, &props, &[0.0, 12.0]), 7.0);
        assert_eq!(read(&follows), [0]);
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn a_double_click_takes_the_word() {
        let t = "Hola, ¿qué tal? it's fine_ok";
        assert_eq!(&t[word_around(t, 1).0..word_around(t, 1).1], "Hola");
        let q = t.find("qué").unwrap() + 2; // inside the «é»: still that word
        let (a, b) = word_around(t, q);
        assert_eq!(&t[a..b], "qué");
        let (a, b) = word_around(t, t.find("it's").unwrap());
        assert_eq!(&t[a..b], "it's");
        let (a, b) = word_around(t, t.find("fine").unwrap());
        assert_eq!(&t[a..b], "fine_ok");
        // On a space or a sign, just that.
        let (a, b) = word_around(t, 4);
        assert_eq!(&t[a..b], ",");
        assert_eq!(word_around(t, t.len()), (t.len(), t.len()));
    }

    #[test]
    fn a_selection_stays_inside_a_text_that_shrank() {
        let s = TextSelection { at: 0, anchor: 40, cursor: 2, dragging: false };
        assert_eq!(s.bytes("ñandú"), (2, "ñandú".len()));
        let s = TextSelection { at: 0, anchor: 1, cursor: 3, dragging: false };
        assert_eq!(s.bytes("ñandú"), (0, 3));
    }
}

/// A step of an agent's hand.
#[derive(Clone)]
enum Step {
    /// Its list scrolls until it is in sight, or gives up at that moment.
    Scroll(Instant),
    /// The hand goes to the zone: to a point of it that is on top.
    Point,
    /// And from there, so far: a drag.
    Move(f32, f32),
    Down(u8),
    Up(u8),
    Wheel(f32),
    /// The session's agent cursor is on its way to the hand: the press waits
    /// for it to arrive (300 ms at most), so it is never behind the action.
    Cursor(std::sync::Arc<std::sync::Mutex<crate::platform::CursorTrip>>, Instant),
    /// So long from when it is reached: it becomes a `Wait`.
    Pause(Duration),
    Wait(Instant),
    /// The hand goes away, and the user's pointer is the pointer again.
    Leave,
    /// A field gets this, as if typed over what it had.
    Type(usize, String),
    Key(String, Mods, Option<String>),
}

/// An action an agent asked for by name, while it is being done.
struct Acting {
    reply: std::sync::mpsc::Sender<String>,
    zone: usize,
    said_name: String,
    /// What it answers before what happened: «pressed save».
    said: String,
    steps: std::collections::VecDeque<Step>,
    hand: Option<(f32, f32)>,
    from: (f32, f32),
    scrolled: bool,
    before: crate::agent::Before,
    events: Vec<(&'static str, &'static str)>,
    done: Option<Instant>,
    heard: String,
    quiet: Instant,
}

impl Acting {
    /// The steps for an action, or why it cannot be done (with whom to tell).
    #[allow(clippy::too_many_arguments, clippy::result_large_err)]
    fn plan(scene: &Scene, c: Ctx, sight: &crate::agent::Sight, act: crate::agent::Act, reply: std::sync::mpsc::Sender<String>, texts: &[String], open: Vec<String>) -> Result<Acting, (std::sync::mpsc::Sender<String>, String)> {
        use crate::agent::Act;
        let now = Instant::now();
        let ms = |n: u64| now + Duration::from_millis(n);
        let mut steps = std::collections::VecDeque::new();
        let mut zone = usize::MAX;
        let name = act.name().to_owned();
        if !matches!(act, Act::Key { .. }) {
            let Some(k) = crate::agent::locate(scene, c, &name, sight) else {
                return Err((reply, format!("? there is nothing called '{name}' on screen: `describe` says what there is")));
            };
            zone = k;
            if let Err(m) = crate::agent::reach_point(scene, c, k, sight) {
                return Err((reply, m));
            }
            steps.push_back(Step::Scroll(ms(1500)));
        }
        let said = match &act {
            Act::Press { button, count, .. } => {
                steps.push_back(Step::Point);
                for i in 0..*count {
                    if i > 0 {
                        steps.push_back(Step::Pause(Duration::from_millis(60)));
                    }
                    steps.push_back(Step::Down(*button));
                    steps.push_back(Step::Pause(Duration::from_millis(40)));
                    steps.push_back(Step::Up(*button));
                }
                steps.push_back(Step::Leave);
                let how = match (button, count) {
                    (0, 1) => String::new(),
                    (b, n) => format!(" ({}{})", ["left", "right", "middle"][*b as usize], if *n > 1 { format!(", {n} times") } else { String::new() }),
                };
                format!("pressed {name}{how}")
            }
            Act::Hold { .. } => {
                // As long as its `on hold` asks, and a little more.
                let wait = scene.rules.iter().filter_map(|r| match &r.when { crate::scene::Trigger::Hold { zone: z, duration } if z.0 as usize == zone => Some(*duration), _ => None }).max().unwrap_or(Duration::from_millis(600));
                steps.push_back(Step::Point);
                steps.push_back(Step::Down(0));
                steps.push_back(Step::Pause(wait + Duration::from_millis(60)));
                steps.push_back(Step::Up(0));
                steps.push_back(Step::Leave);
                format!("held {name}")
            }
            Act::Drag { by, .. } => {
                steps.push_back(Step::Point);
                steps.push_back(Step::Down(0));
                for i in 1..=12 {
                    let f = i as f32 / 12.0;
                    steps.push_back(Step::Pause(Duration::from_millis(40)));
                    steps.push_back(Step::Move(by.0 * f, by.1 * f));
                }
                steps.push_back(Step::Up(0));
                steps.push_back(Step::Leave);
                format!("dragged {name} by {}, {}", by.0, by.1)
            }
            Act::Wheel { notches, .. } => {
                steps.push_back(Step::Point);
                steps.push_back(Step::Wheel(*notches));
                steps.push_back(Step::Pause(Duration::from_millis(60)));
                steps.push_back(Step::Leave);
                format!("turned the wheel {notches} over {name}")
            }
            Act::Type { text, .. } => {
                let Some((k, _, _)) = crate::agent::field_of(scene, &scene.zones[zone]) else {
                    return Err((reply, format!("? {name} is not a field: `type` writes in an `input`")));
                };
                steps.push_back(Step::Type(k, text.clone()));
                format!("typed in {name}")
            }
            Act::Key { name, mods, typed } => {
                steps.push_back(Step::Key(name.clone(), *mods, typed.clone()));
                format!("key {name}")
            }
        };
        let said_name = name.split("#screen").next().unwrap_or(&name).to_owned();
        Ok(Acting { reply, zone, said_name, said: said + "\n", steps, hand: None, from: (0.0, 0.0), scrolled: false, before: crate::agent::before(scene, c, texts, open), events: Vec::new(), done: None, heard: String::new(), quiet: now })
    }
}

/// The surfaces and popups on screen, by name: what an action opened or closed.
fn open_surfaces(scene: &Scene, sheets: &[Sheet]) -> Vec<String> {
    let mut v: Vec<String> = sheets
        .iter()
        .filter(|s| s.open)
        .map(|s| match s.view.popup.and_then(|p| scene.popups.get(p)) {
            Some(p) => format!("popup {}", p.name),
            None => {
                let f = &scene.surfaces[s.view.surface];
                format!("surface {}", if f.name.is_empty() { "main" } else { &f.name })
            }
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Someone watching what happens in the scene (`watch`), until then.
struct Watcher {
    lines: std::sync::mpsc::Sender<String>,
    until: Instant,
    start: Instant,
    before: crate::agent::Before,
}

/// Where the agent's cursor goes for a point of the scene on that sheet: a
/// window's picture in its pixels, or a panel by its size, in its units.
fn cursor_place(scene: &Scene, s: &Sheet, p: (f32, f32)) -> (crate::platform::CursorOn, (f32, f32)) {
    let local = (p.0 - s.view.origin.0, p.1 - s.view.origin.1);
    match scene.surfaces.get(s.view.surface).is_some_and(|f| f.window.is_some()) {
        true => (crate::platform::CursorOn::Window, (local.0 * s.scale, local.1 * s.scale)),
        false => (crate::platform::CursorOn::Panel(s.view.size.0.round() as u32, s.view.size.1.round() as u32), local),
    }
}

/// A surface's pace now: the first lower `rate:` whose condition holds, from the
/// busiest down (`rate: 60, 20 while asleep, 5 while away`), or its `rate:`; 0 is
/// the monitor's own.
fn surface_rate(s: &crate::scene::Surface, c: Ctx) -> u32 {
    s.rate_while.iter().find(|(_, when)| when.is_true(c)).map_or(s.max_fps, |(low, _)| *low)
}

#[cfg(test)]
mod rule_index_tests {
    use super::*;

    #[test]
    fn a_scene_with_only_event_rules_looks_at_them() {
        let (scene, _) = crate::language::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/press-alone.plm")).unwrap();
        let mut index = RuleIndex::default();
        index.build(&scene.rules);
        index.wake(&[], &scene.rules);
        assert_eq!(index.awake_with_events.len(), 1);
        // Waking again with nothing asleep changes nothing, and keeps it.
        index.wake(&[], &scene.rules);
        assert!(!index.woke);
        assert_eq!(index.awake_with_events.len(), 1);
    }
}
