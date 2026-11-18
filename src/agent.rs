//! What the scene tells whoever drives it from outside —an agent, a script, a
//! screen reader one day—: what there is to read and touch, by name, with what
//! it says and where it is. Nothing here is guessed: it is the same data the
//! render draws with and presses with. See `docs/12-agents.md`.

use crate::gpu::{content_text, SeenText};
use crate::scene::{Ctx, Instr, Reach, Scene, Trigger, Zone};
use std::fmt::Write;

/// A piece of the scene that is on screen this frame: an open surface, or a
/// popup, and the stretch of the scene's plane it shows.
#[derive(Clone)]
pub struct Shown {
    pub surface: usize,
    pub popup: Option<usize>,
    pub bounds: [f32; 4],
    pub scale: f32,
}

/// What the render knows this frame and the scene does not.
pub struct Sight<'a> {
    pub shown: Vec<Shown>,
    pub texts_seen: &'a [SeenText],
    /// By instruction: inside a group that is hidden, or in a copy whose
    /// monitor is not there. Its zones are not there either.
    pub gone: &'a dyn Fn(usize) -> bool,
    /// Which zone is on top of which, when groups with `z:` changed it.
    pub rank: Option<&'a [(usize, u8, usize)]>,
    /// What the render already worked out this round: each zone alive (there and
    /// active) and its box if it is. Without it, every zone is worked out again.
    pub zones: Option<(&'a [bool], &'a [Option<[f32; 4]>])>,
    /// The scene's lookups, kept between looks (see `Index`).
    pub index: Option<&'a Index>,
    /// Looked at for the person themself, through their screen reader (`a11y.rs`),
    /// not for an agent: what only a person may use (`agent: no`) can be used, and
    /// says its words —a secret field's never—. What is hidden from agents
    /// (`agent: hidden`) stays hidden: AT-SPI is read by whatever runs as the person.
    pub person: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Role {
    Button,
    Toggle,
    Tab,
    Link,
    Item,
    Slider,
    Field,
    List,
    Region,
    /// Another program's window, held by the scene (`windows`).
    Window,
    /// Words drawn outside every zone: a title, a status line.
    Text,
}

impl Role {
    fn word(self) -> &'static str {
        match self {
            Role::Button => "button",
            Role::Toggle => "toggle",
            Role::Tab => "tab",
            Role::Link => "link",
            Role::Item => "item",
            Role::Slider => "slider",
            Role::Field => "field",
            Role::List => "list",
            Role::Region => "region",
            Role::Window => "window",
            Role::Text => "text",
        }
    }
}

/// One thing to read or touch.
#[derive(Debug)]
pub struct Node {
    /// Its zone; a loose text has none (`usize::MAX`).
    pub zone: usize,
    /// Where it was written: what the nodes are listed by.
    pub order: usize,
    pub name: &'static str,
    pub role: Role,
    pub label: String,
    pub value: Option<String>,
    pub inactive: bool,
    pub covered_by: Option<&'static str>,
    /// `checked:` and `selected:`, when it says them.
    pub checked: Option<bool>,
    pub selected: Option<bool>,
    /// Scrolled out of its list: acting on it scrolls it into sight first.
    pub off_view: bool,
    pub persons: bool,
    /// A secret field (`secret: true`): its words are never told.
    #[cfg_attr(not(all(target_os = "linux", feature = "accessibility")), allow(dead_code))]
    pub secret: bool,
    /// In the surface's logical pixels: x, y, width, height.
    pub at: [f32; 4],
    /// The list it is inside, if any (an index into the nodes).
    pub inside: Option<usize>,
}

/// One surface or popup, with what it holds.
pub struct Part {
    /// Which of `Sight::shown` it is.
    pub shown: usize,
    pub name: String,
    pub kind: String,
    pub size: (f32, f32),
    pub scale: f32,
    pub nodes: Vec<Node>,
}

/// What a zone is, from what it does: its rules, its cursor, its field.
/// The zone a rule's trigger is about, and how: 0 pressed, 1 dragged, 2 the wheel.
fn trigger_zone(t: &Trigger) -> Option<(usize, u8)> {
    match t {
        Trigger::Press(id) | Trigger::Grab(id) | Trigger::PressWith(id, _) | Trigger::Release(id) | Trigger::Receive(id) | Trigger::Carry(id) => Some((id.0 as usize, 0)),
        Trigger::Hold { zone, .. } => Some((zone.0 as usize, 0)),
        Trigger::Drag(id) => Some((id.0 as usize, 1)),
        Trigger::Wheel(id) => Some((id.0 as usize, 2)),
        _ => None,
    }
}

fn add_trigger(t: &mut (bool, bool, bool), how: u8) {
    match how {
        0 => t.0 = true,
        1 => t.1 = true,
        _ => t.2 = true,
    }
}

/// What `describe` looks up for every zone, gathered once: a scene of ten
/// thousand zones went through its 180 000 instructions and its 11 000 rules
/// for each of them, a quarter of a second on the thread that paints, at every
/// look of an agent.
/// Made once for a scene and kept while it lasts.
pub struct Index {
    /// The first field of each zone name: the instruction where it is.
    fields: std::collections::HashMap<&'static str, usize>,
    triggers: Vec<(bool, bool, bool)>,
    texts: std::collections::HashMap<&'static str, usize>,
}

impl Index {
    pub fn new(scene: &Scene) -> Self {
        let mut fields = std::collections::HashMap::new();
        for (k, i) in scene.instrs.iter().enumerate() {
            if let Instr::Field { zone, .. } = i {
                fields.entry(*zone).or_insert(k);
            }
        }
        let mut triggers = vec![(false, false, false); scene.zones.len()];
        for r in &scene.rules {
            if let Some((z, how)) = trigger_zone(&r.when)
                && let Some(t) = triggers.get_mut(z)
            {
                add_trigger(t, how);
            }
        }
        let mut texts = std::collections::HashMap::new();
        for (k, t) in scene.texts.iter().enumerate() {
            texts.entry(t.0).or_insert(k);
        }
        Index { fields, triggers, texts }
    }

    /// As `field_of`: the field drawn where the zone is first, any other with its name after.
    fn field<'s>(&self, scene: &'s Scene, z: &Zone) -> Option<(usize, &'s crate::scene::Content, bool)> {
        let at = match scene.instrs.get(z.at) {
            Some(Instr::Field { zone, .. }) if *zone == z.id => z.at,
            _ => *self.fields.get(z.id)?,
        };
        match scene.instrs.get(at) {
            Some(Instr::Field { text, placeholder, secret, .. }) => Some((text.0 as usize, placeholder, *secret)),
            _ => None,
        }
    }
}

fn role_with(scene: &Scene, z: &Zone, is_field: bool, (presses, drags, wheels): (bool, bool, bool)) -> Role {
    // Said by the scene (`role: toggle`), it is that.
    if let Some(r) = z.told.as_ref().and_then(|t| t.role) {
        return match r {
            "toggle" => Role::Toggle,
            "slider" => Role::Slider,
            "tab" => Role::Tab,
            "link" => Role::Link,
            "item" => Role::Item,
            "list" => Role::List,
            "region" => Role::Region,
            _ => Role::Button,
        };
    }
    if is_field {
        return Role::Field;
    }
    if matches!(scene.instrs.get(z.at), Some(Instr::Window { .. })) {
        return Role::Window;
    }
    if z.scrolls.is_some() {
        return Role::List;
    }
    // Dragged, or only turned with the wheel: a volume bar, a dial. A press
    // that also listens to the wheel is still a button (mute, and the wheel for the volume).
    if drags || (wheels && !presses) {
        return Role::Slider;
    }
    if presses || z.cursor == crate::scene::Cursor::Hand || z.carries.is_some() {
        // A copy of a `for` or a `repeat` (`hit#r3`), not of a component (`hit#Row5`):
        // one of a list's rows.
        let copy = z.id.rsplit('#').next().filter(|_| z.id.contains('#'));
        if copy.is_some_and(|s| s.starts_with(|c: char| c.is_ascii_lowercase()) && !s.starts_with("screen") && !s.starts_with("between")) {
            return Role::Item;
        }
        return Role::Button;
    }
    Role::Region
}

/// The field a zone belongs to: its text, its placeholder and whether it is secret.
pub fn field_of<'s>(scene: &'s Scene, z: &Zone) -> Option<(usize, &'s crate::scene::Content, bool)> {
    match scene.instrs.get(z.at) {
        Some(Instr::Field { text, zone, placeholder, secret, .. }) if *zone == z.id => Some((text.0 as usize, placeholder, *secret)),
        _ => scene.instrs.iter().find_map(|i| match i {
            Instr::Field { text, zone, placeholder, secret, .. } if *zone == z.id => Some((text.0 as usize, placeholder, *secret)),
            _ => None,
        }),
    }
}

fn area(b: [f32; 4]) -> f32 {
    (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0)
}

pub fn inside_box(b: [f32; 4], p: (f32, f32)) -> bool {
    inside(b, p.0, p.1)
}

fn inside(b: [f32; 4], x: f32, y: f32) -> bool {
    x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]
}

