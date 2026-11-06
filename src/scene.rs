//! The contract between whoever thinks and whoever paints.
//!
//! A scene is *data*: named properties, a list of drawing instructions whose
//! numbers are expressions over those properties, behaviors the render carries
//! on its own (blinking, looking, breathing) and mouse-sensitive zones. The
//! render does not know what a little ball is: it evaluates, paints and reports.
//!
//! The logic never sends values: it sends *intentions* ("this property goes to
//! 406 with this spring, in 60 ms").

#![allow(dead_code)] // the contract runs ahead of the scenes that use it

use std::ops::{Add, Div, Mul, Sub};
use std::time::Duration;

/// A scene's names live as long as the program, but each one only once:
/// reloading the same file a hundred times costs no more than the first.
pub fn intern(s: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static TABLE: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut t = TABLE.lock().unwrap();
    let table = t.get_or_insert_with(HashSet::new);
    if let Some(existing) = table.get(s) {
        return existing;
    }
    let new: &'static str = Box::leak(s.to_owned().into_boxed_str());
    table.insert(new);
    new
}

// ── where the scene lives ───────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SurfaceAnchor {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

impl SurfaceAnchor {
    /// Which edges it is attached to, in the usual order: left, top, right,
    /// bottom. Against an attached edge there is no room to ask for —a top bar
    /// goes off the top because it wants to—, so that is where no warning is
    /// given that something gets cut off.
    pub fn attached_edges(&self) -> [bool; 4] {
        let (l, t, r, b) = match self {
            SurfaceAnchor::Top => (false, true, false, false),
            SurfaceAnchor::Bottom => (false, false, false, true),
            SurfaceAnchor::Left => (true, false, false, false),
            SurfaceAnchor::Right => (false, false, true, false),
            SurfaceAnchor::TopLeft => (true, true, false, false),
            SurfaceAnchor::TopRight => (false, true, true, false),
            SurfaceAnchor::BottomLeft => (true, false, false, true),
            SurfaceAnchor::BottomRight => (false, false, true, true),
            SurfaceAnchor::Center => (false, false, false, false),
        };
        [l, t, r, b]
    }
    pub fn from_word(p: &str) -> Option<SurfaceAnchor> {
        Some(match p {
            "top" => SurfaceAnchor::Top,
            "bottom" => SurfaceAnchor::Bottom,
            "left" => SurfaceAnchor::Left,
            "right" => SurfaceAnchor::Right,
            "top_left" => SurfaceAnchor::TopLeft,
            "top_right" => SurfaceAnchor::TopRight,
            "bottom_left" => SurfaceAnchor::BottomLeft,
            "bottom_right" => SurfaceAnchor::BottomRight,
            "center" => SurfaceAnchor::Center,
            _ => return None,
        })
    }
}

/// Which layer of the desktop: behind the windows or in front.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Level {
    Background,
    Below,
    Above,
    Overlay,
}

/// The monitors a text names for `screens:` (`"DP-1, HDMI-A-1"`): split at
/// commas, each name trimmed; an empty text names none.
pub fn monitor_names(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Screens {
    /// One surface on each monitor, and on the ones plugged in later.
    All,
    /// Only on these. A repeated name gives two surfaces on the same one.
    Named(Vec<String>),
    /// Monitor number k of the ones there are, in order. It is what `screens: each` does:
    /// one surface per monitor, each with its own state.
    Number(usize),
}

/// The surface a scene asks for. The size is in logical pixels: on a monitor
/// at scale 2 it is painted with twice as many real pixels.
#[derive(Clone, Debug)]
pub struct Surface {
    /// What it is called. The main one, "".
    pub name: String,
    /// If it comes from a `screens: each`: which of the copies it is. The logic and the drawing
    /// tell it apart by that (`screen.name`, `$screen`).
    pub instance: usize,
    /// Where its own part is drawn, inside the scene's space: each surface looks at
    /// a different piece of the same plane, like popups do. That way they all share
    /// properties, facts and rules without knowing about each other.
    pub origin: (f32, f32),
    /// It exists while this fact is true. Without it, always.
    pub open: Option<Expr>,
    /// 0 is "the whole width of the monitor".
    pub width: u32,
    pub height: u32,
    /// A named one publishes what it really measures: `name.width`, `name.height`.
    pub size_props: Option<(PropId, PropId)>,
    /// A named surface's `name.cursor.x`, `name.cursor.y`: the mouse wherever it
    /// is, in the coordinates this surface draws with.
    pub cursor_props: Option<(PropId, PropId)>,
    pub anchor: SurfaceAnchor,
    /// If the edge it attaches to is decided by a fact: which one, and which anchor
    /// each of its values corresponds to. Layer-shell lets it change on the
    /// fly, so something that has to pick a corner —Marea's recording face
    /// picks the one that falls outside what is being recorded— does not need four
    /// surfaces, one per corner.
    pub anchor_from: Option<(FactId, Vec<SurfaceAnchor>)>,
    /// `level: top, overlay while open`: the level it goes up to while that
    /// holds. Layer-shell lets a live surface change level.
    pub level_while: Option<(Level, Expr)>,
    /// Top, right, bottom, left.
    pub margin: [i32; 4],
    pub level: Level,
    /// If it is a normal window —the kind the compositor decorates and places— instead of
    /// a panel attached to the edge: what its title is. Without this, it is a panel.
    pub window: Option<String>,
    /// A window's own name to the desktop (`app_id: "notes"`): what its
    /// `.desktop` file is called, so the dock gives it its icon and groups its
    /// windows. Without it, "pleamar", like every other.
    pub app_id: Option<String>,
    /// If it is the lock screen: a surface that does NOT exist until its
    /// `open:` becomes true, and that, once it exists, has the session really
    /// locked —the compositor guarantees it, not the drawing—, on all
    /// monitors at once. When `open:` stops being true, it unlocks.
    pub lock_screen: bool,
    /// How much room the compositor reserves for it: windows do not step on it.
    pub exclusive_zone: i32,
    /// `reserve: 72 while taking_room`: the room is kept only while that holds.
    pub reserve_while: Option<Expr>,
    /// How many frames per second AT MOST, decided by whoever writes the
    /// scene and not by whatever monitor it lands on. 0 is "the monitor's". A bar
    /// that breathes does not need 165 frames per second, and painting them is what
    /// costs: with this, what it consumes is the same on any screen.
    /// Whether it says `rate:` itself; one that does not keeps the first surface's pace.
    pub rate_said: bool,
    pub max_fps: u32,
    /// `rate: 60, 20 while asleep, 5 while away`: fewer frames still while that
    /// holds —a character breathing in its sleep does not need sixty a second—,
    /// and as many steps as the scene has paces. The first one that holds is the
    /// one that rules, so they are kept in the order they were written.
    pub rate_while: Vec<(u32, Expr)>,
    pub screens: Screens,
    /// `screens: on_screen`, a text fact: the monitor it is on is that fact's
    /// value, and when the value changes the surface goes to the monitor it
    /// names, with the scene, its logic and its springs as they were. Empty, on
    /// none. (`screens:` is otherwise read once.)
    pub screens_from: Option<TextId>,
    pub keyboard: Keyboard,
    /// The keyboard is only requested while a condition holds: its own
    /// (`keyboard_when`) or, without one, the scene's (`Scene::keyboard_while`).
    pub keyboard_while: bool,
    /// Whether this surface says `keyboard:` itself; one that does not asks for
    /// the keyboard as the first surface does.
    pub keyboard_said: bool,
    /// Its own `keyboard: … while` condition, which only it obeys.
    pub keyboard_when: Option<Expr>,
    /// As long as no rule uses the right button, it closes the program.
    pub right_click_quits: bool,
    /// `captures: hidden`: seen on the monitor but left out of what is
    /// captured of it (a screenshot, a recording, a remote desktop), where
    /// the compositor can do that (pleamar-wm).
    pub hidden_from_captures: bool,
    /// `agent: hidden`: left out of what the scene tells an agent.
    pub agent_hidden: bool,
}

/// Whether the surface wants the keyboard. `OnDemand` is the normal thing in a panel with
/// something to type into; `Always` keeps it all to itself, like a launcher.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Keyboard {
    #[default]
    Never,
    OnDemand,
    Always,
}

impl Default for Surface {
    fn default() -> Self {
        // Neutral: full width, at the top, on all monitors. Whatever the scene asks for wins.
        Surface { name: String::new(), instance: 0, origin: (0.0, 0.0), open: None, window: None, app_id: None, lock_screen: false, width: 0, size_props: None, cursor_props: None, height: 40, anchor: SurfaceAnchor::Top, anchor_from: None, level_while: None, margin: [0; 4], level: Level::Above, exclusive_zone: 0, reserve_while: None, max_fps: 0, rate_said: false, rate_while: Vec::new(), screens: Screens::All, screens_from: None, keyboard: Keyboard::Never, keyboard_while: false, keyboard_said: false, keyboard_when: None, right_click_quits: true, hidden_from_captures: false, agent_hidden: false }
    }
}

// ── properties and expressions ──────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropId(pub u16);
/// Something the logic (or a rule) says is true. A number; yes and no are 1 and 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FactId(pub u16);
/// Something that happens at an instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalId(pub u16);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneId(pub u16);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GestureId(pub u16);
/// A text the logic can change: a fact, but made of letters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextId(pub u16);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageId(pub u16);

/// What an expression is evaluated with: what moves and what is true.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub props: &'a [Animated],
    pub facts: &'a [f32],
    /// Where to note what is read, for whoever wants to know what a result
    /// was made from (see `Seen`); `None`, nobody.
    pub seen: Option<&'a Seen>,
}

/// Which facts and properties were read while something was worked out: a
/// bit each. A result made only from those holds for as long as they do,
/// whatever else changes —a drawing that reads nothing of the mouse is the
/// same drawing wherever the mouse is—.
#[derive(Default, Clone)]
pub struct Seen {
    facts: Vec<std::cell::Cell<u64>>,
    props: Vec<std::cell::Cell<u64>>,
    /// While something is worked out on its own (`log_from_here`), each read
    /// too, in order: facts with `SEEN_FACT` on.
    logging: std::cell::Cell<bool>,
    log: std::cell::RefCell<Vec<u32>>,
}

/// In `Seen`'s log, what is a fact and not a property.
pub const SEEN_FACT: u32 = 1 << 31;

impl Seen {
    /// Nothing read yet, with room for that many of each.
    pub fn reset(&mut self, facts: usize, props: usize) {
        self.facts.clear();
        self.facts.resize(facts.div_ceil(64), std::cell::Cell::new(0));
        self.props.clear();
        self.props.resize(props.div_ceil(64), std::cell::Cell::new(0));
    }
    #[inline]
    pub fn fact(&self, k: usize) {
        if let Some(w) = self.facts.get(k >> 6) {
            w.set(w.get() | 1 << (k & 63));
        }
        if self.logging.get() {
            self.log.borrow_mut().push(k as u32 | SEEN_FACT);
        }
    }
    #[inline]
    pub fn prop(&self, k: usize) {
        if let Some(w) = self.props.get(k >> 6) {
            w.set(w.get() | 1 << (k & 63));
        }
        if self.logging.get() {
            self.log.borrow_mut().push(k as u32);
        }
    }
    /// From here on, each read is also written down, until `log_into`.
    pub fn log_from_here(&self) {
        self.log.borrow_mut().clear();
        self.logging.set(true);
    }
    /// What was read since `log_from_here`, once each, into `out`; and no more writing it down.
    pub fn log_into(&self, out: &mut Vec<u32>) {
        self.logging.set(false);
        let mut log = self.log.borrow_mut();
        log.sort_unstable();
        log.dedup();
        out.clear();
        out.extend_from_slice(&log);
    }
    fn each(words: &[std::cell::Cell<u64>]) -> impl Iterator<Item = usize> + '_ {
        words.iter().enumerate().flat_map(|(w, bits)| {
            let mut left = bits.get();
            std::iter::from_fn(move || {
                (left != 0).then(|| {
                    let k = left.trailing_zeros() as usize;
                    left &= left - 1;
                    w * 64 + k
                })
            })
        })
    }
    /// What is noted here, added to `into` too, and forgotten here: one
    /// stretch's reads kept apart from the next one's (see `DrawList::compose`).
    pub fn pass_to(&self, into: &Seen) {
        for (mine, theirs) in self.facts.iter().zip(&into.facts).chain(self.props.iter().zip(&into.props)) {
            theirs.set(theirs.get() | mine.get());
            mine.set(0);
        }
    }
    /// What `from` noted, noted here instead (and nothing else).
    pub fn take_from(&self, from: &Seen) {
        for (mine, theirs) in self.facts.iter().zip(&from.facts).chain(self.props.iter().zip(&from.props)) {
            mine.set(theirs.get());
        }
    }
    /// What was read, into those lists, and nothing read again: for whoever
    /// keeps one of these for many evaluations, one after another.
    pub fn take(&self, facts: &mut Vec<u16>, props: &mut Vec<u16>) {
        // A word at a time, and only the words with something: a scene with
        // sixteen thousand properties has hundreds of them, nearly all empty.
        fn drain(words: &[std::cell::Cell<u64>], out: &mut Vec<u16>) {
            out.clear();
            for (w, bits) in words.iter().enumerate() {
                let mut left = bits.get();
                if left == 0 {
                    continue;
                }
                bits.set(0);
                while left != 0 {
                    out.push((w * 64 + left.trailing_zeros() as usize) as u16);
                    left &= left - 1;
                }
            }
        }
        drain(&self.facts, facts);
        drain(&self.props, props);
    }
    /// The facts read, by their index.
    pub fn facts(&self) -> impl Iterator<Item = usize> + '_ {
        Seen::each(&self.facts)
    }
    /// The properties read, by their index.
    pub fn props(&self) -> impl Iterator<Item = usize> + '_ {
        Seen::each(&self.props)
    }
}

/// A pure expression over the properties. Since it has no side effects, the render
/// can evaluate it whenever and wherever it wants: it is what a binding would be.
#[derive(Clone, Debug)]
pub enum Expr {
    K(f32),
    P(PropId),
    H(FactId),
    /// The velocity of a property: the render knows it, the logic does not.
    Vel(PropId),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Min(Box<Expr>, Box<Expr>),
    Max(Box<Expr>, Box<Expr>),
    Abs(Box<Expr>),
    /// Rounding down or up: needed to count rows.
    Floor(Box<Expr>),
    /// Sine and cosine, in degrees: what it takes to place something on an arc.
    Sin(Box<Expr>),
    Cos(Box<Expr>),
    Ceil(Box<Expr>),
    /// smoothstep(a, b, x)
    Smoothstep(f32, f32, Box<Expr>),
    /// `mix(a, b, t)` and also `if(c, x, y)`, which is mixing with 0 or 1. Each
    /// side appears only once: written as `a + (b - a) * t`, `a` came out twice, and
    /// a chain of `if`s doubled the tree at every link. And with `t` exactly at
    /// 0 or at 1, which is what a condition gives, only one side is evaluated.
    Mix(Box<Expr>, Box<Expr>, Box<Expr>),
    // Conditions: true is > 0.5, and they return 1 or 0.
    Gt(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    /// The rest of the functions of one argument: `sqrt`, `fract`, `noise`…
    Un(Un, Box<Expr>),
    /// …and of two: `pow`, `atan2`, `mod`, `noise(x, y)`, `length`.
    Bin(Bin, Box<Expr>, Box<Expr>),
    /// Inside a text's `letter_*` properties: 0 which letter it is, 1 how many there are.
    Letter(u8),
    /// `pick(i, a, b, c)`: the one at place `i` (0 is the first), rounded and
    /// kept within the list. Only the chosen one is evaluated.
    Pick(Box<Expr>, Vec<Expr>),
    /// `on chosen(v) { … }`: the value the event arrived with. The render puts
    /// it in when it picks up the rule (`with_payload`); anywhere else it is 0.
    Payload,
}

/// Runs a `Sort`: each slot's keys, the slots in their order, and each
/// property written. What changed is handed to `changed`.
pub fn run_sort(keys: &[Vec<Expr>], ord: &[PropId], pos: &[PropId], props: &mut [Animated], facts: &[f32], mut changed: impl FnMut(PropId)) {
    // `+ 0.0` makes -0 the 0 it equals: keys that are the same tie, by slot.
    let values: Vec<Vec<f32>> = keys.iter().map(|k| k.iter().map(|e| e.eval(Ctx { props: &*props, facts, seen: None }) + 0.0).collect()).collect();
    let mut slots: Vec<usize> = (0..keys.len()).collect();
    slots.sort_by(|a, b| values[*a].iter().zip(&values[*b]).map(|(x, y)| x.total_cmp(y)).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal));
    for (p, slot) in slots.into_iter().enumerate() {
        for (prop, v) in [(ord[p], slot as f32), (pos[slot], p as f32)] {
            let a = &mut props[prop.0 as usize];
            if a.x.to_bits() != v.to_bits() {
                changed(prop);
            }
            a.set(v);
        }
    }
}