/// The scene as it is this frame: every surface and popup on screen, and in
/// each one what can be read and touched.
pub fn describe(scene: &Scene, c: Ctx, texts: &[String], sight: &Sight) -> Vec<Part> {
    // Which zones are there at all, and their box.
    // (With the render's, only a zone there but inactive has its box worked out here:
    // a greyed-out button still says its words.)
    let n = scene.zones.len();
    let known = sight.zones.filter(|(live, boxes)| live.len() == n && boxes.len() == n);
    let there: Vec<Option<[f32; 4]>> = scene
        .zones
        .iter()
        .enumerate()
        .map(|(k, z)| match known {
            _ if z.reach == Reach::Hidden || (sight.gone)(z.at) => None,
            Some((live, boxes)) if live[k] => boxes[k],
            _ => z.bounds(c),
        })
        .collect();
    let active: Vec<bool> = match known {
        // (Alive is there and active: for a zone there, the same.)
        Some((live, _)) => live.to_vec(),
        None => scene.zones.iter().map(|z| z.active.is_true(c)).collect(),
    };
    let made;
    let index = match sight.index {
        Some(i) if i.triggers.len() == n => i,
        _ => {
            made = Index::new(scene);
            &made
        }
    };
    let rank = |k: usize| sight.rank.map_or((k, 0, 0), |r| r[k]);
    // What a press can reach at all, with its box: only a point in that box asks the shape.
    let pressable: Vec<(usize, [f32; 4])> = (0..scene.zones.len()).filter(|k| active[*k]).filter_map(|k| there[k].map(|b| (k, b))).collect();
    // The one on top at a point: what a press there would reach.
    let top_at = |x: f32, y: f32| {
        pressable
            .iter()
            .filter(|(k, b)| inside(*b, x, y) && scene.zones[*k].contains(c, x, y))
            .map(|(k, _)| *k)
            .max_by_key(|k| (rank(*k), *k))
    };
    let mut parts = Vec::new();
    for (shown, s) in sight.shown.iter().enumerate() {
        let Some(surface) = scene.surfaces.get(s.surface) else { continue };
        // The lock screen is never told, nor what is kept out of captures or from agents.
        // Not to a person's reader either: whatever runs as the person reads AT-SPI, an agent too.
        if surface.lock_screen || surface.hidden_from_captures || surface.agent_hidden {
            continue;
        }
        let (name, kind) = match s.popup.and_then(|p| scene.popups.get(p)) {
            Some(p) => (p.name.to_owned(), "popup".to_owned()),
            None => {
                let name = if surface.name.is_empty() { "main".to_owned() } else { surface.name.clone() };
                let name = if surface.instance > 0 || matches!(surface.screens, crate::scene::Screens::Number(_)) { format!("{name} (screen {})", surface.instance) } else { name };
                let kind = match &surface.window {
                    Some(t) if !t.is_empty() => format!("window «{t}»"),
                    Some(_) => "window".to_owned(),
                    None => "panel".to_owned(),
                };
                (name, kind)
            }
        };
        let b = s.bounds;
        let centre = |z: [f32; 4]| ((z[0] + z[2]) * 0.5, (z[1] + z[3]) * 0.5);
        let on_this = |k: usize| there[k].is_some_and(|z| {
            let (x, y) = centre(z);
            inside(b, x, y)
        });
        // Scrolled out of its list: nothing of it is in sight, by the same measure the
        // pointer presses with (#157). By its centre, a button in a corner of a row and
        // the row itself, at the same place, were said one in view and the other not.
        let off_view = |k: usize| scene.zones[k].within.is_some() && there[k].is_some() && scene.visible_part(k, |j| there[j]).is_none();
        // What is on this surface, and the rows of its lists that are scrolled out of it.
        let mine: Vec<usize> = (0..scene.zones.len())
            .filter(|k| there[*k].is_some() && (on_this(*k) || scene.zones[*k].within.is_some_and(|l| on_this(l.0 as usize))))
            .collect();
        // Each text goes to the smallest zone it falls in: a button's word is
        // the button's, not the panel's around it. A text cut out by a clip
        // only says something for a row scrolled out of sight.
        let mut said: Vec<Vec<&SeenText>> = vec![Vec::new(); scene.zones.len()];
        let mut loose: Vec<&SeenText> = Vec::new();
        for t in sight.texts_seen {
            let (x, y) = centre(t.bounds);
            // An active zone first: a closed menu's zone, still in its place, does not
            // take the words of what is drawn where it would be.
            let smallest = mine
                .iter()
                .filter(|k| there[**k].is_some_and(|z| inside(z, x, y)) && scene.zones[**k].scrolls.is_none() && (!t.clipped || off_view(**k)))
                .min_by(|p, q| (!active[**p], area(there[**p].unwrap())).partial_cmp(&(!active[**q], area(there[**q].unwrap()))).unwrap_or(std::cmp::Ordering::Equal));
            match smallest {
                Some(k) => said[*k].push(t),
                None if !t.clipped && inside(b, x, y) => loose.push(t),
                None => {}
            }
        }
        let mut nodes: Vec<Node> = Vec::new();
        // Inside a copy per monitor every name ends in `#screen0`: said once, in the header.
        let suffix = format!("#screen{}", surface.instance);
        for &k in &mine {
            let z = &scene.zones[k];
            let zb = there[k].unwrap();
            let active = active[k];
            let field = index.field(scene, z);
            let role = role_with(scene, z, field.is_some(), index.triggers[k]);
            let mut words: Vec<&SeenText> = said[k].clone();
            // In reading order: by line, then from the left.
            words.sort_by(|p, q| ((p.bounds[1] / 8.0).round(), p.bounds[0]).partial_cmp(&((q.bounds[1] / 8.0).round(), q.bounds[0])).unwrap_or(std::cmp::Ordering::Equal));
            // An inactive zone with nothing drawn in it is not there for anyone: a
            // closed panel's buttons. One with its words is a button seen greyed out.
            if !active && words.is_empty() {
                continue;
            }
            let label = match (&z.label, field) {
                (Some(l), _) => content_text(l, c, texts).into_owned(),
                (None, Some((text, placeholder, _))) if texts.get(text).is_none_or(|t| t.is_empty()) => content_text(placeholder, c, texts).into_owned(),
                (None, Some(_)) => String::new(),
                (None, None) => {
                    let said = words.iter().map(|t| t.text.trim()).collect::<Vec<_>>().join(" ");
                    // Nothing drawn in it: what its own `.title` says, if the scene
                    // has one (another program's window, `win.3.title`).
                    let own = format!("{}.title", z.id.split('#').next().unwrap_or(z.id));
                    match index.texts.get(own.as_str()).copied() {
                        Some(k) if said.is_empty() => texts.get(k).cloned().unwrap_or_default(),
                        _ => said,
                    }
                }
            };
            let told = z.told.as_deref();
            let value = match (field, told.and_then(|t| t.value.as_ref())) {
                (Some((text, _, secret)), _) => Some(if secret || (z.reach == Reach::Person && !sight.person) { "(hidden)".to_owned() } else { texts.get(text).cloned().unwrap_or_default() }),
                (None, Some(crate::scene::Said::Text(t))) => Some(content_text(t, c, texts).into_owned()),
                // A fact with names says its name (`critical`, `true`); any other sum, its number.
                (None, Some(crate::scene::Said::Number(e))) => Some(match e {
                    crate::scene::Expr::H(f) => {
                        let n = scene.facts[f.0 as usize].0;
                        let v = c.facts[f.0 as usize];
                        scene.types.iter().find(|(t, _)| t == n).map_or_else(|| number(v), |(_, t)| t.to_text(v))
                    }
                    e => number(e.eval(c)),
                }),
                (None, None) => None,
            };
            let checked = told.and_then(|t| t.checked.as_ref()).map(|e| e.is_true(c));
            let selected = told.and_then(|t| t.selected.as_ref()).map(|e| e.is_true(c));
            let (cx, cy) = ((zb[0] + zb[2]) * 0.5, (zb[1] + zb[3]) * 0.5);
            let off = off_view(k);
            let covered_by = match (active && !off, role) {
                (true, Role::Button | Role::Toggle | Role::Tab | Role::Link | Role::Item | Role::Field | Role::Slider) => top_at(cx, cy).filter(|t| *t != k && scene.zones[*t].scrolls.is_none()).map(|t| scene.zones[t].id.strip_suffix(suffix.as_str()).unwrap_or(scene.zones[t].id)),
                _ => None,
            };
            nodes.push(Node {
                zone: k,
                order: z.at,
                name: z.id.strip_suffix(suffix.as_str()).unwrap_or(z.id),
                role,
                label,
                value,
                inactive: !active,
                covered_by,
                checked,
                selected,
                off_view: off,
                persons: z.reach == Reach::Person,
                secret: field.is_some_and(|f| f.2),
                at: [zb[0] - b[0], zb[1] - b[1], zb[2] - zb[0], zb[3] - zb[1]],
                inside: None,
            });
        }
        // And the words that are no zone's: a title, a status line. Read, not touched.
        for t in loose {
            nodes.push(Node {
                zone: usize::MAX,
                order: t.at,
                name: "",
                role: Role::Text,
                label: t.text.trim().to_owned(),
                value: None,
                inactive: false,
                covered_by: None,
                checked: None,
                selected: None,
                off_view: false,
                persons: false,
                secret: false,
                at: [t.bounds[0] - b[0], t.bounds[1] - b[1], t.bounds[2] - t.bounds[0], t.bounds[3] - t.bounds[1]],
                inside: None,
            });
        }
        // As written: a status line under the button that changes it.
        nodes.sort_by_key(|n| n.order);
        // What is inside a list hangs from it: the rows it scrolls, and
        // whatever else falls inside its window.
        let node_of: std::collections::HashMap<usize, usize> = nodes.iter().enumerate().rev().map(|(j, n)| (n.zone, j)).collect();
        let lists: Vec<usize> = (0..nodes.len()).filter(|j| nodes[*j].role == Role::List).collect();
        for i in 0..nodes.len() {
            if let Some(j) = scene.zones.get(nodes[i].zone).and_then(|z| z.within).and_then(|l| node_of.get(&(l.0 as usize)).copied()) {
                nodes[i].inside = Some(j);
                continue;
            }
            let (x, y) = (nodes[i].at[0] + nodes[i].at[2] * 0.5, nodes[i].at[1] + nodes[i].at[3] * 0.5);
            nodes[i].inside = lists
                .iter()
                .copied()
                .filter(|j| *j != i)
                .filter(|j| {
                    let a = nodes[*j].at;
                    inside([a[0], a[1], a[0] + a[2], a[1] + a[3]], x, y)
                })
                .min_by(|p, q| (nodes[*p].at[2] * nodes[*p].at[3]).total_cmp(&(nodes[*q].at[2] * nodes[*q].at[3])));
        }
        parts.push(Part { shown, name, kind, size: (b[2] - b[0], b[3] - b[1]), scale: s.scale, nodes });
    }
    parts
}

/// The zones a person could press but that nobody says what they are: what
/// a screen reader would read as «button», and nothing else.
pub fn unnamed(parts: &[Part]) -> Vec<&'static str> {
    parts.iter().flat_map(|p| &p.nodes).filter(|n| matches!(n.role, Role::Button | Role::Toggle | Role::Tab | Role::Link | Role::Item | Role::Slider) && n.label.is_empty()).map(|n| n.name).collect()
}

// ── acting by name ──────────────────────────────────────────────

/// What an agent asks the scene to do, by name, as a hand would.
#[derive(Clone, Debug)]
pub enum Act {
    /// A press and release at the zone, `count` times; 0 is the left button.
    Press { name: String, button: u8, count: u8 },
    /// Pressed until its `on hold` fires.
    Hold { name: String },
    /// Pressed at the zone and moved by so much before letting go.
    Drag { name: String, by: (f32, f32) },
    /// Notches of the wheel over it; positive, upwards.
    Wheel { name: String, notches: f32 },
    /// The field focused and this left in it, as typing would.
    Type { name: String, text: String },
    /// A key to the scene, as the keyboard would send it: `Escape`, `Ctrl+z`.
    Key { name: String, mods: crate::scene::Mods, typed: Option<String> },
}

impl Act {
    /// `press save`, `press row.3 right 2`, `hold card`, `drag knob.1 0 -40`,
    /// `wheel list -3`, `type query some words`, `key escape`.
    pub fn parse(what: &str, rest: &str) -> Option<Result<Act, String>> {
        if !matches!(what, "press" | "hold" | "drag" | "wheel" | "type" | "key") {
            return None;
        }
        let mut w = rest.split_whitespace();
        let name = w.next().map(str::to_owned);
        let need = |n: Option<String>| n.ok_or_else(|| format!("`{what}` needs the name of what to {what}"));
        Some((|| match what {
            "press" => {
                let name = need(name)?;
                let (mut button, mut count) = (0, 1);
                for x in w {
                    match x {
                        "left" => button = 0,
                        "right" => button = 1,
                        "middle" => button = 2,
                        "back" => button = 3,
                        "forward" => button = 4,
                        n => count = n.parse::<u8>().ok().filter(|n| (1..=3).contains(n)).ok_or_else(|| format!("`press {name}` takes left, right, middle, back or forward, and how many times (1 to 3): not `{n}`"))?,
                    }
                }
                Ok(Act::Press { name, button, count })
            }
            "hold" => Ok(Act::Hold { name: need(name)? }),
            "drag" => {
                let name = need(name)?;
                let n: Vec<f32> = w.filter_map(|x| x.parse().ok()).collect();
                match n[..] {
                    [dx, dy] => Ok(Act::Drag { name, by: (dx, dy) }),
                    _ => Err(format!("`drag {name}` needs how far, in pixels: `drag {name} 0 -40`")),
                }
            }
            "wheel" => {
                let name = need(name)?;
                let n = w.next().and_then(|x| x.parse::<f32>().ok()).ok_or_else(|| format!("`wheel {name}` needs how many notches: 3 up, -3 down"))?;
                Ok(Act::Wheel { name, notches: n })
            }
            "type" => {
                let name = need(name)?;
                let text = rest.trim_start().strip_prefix(name.as_str()).unwrap_or("").trim_start().to_owned();
                Ok(Act::Type { name, text })
            }
            "key" => {
                let combo = need(name)?;
                let mut mods = crate::scene::Mods::default();
                let mut key = combo.as_str();
                while let Some((m, k)) = key.split_once('+').filter(|(_, k)| !k.is_empty()) {
                    match m.to_ascii_lowercase().as_str() {
                        "ctrl" | "control" => mods.ctrl = true,
                        "alt" => mods.alt = true,
                        "shift" => mods.shift = true,
                        "super" | "logo" => mods.logo = true,
                        _ => return Err(format!("`{m}` is not a modifier: ctrl, alt, shift, super")),
                    }
                    key = k;
                }
                let (name, typed) = key_name(key);
                Ok(Act::Key { name, mods, typed })
            }
            _ => unreachable!(),
        })())
    }

    pub fn name(&self) -> &str {
        match self {
            Act::Press { name, .. } | Act::Hold { name } | Act::Drag { name, .. } | Act::Wheel { name, .. } | Act::Type { name, .. } | Act::Key { name, .. } => name,
        }
    }
}

/// A key as an agent writes it (`enter`, `escape`, `a`) and as the keyboard
/// names it (`Return`, `Escape`, `a`), with the letter it writes, if any.
fn key_name(k: &str) -> (String, Option<String>) {
    let named = match k.to_ascii_lowercase().as_str() {
        "enter" | "return" => "Return",
        "escape" | "esc" => "Escape",
        "tab" => "Tab",
        "backspace" => "BackSpace",
        "delete" | "del" => "Delete",
        "space" => return ("space".into(), Some(" ".into())),
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Prior",
        "pagedown" => "Next",
        f if f.len() <= 3 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => return (f.to_ascii_uppercase(), None),
        _ if k.chars().count() == 1 => return (k.to_owned(), Some(k.to_owned())),
        _ => return (k.to_owned(), None),
    };
    (named.to_owned(), None)
}

/// The zone a name means: as the scene knows it, or without the `#screen0`
/// that `describe` leaves out, in the copy that is on screen.
pub fn locate(scene: &Scene, c: Ctx, name: &str, sight: &Sight) -> Option<usize> {
    let hidden = |k: &usize| scene.zones[*k].reach == Reach::Hidden;
    // As written, and the copy of each monitor's surface that is on screen.
    let mut candidates: Vec<usize> = scene.zones.iter().position(|z| z.id == name).into_iter().collect();
    for s in sight.shown.iter().filter_map(|s| scene.surfaces.get(s.surface)) {
        let full = format!("{name}#screen{}", s.instance);
        if let Some(k) = scene.zones.iter().position(|z| z.id == full) {
            candidates.push(k);
        }
    }
    candidates.retain(|k| !hidden(k));
    // Of the copies, the one that is where a surface is shown: a scene copied per
    // monitor has the same button on each, and only one of them may be seen.
    let seen = |k: &usize| {
        let z = &scene.zones[*k];
        !(sight.gone)(z.at) && z.bounds(c).is_some_and(|b| sight.shown.iter().any(|s| inside(s.bounds, (b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5)))
    };
    candidates.iter().find(|k| seen(k)).or(candidates.first()).copied()
}

/// Where a hand would press zone `k` now, or why it cannot: the point of
/// it that is on top, its centre if that is. `Ok(None)`: it is scrolled out
/// of its list, which has to bring it into sight first.
pub fn reach_point(scene: &Scene, c: Ctx, k: usize, sight: &Sight) -> Result<Option<(f32, f32)>, String> {
    let z = &scene.zones[k];
    let said = z.id.split("#screen").next().unwrap_or(z.id);
    if z.reach == Reach::Person && !sight.person {
        return Err(format!("? {said} is for a person's hand (agent: no): ask them to"));
    }
    let Some(b) = z.bounds(c).filter(|_| !(sight.gone)(z.at)) else {
        return Err(format!("? {said} is not there now"));
    };
    let centre = ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5);
    let on_screen = |x: f32, y: f32| sight.shown.iter().any(|s| inside(s.bounds, x, y) && scene.surfaces.get(s.surface).is_some_and(|f| !f.lock_screen && !f.agent_hidden));
    if let Some(l) = z.within {
        let lz = &scene.zones[l.0 as usize];
        if let Some(w) = lz.bounds(c)
            && !inside(w, centre.0, centre.1)
            && on_screen((w[0] + w[2]) * 0.5, (w[1] + w[3]) * 0.5)
        {
            return Ok(None);
        }
    }
    if !on_screen(centre.0, centre.1) {
        return Err(format!("? {said} is not on screen"));
    }
    if !z.active.is_true(c) {
        return Err(format!("? {said} is inactive"));
    }
    let rank = |k: usize| sight.rank.map_or((k, 0, 0), |r| r[k]);
    let top = |x: f32, y: f32| {
        scene
            .zones
            .iter()
            .enumerate()
            .filter(|(_, o)| !(sight.gone)(o.at) && o.active.is_true(c) && o.contains(c, x, y))
            .max_by_key(|(j, _)| (rank(*j), *j))
            .map(|(j, _)| j)
    };
    // The centre if it is its own; if not —a ring, a corner under a badge—
    // any point of it that is.
    let mut candidates = vec![centre];
    for i in 1..6 {
        for j in 1..6 {
            candidates.push((b[0] + (b[2] - b[0]) * i as f32 / 6.0, b[1] + (b[3] - b[1]) * j as f32 / 6.0));
        }
    }
    // A list's zone is under its rows, and its wheel and its drag reach it all the same.
    if let Some(p) = candidates.iter().find(|(x, y)| z.contains(c, *x, *y) && (z.scrolls.is_some() || top(*x, *y) == Some(k))) {
        return Ok(Some(*p));
    }
    let over = top(centre.0, centre.1).filter(|t| *t != k).map_or("something", |t| scene.zones[t].id);
    Err(format!("? {said} is covered by {}", over.split("#screen").next().unwrap_or(over)))
}