impl Expr {
    /// What it reads: the properties (their value or their speed) and the
    /// facts. Nothing else goes into it —no clock, no chance—, so with those the
    /// same, it is worth the same.
    /// Whether it depends on nothing but the facts and properties `inputs` gives:
    /// not on an event's payload, nor on the letter being painted. Then, with
    /// those the same, it is worth the same (`random` and `noise` are seeded).
    pub fn only_inputs(&self) -> bool {
        match self {
            Expr::Payload | Expr::Letter(_) => false,
            Expr::K(_) | Expr::P(_) | Expr::Vel(_) | Expr::H(_) => true,
            Expr::Abs(a) | Expr::Floor(a) | Expr::Sin(a) | Expr::Cos(a) | Expr::Ceil(a) | Expr::Not(a) | Expr::Un(_, a) | Expr::Smoothstep(_, _, a) => a.only_inputs(),
            Expr::Add(a, b) | Expr::Sub(a, b) | Expr::Mul(a, b) | Expr::Div(a, b) | Expr::Min(a, b) | Expr::Max(a, b) | Expr::Gt(a, b) | Expr::And(a, b) | Expr::Or(a, b) | Expr::Bin(_, a, b) => a.only_inputs() && b.only_inputs(),
            Expr::Mix(a, b, t) => a.only_inputs() && b.only_inputs() && t.only_inputs(),
            Expr::Pick(i, list) => i.only_inputs() && list.iter().all(Expr::only_inputs),
        }
    }

    pub fn inputs(&self, props: &mut Vec<u16>, facts: &mut Vec<u16>) {
        match self {
            Expr::P(p) | Expr::Vel(p) => {
                if !props.contains(&p.0) {
                    props.push(p.0);
                }
            }
            Expr::H(h) => {
                if !facts.contains(&h.0) {
                    facts.push(h.0);
                }
            }
            Expr::K(_) | Expr::Letter(_) | Expr::Payload => {}
            Expr::Abs(a) | Expr::Floor(a) | Expr::Sin(a) | Expr::Cos(a) | Expr::Ceil(a) | Expr::Not(a) | Expr::Un(_, a) | Expr::Smoothstep(_, _, a) => a.inputs(props, facts),
            Expr::Add(a, b) | Expr::Sub(a, b) | Expr::Mul(a, b) | Expr::Div(a, b) | Expr::Min(a, b) | Expr::Max(a, b) | Expr::Gt(a, b) | Expr::And(a, b) | Expr::Or(a, b) | Expr::Bin(_, a, b) => {
                a.inputs(props, facts);
                b.inputs(props, facts);
            }
            Expr::Mix(a, b, t) => {
                a.inputs(props, facts);
                b.inputs(props, facts);
                t.inputs(props, facts);
            }
            Expr::Pick(i, list) => {
                i.inputs(props, facts);
                for e in list {
                    e.inputs(props, facts);
                }
            }
        }
    }
}