/// How far its list has to scroll for zone `k` to be in sight, and along
/// which property: positive brings what is below up.
pub fn scroll_into_view(scene: &Scene, c: Ctx, k: usize) -> Option<(crate::scene::PropId, f32)> {
    let z = &scene.zones[k];
    let l = &scene.zones[z.within?.0 as usize];
    let (b, w) = (z.bounds(c)?, l.bounds(c)?);
    let along_x = (b[0] + b[2]) * 0.5 < w[0] || (b[0] + b[2]) * 0.5 > w[2];
    let (lo, hi, wlo, whi) = if along_x { (b[0], b[2], w[0], w[2]) } else { (b[1], b[3], w[1], w[3]) };
    let by = if hi > whi { hi - whi + 4.0 } else if lo < wlo { lo - wlo - 4.0 } else { 0.0 };
    Some((l.scrolls?, by))
}

/// What the scene is at a moment, to say afterwards what an action changed.
pub struct Before {
    facts: Vec<f32>,
    texts: Vec<String>,
    open: Vec<String>,
    /// Where each list was scrolled to.
    scrolled: Vec<f32>,
}

fn scrolled(scene: &Scene, c: Ctx) -> Vec<f32> {
    scene.zones.iter().map(|z| z.scrolls.map_or(0.0, |p| c.props[p.0 as usize].x)).collect()
}

fn quiet_fact(n: &str) -> bool {
    n.contains('·')
        || ["pointer.", "local.", "drag.", "screen.", "cursor.", "lock.", "wheel", "pinch.", "key.", "time"].iter().any(|p| n.starts_with(p))
        || n.ends_with(".grab")
        || n.ends_with(".hover")
        || n.ends_with(".pressed")
}

pub fn before(scene: &Scene, c: Ctx, texts: &[String], open: Vec<String>) -> Before {
    Before { facts: c.facts.to_vec(), texts: texts.to_vec(), open, scrolled: scrolled(scene, c) }
}

/// What changed since `b`: the events heard, the facts and texts that are
/// worth something else, the surfaces that opened or closed.
/// `heard`: what happened that is no state —(`event`, its name), (`press`, a zone)—.
pub fn what_happened(scene: &Scene, b: &Before, c: Ctx, texts: &[String], open: &[String], heard: &[(&str, &str)]) -> String {
    let facts = c.facts;
    let mut out = String::new();
    for (kind, name) in heard {
        let _ = writeln!(out, "  {kind:<6} {}", name.split("#screen").next().unwrap_or(name));
    }
    let shown = |n: &str, v: f32| match scene.types.iter().find(|(t, _)| t == n) {
        Some((_, t)) => t.to_text(v),
        None if v.fract() == 0.0 => format!("{}", v as i64),
        None => format!("{v:.3}"),
    };
    for (k, (n, _)) in scene.facts.iter().enumerate() {
        let (was, is) = (b.facts.get(k).copied().unwrap_or(0.0), facts.get(k).copied().unwrap_or(0.0));
        // A change too small to be told apart as it is written (0.181 → 0.181) is not one.
        if was != is && !quiet_fact(n) && shown(n, was) != shown(n, is) {
            let _ = writeln!(out, "  fact   {n}: {} → {}", shown(n, was), shown(n, is));
        }
    }
    let secrets = crate::scene::SECRETS.lock().map(|s| s.clone()).unwrap_or_default();
    let kept: Vec<&str> = scene.zones.iter().filter(|z| z.reach != Reach::Any).map(|z| z.id).collect();
    for (k, (n, _)) in scene.texts.iter().enumerate() {
        let (was, is) = (b.texts.get(k).map_or("", String::as_str), texts.get(k).map_or("", String::as_str));
        if was != is {
            if secrets.contains(n) || kept.contains(n) {
                let _ = writeln!(out, "  text   {n}: changed (hidden)");
            } else {
                let _ = writeln!(out, "  text   {n}: \"{was}\" → \"{is}\"");
            }
        }
    }
    for (k, now) in scrolled(scene, c).into_iter().enumerate() {
        let was = b.scrolled.get(k).copied().unwrap_or(0.0);
        if (now - was).abs() >= 1.0 {
            let n = scene.zones[k].id;
            let _ = writeln!(out, "  scroll {}: {} → {}", n.split("#screen").next().unwrap_or(n), was.round(), now.round());
        }
    }
    for s in open.iter().filter(|s| !b.open.contains(s)) {
        let _ = writeln!(out, "  opened {s}");
    }
    for s in b.open.iter().filter(|s| !open.contains(s)) {
        let _ = writeln!(out, "  closed {s}");
    }
    if out.is_empty() {
        out.push_str("  nothing changed\n");
    }
    out
}

// ── following ───────────────────────────────────────────────────

/// How many lines may wait for a reader that is slow: past that, what changes
/// waits in the scene and is said later, as it is then (see `Follow::tell`).
pub const FOLLOW_ROOM: usize = 1024;

/// Someone following some facts and texts by how their names start —`watch 0
/// lay. free.`, a shell following its window manager—: a line `fact NAME VALUE`
/// or `text NAME VALUE` each time one is worth something else, for a program
/// to read, not a person. Only the names asked for are looked at, and only
/// while someone reads.
pub struct Follow {
    starts: Vec<String>,
    until: Option<std::time::Instant>,
    lines: std::sync::mpsc::SyncSender<String>,
    /// Held as well by whoever passes the lines on: when this is the only
    /// one left, nobody reads any more.
    reader: std::sync::Arc<()>,
    /// What they are worth is said first (`--now`), or only what changes.
    now: bool,
    looked: bool,
    /// Followed: where it is in the scene, its name, and what was last said of it.
    facts: Vec<(usize, &'static str, Option<f32>)>,
    texts: Vec<(usize, &'static str, Option<String>)>,
}

/// What `watch` was asked, if it was asked to follow names: for how long
/// (`None`, until the reader goes), whether to say first what they are worth,
/// and how the names start. `None`: the `watch` for a person, as ever.
pub fn follow_asked(after: &str) -> Option<Result<(Option<std::time::Duration>, bool, Vec<String>), String>> {
    let mut words = after.split_whitespace();
    let first = words.next().unwrap_or("");
    let rest: Vec<&str> = words.collect();
    let secs = first.trim_end_matches('s').parse::<f32>().ok().filter(|s| s.is_finite() && *s >= 0.0);
    if rest.is_empty() && secs != Some(0.0) {
        return None;
    }
    let Some(secs) = secs else { return Some(Err(format!("'{first}' is not a number of seconds: watch 0 lay. free."))) };
    let now = rest.contains(&"--now");
    let starts: Vec<String> = rest.iter().filter(|w| **w != "--now").map(|w| (*w).to_owned()).collect();
    if starts.is_empty() {
        return Some(Err("say how the names start: watch 0 lay. free.".into()));
    }
    Some(Ok(((secs > 0.0).then(|| std::time::Duration::from_secs_f32(secs)), now, starts)))
}

impl Follow {
    pub fn new(starts: Vec<String>, until: Option<std::time::Instant>, lines: std::sync::mpsc::SyncSender<String>, reader: std::sync::Arc<()>, now: bool) -> Follow {
        Follow { starts, until, lines, reader, now, looked: false, facts: Vec::new(), texts: Vec::new() }
    }

    pub fn until(&self) -> Option<std::time::Instant> {
        self.until
    }

    /// What they are worth when it is asked, in the order things arrive: a
    /// change that comes right after it, in the same round, is a change.
    pub fn start(&mut self, scene: &Scene, facts: &[f32], texts: &[String]) {
        if !self.looked {
            self.find(scene, facts, texts);
        }
    }

    /// The scene was loaded again: its names are looked for again, and what
    /// was already said of those still there is not said twice.
    pub fn lost(&mut self) {
        self.looked = false;
    }

    fn find(&mut self, scene: &Scene, facts: &[f32], texts: &[String]) {
        let first = self.facts.is_empty() && self.texts.is_empty();
        let wanted = |n: &str| self.starts.iter().any(|s| n.starts_with(s.as_str()));
        let found_facts: Vec<_> = scene
            .facts
            .iter()
            .enumerate()
            .filter(|(_, (n, _))| wanted(n))
            .map(|(k, (n, _))| {
                let told = match self.facts.iter().find(|f| f.1 == *n) {
                    Some(f) => f.2,
                    // Only what changes from now on, unless what it is worth was asked for.
                    None if first && !self.now => facts.get(k).copied(),
                    None => None,
                };
                (k, *n, told)
            })
            .collect();
        let found_texts: Vec<_> = scene
            .texts
            .iter()
            .enumerate()
            .filter(|(_, (n, _))| wanted(n))
            .map(|(k, (n, _))| {
                let told = match self.texts.iter().find(|t| t.1 == *n) {
                    Some(t) => t.2.clone(),
                    None if first && !self.now => texts.get(k).cloned(),
                    None => None,
                };
                (k, *n, told)
            })
            .collect();
        // A start that nothing has is said once, as a note: it may be a typo.
        if first {
            for s in &self.starts {
                if !found_facts.iter().any(|f| f.1.starts_with(s.as_str())) && !found_texts.iter().any(|t| t.1.starts_with(s.as_str())) {
                    let _ = self.lines.try_send(format!("? no fact or text starts with '{s}'"));
                }
            }
        }
        (self.facts, self.texts, self.looked) = (found_facts, found_texts, true);
    }

    /// Says what changed since the last time. `false` when it is over: nobody
    /// reads any more, or its time is up. A reader that does not keep up
    /// fills its room; then the rest waits here and is said when there is
    /// room, as it is by then: what it was worth in between is lost, never
    /// where it ended.
    pub fn tell(&mut self, scene: &Scene, facts: &[f32], texts: &[String], now: std::time::Instant) -> bool {
        use std::sync::mpsc::TrySendError;
        if std::sync::Arc::strong_count(&self.reader) == 1 || self.until.is_some_and(|u| now >= u) {
            return false;
        }
        if !self.looked {
            self.find(scene, facts, texts);
        }
        for f in &mut self.facts {
            let is = facts.get(f.0).copied().unwrap_or(0.0);
            if f.2.is_some_and(|was| was.to_bits() == is.to_bits()) {
                continue;
            }
            // As `--say … fact` takes it: `3`, `true`, an enum's name.
            let shown = match scene.types.iter().find(|(t, _)| t == f.1) {
                Some((_, t)) => t.to_text(is),
                None => is.to_string(),
            };
            match self.lines.try_send(format!("fact {} {shown}", f.1)) {
                Ok(()) => f.2 = Some(is),
                Err(TrySendError::Full(_)) => return true,
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        if self.texts.iter().any(|t| t.2.as_deref() != Some(texts.get(t.0).map_or("", String::as_str))) {
            let secrets = crate::scene::SECRETS.lock().map(|s| s.clone()).unwrap_or_default();
            for t in &mut self.texts {
                let is = texts.get(t.0).map_or("", String::as_str);
                if t.2.as_deref() == Some(is) {
                    continue;
                }
                // What is not told to an agent is not told here either: only that it changed.
                let kept = secrets.contains(&t.1) || scene.zones.iter().any(|z| z.id == t.1 && z.reach != Reach::Any);
                let line = if kept { format!("hidden {}", t.1) } else { format!("text {} {}", t.1, one_line(is)) };
                match self.lines.try_send(line) {
                    Ok(()) => t.2 = Some(is.to_owned()),
                    Err(TrySendError::Full(_)) => return true,
                    Err(TrySendError::Disconnected(_)) => return false,
                }
            }
        }
        true
    }
}

/// A text on one line: `\` as `\\`, a line break as `\n`, a carriage return as `\r`.
fn one_line(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains(['\\', '\n', '\r']) {
        return s.into();
    }
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").into()
}

// ── waiting ─────────────────────────────────────────────────────

/// What `wait` waits for: `saving == false and rows.count > 0`,
/// `status has "saved"`, `mode == critical`. Facts, texts and properties by
/// their names, numbers, quoted texts, `true`, `false` and an enum's values.
#[derive(Clone, Debug)]
pub enum Cond {
    Or(Box<Cond>, Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Not(Box<Cond>),
    Cmp(Operand, Op, Operand),
    Truth(Operand),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Num(f32),
    Str(String),
    /// A bare word that is no name: an enum's value, `critical`.
    Word(String),
    Fact(usize),
    Text(usize),
    Prop(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    /// A text holds another: `status has "saved"`.
    Has,
}

enum Value {
    N(f32),
    S(String),
}

impl Cond {
    /// `EXPR [TIMEOUT]`: the condition and how long to wait for it (5 s if unsaid).
    pub fn parse(scene: &Scene, src: &str) -> Result<(Cond, std::time::Duration), String> {
        let mut tokens = tokenize(src)?;
        let mut timeout = std::time::Duration::from_secs(5);
        if let Some(last) = tokens.last() {
            let t = last.as_str();
            let secs = t.strip_suffix("ms").and_then(|n| n.parse::<f32>().ok()).map(|n| n / 1000.0).or_else(|| t.strip_suffix('s').and_then(|n| n.parse::<f32>().ok()));
            if let Some(s) = secs.filter(|s| *s > 0.0 && *s <= 60.0) {
                timeout = std::time::Duration::from_secs_f32(s);
                tokens.pop();
            }
        }
        if tokens.is_empty() {
            return Err("`wait` needs what to wait for: `wait saving == false`, `wait status has \"saved\" 3s`".into());
        }
        let mut p = Parser { scene, t: &tokens, i: 0 };
        let c = p.or()?;
        if p.i < tokens.len() {
            return Err(format!("`{}` is left over: join conditions with `and` or `or`", tokens[p.i]));
        }
        Ok((c, timeout))
    }

    pub fn holds(&self, scene: &Scene, c: Ctx, texts: &[String]) -> bool {
        match self {
            Cond::Or(a, b) => a.holds(scene, c, texts) || b.holds(scene, c, texts),
            Cond::And(a, b) => a.holds(scene, c, texts) && b.holds(scene, c, texts),
            Cond::Not(a) => !a.holds(scene, c, texts),
            Cond::Truth(o) => match value(o, scene, c, texts) {
                Value::N(n) => n != 0.0,
                Value::S(s) => !s.is_empty(),
            },
            Cond::Cmp(a, op, b) => {
                // A fact with names (`mode`, a yes or no) against a word: by its number.
                let as_fact = |x: &Operand, y: &Operand| match (x, y) {
                    (Operand::Fact(k), Operand::Word(w) | Operand::Str(w)) => scene.types.iter().find(|(n, _)| n == scene.facts[*k].0).and_then(|(_, t)| t.from_text(w)),
                    _ => None,
                };
                let (va, vb) = match (as_fact(a, b), as_fact(b, a)) {
                    (Some(n), _) => (value(a, scene, c, texts), Value::N(n)),
                    (_, Some(n)) => (Value::N(n), value(b, scene, c, texts)),
                    _ => (value(a, scene, c, texts), value(b, scene, c, texts)),
                };
                match (va, vb) {
                    (Value::N(x), Value::N(y)) => match op {
                        Op::Eq => (x - y).abs() < 1e-4,
                        Op::Ne => (x - y).abs() >= 1e-4,
                        Op::Gt => x > y,
                        Op::Lt => x < y,
                        Op::Ge => x >= y,
                        Op::Le => x <= y,
                        Op::Has => false,
                    },
                    (x, y) => {
                        let text = |v: Value| match v {
                            Value::N(n) if n.fract() == 0.0 => format!("{}", n as i64),
                            Value::N(n) => n.to_string(),
                            Value::S(s) => s,
                        };
                        let (x, y) = (text(x), text(y));
                        match op {
                            Op::Eq => x == y,
                            Op::Ne => x != y,
                            Op::Has => x.contains(&y),
                            Op::Gt => x > y,
                            Op::Lt => x < y,
                            Op::Ge => x >= y,
                            Op::Le => x <= y,
                        }
                    }
                }
            }
        }
    }

    /// What the names it reads are worth now: what is said when it did not come.
    pub fn now(&self, scene: &Scene, c: Ctx, texts: &[String]) -> String {
        let mut seen: Vec<String> = Vec::new();
        self.names(&mut |o| {
            let s = match o {
                Operand::Fact(k) => {
                    let (n, v) = (scene.facts[*k].0, c.facts[*k]);
                    let v = scene.types.iter().find(|(t, _)| t == n).map_or_else(|| number(v), |(_, t)| t.to_text(v));
                    format!("{n} is {v}")
                }
                Operand::Text(k) => format!("{} is \"{}\"", scene.texts[*k].0, texts.get(*k).map_or("", String::as_str)),
                Operand::Prop(k) => format!("{} is {}", scene.props[*k].0, number(c.props[*k].x)),
                _ => return,
            };
            if !seen.contains(&s) {
                seen.push(s);
            }
        });
        seen.join(", ")
    }

    fn names(&self, f: &mut dyn FnMut(&Operand)) {
        match self {
            Cond::Or(a, b) | Cond::And(a, b) => {
                a.names(f);
                b.names(f);
            }
            Cond::Not(a) => a.names(f),
            Cond::Truth(o) => f(o),
            Cond::Cmp(a, _, b) => {
                f(a);
                f(b);
            }
        }
    }
}

fn number(v: f32) -> String {
    if v.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v:.3}") }
}

fn value(o: &Operand, scene: &Scene, c: Ctx, texts: &[String]) -> Value {
    let _ = scene;
    match o {
        Operand::Num(n) => Value::N(*n),
        Operand::Str(s) | Operand::Word(s) => Value::S(s.clone()),
        Operand::Fact(k) => Value::N(c.facts[*k]),
        Operand::Text(k) => Value::S(texts.get(*k).cloned().unwrap_or_default()),
        Operand::Prop(k) => Value::N(c.props[*k].x),
    }
}

/// Words, numbers, quoted texts and the comparisons.
fn tokenize(src: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
        } else if ch == '"' {
            chars.next();
            let mut s = String::from("\"");
            loop {
                match chars.next() {
                    Some('"') => break,
                    // `\"` and `\\`, as in a scene's texts.
                    Some('\\') if matches!(chars.peek(), Some('"') | Some('\\')) => s.push(chars.next().unwrap()),
                    Some(c) => s.push(c),
                    None => return Err("a quoted text is not closed".into()),
                }
            }
            out.push(s);
        } else if "=!<>".contains(ch) {
            let mut s = String::from(ch);
            chars.next();
            if chars.peek() == Some(&'=') {
                s.push('=');
                chars.next();
            }
            if s == "=" || s == "!" {
                return Err(format!("`{s}` is not a comparison: ==, !=, >, <, >=, <=, has"));
            }
            out.push(s);
        } else if ch == '(' || ch == ')' {
            out.push(ch.to_string());
            chars.next();
        } else {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || "=!<>\"()".contains(c) {
                    break;
                }
                s.push(c);
                chars.next();
            }
            out.push(s);
        }
    }
    Ok(out)
}

struct Parser<'a> {
    scene: &'a Scene,
    t: &'a [String],
    i: usize,
}

impl Parser<'_> {
    fn word(&mut self, w: &str) -> bool {
        let yes = self.t.get(self.i).is_some_and(|t| t == w);
        if yes {
            self.i += 1;
        }
        yes
    }
    fn or(&mut self) -> Result<Cond, String> {
        let mut a = self.and()?;
        while self.word("or") {
            a = Cond::Or(Box::new(a), Box::new(self.and()?));
        }
        Ok(a)
    }
    fn and(&mut self) -> Result<Cond, String> {
        let mut a = self.not()?;
        while self.word("and") {
            a = Cond::And(Box::new(a), Box::new(self.not()?));
        }
        Ok(a)
    }
    fn not(&mut self) -> Result<Cond, String> {
        if self.word("not") {
            return Ok(Cond::Not(Box::new(self.not()?)));
        }
        if self.word("(") {
            let c = self.or()?;
            if !self.word(")") {
                return Err("a `(` is not closed".into());
            }
            return Ok(c);
        }
        let a = self.operand(false)?;
        let op = match self.t.get(self.i).map(String::as_str) {
            Some("==") => Op::Eq,
            Some("!=") => Op::Ne,
            Some(">") => Op::Gt,
            Some("<") => Op::Lt,
            Some(">=") => Op::Ge,
            Some("<=") => Op::Le,
            Some("has") => Op::Has,
            _ => return Ok(Cond::Truth(a)),
        };
        self.i += 1;
        let b = self.operand(true)?;
        Ok(Cond::Cmp(a, op, b))
    }
    /// A name of the scene, a number, a quoted text; after a comparison also a
    /// bare word, which an enum's value is.
    fn operand(&mut self, after_comparison: bool) -> Result<Operand, String> {
        let Some(t) = self.t.get(self.i).cloned() else {
            return Err("something to compare is missing at the end".into());
        };
        self.i += 1;
        if let Some(s) = t.strip_prefix('"') {
            return Ok(Operand::Str(s.to_owned()));
        }
        if let Ok(n) = t.parse::<f32>() {
            return Ok(Operand::Num(n));
        }
        match t.as_str() {
            "true" => return Ok(Operand::Num(1.0)),
            "false" => return Ok(Operand::Num(0.0)),
            _ => {}
        }
        let s = self.scene;
        if let Some(k) = s.facts.iter().position(|f| f.0 == t) {
            return Ok(Operand::Fact(k));
        }
        if let Some(k) = s.texts.iter().position(|f| f.0 == t) {
            return Ok(Operand::Text(k));
        }
        if let Some(k) = s.props.iter().position(|f| f.0 == t) {
            return Ok(Operand::Prop(k));
        }
        let enum_value = s.types.iter().any(|(_, ty)| matches!(ty, crate::scene::FactType::Enum(v) if v.contains(&t)));
        if after_comparison && enum_value {
            return Ok(Operand::Word(t));
        }
        let known: Vec<String> = s.facts.iter().map(|f| f.0).chain(s.texts.iter().map(|f| f.0)).chain(s.props.iter().map(|f| f.0)).filter(|n| !n.contains('·')).map(str::to_owned).collect();
        let hint = crate::language::closest_match(&t, known.iter()).map_or(String::new(), |m| format!(": did you mean '{m}'?"));
        Err(format!("there is no fact, text or property called '{t}'{hint}"))
    }
}