thread_local! {
    /// The letter being painted, and how many: what `Expr::Letter` reads. Set by
    /// the render around each letter of a text with `letter_*` properties, on
    /// its own thread; anywhere else it is 0.
    pub static LETTER: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((0.0, 0.0)) };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Un {
    Sqrt,
    Fract,
    Sign,
    Round,
    Exp,
    Ln,
    /// In degrees, like `sin` and `cos`.
    Tan,
    /// Smooth noise in one dimension, from −1 to 1: the same `x`, the same value.
    Noise,
    /// A number from 0 to 1 that looks random and is always the same for the same seed.
    Random,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bin {
    Pow,
    /// The angle of the point (x, y), in degrees: `atan2(y, x)`.
    Atan2,
    /// The remainder, always positive: `mod(-1, 3)` is 2, which is what a
    /// thing going round in a circle needs.
    Mod,
    /// Smooth noise in two dimensions, from −1 to 1.
    Noise2,
    /// How long the vector (x, y) is.
    Length,
}

/// A hash of an integer to [0, 1): the same for the same number, on every
/// machine and in every version.
fn hash01(n: i32) -> f32 {
    let mut x = n as u32;
    x = (x ^ 61) ^ (x >> 16);
    x = x.wrapping_add(x << 3);
    x ^= x >> 4;
    x = x.wrapping_mul(0x27d4eb2d);
    x ^= x >> 15;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// Gradient noise: a slope at each integer, eased between them. It passes
/// through 0 at the integers and never jumps.
pub fn noise1(x: f32) -> f32 {
    let i = x.floor();
    let f = x - i;
    let g = |k: f32| hash01(k as i32) * 2.0 - 1.0;
    let u = f * f * (3.0 - 2.0 * f);
    let a = g(i) * f;
    let b = g(i + 1.0) * (f - 1.0);
    (a + (b - a) * u) * 2.0
}

pub fn noise2(x: f32, y: f32) -> f32 {
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (x - ix, y - iy);
    let grad = |cx: f32, cy: f32, dx: f32, dy: f32| {
        let a = hash01((cx as i32).wrapping_mul(73856093) ^ (cy as i32).wrapping_mul(19349663)) * std::f32::consts::TAU;
        a.cos() * dx + a.sin() * dy
    };
    let (ux, uy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let n00 = grad(ix, iy, fx, fy);
    let n10 = grad(ix + 1.0, iy, fx - 1.0, fy);
    let n01 = grad(ix, iy + 1.0, fx, fy - 1.0);
    let n11 = grad(ix + 1.0, iy + 1.0, fx - 1.0, fy - 1.0);
    let a = n00 + (n10 - n00) * ux;
    let b = n01 + (n11 - n01) * ux;
    ((a + (b - a) * uy) * std::f32::consts::SQRT_2).clamp(-1.0, 1.0)
}

impl Un {
    pub fn apply(self, x: f32) -> f32 {
        match self {
            Un::Sqrt => x.max(0.0).sqrt(),
            Un::Fract => x - x.floor(),
            Un::Sign => if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 },
            Un::Round => x.round(),
            Un::Exp => x.exp(),
            Un::Ln => x.max(f32::MIN_POSITIVE).ln(),
            Un::Tan => x.to_radians().tan(),
            Un::Noise => noise1(x),
            Un::Random => hash01(x.floor() as i32),
        }
    }
}

impl Bin {
    pub fn apply(self, a: f32, b: f32) -> f32 {
        match self {
            Bin::Pow => {
                let v = a.powf(b);
                if v.is_finite() { v } else { 0.0 }
            }
            Bin::Atan2 => a.atan2(b).to_degrees(),
            Bin::Mod => if b == 0.0 { 0.0 } else { a.rem_euclid(b) },
            Bin::Noise2 => noise2(a, b),
            Bin::Length => a.hypot(b),
        }
    }
}

/// Which of `n` a `pick` takes: rounded, and kept within the list.
pub fn pick_index(v: f32, n: usize) -> usize {
    if n == 0 || !v.is_finite() {
        return 0;
    }
    (v.round().max(0.0) as usize).min(n - 1)
}

impl Expr {
    pub fn eval(&self, c: Ctx) -> f32 {
        use Expr::*;
        match self {
            K(v) => *v,
            P(p) => {
                if let Some(s) = c.seen {
                    s.prop(p.0 as usize);
                }
                c.props[p.0 as usize].x
            }
            H(h) => {
                if let Some(s) = c.seen {
                    s.fact(h.0 as usize);
                }
                c.facts[h.0 as usize]
            }
            Vel(p) => {
                if let Some(s) = c.seen {
                    s.prop(p.0 as usize);
                }
                c.props[p.0 as usize].v
            }
            Add(a, b) => a.eval(c) + b.eval(c),
            Sub(a, b) => a.eval(c) - b.eval(c),
            Mul(a, b) => a.eval(c) * b.eval(c),
            Div(a, b) => a.eval(c) / b.eval(c),
            Min(a, b) => a.eval(c).min(b.eval(c)),
            Max(a, b) => a.eval(c).max(b.eval(c)),
            Abs(a) => a.eval(c).abs(),
            Floor(a) => a.eval(c).floor(),
            Sin(a) => a.eval(c).to_radians().sin(),
            Cos(a) => a.eval(c).to_radians().cos(),
            Ceil(a) => a.eval(c).ceil(),
            Smoothstep(a, b, x) => {
                let t = ((x.eval(c) - a) / (b - a)).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            }
            Mix(a, b, t) => match t.eval(c) {
                0.0 => a.eval(c),
                1.0 => b.eval(c),
                t => {
                    let a = a.eval(c);
                    a + (b.eval(c) - a) * t
                }
            },
            Gt(a, b) => (a.eval(c) > b.eval(c)) as u8 as f32,
            And(a, b) => (a.eval(c) > 0.5 && b.eval(c) > 0.5) as u8 as f32,
            Or(a, b) => (a.eval(c) > 0.5 || b.eval(c) > 0.5) as u8 as f32,
            Not(a) => (a.eval(c) <= 0.5) as u8 as f32,
            Un(f, a) => f.apply(a.eval(c)),
            Bin(f, a, b) => f.apply(a.eval(c), b.eval(c)),
            Letter(k) => LETTER.with(|l| if *k == 0 { l.get().0 } else { l.get().1 }),
            Pick(i, options) => options.get(pick_index(i.eval(c), options.len())).map_or(0.0, |e| e.eval(c)),
            Payload => 0.0,
        }
    }
    pub fn is_true(&self, c: Ctx) -> bool {
        self.eval(c) > 0.5
    }
    /// How many nodes it has. A `let` is substituted where it is used, so this is
    /// what it costs each time it is named, and what decides whether it is worth
    /// computing it just once.
    /// Whether any of the facts it reads meets the condition.
    pub fn reads(&self, f: impl Fn(FactId) -> bool + Copy) -> bool {
        use Expr::*;
        match self {
            K(_) | P(_) | Vel(_) | Letter(_) | Payload => false,
            H(h) => f(*h),
            Abs(a) | Floor(a) | Sin(a) | Cos(a) | Ceil(a) | Not(a) | Smoothstep(_, _, a) | Un(_, a) => a.reads(f),
            Add(a, b) | Sub(a, b) | Mul(a, b) | Div(a, b) | Min(a, b) | Max(a, b) | Gt(a, b) | And(a, b) | Or(a, b) | Bin(_, a, b) => a.reads(f) || b.reads(f),
            Mix(a, b, t) => a.reads(f) || b.reads(f) || t.reads(f),
            Pick(i, options) => i.reads(f) || options.iter().any(|e| e.reads(f)),
        }
    }
    pub fn node_count(&self) -> usize {
        use Expr::*;
        match self {
            K(_) | P(_) | H(_) | Vel(_) | Letter(_) | Payload => 1,
            Abs(a) | Floor(a) | Sin(a) | Cos(a) | Ceil(a) | Not(a) | Smoothstep(_, _, a) | Un(_, a) => 1 + a.node_count(),
            Add(a, b) | Sub(a, b) | Mul(a, b) | Div(a, b) | Min(a, b) | Max(a, b) | Gt(a, b) | And(a, b) | Or(a, b) | Bin(_, a, b) => 1 + a.node_count() + b.node_count(),
            Mix(a, b, t) => 1 + a.node_count() + b.node_count() + t.node_count(),
            Pick(i, options) => 1 + i.node_count() + options.iter().map(Expr::node_count).sum::<usize>(),
        }
    }
    /// What it is worth, if it depends on nothing.
    pub fn constant(&self) -> Option<f32> {
        match self {
            Expr::K(v) => Some(*v),
            _ => None,
        }
    }
    /// Whatever can be known when reading the scene is known once, not every frame:
    /// `2 * 3` is `6`, `-4` is `-4` (and not `0 - 4`), `x * 1` is `x`. The children
    /// come already folded, because it is built bottom up.
    fn folded(self) -> Expr {
        use Expr::*;
        let k = |e: &Expr| e.constant();
        let constant_children = match &self {
            K(_) | P(_) | H(_) | Vel(_) | Letter(_) | Payload => return self,
            Abs(a) | Floor(a) | Sin(a) | Cos(a) | Ceil(a) | Not(a) | Smoothstep(_, _, a) | Un(_, a) => k(a).is_some(),
            Add(a, b) | Sub(a, b) | Mul(a, b) | Div(a, b) | Min(a, b) | Max(a, b) | Gt(a, b) | And(a, b) | Or(a, b) | Bin(_, a, b) => k(a).is_some() && k(b).is_some(),
            Mix(a, b, t) => k(a).is_some() && k(b).is_some() && k(t).is_some(),
            // With the place known, it is that one, whatever the others are.
            Pick(i, options) => match k(i) {
                Some(v) => return options.get(pick_index(v, options.len())).cloned().unwrap_or(K(0.0)),
                None => false,
            },
        };
        if constant_children {
            return K(self.eval(Ctx { props: &[], facts: &[], seen: None }));
        }
        match self {
            Add(a, b) if k(&b) == Some(0.0) => *a,
            Add(a, b) if k(&a) == Some(0.0) => *b,
            Sub(a, b) if k(&b) == Some(0.0) => *a,
            Mul(a, b) if k(&b) == Some(1.0) => *a,
            Mul(a, b) if k(&a) == Some(1.0) => *b,
            Div(a, b) if k(&b) == Some(1.0) => *a,
            Mix(a, b, t) => match k(&t) {
                Some(0.0) => *a,
                Some(1.0) => *b,
                _ => Mix(a, b, t),
            },
            other => other,
        }
    }
    /// The same expression with the event's value in place of `Payload`.
    pub fn with_payload(&self, v: f32) -> Expr {
        use Expr::*;
        let b = |e: &Expr| Box::new(e.with_payload(v));
        match self {
            Payload => K(v),
            K(_) | P(_) | H(_) | Vel(_) | Letter(_) => self.clone(),
            Add(a, c) => Add(b(a), b(c)),
            Sub(a, c) => Sub(b(a), b(c)),
            Mul(a, c) => Mul(b(a), b(c)),
            Div(a, c) => Div(b(a), b(c)),
            Min(a, c) => Min(b(a), b(c)),
            Max(a, c) => Max(b(a), b(c)),
            Gt(a, c) => Gt(b(a), b(c)),
            And(a, c) => And(b(a), b(c)),
            Or(a, c) => Or(b(a), b(c)),
            Bin(f, a, c) => Bin(*f, b(a), b(c)),
            Abs(a) => Abs(b(a)),
            Floor(a) => Floor(b(a)),
            Sin(a) => Sin(b(a)),
            Cos(a) => Cos(b(a)),
            Ceil(a) => Ceil(b(a)),
            Not(a) => Not(b(a)),
            Un(f, a) => Un(*f, b(a)),
            Smoothstep(x, y, a) => Smoothstep(*x, *y, b(a)),
            Mix(a, c, t) => Mix(b(a), b(c), b(t)),
            Pick(i, o) => Pick(b(i), o.iter().map(|e| e.with_payload(v)).collect()),
        }
    }
    /// The same folding, for whoever builds a node by hand (`pick`).
    pub fn folded_pub(self) -> Expr {
        self.folded()
    }
        pub fn mix(self, b: impl Into<Expr>, t: impl Into<Expr>) -> Expr {
        Expr::Mix(Box::new(self), Box::new(b.into()), Box::new(t.into())).folded()
    }
    pub fn gt(self, o: impl Into<Expr>) -> Expr {
        Expr::Gt(Box::new(self), Box::new(o.into())).folded()
    }
    pub fn and(self, o: impl Into<Expr>) -> Expr {
        Expr::And(Box::new(self), Box::new(o.into())).folded()
    }
    pub fn or(self, o: impl Into<Expr>) -> Expr {
        Expr::Or(Box::new(self), Box::new(o.into())).folded()
    }
    pub fn not(self) -> Expr {
        Expr::Not(Box::new(self)).folded()
    }
    pub fn min(self, o: impl Into<Expr>) -> Expr {
        Expr::Min(Box::new(self), Box::new(o.into())).folded()
    }
    pub fn max(self, o: impl Into<Expr>) -> Expr {
        Expr::Max(Box::new(self), Box::new(o.into())).folded()
    }
    pub fn clamp(self, a: f32, b: f32) -> Expr {
        self.max(a).min(b)
    }
    pub fn abs(self) -> Expr {
        Expr::Abs(Box::new(self)).folded()
    }
    pub fn sin(self) -> Expr {
        Expr::Sin(Box::new(self)).folded()
    }
    pub fn cos(self) -> Expr {
        Expr::Cos(Box::new(self)).folded()
    }
    pub fn floor(self) -> Expr {
        Expr::Floor(Box::new(self)).folded()
    }
    pub fn ceil(self) -> Expr {
        Expr::Ceil(Box::new(self)).folded()
    }
    pub fn smoothstep(self, a: f32, b: f32) -> Expr {
        Expr::Smoothstep(a, b, Box::new(self)).folded()
    }
    pub fn un(self, f: Un) -> Expr {
        Expr::Un(f, Box::new(self)).folded()
    }
    pub fn bin(self, f: Bin, o: impl Into<Expr>) -> Expr {
        Expr::Bin(f, Box::new(self), Box::new(o.into())).folded()
    }
}

/// a·(1−t) + b·t
pub fn mix(a: f32, b: f32, t: impl Into<Expr>) -> Expr {
    t.into() * (b - a) + a
}

impl PropId {
    pub fn e(self) -> Expr {
        Expr::P(self)
    }
    pub fn vel(self) -> Expr {
        Expr::Vel(self)
    }
}
impl FactId {
    pub fn e(self) -> Expr {
        Expr::H(self)
    }
}
impl From<FactId> for Expr {
    fn from(h: FactId) -> Expr {
        Expr::H(h)
    }
}
impl From<f32> for Expr {
    fn from(v: f32) -> Expr {
        Expr::K(v)
    }
}
impl From<PropId> for Expr {
    fn from(p: PropId) -> Expr {
        Expr::P(p)
    }
}

macro_rules! operator {
    ($op_trait:ident, $method:ident, $variant:ident) => {
        impl<T: Into<Expr>> $op_trait<T> for Expr {
            type Output = Expr;
            fn $method(self, o: T) -> Expr {
                Expr::$variant(Box::new(self), Box::new(o.into())).folded()
            }
        }
        impl<T: Into<Expr>> $op_trait<T> for PropId {
            type Output = Expr;
            fn $method(self, o: T) -> Expr {
                Expr::$variant(Box::new(self.into()), Box::new(o.into())).folded()
            }
        }
        impl $op_trait<Expr> for f32 {
            type Output = Expr;
            fn $method(self, o: Expr) -> Expr {
                Expr::$variant(Box::new(self.into()), Box::new(o)).folded()
            }
        }
        impl $op_trait<PropId> for f32 {
            type Output = Expr;
            fn $method(self, o: PropId) -> Expr {
                Expr::$variant(Box::new(self.into()), Box::new(o.into())).folded()
            }
        }
    };
}
operator!(Add, add, Add);
operator!(Sub, sub, Sub);
operator!(Mul, mul, Mul);
operator!(Div, div, Div);

// ── what gets painted ───────────────────────────────────────────

pub type Point = (Expr, Expr);

pub use crate::shapes::{Affine, PathStep, Shape};

#[derive(Clone, Debug)]
pub struct Shadow {
    /// Everything in it is an expression, as in the rest of the scene: a shadow
    /// that cannot change forces a choice between the halo a little ball wants
    /// and the shadow a panel wants, when they are the same body.
    pub offset: (Expr, Expr),
    pub blur: Expr,
    pub alpha: Expr,
    /// What color. Black if not said, which is what a shadow is on
    /// paper; on a desktop of dark windows, a black shadow has
    /// nothing to darken and what separates a thing from the background is a light
    /// halo. That is why it can be said, and why it is an expression: it can keep
    /// changing from halo to shadow depending on what is underneath.
    pub color: Option<Color>,
}

/// `translations`: each language with its table, original → translated.
pub type Translations = Vec<(String, std::collections::HashMap<String, String>)>;

/// A glass: how much (`glass`, from 0 to 1) and whether it bends what is behind like a
/// lens (`lens`, true or not) or only lets it be seen blurred.
#[derive(Clone, Debug)]
pub struct Glass {
    pub amount: Expr,
    pub lens: Expr,
    /// `shine: x, y`: where the light it catches comes from, a point in the
    /// scene. Without it, from the top left.
    pub shine: Option<(Expr, Expr)>,
    /// `refraction:` how thick the glass is: how much it bends (1 = as it came).
    pub refraction: Expr,
    /// `dispersion:` how far red, green and blue come apart at the edge.
    pub dispersion: Expr,
    /// `dome:` how much the middle magnifies (0 = flat).
    pub dome: Expr,
    /// `ripple:` how much a press ripples it (0 = not at all).
    pub ripple: Expr,
    /// `frost:` how frosted it is inside its bevel: 1 frosted (as it was), 0
    /// clear, and −1 `auto`, frosted only where what is behind is busy.
    pub frost: Expr,
    /// `smoke:` smoked glass, which takes down what is seen through it, the
    /// brighter the more, so light words on it read (0 = none).
    pub smoke: Expr,
    /// `milk:` milky glass, which lifts what is seen through it, the darker the
    /// more, so dark words on it read (0 = none).
    pub milk: Expr,
    /// `tint:` how much of its own colour is laid over what is seen through
    /// it (22 %), and more over something bright or busy, so words on it read.
    pub tint: Expr,
    /// `vivid:` the saturation of what is seen through it (118 %; 100 % as it is).
    pub vivid: Expr,
    /// `mirror:` how much its rim mirrors what is around it (100 %; 0 none).
    pub mirror: Expr,
    /// `highlight:` how bright the light caught on its rim is (100 %; 0 none).
    pub highlight: Expr,
}

/// A vertical gradient of lightness, so the body is not flat.
#[derive(Clone, Debug)]
pub struct Light {
    pub amount: f32,
    pub from_y: Expr,
    pub height: f32,
}

pub type Color = [Expr; 3];

/// A text's effects: a gradient across its letters, an outline, a shadow,
/// and each letter moved, faded or scaled on its own (`letter` is which one,
/// `letters` how many). It goes right before the `Text` it belongs to.
#[derive(Clone, Debug)]
pub struct TextFx {
    pub gradient: Option<Paint>,
    pub outline: Option<(Expr, Color)>,
    pub shadow: Option<Shadow>,
    pub letter_move: Option<Point>,
    pub letter_opacity: Option<Expr>,
    pub letter_scale: Option<Expr>,
}

/// `particles sparks { at: x, y; count: 300; life: 0.6s .. 1.4s; … }`.
///
/// No particle is kept anywhere: each one is worked out in the shader from its
/// number and the time —when it was born, where it went, how it fell—, so
/// thousands cost what their pixels cost. The only state is the emitter's: when
/// it was switched on and off, when its last burst was, and where it was
/// going, so that what it leaves behind stays where it was left.
#[derive(Clone, Debug)]
pub struct Particles {
    pub at: Point,
    /// The emitter is a box this size, centred on `at`; 0, 0 is a point.
    pub area: Point,
    pub count: u32,
    /// Seconds, from one to the other: each particle its own.
    pub life: (Expr, Expr),
    /// Pixels per second.
    pub speed: (Expr, Expr),
    /// Where they go and how open the fan is, in radians (`-90deg` is up).
    pub direction: Expr,
    pub spread: Expr,
    pub gravity: Point,
    /// How much the air brakes them: 0 nothing.
    pub drag: Expr,
    /// At birth and at death.
    pub size: (Expr, Expr),
    pub colors: (Color, Color),
    pub opacity: (Expr, Expr),
    pub shape: ParticleShape,
    /// Continuous: while this is true, they keep being born.
    pub emit: Expr,
    /// Or all at once, each time this event happens.
    pub burst: Option<SignalId>,
    pub alpha: Expr,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParticleShape {
    Dot,
    Square,
    /// A streak along where it is going: sparks, rain.
    Spark,
}

/// What a `group` does to what it holds when blending it: `blur: 12`,
/// `glow: 16, 80%, mint`, `saturation: 0`, `brightness: 1.2`, `contrast: 1.1`,
/// `hue: 40deg`, `mask: …`, `mode: add`, `shader: rain, 0.8`.
#[derive(Clone, Debug)]
pub struct Effects {
    pub alpha: Expr,
    pub blur: Option<Expr>,
    /// Radius, how much, and its colour —without one, the colours of what is inside—.
    pub glow: Option<(Expr, Expr, Option<Color>)>,
    pub saturation: Option<Expr>,
    pub brightness: Option<Expr>,
    pub contrast: Option<Expr>,
    pub hue: Option<Expr>,
    pub mask: Option<Mask>,
    /// How it blends with what is under it: 0 covering, 1 `add` (adds light),
    /// 2 `screen` (lightens), 3 `multiply` (darkens what the scene painted under it).
    pub mode: u8,
    /// One of the scene's own shaders, over what the group holds: it reads it
    /// with `inside(s, at)` and returns what is seen instead.
    pub shader: Option<GroupShader>,
}

/// `shader: rain, 0.8, open`: which one, its numbers, and the time if it reads it.
#[derive(Clone, Debug)]
pub struct GroupShader {
    pub shader: u16,
    pub values: Vec<Expr>,
    pub time: Option<Expr>,
    /// `… while bending`: only while that holds. Otherwise the group is
    /// painted as if it had no shader, and without its pass if nothing else asks for one.
    pub on: Option<Expr>,
}

/// Where a group can be seen: whole at the start, nothing at the end.
#[derive(Clone, Debug)]
pub enum Mask {
    /// `mask: x1, y1 to x2, y2`.
    Linear(Point, Point),
    /// `mask: radial x, y radius r1 to r2`: whole up to r1, nothing from r2.
    Radial(Point, Expr, Expr),
}

/// A group with effects inside another group with opacity: the outer one gives
/// up its layer (see `Instr::Fade`). Called once, when the scene has been read.
pub fn settle_effects(instrs: &mut [Instr]) {
    let mut open: Vec<(usize, bool)> = Vec::new();
    for k in 0..instrs.len() {
        match &instrs[k] {
            Instr::Opacity(Some(_)) => open.push((k, false)),
            Instr::Fade(_) => open.push((k, false)),
            Instr::Effect(_) => {
                for o in open.iter_mut() {
                    o.1 = true;
                }
                open.push((k, false));
            }
            Instr::Opacity(None) => {
                if let Some((at, holds_effects)) = open.pop() {
                    if holds_effects {
                        if let Instr::Opacity(Some(a)) = &instrs[at] {
                            instrs[at] = Instr::Fade(a.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
pub enum Paint {
    Color(Color),
    /// A gradient from one point to another, or from a center outwards. The stops
    /// go in order, each with where it falls (from 0 to 1) and what color it is.
    Gradient { radial: bool, from: Point, to: Point, stops: Vec<(Expr, Color)> },
}
impl From<Color> for Paint {
    fn from(c: Color) -> Paint {
        Paint::Color(c)
    }
}

/// Rotate, scale and move everything that comes after, around a point. It
/// composes with any that were already there: the head rotates, and inside it an eye
/// can rotate on its own.
#[derive(Clone, Debug)]
pub struct Transform {
    pub pivot: Point,
    pub rotate: Expr,
    pub scale: Point,
    pub translate: Point,
}

impl Transform {
    pub fn at(pivot: Point) -> Self {
        Transform { pivot, rotate: 0.0.into(), scale: (1.0.into(), 1.0.into()), translate: (0.0.into(), 0.0.into()) }
    }
    pub fn rotate(mut self, radians: impl Into<Expr>) -> Self {
        self.rotate = radians.into();
        self
    }
    pub fn scale(mut self, x: impl Into<Expr>, y: impl Into<Expr>) -> Self {
        self.scale = (x.into(), y.into());
        self
    }
    pub fn translate(mut self, x: impl Into<Expr>, y: impl Into<Expr>) -> Self {
        self.translate = (x.into(), y.into());
        self
    }
    pub fn affine(&self, c: Ctx) -> Affine {
        Affine::new(
            (self.pivot.0.eval(c), self.pivot.1.eval(c)),
            self.rotate.eval(c),
            (self.scale.0.eval(c), self.scale.1.eval(c)),
            (self.translate.0.eval(c), self.translate.1.eval(c)),
        )
    }
}

// ── text and images ─────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Content {
    Literal(String),
    /// A number that comes from an expression, with so many decimals and whatever goes after it: `54 %`.
    Number(Expr, u8, String),
    /// Whatever a live text is worth right now: the logic changes it.
    Live(TextId),
    /// A text with placeholders: `"{r.title} · {volume * 100} %"`. It is assembled in the render,
    /// every time any of its parts changes.
    Template(Vec<Piece>),
    /// A text with one version per language (`translations`): the one the
    /// `locale` fact says. The first is the one the scene is written in.
    Translated { locale: FactId, versions: Vec<Content> },
    /// `pick(skin, "Liquid", "Light liquid", "Classic")`: the one at that place.
    Pick(Expr, Vec<Content>),
}

#[derive(Clone, Debug)]
pub enum Piece {
    Literal(String),
    /// A live text, as is or turned to upper or lower case.
    Live(TextId, LetterCase),
    /// An expression, with so many decimals.
    Number(Expr, u8),
    /// Some seconds, written the way a clock writes them: `1:07`, and `1:02:07`
    /// when they go past the hour.
    TimeSpan(Expr),
    /// The name of whatever an enumerated fact is worth: `{mode}` → `critical`.
    Name(Expr, Vec<String>),
    /// `{? · {r.body}}`: a span that is only there if none of its texts is empty.
    Optional(Vec<Piece>),
    /// A span that comes from outside already assembled: the text that was passed to a component.
    Span(Vec<Piece>),
    /// A text that was passed empty: it writes nothing, but counts as empty for a `{? …}`.
    Empty,
    /// `{win[i].title}`: the live text at that place among these; outside them, none.
    Pick(Expr, Vec<TextId>, LetterCase),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LetterCase {
    AsIs,
    Upper,
    Lower,
}

impl Piece {
    /// Writes the span. Returns whether it was whole: whether no live text came out empty.
    pub fn write_into(pieces: &[Piece], c: Ctx, texts: &[String], out: &mut String) -> bool {
        use std::fmt::Write;
        let mut whole = true;
        for p in pieces {
            match p {
                Piece::Literal(s) => out.push_str(s),
                Piece::Live(id, case) => {
                    let s = texts.get(id.0 as usize).map_or("", String::as_str);
                    whole &= !s.is_empty();
                    match case {
                        LetterCase::AsIs => out.push_str(s),
                        LetterCase::Upper => out.push_str(&s.to_uppercase()),
                        LetterCase::Lower => out.push_str(&s.to_lowercase()),
                    }
                }
                Piece::Number(e, decimals) => {
                    let _ = write!(out, "{:.*}", *decimals as usize, e.eval(c));
                }
                Piece::TimeSpan(e) => {
                    let t = e.eval(c).max(0.0) as u64;
                    let _ = if t >= 3600 {
                        write!(out, "{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
                    } else {
                        write!(out, "{}:{:02}", t / 60, t % 60)
                    };
                }
                Piece::Name(e, names) => {
                    if let Some(n) = names.get(e.eval(c).round().max(0.0) as usize) {
                        out.push_str(n);
                    }
                }
                Piece::Span(inner) => whole &= Piece::write_into(inner, c, texts, out),
                Piece::Empty => whole = false,
                Piece::Pick(i, ids, case) => {
                    let k = i.eval(c).round();
                    let s = (k >= 0.0).then(|| ids.get(k as usize)).flatten().and_then(|id| texts.get(id.0 as usize)).map_or("", String::as_str);
                    whole &= !s.is_empty();
                    match case {
                        LetterCase::AsIs => out.push_str(s),
                        LetterCase::Upper => out.push_str(&s.to_uppercase()),
                        LetterCase::Lower => out.push_str(&s.to_lowercase()),
                    }
                }
                Piece::Optional(inner) => {
                    let start = out.len();
                    if !Piece::write_into(inner, c, texts, out) {
                        out.truncate(start);
                    }
                }
            }
        }
        whole
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug)]
pub struct Style {
    /// `None` is the system sans, whatever the system is.
    pub family: Option<&'static str>,
    pub px: f32,
    /// 400 normal, 500 medium, 700 bold.
    pub weight: u16,
    pub color: Color,
    /// Line height, in multiples of the size.
    pub line_height: f32,
    pub align: TextAlign,
    /// Beyond this many lines, an ellipsis.
    pub max_lines: Option<usize>,
    /// `markdown: true`: its marks are read, not shown (`crate::markdown`).
    pub markdown: bool,
    /// `tight: true`: with a `width`, it breaks its lines there but takes up only
    /// what its longest line does (left-aligned): a bubble as wide as its text.
    pub tight: bool,
    /// Its size when it changes while the scene runs (`size: 12 + 4 * lane`),
    /// worked out at every frame; otherwise it is `px`.
    pub size: Option<Expr>,
    /// The same for its weight (`weight: 500 + 200 * if(today, 1, 0)`).
    pub weight_of: Option<Expr>,
    /// And for how many lines it takes (`lines: if(open, 12, 2)`): 0 or less, as many as it has.
    pub lines_of: Option<Expr>,
}

impl Style {
    pub fn new(px: f32, color: Color) -> Self {
        Style { family: None, px, weight: 400, color, line_height: 1.3, align: TextAlign::Left, max_lines: None, markdown: false, tight: false, size: None, weight_of: None, lines_of: None }
    }
    pub fn weight(mut self, p: u16) -> Self {
        self.weight = p;
        self
    }
    pub fn align(mut self, a: TextAlign) -> Self {
        self.align = a;
        self
    }
    pub fn lines(mut self, n: usize) -> Self {
        self.max_lines = Some(n);
        self
    }
    pub fn family(mut self, f: &'static str) -> Self {
        self.family = Some(f);
        self
    }
}

/// Where an image comes from.
#[derive(Clone, Debug)]
pub enum ImageSource {
    File(std::path::PathBuf),
    /// A file made of pixels meant to be seen: made bigger, each one stays a
    /// sharp square instead of melting into its neighbours (`pixels "hero.png"`).
    Pixels(std::path::PathBuf),
    /// An icon by its name ("firefox"). Where to find it is something the platform knows.
    Icon(String),
    /// Whatever a live text says: the name of an icon, or a path if it starts
    /// with `/`. It is how the logic chooses an image: by changing that text.
    Live(TextId),
}

pub fn color(r: f32, g: f32, b: f32) -> Color {
    [r.into(), g.into(), b.into()]
}

/// The draw list. The render turns it into elements —one quad each—
/// and paints them in order.
#[derive(Clone, Debug)]
pub enum Instr {
    /// Starts a body: from here on, shapes accumulate.
    Group { shadow: Option<Shadow> },
    /// Adds a shape to the body, merged with what it already has (`fusion` is the
    /// radius of the smooth minimum; 0 is a hard union).
    Shape { shape: Shape, fusion: Expr },
    /// Paints the accumulated body: shadow, fill, light and rim. With `glass_spec`, from
    /// 0 to 1, the fill turns into glass: translucent, and with the light on the edges.
    Fill { paint: Paint, alpha: Expr, rim: Expr, light: Option<Light>, border: Option<(Expr, Color)>, glass_spec: Option<Glass> },
    /// Everything that comes after is clipped to this shape, as well as to the ones
    /// that were already there (up to four). `None` removes the last one.
    Clip(Option<(Shape, f32)>),
    /// Everything that comes after is transformed with this, as well as with whatever
    /// was already there. `None` removes the last one.
    Transform(Option<Transform>),
    /// Everything that comes after is painted apart and blended as ONE thing with
    /// this opacity: what is in front does not let what is behind show through half blended.
    /// `None` closes the group.
    Opacity(Option<Expr>),
    /// A particle emitter: see `Particles`.
    Particles(Box<Particles>),
    /// What the next `Text` does besides being written: see `TextFx`.
    TextFx(Box<TextFx>),
    /// The next `Text` can be selected with the mouse and copied
    /// (`selectable: true`); what is selected is painted behind it in this colour.
    Selectable(Color),
    /// Like `Opacity(Some(..))`, but what is inside is painted apart WITH
    /// effects applied when it is blended: blur, glow, colour, mask, how it
    /// blends. It always wants a layer of its own. `Opacity(None)` closes it.
    Effect(Box<Effects>),
    /// An opacity that multiplies each thing inside instead of painting it
    /// apart: what a group with opacity becomes when there is a group with
    /// effects inside it, so that the effects, which need the layer, get it.
    Fade(Expr),
    /// A loose shape, of a solid color.
    Solid { shape: Shape, color: Color, alpha: Expr, glass_spec: Option<Glass> },
    /// A box painted by one of the scene's own shaders: `target` is where
    /// (x, y, width, height), with `corner` rounding it. `values` are up to eight
    /// numbers and `colors` up to two, which the shader reads as `s.a`, `s.b`,
    /// `s.color` and `s.color2`. `time` and `pointer` only if the shader reads
    /// them: otherwise nothing changes and it is not painted again.
    Shader { shader: u16, target: (Expr, Expr, Expr, Expr), corner: Expr, alpha: Expr, values: Vec<Expr>, colors: Vec<Color>, time: Option<Expr>, pointer: Option<(Expr, Expr)>, behind: bool },
    /// Text. `at` is the reference point and `anchor` which part of the text falls
    /// on it: (0, 0) the top left corner, (0.5, 0.5) the
    /// center. With `width` it is broken into lines; without it, it is a single one.
    /// `measure`, if given, are two properties where the render leaves how much space
    /// the text takes: with them a box can grow with its label.
    Text { content: Content, at: Point, anchor: (f32, f32), width: Option<Expr>, style: Style, alpha: Expr, measure: Option<(PropId, PropId)> },
    /// A field to type into. It edits a live text; the cursor, the selection and the
    /// echo of each key are handled by the render, without waiting for the logic. `zone` is
    /// the name of the zone that focuses it when pressed.
    /// `secret`: what is typed is shown as dots, one per letter. The real
    /// text is still the live text; what changes is what gets painted.
    Field { text: TextId, zone: &'static str, at: Point, width: Expr, style: Style, alpha: Expr, placeholder: Content, selection: Color, secret: bool, passive: bool },
    /// An image or an icon. With `tint`, its shape is painted in that color: what
    /// a symbolic icon wants.
    /// `cell`: the image is a sheet of pictures in a grid, and only one is
    /// drawn —which one (counted along the rows, from 0), and how many
    /// columns and rows the sheet has—.
    /// `fit`: 0 stretched to its box, 1 whole inside it, 2 cut to cover it (`IMAGE_FITS`).
    Image { image: ImageId, target: (Expr, Expr, Expr, Expr), alpha: Expr, tint: Option<Color>, cell: Option<(Expr, Expr, Expr)>, fit: u8 },
    /// Another program's window, from the compositor inside the scene
    /// (`windows`): what it last drew, stretched over `target` (x, y, width,
    /// height). `ask` is the size it is told to have; while the box travels on
    /// its springs, the window keeps its size and is only stretched.
    Window { slot: usize, target: (Expr, Expr, Expr, Expr), alpha: Expr, ask: (Expr, Expr) },
    /// A terminal (`terminal NAME`): a shell on a pty of its own, its screen
    /// drawn over `target` in cells of the font `family` at `px` (see
    /// `crate::terminal`). `run`: while it holds the shell lives, drawn or not;
    /// when it stops, the shell is ended. `colors`: ink, paper and the sixteen
    /// ANSI ones (fewer: the rest are the default ones). `zone` focuses it when pressed.
    Terminal(Box<TerminalSpec>),
}

#[derive(Clone, Debug)]
pub struct TerminalSpec {
    pub name: &'static str,
    pub zone: &'static str,
    pub target: (Expr, Expr, Expr, Expr),
    pub alpha: Expr,
    pub family: Option<&'static str>,
    pub px: Expr,
    pub line_height: f32,
    pub colors: Vec<Color>,
    pub cursor: Option<Color>,
    pub selection: Option<Color>,
    pub run: Expr,
    pub command: Vec<String>,
    pub folder: Option<Content>,
    pub env: Option<Content>,
}

// ── what the render does on its own ─────────────────────────────

#[derive(Clone, Debug)]
pub enum Behavior {
    /// Takes the property from 1 to 0 and back, every now and then.
    Blink { prop: PropId, every: (f32, f32), duration: f32 },
    /// prop = amplitude · sin(frequency · t). With amplitude 0 it costs nothing.
    Wave { prop: PropId, frequency: f32, amplitude: Expr },
    /// The property chases an expression with its spring. It is what in the
    /// language will be `width: label.width + 24 ~lively`.
    Follow { prop: PropId, to: Expr },
    /// prop = expression, without a spring. So that something computed at the end of the
    /// drawing —the space a stack takes— can be read from the beginning.
    Bind { prop: PropId, to: Expr },
    /// `let ord[16] = sort(i in win by a, b)`: the slots in order of their keys
    /// (each slot's, compared in turn; ties by slot), as `ord[p]`, the slot at
    /// place `p`, and `pos[i]`, the place of slot `i`. Worked out once, when a
    /// key's inputs change, for every reader.
    Sort { keys: Vec<Vec<Expr>>, ord: Vec<PropId>, pos: Vec<PropId> },
    /// prop += per_second · dt, endlessly: a hand that goes round and round.
    Advance { prop: PropId, per_second: Expr },
    /// Two properties that pull towards the pointer, with their own spring.
    Gaze { x: PropId, y: PropId, center: Point, reach: (f32, f32), distance: f32, rest: Point },
}

#[derive(Clone, Debug)]
pub struct Transition {
    pub prop: PropId,
    /// Where to. An expression, evaluated when its time comes: `center - 220`.
    pub to: Expr,
    pub spring: Spring,
    pub delay: Duration,
}

pub fn go_to(prop: PropId, to: impl Into<Expr>, spring: Spring, delay_ms: u64) -> Transition {
    Transition { prop, to: to.into(), spring, delay: Duration::from_millis(delay_ms) }
}

/// The keys that go along with another one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Cursor {
    #[default]
    Normal,
    Hand,
    Text,
    Grab,
    Grabbing,
    /// Stretching along an edge: left and right, up and down, and the two diagonals.
    EwResize,
    NsResize,
    NwseResize,
    NeswResize,
    Move,
    NotAllowed,
    Crosshair,
    /// Any other of the standard ones, by its place in `CURSOR_NAMES`.
    Named(u8),
}

/// The standard cursors by name (CSS's, in the order of Wayland's
/// cursor-shape protocol): what a theme calls its files, and a program asks for.
pub const CURSOR_NAMES: &[&str] = &[
    "default", "context-menu", "help", "pointer", "progress", "wait", "cell", "crosshair", "text", "vertical-text", "alias", "copy", "move", "no-drop", "not-allowed", "grab", "grabbing",
    "e-resize", "n-resize", "ne-resize", "nw-resize", "s-resize", "se-resize", "sw-resize", "w-resize", "ew-resize", "ns-resize", "nesw-resize", "nwse-resize", "col-resize", "row-resize",
    "all-scroll", "zoom-in", "zoom-out", "dnd-ask", "all-resize",
];

const CURSOR_KINDS: [(Cursor, &str); 12] = [
    (Cursor::Normal, "default"),
    (Cursor::Hand, "pointer"),
    (Cursor::Text, "text"),
    (Cursor::Grab, "grab"),
    (Cursor::Grabbing, "grabbing"),
    (Cursor::EwResize, "ew-resize"),
    (Cursor::NsResize, "ns-resize"),
    (Cursor::NwseResize, "nwse-resize"),
    (Cursor::NeswResize, "nesw-resize"),
    (Cursor::Move, "move"),
    (Cursor::NotAllowed, "not-allowed"),
    (Cursor::Crosshair, "crosshair"),
];

impl Cursor {
    /// Its standard name (`default`, `pointer`, `ew-resize`…).
    pub fn name(self) -> &'static str {
        match self {
            Cursor::Named(i) => CURSOR_NAMES.get(i as usize).copied().unwrap_or("default"),
            c => CURSOR_KINDS.iter().find(|k| k.0 == c).map_or("default", |k| k.1),
        }
    }

    /// The cursor of that standard name, with `_` or `-` between its words.
    pub fn from_name(name: &str) -> Option<Cursor> {
        let name = name.replace('_', "-");
        if let Some(k) = CURSOR_KINDS.iter().find(|k| k.1 == name) {
            return Some(k.0);
        }
        CURSOR_NAMES.iter().position(|n| *n == name).map(|i| Cursor::Named(i as u8))
    }

    /// A number for it, to keep in an atomic: the twelve first, then the rest.
    pub fn code(self) -> u8 {
        match self {
            Cursor::Named(i) => 12 + i,
            c => CURSOR_KINDS.iter().position(|k| k.0 == c).unwrap_or(0) as u8,
        }
    }

    /// The highest code there is: the last of the named ones.
    pub const LAST_CODE: u8 = (CURSOR_KINDS.len() + CURSOR_NAMES.len() - 1) as u8;

    pub fn from_code(code: u8) -> Cursor {
        match CURSOR_KINDS.get(code as usize) {
            Some(k) => k.0,
            None => Cursor::Named(code - 12),
        }
    }
}

/// A sensitive region: a shape with a name. The render does the hit-test with
/// the same formula it paints with.
#[derive(Clone, Debug)]
pub struct Zone {
    pub id: &'static str,
    pub shape: Shape,
    pub active: Expr,
    /// Which cursor is set when passing over it.
    pub cursor: Cursor,
    /// `cursor: if(held, grabbing, grab)`: worked out every frame instead, to a
    /// cursor's code (`Cursor::code`). Read while the zone is dragged too, so
    /// the hand can close for as long as it holds something.
    pub cursor_when: Option<Expr>,
    /// The transforms it lives under, from outside in: what is seen
    /// rotated is pressed rotated.
    pub under: Vec<Transform>,
    /// Where it was declared among the instructions, and the group with `z:`
    /// it is inside, if any: a zone is on top where what holds it is drawn.
    pub at: usize,
    pub zblock: Option<u16>,
    /// Dragged out, what it gives another program: a file, a link, a text.
    pub carries: Option<Content>,
    /// While it is dragged out (`carries:`), the picture that goes under the pointer: the path of a PNG, as a text with holes (`carry_icon:`). Without it the drag has none.
    pub carry_icon: Option<Content>,
    /// What it is, said in words for whoever cannot see it (`label:`): an
    /// agent, a screen reader. Without it, the texts drawn inside it.
    pub label: Option<Content>,
    /// Who may use it from outside (`agent:`).
    pub reach: Reach,
    /// If it is the zone of a stack that scrolls (`view:`): the property
    /// that scrolls it.
    pub scrolls: Option<PropId>,
    /// The zone of the stack with `view:` it scrolls inside: it can be out of sight.
    pub within: Option<ZoneId>,
    /// What else it says of itself when it is not derived (`role:`, `value:`,
    /// `checked:`, `selected:`). Most zones say none of it.
    pub told: Option<Box<Told>>,
    /// `handle: i` (`handle: i, left`): the zone is a window's handle, by
    /// which it is carried (or stretched by that edge). A window that draws
    /// its own frame and asks to be carried from it gets this zone pressed
    /// for it, until the button is let go.
    pub handle: Option<Box<Handle>>,
    /// `yields: true`: it only counts a press that nothing below the scene takes.
    /// A window manager that puts programs' widgets under its own scene (a
    /// clock on the desktop, on layer-shell's bottom level) gives them the press
    /// first where this zone is; elsewhere, and on the bare desktop, it is the
    /// zone's. Anywhere else it is a zone like any other.
    pub yields: Option<Expr>,
}

/// Which window a zone is the handle of, and by which edges (none: it
/// carries it). The edges are xdg-shell's: 1 top, 2 bottom, 4 left, 8 right.
#[derive(Clone, Debug)]
pub struct Handle {
    pub window: Expr,
    pub edges: u32,
}

/// What a zone says of itself for whoever cannot see it.
#[derive(Clone, Debug, Default)]
pub struct Told {
    /// `role: toggle`: what it is, when its rules do not say it.
    pub role: Option<&'static str>,
    /// `value: "{volume * 100}%"` or `value: volume`: what it is worth.
    pub value: Option<Said>,
    /// `checked: wifi_on`: a toggle's state.
    pub checked: Option<Expr>,
    /// `selected: sel == r.index`: the row or the tab that is chosen.
    pub selected: Option<Expr>,
}

/// A value said as a text, or as a number (or a fact's name, `critical`).
#[derive(Clone, Debug)]
pub enum Said {
    Text(Content),
    Number(Expr),
}

/// `agent: no` and `agent: hidden`: what an agent may do with a zone, a
/// field or a surface, through the commands that read and act on the scene.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Reach {
    /// It reads it and acts on it.
    #[default]
    Any,
    /// It reads it, but only a person's hand acts on it.
    Person,
    /// It is not even described.
    Hidden,
}

/// A `group` with `z:`: its instructions, and which groups it is sorted
/// among —the ones with `z:` beside it, in the same group or copy—.
#[derive(Clone, Debug)]
pub struct ZBlock {
    pub range: std::ops::Range<usize>,
    pub z: Expr,
    pub parent: u32,
}

/// Where the groups with `z:` are drawn this frame: the instructions in the
/// order they are painted, and each zone's place in it (bigger, on top).
/// Shared: a round hands it on to the drawing without copying it, and two
/// rounds that have the same one see so without going through it.
#[derive(Clone)]
pub struct ZArrangement {
    pub order: std::sync::Arc<Vec<usize>>,
    pub zone_rank: std::sync::Arc<Vec<(usize, u8, usize)>>,
}

impl Scene {
    /// Siblings with `z:` sorted by it (the same value: as declared), each
    /// taking the place in the sequence of the one it now stands for.
    pub fn z_arrange(&self, c: Ctx) -> Option<ZArrangement> {
        if self.zblocks.is_empty() {
            return None;
        }
        let n = self.instrs.len();
        // Each set of siblings, sorted: where each one stands among them (its
        // zones' place), and for the ones that draw, the one drawn in their place.
        let mut rank = vec![0usize; self.zblocks.len()];
        let mut set_start = vec![0usize; self.zblocks.len()];
        let mut content: Vec<usize> = (0..self.zblocks.len()).collect();
        let mut parents: Vec<u32> = self.zblocks.iter().map(|b| b.parent).collect();
        parents.sort_unstable();
        parents.dedup();
        for p in parents {
            let set: Vec<usize> = (0..self.zblocks.len()).filter(|k| self.zblocks[*k].parent == p).collect();
            let mut sorted = set.clone();
            sorted.sort_by(|a, b| self.zblocks[*a].z.eval(c).total_cmp(&self.zblocks[*b].z.eval(c)).then(a.cmp(b)));
            let start = set.iter().map(|k| self.zblocks[*k].range.start).min().unwrap_or(0);
            for (r, k) in sorted.iter().enumerate() {
                rank[*k] = r;
                set_start[*k] = start;
            }
            // Only the ones that draw something swap places in the sequence.
            let drawing = |k: &&usize| !self.zblocks[**k].range.is_empty();
            for (slot, drawn) in set.iter().filter(drawing).zip(sorted.iter().filter(drawing)) {
                content[*slot] = *drawn;
            }
        }
        let mut order = Vec::with_capacity(n);
        let starts: std::collections::HashMap<usize, usize> = self.zblocks.iter().enumerate().filter(|(_, b)| !b.range.is_empty()).map(|(k, b)| (b.range.start, k)).collect();
        let mut i = 0;
        while i < n {
            match starts.get(&i) {
                Some(&slot) => {
                    order.extend(self.zblocks[content[slot]].range.clone());
                    i = self.zblocks[slot].range.end.max(i + 1);
                }
                None => {
                    order.push(i);
                    i += 1;
                }
            }
        }
        let zone_rank = self
            .zones
            .iter()
            .map(|z| match z.zblock {
                Some(b) => (set_start[b as usize], 1, rank[b as usize] * 1_000_000 + (z.at - self.zblocks[b as usize].range.start)),
                None => (z.at, 0, 0),
            })
            .collect();
        Some(ZArrangement { order: order.into(), zone_rank: std::sync::Arc::new(zone_rank) })
    }
}

impl Zone {
    /// The cursor over it now.
    pub fn cursor_in(&self, c: Ctx) -> Cursor {
        match &self.cursor_when {
            Some(e) => Cursor::from_code(e.eval(c).round().clamp(0.0, Cursor::LAST_CODE as f32) as u8),
            None => self.cursor,
        }
    }

    /// The box that contains it, to tell the compositor where the mouse comes in.
    pub fn bounds(&self, c: Ctx) -> Option<[f32; 4]> {
        let mut p = self.shape.flatten_into(c, &mut Vec::new());
        p.affine = self.under.iter().fold(Affine::IDENTITY, |a, t| a.mul(t.affine(c)));
        p.bounds()
    }

    /// A point on the screen, seen from inside: in a stack, (0, 0) is the
    /// corner of the slot. It is what a rule reads in `local.x`.
    pub fn to_local(&self, c: Ctx, x: f32, y: f32) -> (f32, f32) {
        self.under.iter().fold(Affine::IDENTITY, |a, t| a.mul(t.affine(c))).inverse().apply(x, y)
    }

    /// A path is measured against its points, as on the GPU: a zone shaped
    /// like an arrow is pressed where the arrow is seen, not in its box.
    pub fn contains(&self, c: Ctx, x: f32, y: f32) -> bool {
        let mut pts = Vec::new();
        let mut p = self.shape.flatten_into(c, &mut pts);
        p.affine = self.under.iter().fold(Affine::IDENTITY, |a, t| a.mul(t.affine(c)));
        p.distance_with(x, y, &pts) < 0.0
    }
}

// ── layers: who wins ────────────────────────────────────────────

/// When a claim holds.
#[derive(Clone, Debug)]
pub enum When {
    Always,
    While(Expr),
    /// For a while after any of these signals.
    After { signals: Vec<SignalId>, duration: Duration },
    /// From one of these signals until one of those.
    FromUntil { from: Vec<SignalId>, until: Vec<SignalId> },
}

#[derive(Clone, Debug)]
pub struct Claim {
    pub name: &'static str,
    pub when: When,
    /// What it sets when it wins: it is what sketch B called "state".
    pub sets: Vec<Transition>,
}

impl Claim {
    pub fn during(name: &'static str, c: impl Into<Expr>) -> Self {
        Claim { name, when: When::While(c.into()), sets: vec![] }
    }
    pub fn after(name: &'static str, signals: &[SignalId], ms: u64) -> Self {
        Claim { name, when: When::After { signals: signals.to_vec(), duration: Duration::from_millis(ms) }, sets: vec![] }
    }
    pub fn from_until(name: &'static str, from: &[SignalId], until: &[SignalId]) -> Self {
        Claim { name, when: When::FromUntil { from: from.to_vec(), until: until.to_vec() }, sets: vec![] }
    }
    pub fn fallback(name: &'static str) -> Self {
        Claim { name, when: When::Always, sets: vec![] }
    }
    pub fn sets(mut self, t: Vec<Transition>) -> Self {
        self.sets = t;
        self
    }
}

/// A slot that many claim. The first claim that holds wins;
/// when it stops holding, the next one shows, by itself. Nobody "turns off" anything.
#[derive(Clone, Debug)]
pub struct Layer {
    pub name: &'static str,
    pub spring: Spring,
    pub claims: Vec<Claim>,
    /// One property per claim, which goes to 1 when it wins and to 0 when not:
    /// with it one drawing is blended into another.
    pub presences: Vec<PropId>,
}

/// What `Scene::layer` returns, to paint according to who wins.
pub struct LayerRef {
    pub presences: Vec<PropId>,
}
impl LayerRef {
    pub fn presence(&self, i: usize) -> PropId {
        self.presences[i]
    }
}

// ── gestures: timelines ─────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum Curve {
    Linear,
    InQuad,
    OutQuad,
    InCubic,
    OutCubic,
    InOutSine,
    OutBack,
    /// `bezier(x1, y1, x2, y2)`: the same two control points as CSS's
    /// `cubic-bezier`, so a curve from any design tool can be copied as is.
    Bezier(f32, f32, f32, f32),
}

/// The CSS cubic bezier: find the t whose x is the time asked for, and
/// give its y. Newton first —it converges in three or four steps—, and bisection
/// if the slope is flat, which is where Newton gets lost.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let bez = |a: f32, b: f32, t: f32| {
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    };
    let slope = |a: f32, b: f32, t: f32| {
        let u = 1.0 - t;
        3.0 * u * u * a + 6.0 * u * t * (b - a) + 3.0 * t * t * (1.0 - b)
    };
    let mut t = x;
    for _ in 0..6 {
        let d = slope(x1, x2, t);
        if d.abs() < 1e-5 {
            break;
        }
        t -= (bez(x1, x2, t) - x) / d;
    }
    if (bez(x1, x2, t) - x).abs() > 1e-4 || !(0.0..=1.0).contains(&t) {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        t = x;
        for _ in 0..30 {
            if bez(x1, x2, t) < x { lo = t } else { hi = t }
            t = (lo + hi) * 0.5;
        }
    }
    bez(y1, y2, t)
}

impl Curve {
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Curve::Linear => t,
            Curve::InQuad => t * t,
            Curve::OutQuad => 1.0 - (1.0 - t) * (1.0 - t),
            Curve::InCubic => t * t * t,
            Curve::OutCubic => 1.0 - (1.0 - t).powi(3),
            Curve::InOutSine => 0.5 - 0.5 * (std::f32::consts::PI * t).cos(),
            Curve::OutBack => {
                let (c1, u) = (1.70158, t - 1.0);
                1.0 + (c1 + 1.0) * u * u * u + c1 * u * u
            }
            Curve::Bezier(x1, y1, x2, y2) => cubic_bezier(x1, y1, x2, y2, t),
        }
    }
}

/// Who can interrupt whom: a gesture only cuts off another of its class or
/// lower. A reflex does not take the face away from something that was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Ambient,
    Posture,
    Reflex,
    Asked,
    State,
}

#[derive(Clone, Debug)]
pub struct Keyframe {
    pub ms: u32,
    pub hold: u32,
    pub curve: Curve,
    /// Whatever is not named goes back to its base pose.
    /// Expressions, which are evaluated when the gesture starts: that way the same gesture
    /// can point to one side or the other depending on a fact.
    pub values: Vec<(PropId, Expr)>,
    pub emit: Option<SignalId>,
}

pub fn keyframe(ms: u32, curve: Curve) -> Keyframe {
    Keyframe { ms, hold: 0, curve, values: vec![], emit: None }
}
impl Keyframe {
    pub fn with(mut self, p: PropId, v: impl Into<Expr>) -> Self {
        self.values.push((p, v.into()));
        self
    }
    pub fn hold(mut self, ms: u32) -> Self {
        self.hold = ms;
        self
    }
    pub fn emit(mut self, s: SignalId) -> Self {
        self.emit = Some(s);
        self
    }
}

#[derive(Clone, Debug)]
pub struct Gesture {
    pub name: &'static str,
    pub class: Class,
    pub keyframes: Vec<Keyframe>,
}

// ── rules: what makes things change ─────────────────────────────

#[derive(Clone, Debug)]
pub enum Trigger {
    Enter(ZoneId),
    Leave(ZoneId),
    /// The left button.
    Press(ZoneId),
    /// Another button: 1 is the right one and 2 the middle one.
    PressWith(ZoneId, u8),
    /// What was pressed here is released, wherever the mouse is by now.
    Release(ZoneId),
    /// The wheel, over it. How much, in `wheel`: +1 per notch upwards.
    Wheel(ZoneId),
    /// The mouse moves with the button held since it was pressed here. Where, in
    /// `local.x` and `local.y`; how much since it was pressed, in `drag.dx` and `drag.dy`.
    Drag(ZoneId),
    /// It has been held pressed for this long.
    Hold { zone: ZoneId, duration: Duration },
    /// A key, by its name: `Escape`, `Return`, `a`, `Ctrl+k`.
    Key(String),
    /// Enter has been pressed in a field.
    Submit(TextId),
    /// The surface gains or loses the keyboard: losing it means someone clicked outside.
    FocusGained,
    FocusLost,
    /// Something dragged from another application has been dropped on it.
    Receive(ZoneId),
    /// What it carries (`carries:`) has just been dragged out of it, to another program.
    Carry(ZoneId),
    /// The mouse has been over it (or over any of `also`) for this long.
    Above { zone: ZoneId, also: Vec<ZoneId>, duration: Duration },
    /// It has been over it (or over any of `also`: a button and the panel it
    /// opens) and has been outside all of them for this long.
    Away { zone: ZoneId, also: Vec<ZoneId>, duration: Duration },
    /// Nobody has touched the mouse for this long, while the condition holds.
    Idle { duration: Duration, during: Expr },
    /// Every now and then, at random, while the condition holds.
    Every { between: (f32, f32), during: Expr },
    /// When that calculation stops being worth what it was worth. The first time does not count: it
    /// fires on changing, not on being born.
    Change(Expr),
    /// When that calculation has been worth the same for this long. It is the reverse of
    /// `Change`, and like it, being born does not count: a change is needed first, or a
    /// scene that starts still would fire by itself on opening. Each change
    /// resets the clock to zero, so a burst —the volume key
    /// pressed six times— is a single wait and not six.
    Still { value: Expr, duration: Duration },
    On(SignalId),
}

#[derive(Clone, Debug)]
pub enum Effect {
    Animate(Transition),
    /// A fact becomes worth whatever the expression is worth at that moment. It can
    /// read the mouse's things: `level = clamp(local.x / 64, 0, 1)`.
    Fact(FactId, Expr),
    /// `x[i] = v`: the fact at place `i` among these. The place and the value
    /// are read once, when it fires; outside the list, nothing changes.
    FactAt(Vec<FactId>, Expr, Expr),
    /// From yes to no and from no to yes.
    Toggle(FactId),
    /// A signal, with a payload if wanted: `emit opened(i)`. It is evaluated when it fires.
    Signal(SignalId, Option<Expr>),
    /// A push to the spring's velocity. The amount is evaluated when it
    /// fires, so it can depend on what is going on: the bounce of
    /// a countdown decreases with the number that is left.
    Impulse(PropId, Expr),
    /// A ring of light from the centre of a zone, without pressing it.
    Ripple(ZoneId),
    Gesture(GestureId),
    /// Put the text cursor in a field, or take it away.
    FocusField(Option<TextId>),
    /// `focus term`, a terminal: the keys go to its shell.
    FocusTerminal(&'static str),
    /// `quit`: the program ends.
    Quit,
    /// A live text becomes that: `name = ""`, `label = draft`.
    Text(TextId, TextValue),
    /// Something done to a window of the compositor inside the scene: which
    /// one is an expression, `close win(win.focus)`.
    Window(WindowAction, Expr),
    /// A program started inside the scene's compositor: `launch "kitty"`. The
    /// command can have holes, like any text: `launch "foot -e top -p {pid}"`,
    /// filled in when the rule fires.
    Launch(Content),
    /// `dock k on s`: that item of monitor `s`'s dock: its window (the next
    /// one, if one of them has the keyboard; brought back, if put away), or
    /// the program started.
    Dock(Expr, Option<Expr>, DockAction),
    /// A window to another monitor: `send win(win.focus) to 1`.
    WindowTo(Expr, Expr),
    /// Two windows change places, in the order and on their monitors:
    /// `swap win(a) with win(b)`.
    WindowSwap(Expr, Expr),
    /// `accept win(i)`: a window waiting under `placement manual` takes its
    /// place in the layout.
    Accept(Expr),
    /// A workspace shown: `workspace 3` on the monitor the pointer is on, or
    /// `workspace 3 on 1` on that one.
    Workspace(Expr, Option<Expr>),
    /// A window to another workspace: `send win(win.focus) to workspace 2`.
    WindowToWorkspace(Expr, Expr, Option<Expr>),
    /// A window put aside: `send win(k) aside`. On no workspace —on no
    /// monitor's stack, not counted, not shown— and alive: its program keeps
    /// running and drawing. Sent to a workspace, it is on that one again.
    WindowAside(Expr),
    /// `send workspace n [on s] to t`: a whole workspace to another monitor.
    WorkspaceTo(Expr, Option<Expr>, Expr),
    /// `close workspace n [on s]`: that workspace gone, its windows to the one
    /// before it (to the next, from the first). The last one left stays.
    WorkspaceClose(Expr, Option<Expr>),
    /// The answer to what the compositor asked the scene to choose (`win.picking`:
    /// what to share of the screen): a window (`pick win(x)`, true), a monitor
    /// (`pick screen 1`, false), or nothing (`pick none`).
    Pick(Option<(bool, Expr)>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowAction {
    /// The keyboard goes to it.
    Focus,
    /// It is asked to close, as its own close button would.
    Close,
    /// It goes first in the order the scene lays them out in.
    Promote,
    /// It goes to fullscreen, or back from it.
    Fullscreen,
    /// It is put away (minimized), or brought back.
    Minimize,
    Restore,
    /// It floats over the layout (the others close up without it), or goes
    /// back into it: one window, not its whole monitor.
    Float,
    Tile,
}

/// What a rule makes a live text: words as written, or what another says.
#[derive(Clone, Debug)]
pub enum TextValue {
    Words(String),
    Of(TextId),
}

impl Effect {
    /// The same effect with the event's value in place (see `Expr::Payload`).
    pub fn with_payload(&self, v: f32) -> Effect {
        match self {
            Effect::Animate(t) => Effect::Animate(Transition { to: t.to.with_payload(v), ..t.clone() }),
            Effect::Fact(h, e) => Effect::Fact(*h, e.with_payload(v)),
            Effect::FactAt(hs, i, e) => Effect::FactAt(hs.clone(), i.with_payload(v), e.with_payload(v)),
            Effect::Signal(s, e) => Effect::Signal(*s, e.as_ref().map(|e| e.with_payload(v))),
            Effect::Impulse(p, e) => Effect::Impulse(*p, e.with_payload(v)),
            Effect::Window(a, e) => Effect::Window(*a, e.with_payload(v)),
            Effect::WindowTo(w, to) => Effect::WindowTo(w.with_payload(v), to.with_payload(v)),
            Effect::WindowSwap(a, b) => Effect::WindowSwap(a.with_payload(v), b.with_payload(v)),
            Effect::Accept(a) => Effect::Accept(a.with_payload(v)),
            Effect::Workspace(w, on) => Effect::Workspace(w.with_payload(v), on.as_ref().map(|e| e.with_payload(v))),
            Effect::WindowToWorkspace(w, to, on) => Effect::WindowToWorkspace(w.with_payload(v), to.with_payload(v), on.as_ref().map(|e| e.with_payload(v))),
            Effect::WindowAside(w) => Effect::WindowAside(w.with_payload(v)),
            Effect::Dock(k, on, a) => Effect::Dock(k.with_payload(v), on.as_ref().map(|e| e.with_payload(v)), *a),
            Effect::WorkspaceTo(n, on, to) => Effect::WorkspaceTo(n.with_payload(v), on.as_ref().map(|e| e.with_payload(v)), to.with_payload(v)),
            Effect::WorkspaceClose(n, on) => Effect::WorkspaceClose(n.with_payload(v), on.as_ref().map(|e| e.with_payload(v))),
            Effect::Pick(Some((window, e))) => Effect::Pick(Some((*window, e.with_payload(v)))),
            other => other.clone(),
        }
    }
}

/// Everything here is executed by the render, whatever state the logic is in.
#[derive(Clone, Debug)]
pub struct Rule {
    pub when: Trigger,
    /// `on press x while open`: only if this is true at the moment it fires.
    pub guard: Option<Expr>,
    pub effects: Vec<Effect>,
}

pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[derive(Default)]
pub struct Scene {
    pub props: Vec<(&'static str, f32, Spring)>,
    pub instrs: Vec<Instr>,
    pub behaviors: Vec<Behavior>,
    pub zones: Vec<Zone>,
    /// The groups with `z:`, drawn over or under their siblings by it.
    pub zblocks: Vec<ZBlock>,
    /// `hit.hover` and `hit.pressed`: springs the render moves by itself, from
    /// 0 to 1 while the pointer is over the zone and while it is pressed.
    pub zone_springs: Vec<(ZoneId, PropId, PropId)>,
    /// `windows win max 6`: the scene holds other programs' windows.
    pub nest: Option<Nest>,
    /// Name and initial value of each live text.
    pub texts: Vec<(&'static str, String)>,
    /// Each image, and the largest logical size it is painted at.
    pub images: Vec<(ImageSource, (u32, u32))>,
    /// Fonts it brings with it (`font file "…"`), besides the system's.
    pub fonts: Vec<std::path::PathBuf>,
    pub facts: Vec<(&'static str, f32)>,
    /// Name, and whether besides the scene it also reaches the logic.
    pub signals: Vec<(&'static str, bool)>,
    pub layers: Vec<Layer>,
    pub gestures: Vec<Gesture>,
    /// The properties that make up the pose: the ones a gesture leads by the hand.
    pub pose: Vec<PropId>,
    /// Gestures that repeat by themselves while something is true.
    pub postures: Vec<(GestureId, Expr)>,
    pub rules: Vec<Rule>,
    /// The windows it asks for. The first is the main one: the one that rules if something belongs to just one.
    pub surfaces: Vec<Surface>,
    /// Files that are not `.plm` and that it is also made of —the SVGs of
    /// its figures, its shaders—: touching them also reloads it.
    pub attachments: Vec<std::path::PathBuf>,
    /// Its own shaders, already checked: `shader aurora = file "aurora.wgsl"`.
    /// Their number is their place here.
    pub shaders: Vec<crate::shaders::UserShader>,
    /// Its own blur, `blur = file "blur.wgsl" { passes: ... }`, already checked,
    /// and the numbers it is given (`values:`). Without one, the default.
    pub blur: Option<crate::blur::Plan>,
    pub blur_values: Vec<Expr>,
    /// `lens = image "file"`: the picture the glass bends instead of a capture of what is
    /// behind. It is not an attachment: the file changes with the wallpaper, and the scene must not reload with it.
    pub lens_image: Option<std::path::PathBuf>,
    /// `lens = image "file" while expr`: the picture is only bent while this holds (live capture otherwise).
    pub lens_image_while: Option<Expr>,
    /// `lens every 250ms`: the least time between two captures of what is behind
    /// a glass, in seconds, looked at every round. Without it, `lens::CAPTURE_INTERVAL`.
    pub lens_every: Option<Expr>,
    /// `lens hold while expr`: while it holds, nothing behind a glass is captured;
    /// when it stops, one capture straight away.
    pub lens_hold_while: Option<Expr>,
    /// `keyboard: exclusive while open`: when it wants the keyboard.
    pub keyboard_while: Option<Expr>,
    pub permissions: Permissions,
    pub popups: Vec<Popup>,
    pub models: Vec<Model>,
    /// Of the facts that are not plain numbers, what they are. By name.
    pub types: Vec<(String, FactType)>,
    /// With `translations`: the fact that says which language (`en`, then each
    /// translation in order), the tables themselves —for the logic's `tr`—, and
    /// the declared texts (`text x = "…"`) that have a version per language.
    pub locale: Option<FactId>,
    pub translations: Translations,
    pub text_versions: Vec<(TextId, Vec<String>)>,
    pub plugins: Vec<Plugin>,
    /// The services the scene asks for by name, and which fields it wants from each one.
    pub services: Vec<Service>,
    /// With `screens: each`, which span of instructions, behaviors and rules
    /// belongs to each copy (by the index of its surface). A copy whose
    /// surface is closed has nothing to show and nothing to move:
    /// the render skips its part entirely. Without this, two copies were twice
    /// the scene per frame even if one was not visible.
    pub spans: Vec<Span>,
    /// With `screens: each`, a rule that comes out the same in every copy —it
    /// names nothing of its own copy, only the scene's facts and props— is the
    /// same rule written twice: for each one, the first of its twins. Only one
    /// of them may act, or a `n = n + 1` counts once per monitor, an `emit` is
    /// heard twice and a `toggle` undoes itself. Empty: every rule is its own.
    pub twin_of: Vec<usize>,
    /// Whether it names `cursor.x` or `cursor.y`: only then is the system asked
    /// where the mouse is when it is not over the scene.
    pub wants_cursor: bool,
    /// The scene's `time`, when only its shaders read it: it stands still
    /// while none of them is drawn, so the scene can rest.
    pub shader_clock: Option<PropId>,
}

#[derive(Clone, Debug, Default)]
pub struct Span {
    pub surface: usize,
    pub instrs: std::ops::Range<usize>,
    pub behaviors: std::ops::Range<usize>,
    pub rules: std::ops::Range<usize>,
}

/// A library with logic of its own. Its boundary —facts, texts, models, signals—
/// lives under its name (`Clock.now`), its logic runs in its own Luau state, it can only
/// name its own things, and what it touches of the system is decided by **its** permissions.
#[derive(Clone, Debug, PartialEq)]
pub struct Plugin {
    pub name: String,
    pub logic: std::path::PathBuf,
    pub permissions: Permissions,
}

/// Shaped data that crosses the boundary: a list of rows, all with the
/// same fields. The logic hands it over whole (`model.rows = list`) and the scene
/// walks through it (`for r in rows`). Inside, each field of each row is a live
/// text or a named fact —`rows.3.label`—: the render does not know there are lists.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub name: String,
    /// How many rows fit. Whatever goes beyond that is not seen, but is counted: `rows.total`.
    pub capacity: usize,
    pub fields: Vec<Field>,
}

/// A system service requested from the scene: whatever arrives fills
/// `alias.field` without anyone writing logic. `service clock as now { time: text }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    /// As the platform knows it: `clock`, `audio`, `battery`, `network`, `media`.
    pub name: String,
    /// In front of each field: `now.time`.
    pub alias: String,
    pub fields: Vec<Field>,
    /// The lists it reports that go into a model of the scene, record by record:
    /// (the service's field, the model). `service tray { list: icons }`.
    pub models: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub kind: FieldType,
    /// What it is worth if the row does not bring it.
    pub fallback: FieldValue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldType {
    Text,
    Number,
    /// Like `Number`, but only 0 or 1, and the logic writes it with `true` and `false`.
    Bool,
    /// One of these names: `low | normal | critical`. Inside, its position.
    Enum(Vec<String>),
    /// The name of an icon or a path, and the image that says, at this size.
    Image(u32, u32),
    /// A list of rows inside the row: a menu with its submenus.
    List(Box<Model>),
}

/// What a fact that is not a plain number is. The scene only sees numbers; this
/// is for whoever sets them and reads them from outside —the logic, `--say`, a placeholder in a
/// text—, who talks about `true` and `critical`, not about 1 and 2.
#[derive(Clone, Debug, PartialEq)]
pub enum FactType {
    Bool,
    Enum(Vec<String>),
}

impl FactType {
    /// As it is shown to someone: `true`, `critical`.
    pub fn to_text(&self, v: f32) -> String {
        match self {
            FactType::Bool => (v > 0.5).to_string(),
            FactType::Enum(names) => names.get(v.round().max(0.0) as usize).cloned().unwrap_or_else(|| v.to_string()),
        }
    }
    /// And the other way round: what someone wrote, as a number.
    pub fn from_text(&self, t: &str) -> Option<f32> {
        match self {
            FactType::Bool => match t { "true" => Some(1.0), "false" => Some(0.0), _ => None },
            FactType::Enum(names) => names.iter().position(|n| n == t).map(|k| k as f32),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Text(String),
    Number(f32),
}

/// A surface that comes out of the main one: a menu, a card. What it paints
/// is one more piece of the scene —same springs, same zones, same rules—,
/// drawn far away, at `origin`; the popup surface is a window onto that piece.
#[derive(Clone, Debug)]
pub struct Popup {
    pub name: &'static str,
    /// It is open while this fact is true. If the system closes it
    /// (someone clicked outside), the render sets it to false.
    pub open: FactId,
    /// Where it comes out, inside the main surface; and how big it is.
    pub at: (Expr, Expr),
    pub size: (Expr, Expr),
    pub origin: (f32, f32),
}

/// What this scene's logic can touch of the system. **Undeclared,
/// nothing**: not a command, not a service. It is in the scene and not in the script
/// so it can be read at a glance, before running anything, what it will be able to do.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Permissions {
    /// The commands that `run` and `spawn` can launch, by their exact name.
    pub commands: Vec<String>,
    /// The services that `sys.watch` and `sys.call` can use: `audio`, `apps`…
    pub services: Vec<String>,
    /// The variables `run` and `spawn` may set for a program (`{ env = … }`)
    /// beyond the ones that are always let through: `env: "PATH", "MY_TOKEN"`.
    pub env: Vec<String>,
}

impl Scene {
    /// The main one: the one declared without a name, or the first.
    pub fn surface(&self) -> &Surface {
        self.surfaces.first().expect("every scene has at least one surface")
    }
    pub fn surface_mut(&mut self) -> &mut Surface {
        if self.surfaces.is_empty() {
            self.surfaces.push(Surface::default());
        }
        &mut self.surfaces[0]
    }

    /// Properties go by name: if the scene is replaced by another, the
    /// ones with the same name keep their value and velocity.
    pub fn prop(&mut self, name: &'static str, initial: f32) -> PropId {
        self.prop_with(name, initial, Spring::LIVELY)
    }
    pub fn prop_with(&mut self, name: &'static str, initial: f32, spring: Spring) -> PropId {
        self.props.push((name, initial, spring));
        PropId(self.props.len() as u16 - 1)
    }
    /// The spring a property was declared with. It is its own: a rule that
    /// sends it somewhere without saying which spring to use goes with this one, which is what
    /// `prop x = 0 ~620ms` says when written.
    pub fn spring_of(&self, p: PropId) -> Spring {
        self.props[p.0 as usize].2
    }
    pub fn paint(&mut self, i: Instr) {
        self.instrs.push(i);
    }
    /// A property of the pose, with its rest value.
    pub fn pose_prop(&mut self, name: &'static str, rest: f32) -> PropId {
        let p = self.prop_with(name, rest, Spring::POSE);
        self.pose.push(p);
        p
    }
    /// Two read-only properties —width and height— that the render fills with
    /// whatever a text measures.
    pub fn measured(&mut self, name: &'static str) -> (PropId, PropId) {
        let n = |suffix: &str| intern(&format!("{name}.{suffix}"));
        (self.prop(n("width"), 0.0), self.prop(n("height"), 0.0))
    }
    pub fn surface_cursor(&mut self, name: &'static str) -> (PropId, PropId) {
        let n = |suffix: &str| intern(&format!("{name}.cursor.{suffix}"));
        (self.prop(n("x"), 0.0), self.prop(n("y"), 0.0))
    }
    pub fn live_text(&mut self, name: &'static str, initial: &str) -> TextId {
        self.texts.push((name, initial.to_owned()));
        TextId(self.texts.len() as u16 - 1)
    }
    pub fn image(&mut self, source: ImageSource, width: u32, height: u32) -> ImageId {
        self.images.push((source, (width, height)));
        ImageId(self.images.len() as u16 - 1)
    }
    pub fn fact(&mut self, name: &'static str, initial: f32) -> FactId {
        self.facts.push((name, initial));
        FactId(self.facts.len() as u16 - 1)
    }
    /// An internal signal: the layers and the rules hear it.
    pub fn signal(&mut self, name: &'static str) -> SignalId {
        self.signals.push((name, false));
        SignalId(self.signals.len() as u16 - 1)
    }
    /// A signal that also goes out to the logic.
    pub fn outgoing_signal(&mut self, name: &'static str) -> SignalId {
        self.signals.push((name, true));
        SignalId(self.signals.len() as u16 - 1)
    }
    pub fn zone(&mut self, id: &'static str, shape: Shape, active: impl Into<Expr>) -> ZoneId {
        self.zone_under(id, shape, active, vec![])
    }
    pub fn zone_under(&mut self, id: &'static str, shape: Shape, active: impl Into<Expr>, under: Vec<Transform>) -> ZoneId {
        self.zones.push(Zone { id, shape, active: active.into(), cursor: Cursor::Normal, cursor_when: None, under, at: self.instrs.len(), zblock: None, carries: None, carry_icon: None, label: None, reach: Reach::Any, scrolls: None, within: None, told: None, handle: None, yields: None });
        ZoneId(self.zones.len() as u16 - 1)
    }
    /// Claims go from more to less priority; the last one should be
    /// `fallback`.
    pub fn layer(&mut self, name: &'static str, spring: Spring, claims: Vec<Claim>) -> LayerRef {
        let presences: Vec<PropId> = claims
            .iter()
            .map(|r| {
                let n = intern(&format!("layer.{name}.{}", r.name));
                self.props.push((n, 0.0, spring));
                PropId(self.props.len() as u16 - 1)
            })
            .collect();
        self.layers.push(Layer { name, spring, claims, presences: presences.clone() });
        LayerRef { presences }
    }
    pub fn gesture(&mut self, name: &'static str, class: Class, keyframes: Vec<Keyframe>) -> GestureId {
        self.gestures.push(Gesture { name, class, keyframes });
        GestureId(self.gestures.len() as u16 - 1)
    }
    pub fn posture(&mut self, gesture: GestureId, during: impl Into<Expr>) {
        self.postures.push((gesture, during.into()));
    }
    pub fn rule(&mut self, when: Trigger, effects: Vec<Effect>) {
        self.rules.push(Rule { when, guard: None, effects });
    }
}

// ── messages ────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub stiffness: f32,
    pub damping: f32,
}

impl Spring {
    pub const LIVELY: Spring = Spring { stiffness: 170.0, damping: 19.0 };
    pub const CALM: Spring = Spring { stiffness: 150.0, damping: 23.0 };
    pub const QUICK: Spring = Spring { stiffness: 420.0, damping: 40.0 };
    pub const SLOW: Spring = Spring { stiffness: 28.0, damping: 11.0 };
    pub const GENTLE: Spring = Spring { stiffness: 190.0, damping: 24.0 };
    pub const POSE: Spring = Spring { stiffness: 260.0, damping: 28.0 };
    /// `~0ms`: not a spring. It is there at once, still, and whatever
    /// moves it next does so with its own spring.
    pub const INSTANT: Spring = Spring { stiffness: f32::INFINITY, damping: 0.0 };

    pub fn is_instant(&self) -> bool {
        self.stiffness.is_infinite()
    }

    /// The spring that gets there in that time without overshooting. Critically damped
    /// (`damping = 2 · √stiffness`), which is the one that does not bounce; with that, reaching 99 %
    /// takes about 6.64 time constants, and the stiffness comes from there.
    pub fn at(seconds: f32) -> Spring {
        let w = 6.64 / seconds.max(0.016);
        Spring { stiffness: w * w, damping: 2.0 * w }
    }
}

pub enum Command {
    Animate(Transition),
    Impulse { prop: PropId, velocity: f32 },
    /// Only in naive mode: the logic lives on the thread that paints, as in
    /// QtQuick, and its work stops the clock for everything.
    Block(Duration),
}

pub enum ToRender {
    Scene(Scene),
    /// A reload that does not go through. A runtime that reloads on save cannot
    /// report it only through a console that maybe nobody looks at: it is shown on the
    /// surface itself, which is where whoever writes it is already looking. `None`
    /// removes it: the scene is fine again.
    ReloadError(Option<String>),
    /// One more surface to paint on: a monitor that was already there or that was just
    /// plugged in.
    Sheet(Box<crate::gpu::NewSheet>),
    SheetGone(u32),
    /// The compositor has already shown that sheet's last frame and wants another.
    Frame(u32),
    /// What was seen on screen behind a glass, with it on top.
    Backdrop(Box<crate::platform::Backdrop>),
    /// The workshop has finished something: a layout, some images.
    Workshop(Box<crate::text::Parcel>),
    Scale(u32, f32),
    /// A surface has changed size: a window someone is stretching.
    SheetSize(u32, (f32, f32)),
    Command(Command),
    /// The boundary: the logic tells what happens, and nothing more.
    Fact(&'static str, f32),
    Text(&'static str, String),
    Signal(&'static str),
    /// A signal that comes from outside the program: the scene and the logic hear it.
    ExternalSignal(&'static str, Option<f32>),
    Gesture(&'static str),
    Pointer(Option<(f32, f32)>),
    /// Where the mouse is on the whole desktop, wherever it is, and the
    /// monitors as they are placed on it: name, x, y, width, height. Only
    /// where the system can say it (Hyprland) and the scene asks for it.
    Cursor((f32, f32), Vec<(String, [i32; 4])>),
    /// 0 is the left one, 1 the right one, 2 the middle one.
    Button(u8, bool),
    /// The same, from an agent's own seat: a press on a zone with `agent: no`
    /// does not get through.
    AgentButton(u8, bool),
    /// Wheel notches: positive, upwards.
    Wheel(f32),
    /// Fingers moving what is under the pointer (a touchpad's two, one on a
    /// phone's glass): `x`, `y`, how far it goes, in units, for a window
    /// (down and to the right, positive: as Wayland says it), and the
    /// `notches` that is worth to the scene's own zones.
    Scroll { x: f32, y: f32, notches: f32 },
    /// The fingers were lifted.
    ScrollEnd,
    /// Nothing happened, but a round is wanted: the logic has started to
    /// listen to every frame, and a render at rest would not tell it of any.
    Wake,
    /// The logic has finished with the frame it was told of: what it said
    /// while at it came before this, and goes into the frame being made.
    FrameSeen,
    /// Live windows' frames on the card to be summed, for the thumbnails
    /// service to tell a new picture from the same one again: answered
    /// without a round, at rest too.
    Thumbnails,
    /// The name of the key, what it types if it types anything, and what it was pressed with.
    /// And its evdev code, to hand it to a window of the scene's compositor (0 if made up).
    Key(String, Option<String>, Mods, u32),
    KeyReleased(String, u32),
    /// The surface has gained or lost the keyboard.
    KeyboardFocus(bool),
    /// Put the text cursor in a field, or take it away from wherever it is.
    FocusField(Option<&'static str>),
    /// How this user's keys repeat: after how many ms it starts and every how many
    /// it goes on. `None`: they have no repetition. The system says so; if it says nothing, 400 and 33.
    KeyRepeat(Option<(u32, u32)>),
    /// A fact set from outside, as it was written: `true`, `critical`, `3`. The render knows what type it is.
    ExternalFact(&'static str, String),
    /// The system has closed a popup: someone clicked outside it.
    PopupClosed(usize),
    /// What the compositor says about the lock: `true`, the session IS locked
    /// (and not before); `false`, it has not granted it or has considered it over.
    LockScreen(bool),
    /// Someone from outside asks how much a fact, a text or a property is worth.
    Query(&'static str, std::sync::mpsc::Sender<String>),
    /// Someone from outside asks what there is to read and touch: the
    /// scene as a tree (`describe`), as text or as JSON.
    Describe(bool, std::sync::mpsc::Sender<String>),
    /// An agent asks the scene to do something by name, as a hand would
    /// (`press save`); it is answered with what happened.
    Act(crate::agent::Act, std::sync::mpsc::Sender<String>),
    /// `wait EXPR [TIMEOUT]`: answered as soon as it holds, or when it is late.
    Wait(String, std::sync::mpsc::Sender<String>),
    /// `watch`: a line for each thing that happens, until then.
    Watch(std::sync::mpsc::Sender<String>, std::time::Instant),
    /// `watch 0 lay. free.`: the facts and texts whose names start so, a line
    /// for a program each time one changes.
    Follow(Box<crate::agent::Follow>),
    /// `pleamar --report`: measure every frame from now (`None`), or answer
    /// with what was measured and stop (`Some`).
    Probe(Option<std::sync::mpsc::Sender<String>>),
    /// Something has been dropped on it, dragged from another application: (type, content).
    Dropped(String, String),
    /// What the compositor inside the scene has to say.
    Nest(NestEvent),
    /// Someone wants a window's picture every time it draws (sharing it,
    /// a thumbnail of it): its slot, who asks (a number of theirs: several
    /// may watch the same window), and where to send it. `None` stops theirs.
    WatchWindow(usize, u64, Option<std::sync::mpsc::Sender<WindowPicture>>),
    /// What was on screen has been lost —back from another TTY—: everything is painted again.
    Repaint,
    Quit,
}

/// `windows win max 6`: its name, and how many windows it holds at once.
#[derive(Clone, Debug)]
pub struct Nest {
    pub name: String,
    pub max: usize,
    /// `dock programs`: the dock is of the programs running, wherever their
    /// windows are —any monitor, any workspace—, as a Mac's: every monitor's
    /// list is that one, an item counts all its program's windows, and a
    /// press on it goes to the one used last, its workspace shown with it.
    pub programs: bool,
    /// `workspaces kept`: a monitor's workspaces are the ones made, and stay
    /// while empty and out of sight, as a Mac's desktops: one more is made by
    /// going one past the last, and one goes when it is closed (`close
    /// workspace`). Without it they are tide pools: an empty one not shown
    /// dries up.
    pub kept: bool,
    /// `placement manual`: a window that opens is left out of the layout
    /// (`place`, `among`, `rank`, `order`, `on`) until the scene takes it
    /// with `accept win(i)`; meanwhile `win.$i.pending` is true.
    pub manual: bool,
}

/// A program pinned to the dock: the names its windows go by (their app id
/// or class, lowercase), its icon, and how it is started.
#[derive(Clone, Debug, Default)]
pub struct DockPin {
    pub keys: Vec<String>,
    pub icon: String,
    pub exec: String,
    pub name: String,
}

/// What `dock k` does to that item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DockAction {
    /// Its window (the next one, if one has the keyboard), or its program started.
    Go,
    /// What was just dropped on it, opened with it.
    OpenDrop,
    Pin,
    Unpin,
    /// Its windows there, asked to close.
    Close,
    /// Its program started again, for another window, whether it has
    /// windows or not (a dock menu's «New window»).
    New,
    /// Its windows there, put away.
    Hide,
}

/// How many items a monitor's dock has at most.
pub const DOCK_ITEMS: usize = 20;

/// What the compositor inside the scene tells the render.
pub enum NestEvent {
    /// Where programs connect: `WAYLAND_DISPLAY`.
    Socket(String),
    /// A window opened, on that monitor (the copy of the scene the pointer was on).
    Opened { slot: usize, title: String, app: String, screen: usize },
    /// A window went to another monitor.
    Screen(usize, usize),
    /// A window went to another monitor with its workspace: its own went
    /// away (unplugged), or came back. The workspace goes whole with it,
    /// not mixed into the one shown there.
    Moved(usize, usize),
    /// Someone asked for that window (a dock, Marea's finder, a link that opens
    /// the browser already running): its monitor shows its workspace.
    Reveal(usize),
    /// A window goes to that workspace: a rule of the compositor's said so
    /// when it opened (`window app=discord workspace 3`).
    Workspace(usize, usize),
    /// What other programs' bars keep for themselves on that monitor, at
    /// each edge: top, right, bottom, left (layer-shell's exclusive zones).
    Reserved(usize, [f32; 4]),
    Title(usize, String),
    App(usize, String),
    /// The program a window belongs to, as its `.desktop` says: its icon,
    /// its name, and how it starts (to open a file dropped on its dock item).
    Program { slot: usize, icon: String, name: String, exec: String },
    /// The programs pinned to the shore (pleamar-wm's dock), in order.
    Dock(Vec<DockPin>),
    /// A window is fullscreen now, or no longer: it asked, or the scene did.
    Fullscreen(usize, bool),
    /// A window asks for attention (it asked to come forward with nothing of
    /// the user's behind it: a message arrived), or no longer (it got the
    /// keyboard). A dock makes its icon hop, as macOS does.
    Urgent(usize, bool),
    /// A window is a dialog (it has a parent, or a size of its own it
    /// cannot leave): it is left out of the layout's order.
    Dialog(usize, bool),
    /// A window lets the compositor draw its frame (server-side decorations),
    /// or draws its own title bar and buttons (client-side, as GTK 4 does).
    Framed(usize, bool),
    /// A window asks, from its own title bar or edge, to be carried with the
    /// mouse (1) or stretched (2) by `edges` (xdg-shell's: 1 top, 2 bottom,
    /// 4 left, 8 right, added); 0 when the button is let go.
    Held { slot: usize, how: u32, edges: u32 },
    /// A window is put away now (minimized), or back: it asked, the scene
    /// did, or whoever lists the windows (Marea) did.
    Minimized(usize, bool),
    /// A window floats now, or is back in the layout (`float`, `tile`). The
    /// compositor keeps it, so it outlives a reload of the scene.
    Floating(usize, bool),
    /// What a window shows now: its pieces —its surface, its subsurfaces, its
    /// menus— in the order they are drawn, placed from the corner of its main
    /// surface, and where the window itself is in that surface (a program may
    /// draw its shadow around it).
    Frame { slot: usize, geometry: [i32; 4], pieces: Vec<WindowPiece> },
    /// Buffers of the programs that no longer exist: what the render kept of them goes.
    Forget(Vec<u64>),
    Closed(usize),
    Focused(Option<usize>),
    /// The slots in the order the scene lays them out in.
    Order(Vec<usize>),
    /// The cursor the window under the pointer asks for.
    Cursor(Cursor),
    /// A program is dragging something (drag and drop): the pointer goes to
    /// whichever window it is over, not only to the one it was pressed on.
    Dragging(bool),
    /// The compositor asks the scene to choose (what to share of the screen):
    /// 1 a monitor, 2 a window, 3 either; 0, it no longer asks. The scene sees
    /// it as `win.picking`, and answers with `pick`.
    Pick(u32),
}

/// A window as it is now, for whoever shares it: its box (the window, without
/// the shadow its program may draw around it), in pixels, BGRA premultiplied.
pub struct WindowPicture {
    pub size: (u32, u32),
    /// Shared by all who watch it: read once from the card, not once each.
    pub pixels: std::sync::Arc<Vec<u8>>,
}

/// What the scene chose, when the compositor asked it to (`pick`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Picked {
    Window(usize),
    Screen(usize),
}

/// One surface of a window: which one (it keeps its place on the card from
/// frame to frame), where from the main surface's corner, and how big —in
/// the window's own units—. Its pixels may be more than that (a program
/// drawing at 1.5 or 2 for a monitor that is): `px` is how many, and `src`
/// which part of them it shows (all of them, unless it crops: wp-viewporter).
#[derive(Debug)]
pub struct WindowPiece {
    pub id: u64,
    pub at: (i32, i32),
    pub size: (u32, u32),
    pub px: (u32, u32),
    pub src: [f32; 4],
    pub content: PieceContent,
}

#[derive(Debug)]
pub enum PieceContent {
    /// The same as last time.
    Kept,
    /// BGRA, premultiplied, opaque if the program said so.
    Pixels(Vec<u8>),
    /// Already on the card: a program that draws with the GPU hands it over
    /// as it is, and it is copied there without passing through here.
    #[cfg(unix)]
    Dmabuf(DmabufPiece),
}

/// A single-plane dmabuf: what it takes to read it.
#[cfg(unix)]
#[derive(Debug)]
pub struct DmabufPiece {
    /// Which buffer of the program: the render keeps it read once, and the
    /// compositor hands it back to the program once it has been copied.
    pub buffer: u64,
    /// Its planes: one for most, two for video (NV12), more for layouts that
    /// keep compression data beside the image.
    pub planes: Vec<DmabufPlane>,
    pub fourcc: u32,
    pub modifier: u64,
}

/// One plane of a buffer on the card: its memory, and where it is in it.
#[cfg(unix)]
#[derive(Debug)]
pub struct DmabufPlane {
    pub fd: std::os::fd::OwnedFd,
    pub stride: u32,
    pub offset: u32,
}

/// What the render tells the compositor inside the scene. Pointer positions
/// are in the window's own pixels, from its corner.
#[derive(Debug)]
pub enum ToNest {
    Size(i32, i32),
    /// A program pinned to the dock (true) or unpinned: by its name.
    Pin(String, bool),
    Pointer { slot: usize, x: f64, y: f64 },
    PointerOut,
    /// evdev codes: 0x110 left, 0x111 right, 0x112 middle.
    Button { code: u32, down: bool },
    Wheel(f64),
    /// Fingers moving what is under the pointer, in the window's own pixels:
    /// down and to the right, positive. And the fingers lifted.
    Scroll(f64, f64),
    ScrollEnd,
    /// An evdev key code.
    Key { code: u32, down: bool },
    /// Whether pleamar's own window has the keyboard.
    HostFocus(bool),
    /// The size it is to have; 0 × 0, the one it chooses.
    Configure { slot: usize, w: i32, h: i32 },
    Focus(usize),
    Close(usize),
    Promote(usize),
    /// That window to fullscreen, or back from it.
    Fullscreen(usize),
    /// That window put away (true), or brought back (false).
    Minimize(usize, bool),
    /// That window floating over the layout (true), or back in it (false).
    Float(usize, bool),
    /// That window, to that monitor.
    Send(usize, usize),
    /// Those two windows change places.
    Swap(usize, usize),
    /// Nobody has the keyboard: what had it is on a workspace no longer shown.
    Blur,
    /// Where a window is seen: on which monitor (by its name) and its box
    /// there, in pixels. Told when it changes (for screenshots of it); no
    /// monitor and an empty box, once it is no longer drawn.
    Shown { slot: usize, monitor: String, rect: [i32; 4] },
    /// The monitor the pointer is on: where new windows open.
    OnScreen(usize),
    Launch(String),
    /// The render has painted: the programs may draw their next frame.
    FrameDone,
    /// The card the scene is painted on, and what it can read straight from a
    /// program's memory on the card: its render node, and the (fourcc,
    /// modifier) pairs it takes.
    Gpu { device: u64, formats: Vec<(u32, u64)> },
    /// These buffers have been copied: they can go back to their program.
    Released(Vec<u64>),
    /// What the scene chose when asked (`pick`); `None`, nothing.
    Picked(Option<Picked>),
    Quit,
}

/// The texts a `secret: true` field edits. What they hold is not written
/// in any log, nor in the echo of the rehearsals: it is a password. It belongs to the whole
/// process because whoever does the echo —the logic— does not have the scene in front of it.
pub static SECRETS: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

#[derive(Clone, Debug)]
pub enum Event {
    Enter(&'static str),
    Leave(&'static str),
    Press(&'static str),
    Release(&'static str),
    Wheel(&'static str, f32),
    Key(String, Option<String>),
    /// A key went down —once, however long it is held— or came up: for a
    /// logic that plays, which wants to know what is held and not what is typed.
    /// By its plain name, a letter always in lower case: `Left`, `space`, `a`.
    KeyDown(String),
    KeyUp(String),
    /// A frame is about to be painted, for a logic that asked to hear each one
    /// (`on("frame", …)`): the seconds since the last it was told of, where the
    /// mouse is on the scene if it is over it, and which of its buttons are down
    /// (1 the left one, 2 the right one, 4 the middle one).
    Frame(f32, Option<(f32, f32)>, u8),
    /// What a field says now, key by key.
    Text(&'static str, String),
    /// Enter in a field.
    Submit(&'static str, String),
    Focus(bool),
    /// Something dropped on a zone: (zone, type, content).
    Received(&'static str, String, String),
    Alarm(&'static str),
    /// A scene signal that goes out to the logic, with its payload if it brings one.
    Signal(&'static str, Option<f32>),
    /// A rule has changed a fact: the logic keeps track of what is true.
    Fact(&'static str, f32),
    /// A command that was launched has finished: (which one, what it wrote, with what code).
    Process(u32, String, i32),
    /// A command that is still running has written a line.
    Line(u32, String),
    /// A system service has something new to tell.
    Data(String, crate::platform::SysValue),
    /// The logic file has changed.
    ReloadLogic,
    /// The scene has been reloaded: these are now its facts and its texts.
    NewScene(Vec<(&'static str, f32)>, Vec<(&'static str, String)>, Permissions, Vec<Model>, Vec<(String, FactType)>, Vec<Plugin>, Vec<&'static str>, Vec<Service>, Translations),
    /// A layer has changed hands: (layer, who wins now).
    Layer(&'static str, &'static str),
    /// A gesture was asked for and there was one of a higher class in place.
    GestureRejected(&'static str),
    /// The mouseless mode: "do the next thing you would do".
    Demo,
}

/// How many logics —the scene's, its plugins'— want to hear every frame. While
/// it is not zero the render does not rest, and tells them of each one.
pub static FRAME_LISTENERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// A frame was told and the logic has not finished with it: the next ones are
/// not queued behind it, their time is added to the one it is told of next.
pub static FRAME_OWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// An animated property: position, velocity and where it wants to go.
#[derive(Clone, Copy)]
pub struct Animated {
    pub x: f32,
    pub v: f32,
    pub target: f32,
    pub spring: Spring,
}

impl Animated {
    pub fn at(x: f32, spring: Spring) -> Self {
        Animated { x, v: 0.0, target: x, spring }
    }

    /// Semi-implicit Euler in steps of 2 ms at most: stable even if a
    /// frame arrives late.
    pub fn step(&mut self, dt: f32) {
        if self.spring.is_instant() {
            self.settle();
            return;
        }
        // Still, where it is going: nothing to work out. Most of a scene's
        // properties are like this most of the time, and each one used to
        // take eight small steps a frame to stay where it was.
        if self.v == 0.0 && self.x == self.target {
            return;
        }
        if (self.x - self.target).abs() < 1e-4 && self.v.abs() < 1e-3 {
            self.settle();
            return;
        }
        let steps = (dt / 0.002).ceil().max(1.0);
        let h = dt / steps;
        for _ in 0..steps as u32 {
            let a = -self.spring.stiffness * (self.x - self.target) - self.spring.damping * self.v;
            self.v += a * h;
            self.x += self.v * h;
        }
    }

    pub fn at_rest(&self) -> bool {
        (self.x - self.target).abs() < 0.02 && self.v.abs() < 0.08
    }

    pub fn settle(&mut self) {
        self.x = self.target;
        self.v = 0.0;
    }

    /// For the ones a behavior writes directly.
    pub fn set(&mut self, x: f32) {
        self.x = x;
        self.target = x;
        self.v = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animated(props: &[f32]) -> Vec<Animated> {
        props.iter().map(|&x| Animated { x, v: 0.0, target: x, spring: Spring::LIVELY }).collect()
    }

    #[test]
    fn a_log_says_what_was_read_since_it_began_once_each() {
        // fact 1 + prop 0 + prop 0, with something read before the log began.
        let e = Expr::Add(Box::new(Expr::Add(Box::new(Expr::H(FactId(1))), Box::new(Expr::P(PropId(0))))), Box::new(Expr::P(PropId(0))));
        let props = animated(&[3.0, 4.0]);
        let mut seen = Seen::default();
        seen.reset(2, 2);
        let c = Ctx { props: &props, facts: &[0.0, 5.0], seen: Some(&seen) };
        Expr::P(PropId(1)).eval(c);
        seen.log_from_here();
        assert_eq!(e.eval(c), 11.0);
        let mut log = Vec::new();
        seen.log_into(&mut log);
        assert_eq!(log, [0, 1 | SEEN_FACT]);
        // It no longer writes down, and the bits were noted as ever.
        Expr::P(PropId(1)).eval(c);
        seen.log_into(&mut log);
        assert_eq!(log, [0, 1 | SEEN_FACT]);
        assert_eq!(seen.props().collect::<Vec<_>>(), [0, 1]);
        assert_eq!(seen.facts().collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn what_an_evaluation_reads_is_only_what_it_reached() {
        // if(fact 0, fact 1 + prop 0, 7): with fact 0 false, nothing else is read.
        let e = Expr::Mix(Box::new(Expr::K(7.0)), Box::new(Expr::Add(Box::new(Expr::H(FactId(1))), Box::new(Expr::P(PropId(0))))), Box::new(Expr::H(FactId(0))));
        let props = animated(&[3.0]);
        let mut seen = Seen::default();
        seen.reset(70, 1);
        assert_eq!(e.eval(Ctx { props: &props, facts: &[0.0, 5.0], seen: Some(&seen) }), 7.0);
        assert_eq!(seen.facts().collect::<Vec<_>>(), [0]);
        assert_eq!(seen.props().count(), 0);
        // True, it reads the rest; and taking them leaves it clean for the next.
        assert_eq!(e.eval(Ctx { props: &props, facts: &[1.0, 5.0], seen: Some(&seen) }), 8.0);
        let (mut facts, mut read_props) = (Vec::new(), Vec::new());
        seen.take(&mut facts, &mut read_props);
        assert_eq!((facts, read_props), (vec![0, 1], vec![0]));
        assert_eq!(seen.facts().count() + seen.props().count(), 0);
        // A condition stops at its first false side; a fact past the first word is noted too.
        let both = Expr::And(Box::new(Expr::H(FactId(0))), Box::new(Expr::H(FactId(65))));
        let mut facts = [0.0; 70];
        both.eval(Ctx { props: &props, facts: &facts, seen: Some(&seen) });
        assert_eq!(seen.facts().collect::<Vec<_>>(), [0]);
        facts[0] = 1.0;
        both.eval(Ctx { props: &props, facts: &facts, seen: Some(&seen) });
        assert_eq!(seen.facts().collect::<Vec<_>>(), [0, 65]);
    }

    #[test]
    fn constants_fold_on_read() {
        assert!(matches!(Expr::K(2.0) * 3.0 + 1.0, Expr::K(7.0)));
        assert!(matches!(Expr::K(0.0) - Expr::K(4.0), Expr::K(-4.0)));
        assert!(matches!(PropId(0) * 1.0, Expr::P(_)));
        assert!(matches!(PropId(0) + 0.0, Expr::P(_)));
        assert!(matches!(Expr::K(3.0).mix(PropId(0), 0.0), Expr::K(3.0)));
        assert!(matches!(Expr::K(3.0).mix(PropId(0), 1.0), Expr::P(_)));
    }

    #[test]
    fn mix_matches_lerp() {
        let props = animated(&[0.25, 1.0, 0.0]);
        let c = Ctx { props: &props, facts: &[], seen: None };
        let (a, b) = (Expr::K(10.0), Expr::K(20.0));
        for p in 0..3 {
            let t = props[p].x;
            assert_eq!(a.clone().mix(b.clone() + PropId(2), PropId(p as u16)).eval(c), 10.0 + 10.0 * t);
        }
    }

    #[test]
    fn bezier_matches_css() {
        // CSS's `ease` is cubic-bezier(0.25, 0.1, 0.25, 1): at half the time it is at 80.24 %.
        let ease = Curve::Bezier(0.25, 0.1, 0.25, 1.0);
        assert!((ease.apply(0.5) - 0.8024).abs() < 0.002, "{}", ease.apply(0.5));
        assert_eq!(ease.apply(0.0), 0.0);
        assert!((ease.apply(1.0) - 1.0).abs() < 1e-5);
        // The one that overshoots, from any design tool.
        let back = Curve::Bezier(0.34, 1.56, 0.64, 1.0);
        assert!((0..=100).map(|k| back.apply(k as f32 / 100.0)).any(|v| v > 1.05));
    }

    #[test]
    fn noise_is_smooth_and_bounded() {
        let mut last = noise1(0.0);
        for k in 1..4000 {
            let x = k as f32 * 0.01;
            let v = noise1(x);
            assert!((-1.0..=1.0).contains(&v));
            assert!((v - last).abs() < 0.08, "a jump at {x}: {last} → {v}");
            last = v;
            let w = noise2(x, x * 0.7);
            assert!((-1.0..=1.0).contains(&w));
        }
        assert_eq!(noise1(12.34), noise1(12.34));
        assert!(Un::Random.apply(3.0) >= 0.0 && Un::Random.apply(3.0) < 1.0);
        assert_eq!(Bin::Mod.apply(-1.0, 3.0), 2.0);
        assert!((Bin::Atan2.apply(1.0, 1.0) - 45.0).abs() < 1e-4);
    }

    #[test]
    fn if_chain_grows_linearly() {
        let mut e = Expr::K(1.0);
        for k in 0..30 {
            e = e.mix(Expr::K(k as f32), PropId(0).e().gt(k as f32));
        }
        assert!(e.node_count() < 200, "{} nodes", e.node_count());
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    /// Every standard name comes back as itself, and so does its number.
    #[test]
    fn cursors_go_by_their_names() {
        for n in CURSOR_NAMES {
            let c = Cursor::from_name(n).unwrap();
            assert_eq!(c.name(), *n);
            assert_eq!(Cursor::from_code(c.code()), c);
        }
        assert_eq!(Cursor::from_name("ew_resize"), Some(Cursor::EwResize));
        assert_eq!(Cursor::from_name("pointer"), Some(Cursor::Hand));
        assert_eq!(Cursor::from_name("zoom_in").map(Cursor::name), Some("zoom-in"));
        assert_eq!(Cursor::from_name("bogus"), None);
    }
}