fn round(v: f32) -> i32 {
    v.round() as i32
}

/// As text: one line per thing, indented under the list that holds it.
pub fn to_text(parts: &[Part]) -> String {
    let mut out = String::new();
    if parts.is_empty() {
        out.push_str("nothing on screen\n");
    }
    for p in parts {
        let _ = writeln!(out, "{} · {} {}×{} · scale {}", p.name, p.kind, round(p.size.0), round(p.size.1), p.scale);
        let width = p.nodes.iter().map(|n| n.name.chars().count()).max().unwrap_or(0);
        let mut write = |n: &Node, depth: usize| {
            let mut line = format!("{}{:<7} {:<width$}", "  ".repeat(depth + 1), n.role.word(), n.name);
            if !n.label.is_empty() {
                let _ = write!(line, "  «{}»", n.label);
            }
            if let Some(v) = &n.value {
                let _ = write!(line, "  \"{v}\"");
            }
            match n.checked {
                Some(true) => line.push_str("  · checked"),
                Some(false) => line.push_str("  · not checked"),
                None => {}
            }
            if n.selected == Some(true) {
                line.push_str("  · selected");
            }
            if n.inactive {
                line.push_str("  · inactive");
            }
            if let Some(t) = n.covered_by {
                let _ = write!(line, "  · covered by {t}");
            }
            if n.off_view {
                line.push_str("  · off view");
            }
            if n.persons {
                line.push_str("  · a person's");
            }
            let _ = writeln!(out, "{line}  at {},{} {}×{}", round(n.at[0]), round(n.at[1]), round(n.at[2]), round(n.at[3]));
        };
        // Depth first, in the order they were written.
        fn walk(nodes: &[Node], parent: Option<usize>, depth: usize, write: &mut dyn FnMut(&Node, usize)) {
            for (i, n) in nodes.iter().enumerate() {
                if n.inside == parent {
                    write(n, depth);
                    walk(nodes, Some(i), depth + 1, write);
                }
            }
        }
        walk(&p.nodes, None, 0, &mut write);
    }
    out
}

/// As JSON, for a program.
pub fn to_json(parts: &[Part]) -> String {
    fn node(nodes: &[Node], i: usize) -> serde_json::Value {
        let n = &nodes[i];
        let mut v = serde_json::json!({
            "role": n.role.word(),
            "label": n.label,
            "box": [round(n.at[0]), round(n.at[1]), round(n.at[2]), round(n.at[3])],
        });
        // A loose text is only read: it has no name to act on.
        if n.role != Role::Text {
            v["name"] = n.name.into();
            v["active"] = (!n.inactive).into();
            v["agent"] = (if n.persons { "no" } else { "yes" }).into();
        }
        if let Some(t) = &n.value {
            v["value"] = t.clone().into();
        }
        if let Some(t) = n.covered_by {
            v["covered_by"] = t.into();
        }
        if n.off_view {
            v["off_view"] = true.into();
        }
        if let Some(b) = n.checked {
            v["checked"] = b.into();
        }
        if let Some(b) = n.selected {
            v["selected"] = b.into();
        }
        let inner: Vec<serde_json::Value> = (0..nodes.len()).filter(|j| nodes[*j].inside == Some(i)).map(|j| node(nodes, j)).collect();
        if !inner.is_empty() {
            v["children"] = inner.into();
        }
        v
    }
    let all: Vec<serde_json::Value> = parts
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "kind": p.kind,
                "size": [round(p.size.0), round(p.size.1)],
                "scale": p.scale,
                "nodes": (0..p.nodes.len()).filter(|i| p.nodes[*i].inside.is_none()).map(|i| node(&p.nodes, i)).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_string(&all).unwrap_or_default() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Animated;

    /// `tests/agent.plm` as it starts, with every surface on screen.
    fn told(dirty: bool) -> Vec<Part> {
        told_with(dirty, &[])
    }

    fn told_with(dirty: bool, seen: &[SeenText]) -> Vec<Part> {
        let (scene, _) = crate::language::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/agent.plm")).unwrap();
        let props: Vec<Animated> = scene.props.iter().map(|(_, v, s)| Animated { x: *v, v: 0.0, target: *v, spring: *s }).collect();
        let mut facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        if dirty {
            facts[scene.facts.iter().position(|f| f.0 == "dirty").unwrap()] = 1.0;
        }
        let texts: Vec<String> = scene.texts.iter().map(|t| t.1.clone()).collect();
        let shown = scene
            .surfaces
            .iter()
            .enumerate()
            .map(|(k, s)| Shown { surface: k, popup: None, bounds: [s.origin.0, s.origin.1, s.origin.0 + s.width as f32, s.origin.1 + s.height as f32], scale: 1.0 })
            .collect();
        let never = |_: usize| false;
        let sight = Sight { shown, texts_seen: seen, gone: &never, rank: None, zones: None, index: None, person: false };
        describe(&scene, Ctx { props: &props, facts: &facts, seen: None }, &texts, &sight)
    }

    /// The render's zones and a kept index say the same as working it all out here,
    /// dirty or not (a zone that turns active or inactive).
    #[test]
    fn what_the_render_knows_says_the_same() {
        let (scene, _) = crate::language::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/agent.plm")).unwrap();
        let props: Vec<Animated> = scene.props.iter().map(|(_, v, s)| Animated { x: *v, v: 0.0, target: *v, spring: *s }).collect();
        let texts: Vec<String> = scene.texts.iter().map(|t| t.1.clone()).collect();
        let index = Index::new(&scene);
        for dirty in [false, true] {
            let mut facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
            if dirty {
                facts[scene.facts.iter().position(|f| f.0 == "dirty").unwrap()] = 1.0;
            }
            let c = Ctx { props: &props, facts: &facts, seen: None };
            let live: Vec<bool> = scene.zones.iter().map(|z| z.active.is_true(c)).collect();
            let boxes: Vec<Option<[f32; 4]>> = scene.zones.iter().zip(&live).map(|(z, l)| if *l { z.bounds(c) } else { None }).collect();
            let never = |_: usize| false;
            let shown = || scene.surfaces.iter().enumerate().map(|(k, s)| Shown { surface: k, popup: None, bounds: [s.origin.0, s.origin.1, s.origin.0 + s.width as f32, s.origin.1 + s.height as f32], scale: 1.0 }).collect();
            let alone = describe(&scene, c, &texts, &Sight { shown: shown(), texts_seen: &[], gone: &never, rank: None, zones: None, index: None, person: false });
            let given = describe(&scene, c, &texts, &Sight { shown: shown(), texts_seen: &[], gone: &never, rank: None, zones: Some((&live, &boxes)), index: Some(&index), person: false });
            assert_eq!(to_text(&alone), to_text(&given));
            assert_eq!(to_json(&alone), to_json(&given));
        }
    }

    fn node<'p>(parts: &'p [Part], name: &str) -> Option<&'p Node> {
        parts.iter().flat_map(|p| &p.nodes).find(|n| n.name == name)
    }

    #[test]
    fn a_scene_says_what_it_holds() {
        let parts = told(false);
        // The vault is `agent: hidden`: only the window is told.
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, "window «Told»");
        let query = node(&parts, "query").unwrap();
        assert_eq!((query.role, query.label.as_str(), query.value.as_deref()), (Role::Field, "Search notes", Some("")));
        // A secret field, and one kept for a person: its value is not given.
        let pin = node(&parts, "pin").unwrap();
        assert_eq!((pin.value.as_deref(), pin.persons), (Some("(hidden)"), true));
        // Dragged: sliders, each copy with its own word.
        let knobs: Vec<(Role, &str)> = (0..3).map(|k| node(&parts, &format!("knob.{k}")).map(|n| (n.role, n.label.as_str())).unwrap()).collect();
        assert_eq!(knobs, [(Role::Slider, "Brightness"), (Role::Slider, "Volume"), (Role::Slider, "Microphone")]);
        // Said by the scene: a toggle that is on, a tab that is chosen, and what each is worth.
        let switch = node(&parts, "switch").unwrap();
        assert_eq!((switch.role, switch.checked, switch.value.as_deref()), (Role::Toggle, Some(true), Some("loud")));
        let tab = node(&parts, "tab").unwrap();
        assert_eq!((tab.role, tab.selected, tab.value.as_deref()), (Role::Tab, Some(true), Some("3")));
        assert_eq!(node(&parts, "knob.0").unwrap().value.as_deref(), Some("30%"));
        assert!(to_text(&parts).contains("toggle  switch  «Wi-Fi»  \"loud\"  · checked"), "{}", to_text(&parts));
        let delete = node(&parts, "delete").unwrap();
        assert_eq!((delete.role, delete.persons), (Role::Button, true));
        // Hidden is not even named; inactive with nothing drawn is not there.
        assert!(node(&parts, "private").is_none());
        assert!(node(&parts, "save").is_none());
        // And what can be pressed with nothing saying what it is gets pointed at.
        assert_eq!(unnamed(&parts), ["close"]);
        let text = to_text(&parts);
        assert!(text.contains("«Delete»  · a person's"), "{text}");
    }

    #[test]
    fn a_label_follows_what_it_names() {
        let parts = told(true);
        let save = node(&parts, "save").unwrap();
        assert_eq!((save.label.as_str(), save.inactive), ("Save Ready", false));
        let json: serde_json::Value = serde_json::from_str(&to_json(&parts)).unwrap();
        assert!(json[0]["nodes"].as_array().unwrap().iter().any(|n| n["name"] == "save" && n["label"] == "Save Ready"));
    }

    #[test]
    fn an_action_is_read_as_it_is_written() {
        let p = |w: &str, r: &str| Act::parse(w, r).unwrap();
        assert!(matches!(p("press", "save"), Ok(Act::Press { button: 0, count: 1, .. })));
        assert!(matches!(p("press", "row.3 right 2"), Ok(Act::Press { button: 1, count: 2, .. })));
        assert!(matches!(p("drag", "knob.1 0 -40"), Ok(Act::Drag { by: (0.0, -40.0), .. })));
        assert!(matches!(p("type", "query  two words"), Ok(Act::Type { ref text, .. }) if text == "two words"));
        assert!(matches!(p("key", "ctrl+z"), Ok(Act::Key { ref name, mods, .. }) if name == "z" && mods.ctrl));
        assert!(matches!(p("key", "enter"), Ok(Act::Key { ref name, .. }) if name == "Return"));
        assert!(p("press", "").is_err());
        assert!(p("wheel", "list up").is_err());
        assert!(Act::parse("emit", "x").is_none());
    }

    #[test]
    fn a_hand_is_refused_what_a_person_could_not_do_either() {
        let (scene, _) = crate::language::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/agent.plm")).unwrap();
        let props: Vec<Animated> = scene.props.iter().map(|(_, v, s)| Animated { x: *v, v: 0.0, target: *v, spring: *s }).collect();
        let facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        let c = Ctx { props: &props, facts: &facts, seen: None };
        let shown = vec![Shown { surface: 0, popup: None, bounds: [0.0, 0.0, 400.0, 300.0], scale: 1.0 }];
        let never = |_: usize| false;
        let sight = Sight { shown, texts_seen: &[], gone: &never, rank: None, zones: None, index: None, person: false };
        let at = |n: &str| reach_point(&scene, c, locate(&scene, c, n, &sight).unwrap(), &sight);
        assert_eq!(at("knob.1"), Ok(Some((100.0, 100.0))));
        assert!(at("delete").unwrap_err().contains("a person's hand"));
        assert!(at("save").unwrap_err().contains("inactive"));
        // Hidden is not even there to be found.
        assert_eq!(locate(&scene, c, "private", &sight), None);
        // A field shown, with a hidden one declared over it: the hidden one covers nothing.
        assert!(at("name").is_ok_and(|p| p.is_some()), "{:?}", at("name"));
        assert!(at("other").is_err());
    }

    #[test]
    fn a_wait_reads_the_scene_as_it_is() {
        let (scene, _) = crate::language::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/agent.plm")).unwrap();
        let props: Vec<Animated> = scene.props.iter().map(|(_, v, s)| Animated { x: *v, v: 0.0, target: *v, spring: *s }).collect();
        let mut facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        let texts: Vec<String> = scene.texts.iter().map(|t| t.1.clone()).collect();
        let holds = |src: &str, facts: &[f32]| Cond::parse(&scene, src).unwrap().0.holds(&scene, Ctx { props: &props, facts, seen: None }, &texts);
        assert!(holds("dirty == false", &facts));
        assert!(holds("status has \"Rea\" and not dirty", &facts));
        assert!(!holds("status == \"Saved\" or dirty", &facts));
        facts[scene.facts.iter().position(|f| f.0 == "dirty").unwrap()] = 1.0;
        assert!(holds("(dirty == true) and status != \"\"", &facts));
        assert_eq!(Cond::parse(&scene, "dirty 2s").unwrap().1, std::time::Duration::from_secs(2));
        assert!(Cond::parse(&scene, "drity").unwrap_err().contains("did you mean 'dirty'"));
        assert!(Cond::parse(&scene, "dirty == true extra").is_err());
    }

    #[test]
    fn words_go_to_their_zone_or_stand_alone() {
        // «Close» drawn on the close button; «Ready» drawn where no zone is.
        let seen = [
            SeenText { at: 0, text: "Close".into(), bounds: [362.0, 15.0, 388.0, 35.0], ink: [362.0, 15.0, 388.0, 35.0], clipped: false, left: true, room: [362.0, 15.0, 388.0, 35.0] },
            SeenText { at: 1, text: "Ready".into(), bounds: [20.0, 200.0, 70.0, 220.0], ink: [20.0, 200.0, 70.0, 220.0], clipped: false, left: true, room: [20.0, 200.0, 70.0, 220.0] },
            // Cut out by a clip, and no row of a list to be: nobody's.
            SeenText { at: 2, text: "Gone".into(), bounds: [20.0, 160.0, 70.0, 180.0], ink: [20.0, 160.0, 70.0, 180.0], clipped: true, left: true, room: [20.0, 160.0, 70.0, 180.0] },
        ];
        let parts = told_with(false, &seen);
        assert_eq!(node(&parts, "close").unwrap().label, "Close");
        assert!(unnamed(&parts).is_empty());
        let loose: Vec<&str> = parts[0].nodes.iter().filter(|n| n.role == Role::Text).map(|n| n.label.as_str()).collect();
        assert_eq!(loose, ["Ready"]);
        assert!(to_text(&parts).contains("text            «Ready»"), "{}", to_text(&parts));
    }

    /// `tests/follow.plm` as it starts: the scene, its facts and its texts.
    fn followed() -> (Scene, Vec<f32>, Vec<String>) {
        let (scene, _) = crate::language::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/follow.plm")).unwrap();
        let facts = scene.facts.iter().map(|f| f.1).collect();
        let texts = scene.texts.iter().map(|t| t.1.clone()).collect();
        (scene, facts, texts)
    }

    fn set(scene: &Scene, facts: &mut [f32], name: &str, v: f32) {
        facts[scene.facts.iter().position(|f| f.0 == name).unwrap()] = v;
    }

    /// Who follows, the token its reader holds, and what reaches the reader.
    fn following(starts: &[&str], now: bool, room: usize) -> (Follow, std::sync::Arc<()>, std::sync::mpsc::Receiver<String>) {
        let (lines, heard) = std::sync::mpsc::sync_channel(room);
        let reader = std::sync::Arc::new(());
        (Follow::new(starts.iter().map(|s| (*s).to_owned()).collect(), None, lines, reader.clone(), now), reader, heard)
    }

    fn said(heard: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
        heard.try_iter().collect()
    }

    #[test]
    fn following_says_one_line_for_what_changes_and_nothing_else() {
        let (scene, mut facts, texts) = followed();
        let (mut f, _reader, heard) = following(&["lay."], false, FOLLOW_ROOM);
        let at = std::time::Instant::now();
        // What it is worth when it starts is not a change.
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), Vec::<String>::new());
        // A layout and the mouse change in the same frame: only the layout is followed.
        set(&scene, &mut facts, "lay.0", 3.0);
        set(&scene, &mut facts, "mouse.x", 512.0);
        set(&scene, &mut facts, "free.0", 1.0);
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), ["fact lay.0 3"]);
        // And it is said once: the next frame, if nothing changed, says nothing.
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), Vec::<String>::new());
    }

    #[test]
    fn a_change_in_the_same_round_as_the_asking_is_one() {
        // A scene still starting takes the `watch` and a `fact` in the same
        // round: what they were worth is taken when the `watch` arrives.
        let (scene, mut facts, texts) = followed();
        let (mut f, _reader, heard) = following(&["lay."], false, FOLLOW_ROOM);
        f.start(&scene, &facts, &texts);
        set(&scene, &mut facts, "lay.0", 3.0);
        assert!(f.tell(&scene, &facts, &texts, std::time::Instant::now()));
        assert_eq!(said(&heard), ["fact lay.0 3"]);
    }

    #[test]
    fn following_says_values_as_fact_takes_them() {
        let (scene, mut facts, mut texts) = followed();
        let (mut f, _reader, heard) = following(&["remote.", "mode", "agt.", "pin", "lay.1"], false, FOLLOW_ROOM);
        let at = std::time::Instant::now();
        assert!(f.tell(&scene, &facts, &texts, at));
        set(&scene, &mut facts, "remote.on", 1.0);
        set(&scene, &mut facts, "mode", 1.0);
        set(&scene, &mut facts, "lay.1", 0.25);
        texts[scene.texts.iter().position(|t| t.0 == "agt.0").unwrap()] = "claude · ~/x\nwaits \\ asks".into();
        texts[scene.texts.iter().position(|t| t.0 == "pin").unwrap()] = "1234".into();
        assert!(f.tell(&scene, &facts, &texts, at));
        // A bool and an enum by their names, a number as it is, a text on one
        // line, and of a text kept from agents only that it changed.
        assert_eq!(said(&heard), ["fact lay.1 0.25", "fact remote.on true", "fact mode loud", "text agt.0 claude · ~/x\\nwaits \\\\ asks", "hidden pin"]);
    }

    #[test]
    fn following_now_says_first_what_they_are_worth() {
        let (scene, facts, texts) = followed();
        let (mut f, _reader, heard) = following(&["lay.", "nope."], true, FOLLOW_ROOM);
        assert!(f.tell(&scene, &facts, &texts, std::time::Instant::now()));
        assert_eq!(said(&heard), ["? no fact or text starts with 'nope.'", "fact lay.0 0", "fact lay.1 0", "fact lay.2 0", "fact lay.3 0"]);
    }

    #[test]
    fn following_ends_when_nobody_reads_or_its_time_is_up() {
        let (scene, mut facts, texts) = followed();
        let at = std::time::Instant::now();
        // The reader let go of its token: over, even with nothing to say.
        let (mut f, reader, _heard) = following(&["lay."], false, FOLLOW_ROOM);
        assert!(f.tell(&scene, &facts, &texts, at));
        drop(reader);
        assert!(!f.tell(&scene, &facts, &texts, at));
        // Its lines are not taken any more: over at the next change.
        let (mut f, _reader, heard) = following(&["lay."], false, FOLLOW_ROOM);
        assert!(f.tell(&scene, &facts, &texts, at));
        drop(heard);
        set(&scene, &mut facts, "lay.0", 2.0);
        assert!(!f.tell(&scene, &facts, &texts, at));
        // For a while: over when it is.
        let (lines, _heard) = std::sync::mpsc::sync_channel(4);
        let reader = std::sync::Arc::new(());
        let mut f = Follow::new(vec!["lay.".into()], Some(at + std::time::Duration::from_secs(5)), lines, reader.clone(), false);
        assert!(f.tell(&scene, &facts, &texts, at));
        assert!(!f.tell(&scene, &facts, &texts, at + std::time::Duration::from_secs(5)));
    }

    #[test]
    fn a_slow_reader_gets_where_things_ended() {
        let (scene, mut facts, texts) = followed();
        let at = std::time::Instant::now();
        let (mut f, _reader, heard) = following(&["lay."], false, 1);
        assert!(f.tell(&scene, &facts, &texts, at));
        set(&scene, &mut facts, "lay.0", 1.0);
        set(&scene, &mut facts, "lay.1", 2.0);
        // Room for one line: the other waits in the scene, not in memory.
        assert!(f.tell(&scene, &facts, &texts, at));
        set(&scene, &mut facts, "lay.0", 5.0);
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), ["fact lay.0 1"]);
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), ["fact lay.0 5"]);
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), ["fact lay.1 2"]);
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), Vec::<String>::new());
    }

    #[test]
    fn following_survives_the_scene_loaded_again() {
        let (scene, mut facts, texts) = followed();
        let at = std::time::Instant::now();
        let (mut f, _reader, heard) = following(&["lay."], false, FOLLOW_ROOM);
        set(&scene, &mut facts, "lay.2", 4.0);
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), Vec::<String>::new());
        // Loaded again, the same names: what was said is not said twice.
        f.lost();
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), Vec::<String>::new());
        set(&scene, &mut facts, "lay.2", 1.0);
        assert!(f.tell(&scene, &facts, &texts, at));
        assert_eq!(said(&heard), ["fact lay.2 1"]);
    }

    #[test]
    fn what_watch_is_asked() {
        use std::time::Duration;
        // For a person, as ever.
        assert!(follow_asked("").is_none());
        assert!(follow_asked("3").is_none());
        assert!(follow_asked("30s").is_none());
        // For a program: names, and `0` for as long as it reads.
        assert_eq!(follow_asked("0 lay. free.").unwrap().unwrap(), (None, false, vec!["lay.".to_owned(), "free.".to_owned()]));
        assert_eq!(follow_asked("30s --now lay.").unwrap().unwrap(), (Some(Duration::from_secs(30)), true, vec!["lay.".to_owned()]));
        assert!(follow_asked("0").unwrap().unwrap_err().contains("watch 0 lay."));
        assert!(follow_asked("0 --now").unwrap().is_err());
        assert!(follow_asked("soon lay.").unwrap().unwrap_err().contains("'soon'"));
    }

    /// #157: a list with `view:` scrolled down. «Off view» is said of a row when nothing of
    /// it is in sight —the measure the pointer presses with—, a row's button the same as the
    /// row, and a press on the button above the list finds only the button.
    #[test]
    fn off_view_is_what_the_pointer_cannot_press() {
        let (scene, _) = crate::language::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/list-view-presses.plm")).unwrap();
        let mut props: Vec<Animated> = scene.props.iter().map(|(_, v, s)| Animated { x: *v, v: 0.0, target: *v, spring: *s }).collect();
        let facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        let texts: Vec<String> = scene.texts.iter().map(|t| t.1.clone()).collect();
        let zone = |n: &str| scene.zones.iter().position(|z| z.id == n).unwrap_or_else(|| panic!("no zone {n}"));
        // Scrolled 300 px: rows 0 to 3 have gone up out of the window (68 px a row), 4 is cut.
        let scroll = scene.zones.iter().find_map(|z| z.scrolls).expect("the list scrolls");
        props[scroll.0 as usize].x = 300.0;
        let c = Ctx { props: &props, facts: &facts, seen: None };
        let boxes: Vec<Option<[f32; 4]>> = scene.zones.iter().map(|z| if z.active.is_true(c) { z.bounds(c) } else { None }).collect();
        let shown = scene.surfaces.iter().enumerate().map(|(k, s)| Shown { surface: k, popup: None, bounds: [s.origin.0, s.origin.1, s.origin.0 + s.width as f32, s.origin.1 + s.height as f32], scale: 1.0 }).collect();
        let never = |_: usize| false;
        let parts = describe(&scene, c, &texts, &Sight { shown, texts_seen: &[], gone: &never, rank: None, zones: None, index: None, person: false });
        let off = |n: &str| node(&parts, n).unwrap_or_else(|| panic!("{n} is not told")).off_view;
        for k in 0..10 {
            let (card, cross) = (format!("iz.{k}"), format!("ix.{k}"));
            assert_eq!(off(&card), scene.visible_part(zone(&card), |j| boxes[j]).is_none(), "{card}");
            assert_eq!(off(&cross), scene.visible_part(zone(&cross), |j| boxes[j]).is_none(), "{cross}");
            // A row out of sight takes its button with it.
            assert!(!off(&card) || off(&cross), "{card} off view and {cross} not");
        }
        assert!(off("iz.0") && off("ix.3") && !off("iz.5") && !off("btn"));
        // Where the reporter pressed the button: a row was there before the scroll, and is not in sight now.
        let (x, y) = (175.0, 15.0);
        let pressed: Vec<&str> = (0..scene.zones.len())
            .filter(|&k| boxes[k].is_some_and(|b| x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]))
            .filter(|&k| scene.zones[k].within.is_none() || scene.visible_part(k, |j| boxes[j]).is_some_and(|p| x >= p[0] && x <= p[2] && y >= p[1] && y <= p[3]))
            .map(|k| scene.zones[k].id)
            .collect();
        assert_eq!(pressed, ["btn"]);
    }
}
