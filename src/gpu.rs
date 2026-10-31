//! The part that talks to the GPU: the device, the sheets —one per Wayland
//! surface— and the composition of the draw list into elements.

use crate::scene::*;
use crate::platform::PlatformWindow;
use crate::text::{LayoutKey, AtlasSlot, Texts, ATLAS_MOST, ATLAS_SIZE};
use std::ops::Range;

const PER_SHAPE: usize = 20;
const PER_ELEMENT: usize = 64;
/// How many groups with opacity or effects can be blending in the same frame.
/// They share a few layers: each one is painted into one right before it is
/// blended (see `paint`), so this is not memory, only a sanity limit.
pub const MAX_LAYERS: usize = 64;
/// How many layers the groups that do not reach each other are shared out
/// among, to be painted together (see `share_layers`).
const LAYER_SLOTS: usize = 4;
/// The most the windows' texture is let take on the card: every layer is as
/// big as the biggest window, so a huge one is that many times its size.
/// `PLEAMAR_WINDOWS_BUDGET_MB` says another (a card with little memory, a test).
fn windows_budget() -> u64 {
    static BUDGET: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BUDGET.get_or_init(|| std::env::var("PLEAMAR_WINDOWS_BUDGET_MB").ok().and_then(|v| v.parse::<u64>().ok()).filter(|m| *m > 0).map_or(1 << 30, |m| m << 20))
}
/// How many frames painted without groups with opacity until their layers are given back.
const IDLE_LAYER_FRAMES: u32 = 300;
/// How many frames a surface keeps what it took of its own painting for its
/// glass after the last one that needed it.
const IDLE_OWN_FRAMES: u32 = 300;
/// How many frames a surface keeps its canvas (`Sheet::kept`) painting all of it every time.
const IDLE_KEPT_FRAMES: u32 = 300;
/// The strip added to the surface for the frame graph.
pub const HUD_HEIGHT: f32 = 84.0;
/// How much room to start with. If a scene asks for more, the stores grow to double.
const INITIAL_SHAPES: usize = 1024;
/// Floats (x, y pairs) for the paths. It grows like the others.
const INITIAL_POINTS: usize = 1024;
/// Floats (r, g, b, where) for the gradient stops.
const INITIAL_STOPS: usize = 512;
const INITIAL_ELEMENTS: usize = 1024;

/// What a region behind the surface is wanted for: glass the compositor
/// blurs, glass that bends it (captured), or a shader that reads it (captured,
/// and never blurred in its place while there is no capture yet: what it draws
/// is its own business).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Behind {
    Blur,
    Lens,
    Read,
}

/// The draw list turned into what the GPU paints: evaluated shapes and
/// elements with their box. It is recomposed every frame; it is a few hundred
/// numbers.
#[derive(Default)]
pub struct DrawList {
    pub shapes: Vec<f32>,
    /// The box of each shape, in the scene's plane (empty, one that does not
    /// exist): when only some shapes of a body change, that is where it changed.
    pub shape_bounds: Vec<[f32; 4]>,
    /// The points of the paths, in x, y pairs: each path takes a stretch.
    pub points: Vec<f32>,
    /// The gradient stops: r, g, b and where each one falls.
    pub stops: Vec<f32>,
    pub elements: Vec<f32>,
    size: (f32, f32),
    /// What the texts that asked for it have measured: property and value.
    pub measurements: Vec<(PropId, f32)>,
    /// The text fields, as they ended up: to know where a click falls.
    pub fields: Vec<PlacedField>,
    /// The texts that can be selected (`selectable: true`), as they ended up
    /// this frame, in the order they were painted.
    pub selectable: Vec<PlacedText>,
    /// While someone from outside asks what the scene says (`describe`):
    /// every text painted this frame and seen, with its box in the scene's
    /// plane. Only then: it is a copy of every text.
    pub collect_texts: bool,
    pub texts_seen: Vec<SeenText>,
    /// What is selected of one of them: its instruction and the bytes from and to.
    pub selected: Option<(usize, usize, usize)>,
    /// Groups painted separately: which elements, and on which layer.
    pub offscreen_groups: Vec<(Range<u32>, usize)>,
    /// Where each batch of them starts, by their number: the groups of a
    /// batch are painted on their layers together, one pass a layer.
    pub layer_batches: Vec<usize>,
    /// The pieces of the scene that some open popup is showing.
    pub views: Vec<[f32; 4]>,
    clips_warned: bool,
    effects_warned: bool,
    /// The seconds the render has been running, and when each event last
    /// happened (−1: never): what the particles are worked out from.
    pub clock: f32,
    pub signal_times: Vec<f32>,
    pub reduced_motion: bool,
    /// Each emitter's own state, by its instruction: when it was switched on
    /// and off, and where it was going. It outlives the frame.
    emitters: std::collections::HashMap<usize, Emitter>,
    /// Where the particles go among the elements: after which element, and how many.
    pub particle_marks: Vec<(u32, u32)>,
    /// The elements that blend another way: which one, and how (2 screen, 3 multiply).
    pub blend_marks: Vec<(u32, u8)>,
    /// The images that are pictures of their own, not pieces of the atlas:
    /// which element, and which picture. Each is drawn with its texture bound.
    pub picture_marks: Vec<(u32, u32)>,
    /// Where a glass that bends what is behind it starts among the elements:
    /// where a scene that is the whole screen takes what it has painted so far
    /// as what is behind (`lens::Own`).
    pub glass_marks: Vec<u32>,
    /// Some particle is still alive: the scene must not rest.
    pub particles_alive: bool,
    /// When an image that moves changes frame next, in the render's seconds:
    /// the render wakes up then, and not every frame for a GIF at 10 per second.
    pub wake_at: Option<f32>,
    /// A cut shadow and a cut shape, waiting to be reported.
    shadow: Pending,
    clipping: Pending,
    /// The size of the scene's surface, without the instruments strip.
    own_size: (f32, f32),
    /// Stretches of instructions that are not looked at this frame: those of a
    /// per-screen copy whose surface is closed.
    pub skip: Vec<std::ops::Range<usize>>,
    /// The order the instructions are painted in, if groups with `z:` changed it.
    pub order: Option<std::sync::Arc<Vec<usize>>>,
    /// By instruction: inside a group that was hidden the last time it was
    /// put together. Its zones catch nothing, and are not even looked at.
    pub hidden: Vec<bool>,
    /// And which edges it is attached to: nothing is reported against those.
    attached_edges: [bool; 4],
    /// Where there is glass this frame, in strips of the scene plane: what the
    /// compositor is asked to blur, or what is captured to bend or to read.
    pub glass_regions: Vec<([f32; 4], Behind)>,
    /// The strips of each glass shape, at the origin, keyed by what makes it
    /// what it is except where it is: while it only moves, they are not searched again.
    glass_cache: std::collections::HashMap<[u32; 12], (Vec<[f32; 4]>, bool)>,
    /// The windows of the scene's compositor that have an image: for each slot,
    /// where it is in the windows' texture (its layer, and the corners of the
    /// window itself inside it). The render sets it before composing.
    pub window_tex: Vec<Option<WindowTex>>,
    /// Where each window was drawn this frame: its slot, its box and what it
    /// lives under. It is what a click on it is measured against.
    pub windows_drawn: Vec<(usize, [f32; 4], Affine)>,
    /// Something is shown that moves without its elements changing —particles,
    /// a shader that reads the time, an image that moves—: comparing elements
    /// cannot say where the frame changed, so it is painted whole.
    pub timed: bool,
    /// The stretches of the drawing (see `Stretch`), by where each starts in the
    /// order of painting, and for which instructions and order they were found.
    stretch_list: Vec<Stretch>,
    stretch_at: Vec<u32>,
    stretches_for: Option<((usize, usize), Option<std::sync::Arc<Vec<usize>>>)>,
    /// What each made last time (see `Kept`), and the frame they are copied from.
    kept: Vec<Option<Kept>>,
    last: LastFrame,
    copy_key: Option<CopyKey>,
    /// What the whole drawing read, while each stretch's own reads are told apart.
    reads: Seen,
    one_stretch: Seen,
    /// Put together whole, copying nothing (to compare, see PLEAMAR_CHECK_SHORTCUTS).
    pub no_copy: bool,
    /// This frame: stretches copied, and stretches put together.
    pub copied: (u32, u32),
    /// With `PLEAMAR_DEBUG_STRETCHES`: which stretches were put together again, and why
    /// (stretch, reason, fact or property), with how many times and how long each is.
    pub remade: std::collections::BTreeMap<(usize, u8, u32), (u32, usize)>,
    plan: Plan,
    /// Each body shape as it came out the last time, by instruction, and for
    /// which instructions (see `ShapeMemo`).
    shape_memo: Vec<Option<Box<ShapeMemo>>>,
    shape_memo_for: (usize, usize),
}

/// A body's shape as it came out the last time it was worked out —before what
/// it is under moves it—, its fusion, and the facts and properties it read
/// then, with their values. A stretch is put together again when anything in
/// it changed, or when what it is under moves: a panel sliding in made each of
/// its two thousand shapes again, every frame, to get the same numbers. A shape
/// whose reads hold comes out as it came out then, without evaluating it.
#[derive(Default)]
struct ShapeMemo {
    reads: Vec<u32>,
    values: Vec<(f32, f32)>,
    flat: Option<crate::shapes::FlatShape>,
    points: Vec<f32>,
    fusion: f32,
}

/// A window of the scene's compositor as the card has it: where the window
/// itself is in its main surface (x, y, width, height), and each of its pieces
/// —its layer of the windows' texture, its corners in it, where it goes from
/// the main surface's corner (x, y, width, height), and whether it is opaque—.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowTex {
    pub geometry: [f32; 4],
    pub pieces: Vec<(u32, [f32; 4], [f32; 4], bool)>,
}

/// The draw list of the previous frame, to know **where** something has changed.
/// Each surface pays for presenting a frame (~0.5 ms, see P11) even if nothing
/// of its own changes: with one copy per monitor, the little ball breathing on
/// one repainted the other too. Comparing bit by bit is much cheaper than that.
#[derive(Default)]
pub struct PreviousFrame {
    shapes: Vec<f32>,
    shape_bounds: Vec<[f32; 4]>,
    points: Vec<f32>,
    stops: Vec<f32>,
    elements: Vec<f32>,
    offscreen_groups: Vec<(Range<u32>, usize)>,
    valid: bool,
}

impl PreviousFrame {
    /// The boxes —in the scene plane— of what has changed since the previous
    /// frame: the one from before and the one from now, because what leaves a
    /// surface also changes it. `false` if it cannot be known element by
    /// element —something has appeared or disappeared and the indices no
    /// longer match—: then everything has changed.
    pub fn changed_rects(&mut self, d: &DrawList, rects: &mut Vec<[f32; 4]>) -> bool {
        rects.clear();
        fn bits(v: &[f32]) -> &[u32] {
            bytemuck::cast_slice(v)
        }
        let same_layout = self.valid
            && self.shapes.len() == d.shapes.len()
            && self.points.len() == d.points.len()
            && self.stops.len() == d.stops.len()
            && self.elements.len() == d.elements.len()
            && self.offscreen_groups == d.offscreen_groups;
        let known = same_layout && {
            let same_points = bits(&self.points) == bits(&d.points);
            let same_stops = bits(&self.stops) == bits(&d.stops);
            // A shape changes if its numbers change, or if it is a path and the points have changed.
            let shape_changed: Vec<bool> = self
                .shapes
                .chunks_exact(PER_SHAPE)
                .zip(d.shapes.chunks_exact(PER_SHAPE))
                .map(|(a, b)| bits(a) != bits(b) || (!same_points && b[0] as u32 == 4))
                .collect();
            let changed = |k: f32| k >= 0.0 && shape_changed.get(k as usize).copied().unwrap_or(true);
            for (a, b) in self.elements.chunks_exact(PER_ELEMENT).zip(d.elements.chunks_exact(PER_ELEMENT)) {
                // A clip is also a shape.
                let change = bits(a) != bits(b) || b[32..36].iter().any(|&r| changed(r)) || (b[0] == 0.0 && !same_stops && b[15] > 0.5);
                // A body is its shapes: if only some of them changed —a drawer
                // that slides out of a bar the size of the screen—, it changed
                // where they are, before and after, and as far as their melting
                // and the body's shadow and border reach around them.
                if !change && b[0] == 0.0 {
                    let mine = b[1] as usize..b[1] as usize + b[2] as usize;
                    if mine.clone().any(|k| changed(k as f32)) {
                        let melt = mine.clone().filter_map(|k| d.shapes.get(k * PER_SHAPE + 1)).fold(0.0f32, |m, k| m.max(*k));
                        let (dx, dy) = (b[28].abs(), b[29].abs());
                        let m = melt + b[30].max(0.0) + b[23].max(0.0) + 4.0;
                        for k in mine.filter(|k| changed(*k as f32)) {
                            for r in [self.shape_bounds.get(k), d.shape_bounds.get(k)].into_iter().flatten() {
                                if r[2] > r[0] && r[3] > r[1] {
                                    rects.push([r[0] - m - dx, r[1] - m - dy, r[2] + m + dx, r[3] + m + dy]);
                                }
                            }
                        }
                    }
                }
                if change {
                    rects.push([a[4], a[5], a[6], a[7]]);
                    rects.push([b[4], b[5], b[6], b[7]]);
                }
            }
            true
        };
        self.shapes.clone_from(&d.shapes);
        self.shape_bounds.clone_from(&d.shape_bounds);
        self.points.clone_from(&d.points);
        self.stops.clone_from(&d.stops);
        self.elements.clone_from(&d.elements);
        self.offscreen_groups.clone_from(&d.offscreen_groups);
        self.valid = true;
        known
    }

    /// The list was not composed again this round, so it is the one compared last
    /// time, element for element: nothing in it changed, and there is nothing to
    /// compare or copy. Unless what was in the atlas stopped being valid (`forget`):
    /// then, as for a list not known, all of it.
    pub fn unchanged(&mut self, rects: &mut Vec<[f32; 4]>) -> bool {
        rects.clear();
        self.valid
    }

    /// That the coming frame is not compared with this one: what was in the atlas is no longer valid.
    /// (And see `DrawList::forget_kept`.)
    pub fn forget(&mut self) {
        self.valid = false;
    }
}

/// The field being typed into, as seen by whoever paints.
#[derive(Clone, Copy, PartialEq)]
pub struct FieldView {
    pub text: usize,
    pub cursor: usize,
    pub anchor: usize,
    /// The cursor blinks.
    pub visible: bool,
}

/// A text that can be selected, where it was painted: what it said, how it
/// was laid out, its corner and the transform it was under, and the box that
/// clipped it (what is outside is not seen, and so not pressed).
#[derive(Clone)]
pub struct PlacedText {
    pub at: usize,
    pub text: String,
    pub layout: std::sync::Arc<crate::text::Layout>,
    pub origin: (f32, f32),
    pub affine: Affine,
    pub limit: Option<[f32; 4]>,
}

impl PlacedText {
    /// A point of the scene in the text's own place, from its corner.
    pub fn local(&self, x: f32, y: f32) -> (f32, f32) {
        let (x, y) = self.affine.inverse().apply(x, y);
        (x - self.origin.0, y - self.origin.1)
    }

    /// Whether a point of the scene falls on its letters.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        if self.limit.is_some_and(|b| x < b[0] || y < b[1] || x > b[2] || y > b[3]) {
            return false;
        }
        let (lx, ly) = self.local(x, y);
        self.layout.lines.iter().any(|l| {
            let (a, b) = (l.cursors.first().map_or(0.0, |c| c.1), l.cursors.last().map_or(0.0, |c| c.1));
            ly >= l.top && ly < l.bottom && lx >= a.min(b) - 4.0 && lx <= a.max(b) + 4.0
        })
    }
}

/// A text as it was painted, for whoever asks what the scene says.
pub struct SeenText {
    pub at: usize,
    pub text: String,
    pub bounds: [f32; 4],
    /// Cut out by a clip: a row scrolled out of its list. Only that row's.
    pub clipped: bool,
}

#[derive(Clone)]
pub struct PlacedField {
    pub text: usize,
    pub zone: &'static str,
    pub layout: Option<std::sync::Arc<crate::text::Layout>>,
    /// Where the text starts, and how much it has scrolled so the cursor is visible.
    pub x0: f32,
    pub scroll: f32,
    /// Whether what is painted are dots: then a byte of the layout is not a
    /// byte of the text, and one has to go from one to the other by letter number.
    pub secret: bool,
    /// Takes no keys: see `passive` in the language.
    pub passive: bool,
}

/// The dot each letter of a secret field is painted with. It is three bytes
/// long, whatever the letter it covers: both computations come from that.
pub const MASK_DOT: &str = "•";
/// The byte of the real text that corresponds to one of the dots layout.
pub fn unmasked_byte(value: &str, in_dots: usize) -> usize {
    value.char_indices().nth(in_dots / MASK_DOT.len()).map_or(value.len(), |(i, _)| i)
}

/// A group with opacity, while it is being filled.
enum OpacityGroup {
    /// No layer needed: fully opaque, or there are no layers left. It multiplies and that is it.
    Multiply(f32),
    /// Invisible: nothing inside is emitted.
    Hidden,
    Layer { alpha: f32, index: usize, first_element: usize, fx: Option<Fx> },
}

/// A stretch of the drawing that can be put together on its own: from where
/// something opens (an opacity, a fade, effects, a move, a clip) to where it
/// closes, in the order the instructions are painted. What comes after it
/// depends on how much it made, not on how it made it.
#[derive(Clone, Copy)]
struct Stretch {
    from: usize,
    to: usize,
}

/// How long each of the lists a drawing makes is, at some point of it.
#[derive(Clone, Copy, PartialEq, Default)]
struct Lens {
    shapes: usize,
    shape_bounds: usize,
    points: usize,
    stops: usize,
    elements: usize,
    offscreen_groups: usize,
    particle_marks: usize,
    blend_marks: usize,
    picture_marks: usize,
    glass_marks: usize,
    glass_regions: usize,
    windows_drawn: usize,
    fields: usize,
    selectable: usize,
    measurements: usize,
}

/// What a stretch made the last time it was put together, and from what: if
/// nothing it read changed, it comes in as it came in then and starts where it
/// started then, what it makes is what it made, number for number —the same
/// shapes at the same places, so every index in it still holds—. It is copied
/// from the frame before instead of being put together again: a window's frame
/// with an agent's light breathing in its title made all of a window manager's
/// thirty thousand instructions again, every frame, for those few.
struct Kept {
    entry: u64,
    starts: Lens,
    ends: Lens,
    facts: Vec<u16>,
    props: Vec<u16>,
    fact_values: Vec<f32>,
    prop_values: Vec<(f32, f32)>,
    keys: Vec<crate::text::WorkingKey>,
    /// Some instruction in it was inside a hidden group.
    any_hidden: bool,
    /// Nothing in it changes by itself (particles, a shader that reads the
    /// time, an image that moves) and it closes what it opens.
    copyable: bool,
}

/// The lists of the frame before, to copy kept stretches from.
#[derive(Default)]
struct LastFrame {
    shapes: Vec<f32>,
    shape_bounds: Vec<[f32; 4]>,
    points: Vec<f32>,
    stops: Vec<f32>,
    elements: Vec<f32>,
    offscreen_groups: Vec<(Range<u32>, usize)>,
    particle_marks: Vec<(u32, u32)>,
    blend_marks: Vec<(u32, u8)>,
    picture_marks: Vec<(u32, u32)>,
    glass_marks: Vec<u32>,
    glass_regions: Vec<([f32; 4], Behind)>,
    windows_drawn: Vec<(usize, [f32; 4], Affine)>,
    fields: Vec<PlacedField>,
    selectable: Vec<PlacedText>,
    measurements: Vec<(PropId, f32)>,
    hidden: Vec<bool>,
}

/// What the whole drawing depends on besides facts and properties: if any of
/// it changed, no stretch is copied this frame.
#[derive(Clone, PartialEq)]
struct CopyKey {
    instrs: (usize, usize),
    size: (f32, f32),
    hud: bool,
    scale: f32,
    reduced_motion: bool,
    field: Option<FieldView>,
    selected: Option<(usize, usize, usize)>,
    skip: Vec<std::ops::Range<usize>>,
    order: Option<std::sync::Arc<Vec<usize>>>,
    views: Vec<[f32; 4]>,
    texts: Vec<String>,
    window_tex: Vec<Option<WindowTex>>,
}

/// The order a drawing is painted in and what goes with it —what is not looked
/// at, where each group closes, how many emitters come before each place—,
/// worked out again only when the instructions, their order or what is asleep
/// change: making them every frame was half of putting a window manager's
/// drawing together.
#[derive(Default)]
struct Plan {
    made_for: Option<((usize, usize), Option<std::sync::Arc<Vec<usize>>>, Vec<std::ops::Range<usize>>)>,
    sequence: Vec<usize>,
    asleep: Vec<bool>,
    closes: Vec<usize>,
    emitters_before: Vec<u32>,
}

/// A stretch being put together, to be kept when it closes.
struct Open {
    which: usize,
    from: usize,
    to: usize,
    entry: u64,
    starts: Lens,
    depth: (usize, usize, usize),
    /// What the frame had of these before it: the stretch's own are told apart.
    before: (bool, bool, Option<f32>, usize),
}

/// A hash of numbers, by their bits: each one mixed in whole.
struct Fnv(u64);
impl Fnv {
    fn u(&mut self, v: u64) {
        self.0 = (self.0 ^ v).wrapping_mul(0x9E3779B97F4A7C15).rotate_left(29);
    }
    fn f(&mut self, v: f32) {
        self.u(v.to_bits() as u64);
    }
}

/// Where a group painted on a layer reaches (see `share_layers`).
enum Reach {
    /// It is not blended: nothing reads it.
    Nothing,
    /// Where it paints and where it is read from, in the scene's plane.
    Within([f32; 4], [f32; 4]),
    /// It cannot share its layer with anyone.
    Anywhere,
}

/// What an emitter remembers between frames.
#[derive(Clone, Copy)]
struct Emitter {
    on: bool,
    start: f32,
    stop: f32,
    at: (f32, f32),
    velocity: (f32, f32),
    seen: f32,
}

/// How `mode: screen` and `mode: multiply` blend, with premultiplied colour.
/// Screen: what is under it plus what it brings, minus their product —it only
/// lightens—; over nothing it is simply itself. Multiply: what is under it
/// times what it brings, where it covers —it only darkens—; the alpha of what
/// is under it stays, so over nothing (the desktop behind the surface, which
/// pleamar cannot see) it paints nothing instead of black.
const BLENDS: [wgpu::BlendState; 2] = [
    wgpu::BlendState {
        color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::OneMinusDst, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
        alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
    },
    wgpu::BlendState {
        color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Dst, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
        alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
    },
];

/// How many instances an emitter can have: the instance number carries the
/// emitter's element as well, `element * PARTICLE_SLOTS + particle`.
pub const PARTICLE_SLOTS: u32 = 4096;

/// A group's effects, already evaluated for this frame: what goes into the
/// element that blends its layer. See `Instr::Effect` and `shape.wgsl`.
#[derive(Clone, Copy)]
struct Fx {
    blur: f32,
    glow: (f32, f32, Option<[f32; 3]>),
    tone: [f32; 4],
    mask: (f32, [f32; 4]),
    mode: u8,
    affine: Affine,
    /// A shader of the scene's over what it holds: its number, its eight numbers, the time.
    shader: Option<(u16, [f32; 8], f32)>,
}

struct OpenBody {
    first: usize,
    n: usize,
    bounds: Option<[f32; 4]>,
    slack: f32,
    shadow: Option<Shadow>,
    /// Its shapes as they are, to know on the CPU where its edge goes: the
    /// glass needs it, since it asks the compositor to blur exactly there.
    flats: Vec<crate::shapes::FlatShape>,
}

/// What a text says right now. Without copying anything when it does not have to: a
/// literal or a live text are borrowed; only what has to be assembled is.
pub fn content_text<'t>(content: &'t Content, c: Ctx, texts: &'t [String]) -> std::borrow::Cow<'t, str> {
    use std::borrow::Cow;
    match content {
        Content::Literal(t) => Cow::Borrowed(t.as_str()),
        Content::Live(id) => Cow::Borrowed(texts.get(id.0 as usize).map_or("", String::as_str)),
        Content::Number(e, decimals, after) => Cow::Owned(format!("{:.*}{after}", *decimals as usize, e.eval(c))),
        Content::Template(pieces) => {
            let mut assembled = String::new();
            Piece::write_into(pieces, c, texts, &mut assembled);
            Cow::Owned(assembled)
        }
        // The version of the language the `locale` fact says.
        Content::Translated { locale, versions } => {
            if let Some(s) = c.seen {
                s.fact(locale.0 as usize);
            }
            let k = (c.facts[locale.0 as usize].round().max(0.0) as usize).min(versions.len().saturating_sub(1));
            content_text(&versions[k], c, texts)
        }
        Content::Pick(i, options) => match options.get(crate::scene::pick_index(i.eval(c), options.len())) {
            Some(o) => content_text(o, c, texts),
            None => Cow::Borrowed(""),
        },
    }
}

/// What a glass says besides being glass, in its slots: how thick it is
/// (`uv.w`), towards its light and its dome (`glass2`), its dispersion,
/// its ripple and its centre (`glass3`), and its frost, smoke and milk. The light goes as a direction from
/// the centre of the shape, which is what the shader wants: one per shape.
fn glass_options(e: &mut [f32], g: Option<&crate::scene::Glass>, b: [f32; 4], c: Ctx) {
    let Some(g) = g else { return };
    let centre = ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5);
    e[43] = g.refraction.eval(c);
    if let Some((x, y)) = &g.shine {
        let (dx, dy) = (x.eval(c) - centre.0, y.eval(c) - centre.1);
        // In steps of 2°: a light that follows the pointer changes the glass
        // every few pixels it moves, not on every one —each change is the
        // whole body painted again, and a bar round the screen is the
        // screen—. Its edges brighten or dim by a hair at each step.
        const STEP: f32 = std::f32::consts::TAU / 180.0;
        let (dx, dy) = if dx * dx + dy * dy > 1.0 {
            let a = (dy.atan2(dx) / STEP).round() * STEP;
            (a.cos(), a.sin())
        } else {
            (0.0, -1.0)
        };
        e[52..55].copy_from_slice(&[dx, dy, 1.0]);
    }
    e[55] = g.dome.eval(c);
    e[56..60].copy_from_slice(&[g.dispersion.eval(c), g.ripple.eval(c), centre.0, centre.1]);
    // In the slots a body and a loose shape leave free: its frost (`border.w`,
    // −1 auto), its smoke and its milk (`dest.zw`, which only a texture and
    // a gradient's first two use).
    e[27] = g.frost.eval(c);
    e[38] = g.smoke.eval(c);
    e[39] = g.milk.eval(c);
    // Its own slot (`glass4`): tint, saturation, mirror, highlight.
    e[60..64].copy_from_slice(&[g.tint.eval(c), g.vivid.eval(c), g.mirror.eval(c), g.highlight.eval(c)]);
}

/// The width of a glass's bevel: a third of its short side, not going over 30 px.
fn bevel_for(bounds: [f32; 4]) -> f32 {
    ((bounds[2] - bounds[0]).min(bounds[3] - bounds[1]) * 0.33).clamp(4.0, 30.0)
}

/// Below this, a glass can barely be seen and no blur is requested.
const GLASS_VISIBLE: f32 = 0.3;
/// The height of each strip of the blur region, in logical pixels.
const STRIP: f32 = 2.0;

/// The silhouette of a shape as rectangles, **centred at the origin**: strips
/// of 2 px, and in each one the spans that fall inside. The region requested
/// from the compositor is made of rectangles; with the whole box, around a
/// round little ball one could see the corners of a blurred square. Identical
/// consecutive rows —the straight sides of a card— are merged into one.
///
/// At the origin because what almost always changes about a shape is where it
/// is: that way, a little ball that travels is not measured again, only shifted.
fn strips_of(p: &crate::shapes::FlatShape, points: &[f32]) -> Vec<[f32; 4]> {
    let mut strips: Vec<[f32; 4]> = Vec::new();
    let Some(bounds) = p.bounds() else { return strips };
    // A rounded box or an ellipse, filled, not rotated or skewed: the width of
    // each row has a formula, and there is no need to search for it. It is what
    // makes a card that grows as it opens not cost a millisecond per frame.
    let m = p.affine.m;
    if (p.kind == 0 || p.kind == 1) && p.rotation == 0.0 && p.stroke == 0.0 && m[1] == 0.0 && m[2] == 0.0 && m[0] > 0.0 && m[3] > 0.0 {
        let (sx, sy) = (m[0], m[3]);
        // Half height and, for each height from the centre, half width: in local.
        let (half_height, half_width): (f32, Box<dyn Fn(f32) -> f32>) = if p.kind == 1 {
            let r = p.radius.clamp(0.0, p.mx.min(p.my));
            let (mx, my) = (p.mx, p.my);
            (my, Box::new(move |y: f32| {
                let dy = y.abs() - (my - r);
                if dy <= 0.0 { mx } else { mx - r + (r * r - dy * dy).max(0.0).sqrt() }
            }))
        } else {
            let (a, b) = (p.radius * p.ex, p.radius * p.ey);
            (b, Box::new(move |y: f32| a * (1.0 - (y / b).powi(2)).max(0.0).sqrt()))
        };
        let (y0, y1) = (-half_height * sy, half_height * sy);
        let mut y = (y0 / STRIP).floor() * STRIP;
        while y < y1 {
            // The width at mid strip, as when it is searched for.
            let half = half_width(((y + STRIP * 0.5) / sy).clamp(-half_height, half_height)) * sx;
            if half > 0.25 {
                let (a, b) = ((-half).floor(), half.ceil());
                match strips.last_mut() {
                    Some(f) if f[0] == a && f[2] == b && f[3] == y => f[3] = y + STRIP,
                    _ => strips.push([a, y, b, y + STRIP]),
                }
            }
            y += STRIP;
        }
        return strips;
    }
    let inside = |x: f32, y: f32| p.distance_with(x, y, points) < 0.0;
    // Where it changes from outside to inside between a and b: by bisection, to a quarter of a pixel.
    let edge = |mut a: f32, mut b: f32, y: f32, entering: bool| {
        while b - a > 0.25 {
            let m = (a + b) * 0.5;
            if inside(m, y) == entering { b = m } else { a = m }
        }
        if entering { a } else { b }
    };
    let step = 3.0;
    let mut y = (bounds[1] / STRIP).floor() * STRIP;
    let mut row: Vec<(f32, f32)> = Vec::new();
    while y < bounds[3] {
        let yc = y + STRIP * 0.5;
        row.clear();
        let mut x = bounds[0];
        let mut start: Option<f32> = inside(x, yc).then_some(x);
        while x < bounds[2] {
            let next = (x + step).min(bounds[2]);
            let is_in = inside(next, yc);
            match (start, is_in) {
                (None, true) => start = Some(edge(x, next, yc, true)),
                (Some(a), false) => {
                    row.push((a, edge(x, next, yc, false)));
                    start = None;
                }
                _ => {}
            }
            x = next;
        }
        if let Some(a) = start {
            row.push((a, bounds[2]));
        }
        for &(a, b) in &row {
            let (a, b) = (a.floor(), b.ceil());
            // The row above with the same span, touching this one: it gets extended.
            match strips.iter_mut().rev().take(row.len() + 2).find(|f| f[0] == a && f[2] == b && f[3] == y) {
                Some(f) => f[3] = y + STRIP,
                None => strips.push([a, y, b, y + STRIP]),
            }
        }
        y += STRIP;
    }
    strips
}

/// A filled rounded box or ellipse, not rotated or skewed, centred at `(ox, oy)` on the surface, as rows one pixel tall on the surface's own
/// pixels: in each row, the whole pixels that are inside the shape from the top of the row to its bottom. Rounded inwards, so the blur the
/// compositor puts behind it never shows outside the edge (the edge's antialiased pixel, half glass, covers the little it leaves out); a
/// pixel tall, so a small corner is a curve and not a staircase of two-pixel steps. The straight sides merge into one rectangle, as in
/// `strips_of`. Any other shape is `None`, and is searched for by `strips_of`.
fn rows_on_pixels(p: &crate::shapes::FlatShape, ox: f32, oy: f32) -> Option<Vec<[f32; 4]>> {
    let m = p.affine.m;
    if !((p.kind == 0 || p.kind == 1) && p.rotation == 0.0 && p.stroke == 0.0 && m[1] == 0.0 && m[2] == 0.0 && m[0] > 0.0 && m[3] > 0.0) {
        return None;
    }
    let (sx, sy) = (m[0], m[3]);
    let (half_height, half_width): (f32, Box<dyn Fn(f32) -> f32>) = if p.kind == 1 {
        let r = p.radius.clamp(0.0, p.mx.min(p.my));
        let (mx, my) = (p.mx, p.my);
        (my, Box::new(move |y: f32| {
            let dy = y.abs() - (my - r);
            if dy <= 0.0 { mx } else { mx - r + (r * r - dy * dy).max(0.0).sqrt() }
        }))
    } else {
        let (a, b) = (p.radius * p.ex, p.radius * p.ey);
        (b, Box::new(move |y: f32| a * (1.0 - (y / b).powi(2)).max(0.0).sqrt()))
    };
    let mut rows: Vec<[f32; 4]> = Vec::new();
    let bottom = oy + half_height * sy;
    let mut y = (oy - half_height * sy).ceil();
    while y + 1.0 <= bottom + 1e-3 {
        // The narrowest of the row is at its end further from the centre.
        let (ya, yb) = ((y - oy) / sy, (y + 1.0 - oy) / sy);
        let far = if ya.abs() > yb.abs() { ya } else { yb };
        let half = half_width(far.clamp(-half_height, half_height)) * sx;
        let (a, b) = ((ox - half).ceil(), (ox + half).floor());
        if b > a {
            match rows.last_mut() {
                Some(f) if f[0] == a && f[2] == b && f[3] == y => f[3] = y + 1.0,
                _ => rows.push([a, y, b, y + 1.0]),
            }
        }
        y += 1.0;
    }
    Some(rows)
}

fn union(a: Option<[f32; 4]>, b: [f32; 4]) -> [f32; 4] {
    a.map_or(b, |a| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
}

/// Something that looks cut and has not been reported yet: the worst seen and
/// how to tell it. It is not reported as soon as it is seen —a card that opens
/// sticks out further every frame, and the first pixel is not the one that
/// needs fixing—, but when the count stops growing or the scene goes still.
/// And if it stops being cut, it was just passing through: then it is never
/// reported.
#[derive(Default)]
struct Pending {
    /// The worst of this episode, which is what says whether the thing keeps
    /// growing, and the worst of THIS frame, which is what sets the text when
    /// several things are cut at once.
    worst: f32,
    worst_now: f32,
    message: Option<String>,
    not_growing: u8,
    /// How many frames in a row it has been cut. A live scene —one that
    /// breathes— never goes still, so waiting for rest would mean never
    /// reporting it: after three seconds cut, it was not just passing through.
    seen: u16,
    this_frame: bool,
    told: bool,
}

impl Pending {
    fn note(&mut self, worst: f32, message: impl FnOnce() -> String) {
        self.this_frame = true;
        if worst > self.worst {
            self.worst = worst;
            self.not_growing = 0;
        }
        // The text is the one from NOW, not the one from the peak: a card that
        // opens goes past the top edge and ends up sticking out at the bottom,
        // and what needs fixing is the latter. It is redone four times per
        // second, and it is told by the worst of those cut in that frame.
        if worst >= self.worst_now {
            self.worst_now = worst;
            if self.message.is_none() || self.seen % 15 == 14 {
                self.message = Some(message());
            }
        }
    }
    /// When closing the frame: what is no longer cut is forgotten. And what has
    /// gone a quarter of a second without growing is reported now, if it is of
    /// the kind that can be reported early. **A shape is not**: crossing an
    /// edge on the way is normal —a card that opens rises past the edge and
    /// comes back—, so those are only mentioned when the scene goes still and
    /// what is cut is what stays in view.
    fn end_frame(&mut self, can_report_early: bool) -> Option<String> {
        self.worst_now = 0.0;
        if !std::mem::take(&mut self.this_frame) {
            self.message = None;
            self.worst = 0.0;
            self.not_growing = 0;
            self.seen = 0;
            return None;
        }
        self.seen = self.seen.saturating_add(1);
        self.not_growing = self.not_growing.saturating_add(1);
        if (can_report_early && self.not_growing >= 15) || self.seen >= 180 {
            return self.flush();
        }
        None
    }
    fn flush(&mut self) -> Option<String> {
        let message = self.message.take()?;
        self.told = true;
        Some(message)
    }
}

impl DrawList {
    /// Where a press's light (and its ripple, with `ripple`) can be seen: the
    /// boxes of the glass that catches it —`highlight:` above zero, or `ripple:`
    /// for the ripple—. Nothing else draws them (see `shape.wgsl`). Empty, none
    /// would show; None, one is painted on a layer of its own, whose box is
    /// not the scene's: then all of it.
    pub fn lit_by_a_press(&self, ripple: bool) -> Option<Vec<[f32; 4]>> {
        let mut out = Vec::new();
        for (k, e) in self.elements.chunks_exact(PER_ELEMENT).enumerate() {
            let catches = e[40] > 0.0 && (e[63] > 0.0 || (ripple && e[57] > 0.0));
            if !catches {
                continue;
            }
            if self.offscreen_groups.iter().any(|(r, _)| r.contains(&(k as u32))) {
                return None;
            }
            out.push([e[4] - 2.0, e[5] - 2.0, e[6] + 2.0, e[7] + 2.0]);
        }
        Some(out)
    }

    /// That nothing is copied from this frame: what was in the atlas is no
    /// longer valid, and a letter may have moved without changing its number.
    pub fn forget_kept(&mut self) {
        self.copy_key = None;
    }

    /// Another scene: nothing of the one before holds, even if its
    /// instructions happen to be where those were and as many.
    pub fn new_scene(&mut self) {
        self.copy_key = None;
        self.shape_memo.clear();
    }

    /// A body's shape and its fusion, from `shape_memo` if what it read the last
    /// time still holds; worked out, and remembered, if not.
    fn shape_once(&mut self, idx: usize, shape: &Shape, fusion: &Expr, c: Ctx, seen: &Seen) -> (crate::shapes::FlatShape, f32) {
        let m = &mut self.shape_memo[idx];
        if let Some(m) = m {
            let holds = m.reads.iter().zip(&m.values).all(|(r, v)| {
                if r & SEEN_FACT != 0 {
                    c.facts.get((r & !SEEN_FACT) as usize).is_some_and(|x| x.to_bits() == v.0.to_bits())
                } else {
                    c.props.get(*r as usize).is_some_and(|x| x.x.to_bits() == v.0.to_bits() && x.v.to_bits() == v.1.to_bits())
                }
            });
            if let (true, Some(mut p)) = (holds, m.flat) {
                for r in &m.reads {
                    if r & SEEN_FACT != 0 {
                        seen.fact((r & !SEEN_FACT) as usize);
                    } else {
                        seen.prop(*r as usize);
                    }
                }
                // A path's points go where the list is now.
                if p.kind == 4 {
                    p.radius = (self.points.len() / 2) as f32;
                }
                self.points.extend_from_slice(&m.points);
                return (p, m.fusion);
            }
        }
        let m = m.get_or_insert_with(Box::default);
        let first = self.points.len();
        seen.log_from_here();
        let p = shape.flatten_into(c, &mut self.points);
        let k = fusion.eval(c).max(0.0);
        seen.log_into(&mut m.reads);
        m.values.clear();
        m.values.extend(m.reads.iter().map(|r| {
            if r & SEEN_FACT != 0 {
                (c.facts.get((r & !SEEN_FACT) as usize).copied().unwrap_or(f32::NAN), 0.0)
            } else {
                c.props.get(*r as usize).map_or((f32::NAN, 0.0), |x| (x.x, x.v))
            }
        }));
        m.flat = Some(p);
        m.points.clear();
        m.points.extend_from_slice(&self.points[first..]);
        m.fusion = k;
        (p, k)
    }

    /// What was just put together, copying stretches, against putting it all
    /// together again (`compose`, with nothing copied): the first difference, if any.
    pub fn check_copy(&mut self, compose: impl FnOnce(&mut Self)) -> Option<String> {
        fn bits(v: &[f32]) -> Vec<u32> {
            v.iter().map(|x| x.to_bits()).collect()
        }
        let before = (
            bits(&self.shapes),
            self.shape_bounds.iter().map(|b| bits(b)).collect::<Vec<_>>(),
            bits(&self.points),
            bits(&self.stops),
            bits(&self.elements),
            self.offscreen_groups.clone(),
            self.blend_marks.clone(),
            self.picture_marks.clone(),
            self.glass_marks.clone(),
            self.hidden.clone(),
            self.windows_drawn.iter().map(|(s, b, _)| (*s, bits(b))).collect::<Vec<_>>(),
            self.measurements.iter().map(|(p, v)| (p.0, v.to_bits())).collect::<Vec<_>>(),
            self.fields.len(),
        );
        self.no_copy = true;
        compose(self);
        self.no_copy = false;
        let after = (
            bits(&self.shapes),
            self.shape_bounds.iter().map(|b| bits(b)).collect::<Vec<_>>(),
            bits(&self.points),
            bits(&self.stops),
            bits(&self.elements),
            self.offscreen_groups.clone(),
            self.blend_marks.clone(),
            self.picture_marks.clone(),
            self.glass_marks.clone(),
            self.hidden.clone(),
            self.windows_drawn.iter().map(|(s, b, _)| (*s, bits(b))).collect::<Vec<_>>(),
            self.measurements.iter().map(|(p, v)| (p.0, v.to_bits())).collect::<Vec<_>>(),
            self.fields.len(),
        );
        let first = |a: &[u32], b: &[u32]| (0..a.len().max(b.len())).find(|k| a.get(*k) != b.get(*k));
        if before.4 != after.4 {
            let k = first(&before.4, &after.4).unwrap_or(0);
            return Some(format!("elements: {} copied vs {} whole, first different number at {} (element {}, field {})", before.4.len() / PER_ELEMENT, after.4.len() / PER_ELEMENT, k, k / PER_ELEMENT, k % PER_ELEMENT));
        }
        let names = ["shapes", "", "points", "stops"];
        if before.0 != after.0 {
            return Some(format!("{}: first different at {:?}", names[0], first(&before.0, &after.0)));
        }
        if before.1 != after.1 {
            return Some("shape bounds".into());
        }
        if before.2 != after.2 {
            return Some(format!("points: first different at {:?}", first(&before.2, &after.2)));
        }
        if before.3 != after.3 {
            return Some(format!("stops: first different at {:?}", first(&before.3, &after.3)));
        }
        if before.5 != after.5 || before.6 != after.6 || before.7 != after.7 || before.8 != after.8 {
            return Some("the marks (layers, blends, pictures, glass)".into());
        }
        if before.9 != after.9 {
            return Some(format!("hidden: first different at {:?}", (0..before.9.len()).find(|k| before.9.get(*k) != after.9.get(*k))));
        }
        if before.10 != after.10 {
            return Some("the windows drawn".into());
        }
        if before.11 != after.11 || before.12 != after.12 {
            return Some("the measurements or the fields".into());
        }
        None
    }

    fn lens(&self, stops: usize) -> Lens {
        Lens {
            shapes: self.shapes.len(),
            shape_bounds: self.shape_bounds.len(),
            points: self.points.len(),
            stops,
            elements: self.elements.len(),
            offscreen_groups: self.offscreen_groups.len(),
            particle_marks: self.particle_marks.len(),
            blend_marks: self.blend_marks.len(),
            picture_marks: self.picture_marks.len(),
            glass_marks: self.glass_marks.len(),
            glass_regions: self.glass_regions.len(),
            windows_drawn: self.windows_drawn.len(),
            fields: self.fields.len(),
            selectable: self.selectable.len(),
            measurements: self.measurements.len(),
        }
    }

    /// The stretches of these instructions in this order: from each opening to
    /// its close, the outermost ones of at most `MOST` instructions, not inside
    /// one another (a group too big is looked into for smaller ones); and then
    /// those side by side —nothing opening or closing between them— joined, up
    /// to `RUN`: a window manager's drawing is a few big groups and, inside,
    /// hundreds of little ones, and a stretch copied costs a little however small.
    fn find_stretches(instrs: &[Instr], sequence: &[usize]) -> Vec<Stretch> {
        const MOST: usize = 2500;
        const LEAST: usize = 3;
        let mut open: Vec<usize> = Vec::new();
        let mut groups: Vec<Stretch> = Vec::new();
        for (p, idx) in sequence.iter().enumerate() {
            match &instrs[*idx] {
                Instr::Opacity(Some(_)) | Instr::Fade(_) | Instr::Effect(_) | Instr::Transform(Some(_)) | Instr::Clip(Some(_)) => open.push(p),
                Instr::Opacity(None) | Instr::Transform(None) | Instr::Clip(None) => {
                    if let Some(o) = open.pop() {
                        groups.push(Stretch { from: o, to: p + 1 });
                    }
                }
                _ => {}
            }
        }
        groups.sort_by_key(|g| (g.from, usize::MAX - g.to));
        let mut chosen: Vec<Stretch> = Vec::new();
        let mut past = 0;
        for g in groups {
            let n = g.to - g.from;
            if g.from >= past && (LEAST..=MOST).contains(&n) {
                past = g.to;
                chosen.push(g);
            }
        }
        const RUN: usize = 400;
        let opens_or_closes = |p: usize| {
            matches!(
                &instrs[sequence[p]],
                Instr::Opacity(_) | Instr::Fade(_) | Instr::Effect(_) | Instr::Transform(_) | Instr::Clip(_) | Instr::Group { .. } | Instr::Fill { .. }
            )
        };
        let mut runs: Vec<Stretch> = Vec::new();
        for s in chosen {
            if let Some(r) = runs.last_mut()
                && s.to - r.from <= RUN
                && !(r.to..s.from).any(opens_or_closes)
            {
                r.to = s.to;
                continue;
            }
            runs.push(s);
        }
        runs
    }

    /// A stretch just closed: what it made and read is kept, to copy it in the
    /// frames where nothing it read has changed.
    #[allow(clippy::too_many_arguments)]
    fn keep(&mut self, o: Open, c: Ctx, tip: &mut Texts, stops: usize, depth: (usize, usize, usize), closed: bool, sequence: &[usize]) {
        let ends = self.lens(stops);
        let any_hidden = sequence.get(o.from..o.to).is_some_and(|s| s.iter().any(|i| self.hidden.get(*i).copied().unwrap_or(false)));
        let (timed, particles, wake) = (self.timed, self.particles_alive, self.wake_at);
        let copyable = closed
            && depth == o.depth
            && !timed
            && !particles
            && wake.is_none()
            && self.texts_seen.len() == o.before.3
            && ends.particle_marks == o.starts.particle_marks;
        // The frame's own again, with the stretch's added.
        self.timed |= o.before.0;
        self.particles_alive |= o.before.1;
        self.wake_at = match (o.before.2, wake) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let (mut facts, mut props) = (Vec::new(), Vec::new());
        if let Some(seen) = c.seen {
            self.one_stretch.reset(c.facts.len(), c.props.len());
            self.one_stretch.take_from(seen);
            seen.pass_to(&self.reads);
            self.one_stretch.take(&mut facts, &mut props);
        }
        let keys = tip.recorded();
        let fact_values = facts.iter().map(|k| c.facts.get(*k as usize).copied().unwrap_or(f32::NAN)).collect();
        let prop_values = props.iter().map(|k| c.props.get(*k as usize).map_or((f32::NAN, f32::NAN), |p| (p.x, p.v))).collect();
        if let Some(slot) = self.kept.get_mut(o.which) {
            *slot = Some(Kept { entry: o.entry, starts: o.starts, ends, facts, props, fact_values, prop_values, keys, any_hidden, copyable });
        }
    }

    pub fn element_count(&self) -> usize {
        self.elements.len() / PER_ELEMENT
    }

    /// Whether some glass that bends the light, in that piece of the plane,
    /// has something of the scene's painted under it: an element drawn before
    /// it, seen, whose box crosses its own.
    fn glass_over_own(&self, v: [f32; 4]) -> bool {
        let of = |k: usize| &self.elements[k * PER_ELEMENT..k * PER_ELEMENT + 8];
        let count = self.element_count();
        self.glass_marks.iter().map(|m| *m as usize).filter(|m| *m < count).any(|m| {
            let g = &of(m)[4..8];
            g[0] < v[2] && g[2] > v[0] && g[1] < v[3] && g[3] > v[1] && (0..m).any(|k| {
                let e = of(k);
                e[3] > 0.004 && e[4] < e[6] && e[5] < e[7] && e[4] < g[2] && e[6] > g[0] && e[5] < g[3] && e[7] > g[1]
            })
        })
    }

    /// Whether some element of that span falls in that piece of the plane.
    fn touches_view(&self, span: &Range<u32>, v: [f32; 4]) -> bool {
        span.clone().any(|k| {
            let b = &self.elements[k as usize * PER_ELEMENT + 4..k as usize * PER_ELEMENT + 8];
            b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]
        })
    }

    fn push_shape(&mut self, p: crate::shapes::FlatShape, blend: f32) -> usize {
        let k = self.shapes.len() / PER_SHAPE;
        self.shapes.resize(self.shapes.len() + PER_SHAPE, 0.0);
        p.encode(blend, &mut self.shapes[k * PER_SHAPE..]);
        self.shape_bounds.push(p.bounds().unwrap_or([0.0; 4]));
        k
    }

    /// A piece of the atlas —a glyph, an image— placed on screen. With
    /// `tint`, its alpha is a mask painted in that colour.
    #[allow(clippy::too_many_arguments)]
    fn sprite(&mut self, d: [f32; 4], uv: [f32; 4], alpha: f32, tint: Option<[f32; 3]>, text: bool, sharp: bool, affine: Affine, clips: &[(usize, [f32; 4])]) {
        let bounds = affine.bounds([d[0], d[1], d[0] + d[2], d[1] + d[3]]);
        self.element(1.0, [bounds[0] - 1.0, bounds[1] - 1.0, bounds[2] + 1.0, bounds[3] + 1.0], clips, |e| {
            affine.encode(&mut e[44..52]);
            e[3] = alpha;
            if let Some(rgb) = tint {
                e[2] = if text { 2.0 } else { 1.0 };
                e[8..11].copy_from_slice(&rgb);
            }
            if sharp {
                e[12] = 1.0;
            }
            e[36..40].copy_from_slice(&d);
            e[40..44].copy_from_slice(&uv);
        });
    }

    /// A letter with its text's effects: its gradient, outline and shadow
    /// (`looks`, see the text's arm in `compose`), in a box grown by `pad` so
    /// the outline and the shadow fit.
    #[allow(clippy::too_many_arguments)]
    fn sprite_with_effects(&mut self, d: [f32; 4], uv: [f32; 4], alpha: f32, tint: [f32; 3], colored: bool, looks: &[f32; 20], pad: f32, affine: Affine, clips: &[(usize, [f32; 4])]) {
        let bounds = affine.bounds([d[0] - pad, d[1] - pad, d[0] + d[2] + pad, d[1] + d[3] + pad]);
        self.element(1.0, [bounds[0] - 1.0, bounds[1] - 1.0, bounds[2] + 1.0, bounds[3] + 1.0], clips, |e| {
            affine.encode(&mut e[44..52]);
            e[2] = if colored { 0.0 } else { 2.0 };
            e[3] = alpha;
            e[8..11].copy_from_slice(&tint);
            e[12..15].copy_from_slice(&looks[15..18]);
            e[15] = looks[0];
            e[16..20].copy_from_slice(&looks[1..5]);
            e[20] = looks[5];
            e[21] = looks[6];
            e[23] = 1.0;
            e[24..27].copy_from_slice(&looks[8..11]);
            e[27] = looks[7];
            e[28..32].copy_from_slice(&looks[11..15]);
            e[36..40].copy_from_slice(&d);
            e[40..44].copy_from_slice(&uv);
        });
    }

    /// A shadow is never cut on purpose. If the shape fits whole in the
    /// surface and its shadow does not, the edge is left straight and whoever
    /// sees it has nowhere to look: the shadow is not declared with a size, it
    /// comes from two numbers. The computation is already done up there;
    /// reporting it costs four subtractions.
    fn check_shadow(&mut self, shape: [f32; 4], (dx, dy, d): (f32, f32, f32)) {
        if self.shadow.told {
            return;
        }
        let (w, height) = self.own_size;
        // What the shadow asks for is its own numbers: offset and blur. The
        // margins the render keeps do not count, or the warning would mention
        // two pixels nobody wrote.
        let missing = [d - dx - shape[0], d - dy - shape[1], shape[2] + dx + d - w, shape[3] + dy + d - height];
        // Only on the sides where the shape floats inside. A bar attached to
        // the top edge has its shadow cut at the top, of course: there was no
        // room there nor was it wanted. What does not explain itself is a card
        // that fits whole and whose shadow, even so, hits the edge.
        let floats = [shape[0] > 0.5, shape[1] > 0.5, shape[2] < w - 0.5, shape[3] < height - 0.5];
        let sides = ["on the left", "above", "on the right", "below"];
        let told: Vec<String> = (0..4)
            .filter(|&k| floats[k] && missing[k] > 0.5)
            .map(|k| format!("{:.0} px {}", missing[k].ceil(), sides[k]))
            .collect();
        let worst = missing.iter().zip(floats).filter(|(_, f)| *f).map(|(v, _)| *v).fold(0.0f32, f32::max);
        if told.is_empty() {
            return;
        }
        self.shadow.note(worst, || {
            format!(
                "render · a shadow is cut: it needs {} more than this {:.0} x {:.0} surface has. The shape fits; its shadow does not",
                told.join(" and "),
                w,
                height
            )
        });
    }

    /// And the same for the shape, which is what really looks cut when a
    /// panel grows more than its surface has. Sticking out of an edge on
    /// purpose is legitimate —the neck of a little ball hangs from the top
    /// edge, and there is no room to spare there nor is it wanted—, so it is
    /// only reported for what **almost entirely** fitted: if three quarters of
    /// what is drawn are inside and the rest hits the edge, the one that fell
    /// short is the surface, and nobody will see it in `--check` because where
    /// a card ends is a computation that only exists while it runs.
    fn check_clipping(&mut self, shape: [f32; 4]) {
        if self.clipping.told {
            return;
        }
        let (w, height) = self.own_size;
        let (size_x, size_y) = (shape[2] - shape[0], shape[3] - shape[1]);
        if size_x <= 0.5 || size_y <= 0.5 {
            return;
        }
        let missing = [-shape[0], -shape[1], shape[2] - w, shape[3] - height];
        let inside_x = (shape[2].min(w) - shape[0].max(0.0)).max(0.0) / size_x;
        let inside_y = (shape[3].min(height) - shape[1].max(0.0)).max(0.0) / size_y;
        let almost = [inside_x, inside_y, inside_x, inside_y];
        //  And only what is seen at all: what a scene parks far below its
        //  surface is not cut on the right, however far its side reaches.
        let across = [inside_y, inside_x, inside_y, inside_x];
        let sides = ["on the left", "above", "on the right", "below"];
        let counts = |k: usize| !self.attached_edges[k] && missing[k] > 0.5 && almost[k] >= 0.75 && across[k] > 0.0;
        let told: Vec<String> = (0..4).filter(|&k| counts(k)).map(|k| format!("{:.0} px {}", missing[k].ceil(), sides[k])).collect();
        if told.is_empty() {
            return;
        }
        let worst = (0..4).filter(|&k| counts(k)).map(|k| missing[k]).fold(0.0f32, f32::max);
        self.clipping.note(worst, || {
            format!(
                "render · a drawing is cut: it needs {} more than this {:.0} x {:.0} surface has. Almost all of it is inside, so it looks like the surface is the one that fell short",
                told.join(" and "),
                w,
                height
            )
        });
    }

    /// The shadow business, once it is fully known: the scene has gone
    /// still, or the count has gone a quarter of a second without growing.
    pub fn report_pending(&mut self) {
        for message in [self.shadow.flush(), self.clipping.flush()].into_iter().flatten() {
            eprintln!("{message}");
        }
    }

    /// That the compositor blurs what is behind these shapes: their strips,
    /// each one separately —the union of their silhouettes is that of the
    /// body, minus the neck where two blend, which is little— and clipped.
    fn request_glass(&mut self, flats: &[crate::shapes::FlatShape], clips: &[(usize, [f32; 4])], lens: bool) {
        let mut cut = [f32::MIN, f32::MIN, f32::MAX, f32::MAX];
        for (_, r) in clips {
            cut = [cut[0].max(r[0]), cut[1].max(r[1]), cut[2].min(r[2]), cut[3].min(r[3])];
        }
        for p in flats {
            let (ox, oy) = p.affine.apply(p.cx, p.cy);
            // A rounded box or an ellipse is laid on the surface's pixels where it is, every frame (its formula costs nothing): strips
            // measured at the origin and shifted by a fraction of a pixel, then rounded outwards into the region, let the blur show past
            // the edge by up to two pixels, a stepped halo round the corners as soon as the shape is not on whole pixels.
            if let Some(rows) = rows_on_pixels(p, ox, oy) {
                self.glass_regions.extend(rows.into_iter().map(|f| [f[0].max(cut[0]), f[1].max(cut[1]), f[2].min(cut[2]), f[3].min(cut[3])]).filter(|f| f[2] > f[0] && f[3] > f[1]).map(|f| (f, if lens { Behind::Lens } else { Behind::Blur })));
                continue;
            }
            let mut at_origin = *p;
            (at_origin.cx, at_origin.cy, at_origin.affine.t) = (0.0, 0.0, [0.0, 0.0]);
            // A path is its points, which the key does not see: that one is always measured.
            let strips = if p.kind == 4 {
                strips_of(&at_origin, &self.points)
            } else {
                let key = [at_origin.kind as f32, at_origin.mx, at_origin.my, at_origin.radius, at_origin.rotation, at_origin.ex, at_origin.ey, at_origin.stroke, at_origin.affine.m[0], at_origin.affine.m[1], at_origin.affine.m[2], at_origin.affine.m[3]].map(f32::to_bits);
                let cached = self.glass_cache.entry(key).or_insert_with(|| (strips_of(&at_origin, &[]), false));
                cached.1 = true;
                cached.0.clone()
            };
            self.glass_regions.extend(strips.into_iter().map(|f| [(f[0] + ox).max(cut[0]), (f[1] + oy).max(cut[1]), (f[2] + ox).min(cut[2]), (f[3] + oy).min(cut[3])]).filter(|f| f[2] > f[0] && f[3] > f[1]).map(|f| (f, if lens { Behind::Lens } else { Behind::Blur })));
        }
    }

    /// An element only exists if its box, clipped, touches the screen.
    fn element(&mut self, kind: f32, bounds: [f32; 4], clips: &[(usize, [f32; 4])], fill: impl FnOnce(&mut [f32])) {
        // What does not fall on the surface nor on any open popup does not exist.
        let touches = |v: &[f32; 4]| bounds[0] < v[2] && bounds[2] > v[0] && bounds[1] < v[3] && bounds[3] > v[1];
        let frame = [0.0, 0.0, self.size.0, self.size.1];
        let frame = if touches(&frame) { frame } else { self.views.iter().copied().find(|v| touches(v)).unwrap_or(frame) };
        let mut c = [bounds[0].max(frame[0]), bounds[1].max(frame[1]), bounds[2].min(frame[2]), bounds[3].min(frame[3])];
        for (_, r) in clips {
            c = [c[0].max(r[0]), c[1].max(r[1]), c[2].min(r[2]), c[3].min(r[3])];
        }
        if c[2] <= c[0] || c[3] <= c[1] {
            return;
        }
        let k = self.elements.len();
        self.elements.resize(k + PER_ELEMENT, 0.0);
        let e = &mut self.elements[k..];
        e[0] = kind;
        e[4..8].copy_from_slice(&c);
        for j in 0..4 {
            // Four fit: if there are more, the innermost ones. The box is indeed that of all of them.
            e[32 + j] = clips[clips.len().saturating_sub(4)..].get(j).map_or(-1.0, |r| r.0 as f32);
        }
        fill(e);
    }

    /// The groups painted on a layer of their own, shared out in batches: in a
    /// batch, those that do not reach each other —neither paints where the
    /// other reads— go on the same layer, on up to `LAYER_SLOTS` of them, and
    /// each layer is painted in one pass. A pass each, 60 cards fading cost
    /// wgpu more than all they draw, and the card stopped between every two
    /// to read the layer just painted (#38).
    fn share_layers(&mut self) {
        self.layer_batches.clear();
        let apart = |a: &[f32; 4], b: &[f32; 4]| a[2] <= b[0] || b[2] <= a[0] || a[3] <= b[1] || b[3] <= a[1];
        // Of each layer of the batch open: where its groups paint, and where they read.
        let mut slots: Vec<Vec<([f32; 4], [f32; 4])>> = Vec::new();
        // The batch open has a group that cannot share it.
        let mut alone = true;
        for g in 0..self.offscreen_groups.len() {
            let span = self.offscreen_groups[g].0.clone();
            let slot = match self.group_reach(&span) {
                // Blended by nothing: it goes in whatever batch is open.
                Reach::Nothing if !self.layer_batches.is_empty() => 0,
                Reach::Within(w, r) if !alone => match slots.iter().position(|s| s.iter().all(|(w2, r2)| apart(&w, r2) && apart(&r, w2))) {
                    Some(k) => {
                        slots[k].push((w, r));
                        k
                    }
                    None if slots.len() < LAYER_SLOTS => {
                        slots.push(vec![(w, r)]);
                        slots.len() - 1
                    }
                    None => {
                        self.layer_batches.push(g);
                        slots = vec![vec![(w, r)]];
                        0
                    }
                },
                reach => {
                    self.layer_batches.push(g);
                    slots.clear();
                    alone = matches!(reach, Reach::Anywhere);
                    if let Reach::Within(w, r) = reach {
                        slots.push(vec![(w, r)]);
                    }
                    0
                }
            };
            self.offscreen_groups[g].1 = slot;
            if self.blends(&span) {
                self.elements[span.end as usize * PER_ELEMENT + 1] = slot as f32;
            }
        }
    }

    /// Whether the element right after a group's is the one that blends it
    /// (a group with nothing inside, or whose box falls off the surface, has none).
    fn blends(&self, span: &Range<u32>) -> bool {
        let k = span.end as usize;
        !span.is_empty() && k < self.element_count() && self.elements[k * PER_ELEMENT] == 2.0
    }

    /// Where a group paints on its layer and where it is read from.
    fn group_reach(&self, span: &Range<u32>) -> Reach {
        if !self.blends(span) {
            return Reach::Nothing;
        }
        let e = &self.elements[span.end as usize * PER_ELEMENT..(span.end as usize + 1) * PER_ELEMENT];
        // A shader of the scene's reads its layer wherever it likes; particles
        // fly out of any box; and a glass inside takes what is behind it in between.
        let particles = self.particle_marks.iter().any(|(at, _)| span.contains(at));
        let glass = self.glass_marks.iter().any(|at| span.contains(at));
        if e[40] > 0.0 || particles || glass {
            return Reach::Anywhere;
        }
        let paints = (span.start..span.end).map(|k| {
            let b = &self.elements[k as usize * PER_ELEMENT + 4..k as usize * PER_ELEMENT + 8];
            [b[0], b[1], b[2], b[3]]
        });
        let Some(paints) = paints.reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]) else { return Reach::Nothing };
        // The blend reads its box, and a blur or a glow as far as its radius
        // around it; and a pixel more, between two.
        let r = e[20].max(e[21]).max(0.0) + 2.0;
        Reach::Within(paints, [e[4] - r, e[5] - r, e[6] + r, e[7] + r])
    }

    /// Which edges the surface is attached to, which whoever requests it knows.
    pub fn set_attached_edges(&mut self, sides: [bool; 4]) {
        self.attached_edges = sides;
    }

    pub fn compose(&mut self, instrs: &[Instr], c: Ctx, texts: &[String], tip: &mut Texts, field: Option<FieldView>, size: (f32, f32), hud: bool) {
        tip.begin_frame();
        // Stretches copied from the frame before (see `Kept`): only if nothing the
        // whole drawing depends on besides facts and properties changed, and
        // knowing what is read.
        // (Compared where they are; copied only when they changed.)
        let instrs_id = (instrs.as_ptr() as usize, instrs.len());
        let same_key = self.copy_key.as_ref().is_some_and(|k| {
            k.instrs == instrs_id
                && k.size == size
                && k.hud == hud
                && k.scale == tip.scale()
                && k.reduced_motion == self.reduced_motion
                && k.field == field
                && k.selected == self.selected
                && k.skip == self.skip
                && k.order == self.order
                && k.views == self.views
                && k.texts == texts
                && k.window_tex == self.window_tex
        });
        let copying = !self.no_copy && c.seen.is_some() && !self.collect_texts && same_key;
        if self.shape_memo_for != instrs_id || self.shape_memo.len() != instrs.len() {
            self.shape_memo_for = instrs_id;
            self.shape_memo.clear();
            self.shape_memo.resize_with(instrs.len(), || None);
        }
        if !same_key {
            self.copy_key = Some(CopyKey {
                instrs: instrs_id,
                size,
                hud,
                scale: tip.scale(),
                reduced_motion: self.reduced_motion,
                field,
                selected: self.selected,
                skip: self.skip.clone(),
                order: self.order.clone(),
                views: self.views.clone(),
                texts: texts.to_vec(),
                window_tex: self.window_tex.clone(),
            });
        }
        // What this frame makes goes into the lists of two frames ago; the last one's are kept to copy from.
        std::mem::swap(&mut self.shapes, &mut self.last.shapes);
        std::mem::swap(&mut self.shape_bounds, &mut self.last.shape_bounds);
        std::mem::swap(&mut self.points, &mut self.last.points);
        std::mem::swap(&mut self.stops, &mut self.last.stops);
        std::mem::swap(&mut self.elements, &mut self.last.elements);
        std::mem::swap(&mut self.offscreen_groups, &mut self.last.offscreen_groups);
        std::mem::swap(&mut self.particle_marks, &mut self.last.particle_marks);
        std::mem::swap(&mut self.blend_marks, &mut self.last.blend_marks);
        std::mem::swap(&mut self.picture_marks, &mut self.last.picture_marks);
        std::mem::swap(&mut self.glass_marks, &mut self.last.glass_marks);
        std::mem::swap(&mut self.glass_regions, &mut self.last.glass_regions);
        std::mem::swap(&mut self.windows_drawn, &mut self.last.windows_drawn);
        std::mem::swap(&mut self.fields, &mut self.last.fields);
        std::mem::swap(&mut self.selectable, &mut self.last.selectable);
        std::mem::swap(&mut self.measurements, &mut self.last.measurements);
        std::mem::swap(&mut self.hidden, &mut self.last.hidden);
        self.reads.reset(c.facts.len(), c.props.len());
        let (mut copied, mut made) = (0u32, 0u32);
        self.measurements.clear();
        self.fields.clear();
        self.selectable.clear();
        self.texts_seen.clear();
        self.size = size;
        self.own_size = (size.0, size.1 - if hud { HUD_HEIGHT } else { 0.0 });
        self.shapes.clear();
        self.shape_bounds.clear();
        self.points.clear();
        self.elements.clear();
        let mut placed_stops: Vec<f32> = Vec::new();
        // A text's effects, waiting for the text they belong to.
        let mut text_fx: Option<&TextFx> = None;
        let mut text_select: Option<&Color> = None;
        self.offscreen_groups.clear();
        self.particle_marks.clear();
        self.blend_marks.clear();
        self.picture_marks.clear();
        self.glass_marks.clear();
        self.particles_alive = false;
        self.wake_at = None;
        self.glass_regions.clear();
        self.windows_drawn.clear();
        self.timed = false;
        let mut clips: Vec<(usize, [f32; 4])> = Vec::new();
        // Each entry is already the product of all those above it.
        let mut transforms: Vec<Affine> = Vec::new();
        let mut opacity_groups: Vec<OpacityGroup> = Vec::new();
        let mut body: Option<OpenBody> = None;
        let flatten = |f: &Shape, transforms: &[Affine], pts: &mut Vec<f32>| {
            let mut p = f.flatten_into(c, pts);
            p.affine = transforms.last().copied().unwrap_or(Affine::IDENTITY);
            p
        };
        let color = |col: &Color| [col[0].eval(c), col[1].eval(c), col[2].eval(c)];
        let tip_scale = tip.scale();

        // What is not looked at this frame, by instruction; and in which order
        // (groups with `z:` drawn over their siblings as it says).
        let mut plan = std::mem::take(&mut self.plan);
        if plan.made_for.as_ref().is_none_or(|(i, o, s)| *i != instrs_id || *o != self.order || *s != self.skip) {
            plan.asleep = vec![false; instrs.len()];
            for r in &self.skip {
                let end = r.end.min(plan.asleep.len());
                for k in r.start.min(end)..end {
                    plan.asleep[k] = true;
                }
            }
            plan.sequence = match &self.order {
                Some(o) => o.to_vec(),
                None => (0..instrs.len()).collect(),
            };
            // Where each group that opens in the sequence closes, and how many
            // emitters come before each place: a group that is hidden is jumped
            // over whole, instead of being looked at instruction by instruction
            // (a window manager's sixteen slots, most of them empty, are most of
            // its drawing). Not one with an emitter inside: an emitter keeps
            // count of its own while hidden, so it does not burst late on appearing.
            plan.closes = vec![usize::MAX; plan.sequence.len()];
            plan.emitters_before = vec![0u32; plan.sequence.len() + 1];
            let mut open: Vec<usize> = Vec::new();
            for (p, idx) in plan.sequence.iter().enumerate() {
                plan.emitters_before[p + 1] = plan.emitters_before[p] + matches!(instrs[*idx], Instr::Particles(_)) as u32;
                match &instrs[*idx] {
                    Instr::Opacity(Some(_)) | Instr::Fade(_) | Instr::Effect(_) => open.push(p),
                    Instr::Opacity(None) => {
                        if let Some(o) = open.pop() {
                            plan.closes[o] = p;
                        }
                    }
                    _ => {}
                }
            }
            plan.made_for = Some((instrs_id, self.order.clone(), self.skip.clone()));
        }
        let (asleep, sequence, closes, emitters_before) = (&plan.asleep, &plan.sequence, &plan.closes, &plan.emitters_before);
        if self.stretches_for.as_ref().is_none_or(|(i, o)| *i != instrs_id || *o != self.order) {
            self.stretch_list = Self::find_stretches(instrs, sequence);
            if std::env::var_os("PLEAMAR_DEBUG_STRETCHES").is_some() {
                let covered: usize = self.stretch_list.iter().map(|s| s.to - s.from).sum();
                let mut sizes: Vec<usize> = self.stretch_list.iter().map(|s| s.to - s.from).collect();
                sizes.sort_unstable();
                let q = |f: f32| sizes.get(((sizes.len() as f32 - 1.0) * f) as usize).copied().unwrap_or(0);
                // Loose: what is between stretches, by kind.
                let mut inside = vec![false; sequence.len()];
                for s in &self.stretch_list {
                    for x in &mut inside[s.from..s.to] {
                        *x = true;
                    }
                }
                let mut kinds: std::collections::BTreeMap<&str, usize> = Default::default();
                for (p, idx) in sequence.iter().enumerate() {
                    if !inside[p] {
                        let kind = match &instrs[*idx] {
                            Instr::Opacity(_) | Instr::Fade(_) | Instr::Effect(_) => "opacity",
                            Instr::Transform(_) => "move",
                            Instr::Clip(_) => "clip",
                            Instr::Group { .. } | Instr::Shape { .. } | Instr::Fill { .. } => "body",
                            Instr::Text { .. } => "text",
                            _ => "other",
                        };
                        *kinds.entry(kind).or_default() += 1;
                    }
                }
                eprintln!("stretches · {} of them over {} of {} instructions · sizes: median {} · 90 % {} · largest {} · left out, by kind: {:?}", self.stretch_list.len(), covered, sequence.len(), q(0.5), q(0.9), sizes.last().copied().unwrap_or(0), kinds);
            }
            self.stretch_at = vec![u32::MAX; sequence.len()];
            for (k, s) in self.stretch_list.iter().enumerate() {
                self.stretch_at[s.from] = k as u32;
            }
            self.kept = (0..self.stretch_list.len()).map(|_| None).collect();
            self.stretches_for = Some((instrs_id, self.order.clone()));
        }
        if !copying {
            for k in &mut self.kept {
                *k = None;
            }
        }
        let mut open_stretch: Option<Open> = None;
        // Jumped over from `p` (an opening that turned out hidden): past its close.
        let jump = |p: usize| -> Option<usize> {
            let close = *closes.get(p)?;
            (close != usize::MAX && emitters_before[close] == emitters_before[p]).then_some(close + 1)
        };
        // A hidden group's instructions, from its opening to its close: they
        // are one stretch of the scene, wherever `z:` puts it.
        self.hidden.clear();
        self.hidden.resize(instrs.len(), false);
        // (From just after its opening: a zone is placed by the instruction
        // that follows it, and one declared right before a hidden group
        // stands on that group's opening without being in it —Marea's body
        // was, and she caught no click—.)
        fn hide(hidden: &mut [bool], from: usize, to: usize) {
            let (a, b) = (from.min(to) + 1, from.max(to).min(hidden.len().saturating_sub(1)));
            for h in hidden.iter_mut().take(b + 1).skip(a) {
                *h = true;
            }
        }
        let mut next = 0usize;
        while next < sequence.len() {
            let here = next;
            // A stretch that has just closed is kept.
            if open_stretch.as_ref().is_some_and(|o| here >= o.to) {
                if let Some(o) = open_stretch.take() {
                    let closed = body.is_none() && text_fx.is_none() && text_select.is_none();
                    self.keep(o, c, tip, placed_stops.len(), (transforms.len(), clips.len(), opacity_groups.len()), closed, sequence);
                }
            }
            // One that starts here: copied from the frame before if nothing it
            // read has changed, as it came in and where it started then;
            // otherwise put together, and kept.
            if open_stretch.is_none() && body.is_none() && text_fx.is_none() && text_select.is_none() {
                if let Some(which) = self.stretch_at.get(here).copied().filter(|k| *k != u32::MAX).map(|k| k as usize) {
                    let to = self.stretch_list[which].to;
                    let entry = {
                        let mut h = Fnv(0xcbf29ce484222325);
                        let a = transforms.last().copied().unwrap_or(Affine::IDENTITY);
                        for v in a.m.iter().chain(&a.t) {
                            h.f(*v);
                        }
                        h.u(clips.len() as u64);
                        for (k, r) in &clips {
                            h.u(*k as u64);
                            for v in r {
                                h.f(*v);
                            }
                        }
                        h.u(opacity_groups.len() as u64);
                        for g in &opacity_groups {
                            match g {
                                OpacityGroup::Multiply(a) => {
                                    h.u(1);
                                    h.f(*a);
                                }
                                OpacityGroup::Hidden => h.u(2),
                                OpacityGroup::Layer { alpha, .. } => {
                                    h.u(3);
                                    h.f(*alpha);
                                }
                            }
                        }
                        h.0
                    };
                    let starts = self.lens(placed_stops.len());
                    let last = &self.last;
                    let fits = |k: &Kept| {
                        let e = &k.ends;
                        e.shapes <= last.shapes.len()
                            && e.shape_bounds <= last.shape_bounds.len()
                            && e.points <= last.points.len()
                            && e.stops <= last.stops.len()
                            && e.elements <= last.elements.len()
                            && e.offscreen_groups <= last.offscreen_groups.len()
                            && e.blend_marks <= last.blend_marks.len()
                            && e.picture_marks <= last.picture_marks.len()
                            && e.glass_marks <= last.glass_marks.len()
                            && e.glass_regions <= last.glass_regions.len()
                            && e.windows_drawn <= last.windows_drawn.len()
                            && e.fields <= last.fields.len()
                            && e.selectable <= last.selectable.len()
                            && e.measurements <= last.measurements.len()
                    };
                    let same = |k: &Kept| {
                        k.copyable
                            && k.entry == entry
                            && k.starts == starts
                            && fits(k)
                            && k.facts.iter().zip(&k.fact_values).all(|(f, v)| c.facts.get(*f as usize).is_some_and(|x| x.to_bits() == v.to_bits()))
                            && k.props.iter().zip(&k.prop_values).all(|(p, v)| c.props.get(*p as usize).is_some_and(|x| x.x.to_bits() == v.0.to_bits() && x.v.to_bits() == v.1.to_bits()))
                    };
                    if copying && self.kept.get(which).and_then(Option::as_ref).is_some_and(same) {
                        let k = self.kept[which].as_ref().unwrap();
                        let (a, b) = (k.starts, k.ends);
                        let last = &self.last;
                        self.shapes.extend_from_slice(&last.shapes[a.shapes..b.shapes]);
                        self.shape_bounds.extend_from_slice(&last.shape_bounds[a.shape_bounds..b.shape_bounds]);
                        self.points.extend_from_slice(&last.points[a.points..b.points]);
                        placed_stops.extend_from_slice(&last.stops[a.stops..b.stops]);
                        self.elements.extend_from_slice(&last.elements[a.elements..b.elements]);
                        self.offscreen_groups.extend_from_slice(&last.offscreen_groups[a.offscreen_groups..b.offscreen_groups]);
                        self.blend_marks.extend_from_slice(&last.blend_marks[a.blend_marks..b.blend_marks]);
                        self.picture_marks.extend_from_slice(&last.picture_marks[a.picture_marks..b.picture_marks]);
                        self.glass_marks.extend_from_slice(&last.glass_marks[a.glass_marks..b.glass_marks]);
                        self.glass_regions.extend_from_slice(&last.glass_regions[a.glass_regions..b.glass_regions]);
                        self.windows_drawn.extend_from_slice(&last.windows_drawn[a.windows_drawn..b.windows_drawn]);
                        self.fields.extend_from_slice(&last.fields[a.fields..b.fields]);
                        self.selectable.extend_from_slice(&last.selectable[a.selectable..b.selectable]);
                        self.measurements.extend_from_slice(&last.measurements[a.measurements..b.measurements]);
                        if k.any_hidden {
                            for &i in &sequence[here..to] {
                                if let Some(h) = self.hidden.get_mut(i) {
                                    *h = last.hidden.get(i).copied().unwrap_or(false);
                                }
                            }
                        }
                        tip.note_again(&k.keys);
                        for f in &k.facts {
                            self.reads.fact(*f as usize);
                        }
                        for p in &k.props {
                            self.reads.prop(*p as usize);
                        }
                        copied += 1;
                        next = to;
                        continue;
                    }
                    if debug_stretches() {
                        let st = &self.stretch_list[which];
                        let (reason, id) = match self.kept.get(which).and_then(Option::as_ref) {
                            None => (0, 0),
                            Some(k) if !k.copyable => (1, 0),
                            Some(k) if k.entry != entry => (2, 0),
                            Some(k) if k.starts != starts => {
                                let (a, b) = (k.starts, starts);
                                let diff = [a.shapes != b.shapes, a.shape_bounds != b.shape_bounds, a.points != b.points, a.stops != b.stops, a.elements != b.elements, a.offscreen_groups != b.offscreen_groups, a.particle_marks != b.particle_marks, a.blend_marks != b.blend_marks, a.picture_marks != b.picture_marks, a.glass_marks != b.glass_marks, a.glass_regions != b.glass_regions, a.windows_drawn != b.windows_drawn, a.fields != b.fields, a.selectable != b.selectable, a.measurements != b.measurements];
                                (3, diff.iter().enumerate().fold(0u32, |m, (i, d)| m | ((*d as u32) << i)))
                            }
                            Some(k) if !fits(k) => (4, 0),
                            Some(k) => match k.facts.iter().zip(&k.fact_values).find(|(f, v)| !c.facts.get(**f as usize).is_some_and(|x| x.to_bits() == v.to_bits())) {
                                Some((f, _)) => (5, *f as u32),
                                None => match k.props.iter().zip(&k.prop_values).find(|(p, v)| !c.props.get(**p as usize).is_some_and(|x| x.x.to_bits() == v.0.to_bits() && x.v.to_bits() == v.1.to_bits())) {
                                    Some((p, _)) => (6, *p as u32),
                                    None => (7, 0),
                                },
                            },
                        };
                        let e = self.remade.entry((which, reason, id)).or_insert((0, st.to - st.from));
                        e.0 += 1;
                    }
                    if let Some(seen) = c.seen {
                        seen.pass_to(&self.reads);
                    }
                    tip.record();
                    let before = (self.timed, self.particles_alive, self.wake_at, self.texts_seen.len());
                    (self.timed, self.particles_alive, self.wake_at) = (false, false, None);
                    open_stretch = Some(Open { which, from: here, to, entry, starts, depth: (transforms.len(), clips.len(), opacity_groups.len()), before });
                    made += 1;
                }
            }
            let idx = sequence[here];
            next += 1;
            if asleep[idx] {
                continue;
            }
            let i = &instrs[idx];
            let hidden = opacity_groups.iter().any(|g| matches!(g, OpacityGroup::Hidden));
            // What multiplies each element: the groups that have no layer of their own.
            let mult: f32 = opacity_groups.iter().map(|g| if let OpacityGroup::Multiply(a) = g { *a } else { 1.0 }).product();
            // And what the groups with a layer of their own take away when they
            // blend it: a glass fading inside one is seen that much less (its
            // blur region kept for the whole fade, #39).
            let layered: f32 = opacity_groups.iter().map(|g| if let OpacityGroup::Layer { alpha, .. } = g { *alpha } else { 1.0 }).product::<f32>();
            let affine = transforms.last().copied().unwrap_or(Affine::IDENTITY);
            match i {
                Instr::Opacity(Some(a)) => {
                    let a = a.eval(c).clamp(0.0, 1.0);
                    if a <= 0.001 {
                        if let Some(past) = jump(here) {
                            hide(&mut self.hidden, idx, sequence[past - 1]);
                            next = past;
                            continue;
                        }
                    }
                    let inside_layer = opacity_groups.iter().any(|g| matches!(g, OpacityGroup::Layer { .. }));
                    opacity_groups.push(if a <= 0.001 {
                        OpacityGroup::Hidden
                    } else if a >= 0.999 || inside_layer || self.offscreen_groups.len() >= MAX_LAYERS {
                        OpacityGroup::Multiply(a)
                    } else {
                        OpacityGroup::Layer { alpha: a, index: self.offscreen_groups.len(), first_element: self.element_count(), fx: None }
                    });
                    if let Some(OpacityGroup::Layer { first_element, .. }) = opacity_groups.last() {
                        self.offscreen_groups.push((*first_element as u32..*first_element as u32, 0));
                    }
                }
                Instr::Effect(fx) => {
                    let a = fx.alpha.eval(c).clamp(0.0, 1.0);
                    if a <= 0.001 {
                        if let Some(past) = jump(here) {
                            hide(&mut self.hidden, idx, sequence[past - 1]);
                            next = past;
                            continue;
                        }
                    }
                    let inside_layer = opacity_groups.iter().any(|g| matches!(g, OpacityGroup::Layer { .. }));
                    let full = self.offscreen_groups.len() >= MAX_LAYERS;
                    if full && !std::mem::replace(&mut self.effects_warned, true) {
                        eprintln!("render · more than {MAX_LAYERS} groups with effects or opacity at once: the rest are painted without their effects");
                    }
                    opacity_groups.push(if a <= 0.001 {
                        OpacityGroup::Hidden
                    } else if inside_layer || full {
                        OpacityGroup::Multiply(a)
                    } else {
                        let v = |e: &Option<Expr>, default: f32| e.as_ref().map_or(default, |e| e.eval(c));
                        let point = |p: &Point| [p.0.eval(c), p.1.eval(c)];
                        let mask = match &fx.mask {
                            None => (0.0, [0.0; 4]),
                            Some(Mask::Linear(from, to)) => {
                                let (a, b) = (point(from), point(to));
                                (1.0, [a[0], a[1], b[0], b[1]])
                            }
                            Some(Mask::Radial(at, r1, r2)) => {
                                let a = point(at);
                                (2.0, [a[0], a[1], r1.eval(c).max(0.0), r2.eval(c).max(0.0)])
                            }
                        };
                        let glow = fx.glow.as_ref().map_or((0.0, 0.0, None), |(r, k, col)| (r.eval(c).max(0.0), k.eval(c).max(0.0), col.as_ref().map(&color)));
                        let shader = fx.shader.as_ref().filter(|g| g.on.as_ref().is_none_or(|e| e.eval(c) != 0.0)).map(|g| {
                            let mut v = [0f32; 8];
                            for (slot, e) in v.iter_mut().zip(&g.values) {
                                *slot = e.eval(c);
                            }
                            self.timed |= g.time.is_some();
                            (g.shader, v, g.time.as_ref().map_or(0.0, |e| e.eval(c)))
                        });
                        let fx = Fx {
                            blur: v(&fx.blur, 0.0).max(0.0),
                            glow,
                            tone: [v(&fx.saturation, 1.0), v(&fx.brightness, 1.0), v(&fx.contrast, 1.0), v(&fx.hue, 0.0)],
                            mask,
                            mode: fx.mode,
                            affine,
                            shader,
                        };
                        // Effects that, right now, do nothing —a window with the
                        // keyboard at full colour, a glow that has gone out— need
                        // no layer of their own: what is inside is painted as it is.
                        let neutral = fx.blur < 0.01
                            && (fx.glow.0 < 0.01 || fx.glow.1 < 0.001)
                            && (fx.tone[0] - 1.0).abs() < 0.001
                            && (fx.tone[1] - 1.0).abs() < 0.001
                            && (fx.tone[2] - 1.0).abs() < 0.001
                            && fx.tone[3].abs() < 0.001
                            && fx.mask.0 == 0.0
                            && fx.mode == 0
                            && fx.shader.is_none();
                        if neutral && a >= 0.999 {
                            OpacityGroup::Multiply(1.0)
                        } else {
                            OpacityGroup::Layer { alpha: a, index: self.offscreen_groups.len(), first_element: self.element_count(), fx: Some(fx) }
                        }
                    });
                    if let Some(OpacityGroup::Layer { first_element, .. }) = opacity_groups.last() {
                        self.offscreen_groups.push((*first_element as u32..*first_element as u32, 0));
                    }
                }
                Instr::Particles(pp) => {
                    // With reduced motion there is nothing that carries itself: no particles.
                    if self.reduced_motion {
                        continue;
                    }
                    let now = self.clock;
                    let at = (pp.at.0.eval(c), pp.at.1.eval(c));
                    let st = self.emitters.entry(idx).or_insert(Emitter { on: false, start: f32::MIN, stop: f32::MIN, at, velocity: (0.0, 0.0), seen: now });
                    // Where the emitter is going, softened: what it leaves behind is
                    // born where it was, not where it is now.
                    let dt = now - st.seen;
                    if dt > 1e-4 {
                        let v = ((at.0 - st.at.0) / dt, (at.1 - st.at.1) / dt);
                        let k = (dt / 0.08).min(1.0);
                        st.velocity = (st.velocity.0 + (v.0 - st.velocity.0) * k, st.velocity.1 + (v.1 - st.velocity.1) * k);
                        (st.at, st.seen) = (at, now);
                    }
                    let emitting = pp.emit.eval(c) > 0.5;
                    if emitting && !st.on {
                        (st.on, st.start) = (true, now);
                    } else if !emitting && st.on {
                        (st.on, st.stop) = (false, now);
                    }
                    let st = *st;
                    let life = (pp.life.0.eval(c).max(0.01), pp.life.1.eval(c).max(0.01));
                    let life_max = life.0.max(life.1);
                    let burst = pp.burst.map_or(-1.0, |s| self.signal_times.get(s.0 as usize).copied().unwrap_or(-1.0));
                    let alive = st.on || now - st.stop < life_max || (burst >= 0.0 && now - burst < life_max + 0.1);
                    let a = pp.alpha.eval(c).clamp(0.0, 1.0) * mult;
                    // Inside a group that is not shown, it is not shown either. Its
                    // emitter kept count above, so it does not burst late on appearing.
                    if !alive || a <= 0.001 || hidden {
                        continue;
                    }
                    self.particles_alive = true;
                    let speed = (pp.speed.0.eval(c), pp.speed.1.eval(c));
                    let gravity = (pp.gravity.0.eval(c), pp.gravity.1.eval(c));
                    let size = (pp.size.0.eval(c).max(0.0), pp.size.1.eval(c).max(0.0));
                    let area = (pp.area.0.eval(c).abs(), pp.area.1.eval(c).abs());
                    // How far one can get: the fastest, all its life, plus what it falls
                    // and what the emitter moved meanwhile.
                    let reach = speed.0.abs().max(speed.1.abs()) * life_max
                        + 0.5 * gravity.0.hypot(gravity.1) * life_max * life_max
                        + st.velocity.0.hypot(st.velocity.1) * life_max
                        + size.0.max(size.1) * 2.0;
                    let b = affine.bounds([at.0 - area.0 * 0.5 - reach, at.1 - area.1 * 0.5 - reach, at.0 + area.0 * 0.5 + reach, at.1 + area.1 * 0.5 + reach]);
                    let (c0, c1) = (color(&pp.colors.0), color(&pp.colors.1));
                    let before = self.element_count();
                    let shape = match pp.shape {
                        ParticleShape::Dot => 0.0,
                        ParticleShape::Square => 1.0,
                        ParticleShape::Spark => 2.0,
                    };
                    let (start, stop) = (if st.start == f32::MIN { 1e9 } else { st.start }, if st.on { 1e9 } else { st.stop });
                    let fields = [
                        idx as f32, pp.count as f32, a,
                        c0[0], c0[1], c0[2], pp.opacity.0.eval(c),
                        c1[0], c1[1], c1[2], pp.opacity.1.eval(c),
                        at.0, at.1, area.0, area.1,
                        life.0, life.1, speed.0, speed.1,
                        pp.direction.eval(c), pp.spread.eval(c), pp.drag.eval(c).max(0.0), shape,
                        gravity.0, gravity.1, size.0, size.1,
                        now, start, stop, burst,
                        st.velocity.0, st.velocity.1, if pp.burst.is_some() { 1.0 } else { 0.0 },
                    ];
                    self.element(4.0, b, &clips, |e| {
                        e[1..3].copy_from_slice(&fields[0..2]);
                        e[3] = fields[2];
                        e[8..16].copy_from_slice(&fields[3..11]);
                        e[16..20].copy_from_slice(&fields[11..15]);
                        e[20..24].copy_from_slice(&fields[15..19]);
                        e[24..28].copy_from_slice(&fields[19..23]);
                        e[28..32].copy_from_slice(&fields[23..27]);
                        e[36..40].copy_from_slice(&fields[27..31]);
                        e[40..43].copy_from_slice(&fields[31..34]);
                        affine.encode(&mut e[44..52]);
                    });
                    if self.element_count() > before {
                        self.particle_marks.push((before as u32, pp.count));
                    }
                }
                Instr::Fade(a) => {
                    let a = a.eval(c).clamp(0.0, 1.0);
                    if a <= 0.001 {
                        if let Some(past) = jump(here) {
                            hide(&mut self.hidden, idx, sequence[past - 1]);
                            next = past;
                            continue;
                        }
                    }
                    opacity_groups.push(if a <= 0.001 { OpacityGroup::Hidden } else { OpacityGroup::Multiply(a) });
                }
                Instr::Opacity(None) => {
                    if let Some(OpacityGroup::Layer { alpha, index, first_element, fx }) = opacity_groups.pop() {
                        let end = self.element_count();
                        self.offscreen_groups[index].0 = first_element as u32..end as u32;
                        // The group's box is the union of those inside.
                        let bounds = (first_element..end).fold(None, |u, k| {
                            let e = &self.elements[k * PER_ELEMENT + 4..k * PER_ELEMENT + 8];
                            Some(union(u, [e[0], e[1], e[2], e[3]]))
                        });
                        if let Some(bounds) = bounds {
                            // A blur and a glow spill out of what is inside: the box grows with them.
                            let spill = fx.map_or(0.0, |f| f.blur.max(f.glow.0) * 1.5 + 2.0);
                            let bounds = [bounds[0] - spill, bounds[1] - spill, bounds[2] + spill, bounds[3] + spill];
                            let before = self.element_count();
                            if let Some(f) = fx.filter(|f| f.mode >= 2) {
                                self.blend_marks.push((before as u32, f.mode));
                            }
                            self.element(2.0, bounds, &[], |e| {
                                e[1] = 0.0;
                                e[3] = alpha * mult;
                                if let Some(f) = fx {
                                    if let Some(k) = f.glow.2 {
                                        e[8..11].copy_from_slice(&k);
                                    }
                                    e[11] = f.glow.1;
                                    e[12..16].copy_from_slice(&f.tone);
                                    e[16..20].copy_from_slice(&f.mask.1);
                                    e[20..24].copy_from_slice(&[f.blur, f.glow.0, f.mask.0, f.mode as f32]);
                                    e[24..28].copy_from_slice(&[f.glow.2.is_some() as u8 as f32, 1.0, 0.0, 0.0]);
                                    f.affine.encode(&mut e[44..52]);
                                    // Its box is what the group holds, without what spills.
                                    if let Some((k, v, t)) = f.shader {
                                        e[36..40].copy_from_slice(&[bounds[0] + spill, bounds[1] + spill, bounds[2] - bounds[0] - 2.0 * spill, bounds[3] - bounds[1] - 2.0 * spill]);
                                        e[40..42].copy_from_slice(&[k as f32 + 1.0, t]);
                                        e[52..60].copy_from_slice(&v);
                                    }
                                }
                            });
                        }
                    }
                }
                _ if hidden => {}
                Instr::Group { shadow } => {
                    body = Some(OpenBody { first: self.shapes.len() / PER_SHAPE, n: 0, bounds: None, slack: 0.0, shadow: shadow.clone(), flats: Vec::new() })
                }
                Instr::Shape { shape, fusion: blend } => {
                    let (mut p, k) = match c.seen {
                        Some(seen) if !self.no_copy => self.shape_once(idx, shape, blend, c, seen),
                        _ => (shape.flatten_into(c, &mut self.points), blend.eval(c).max(0.0)),
                    };
                    p.affine = affine;
                    let bounds = p.bounds();
                    self.push_shape(p, k);
                    if let Some(g) = &mut body {
                        g.flats.push(p);
                        g.n += 1;
                        g.slack = g.slack.max(k * 0.5);
                        if let Some(b) = bounds {
                            g.bounds = Some(union(g.bounds, b));
                        }
                    }
                }
                Instr::Fill { paint, alpha, rim: edge, light, border, glass_spec } => {
                    let Some(g) = body.take() else { continue };
                    let Some(mut bounds) = g.bounds else { continue };
                    let shape_only = bounds;
                    let h = g.slack + 2.0;
                    bounds = [bounds[0] - h, bounds[1] - h, bounds[2] + h, bounds[3] + h];
                    //  Its numbers are read here, which is where it is known how
                    //  the scene is right now: a shadow can keep changing.
                    let shadow = g.shadow.as_ref().map(|s| (s.offset.0.eval(c), s.offset.1.eval(c), s.blur.eval(c).max(0.0), s.alpha.eval(c).clamp(0.0, 1.0)));
                    if let Some((sx, sy, sd, _)) = shadow {
                        let d = sd + 2.0;
                        bounds = union(Some(bounds), [bounds[0] + sx - d, bounds[1] + sy - d, bounds[2] + sx + d, bounds[3] + sy + d]);
                    }
                    let a = alpha.eval(c).clamp(0.0, 1.0) * mult;
                    if let Some((sx, sy, sd, sa)) = shadow.filter(|s| a > 0.01 && s.3 > 0.01) {
                        self.check_shadow(shape_only, (sx, sy, sd));
                        let _ = sa;
                    }
                    if a > 0.01 {
                        self.check_clipping(shape_only);
                    }
                    // A visible glass asks for what is behind to be blurred, following its silhouette.
                    let v = glass_spec.as_ref().map_or(0.0, |v| v.amount.eval(c));
                    let lens = glass_spec.as_ref().is_some_and(|v| v.lens.is_true(c));
                    if v * a * layered > GLASS_VISIBLE {
                        self.request_glass(&g.flats, &clips, lens);
                        if lens {
                            self.glass_marks.push(self.element_count() as u32);
                        }
                    }
                    self.element(0.0, bounds, &clips, |e| {
                        affine.encode(&mut e[44..52]);
                        e[1] = g.first as f32;
                        e[2] = g.n as f32;
                        e[3] = a;
                        match paint {
                            Paint::Color(col) => e[8..11].copy_from_slice(&color(col)),
                            Paint::Gradient { radial, from, to, stops } => {
                                e[15] = if *radial { 2.0 } else { 1.0 };
                                e[16..20].copy_from_slice(&[from.0.eval(c), from.1.eval(c), to.0.eval(c), to.1.eval(c)]);
                                // The stops go in their store: where each one falls and what colour.
                                e[36] = (placed_stops.len() / 4) as f32;
                                e[37] = stops.len() as f32;
                                for (at, col) in stops {
                                    let rgb = color(col);
                                    placed_stops.extend_from_slice(&[rgb[0], rgb[1], rgb[2], at.eval(c).clamp(0.0, 1.0)]);
                                }
                            }
                        }
                        e[11] = edge.eval(c);
                        // A body does not use `uv`, which is for textures: the
                        // glass goes there, and the width of its bevel, which
                        // grows with it: something big bends the light more,
                        // like thicker glass.
                        e[40] = v;
                        e[41] = bevel_for(shape_only);
                        e[42] = lens as u8 as f32;
                        glass_options(e, glass_spec.as_ref(), shape_only, c);
                        if let Some(l) = light {
                            e[20..23].copy_from_slice(&[l.amount, l.from_y.eval(c), l.height]);
                        }
                        if let Some((thickness, col)) = border {
                            e[23] = thickness.eval(c).max(0.0);
                            e[24..27].copy_from_slice(&color(col));
                        }
                        if let Some((sx, sy, sd, sa)) = shadow {
                            e[28..32].copy_from_slice(&[sx, sy, sd, sa]);
                            //  Its colour goes in the three slots of `color1`,
                            //  which only used the fourth to say whether there is a gradient.
                            if let Some(col) = g.shadow.as_ref().and_then(|s| s.color.as_ref()) {
                                e[12..15].copy_from_slice(&color(col));
                            }
                        }
                    });
                }
                Instr::Solid { shape, color: col, alpha, glass_spec } => {
                    let a = alpha.eval(c).clamp(0.0, 1.0) * mult;
                    if a <= 0.001 {
                        continue; // what is invisible does not take up even a quad
                    }
                    let p = flatten(shape, &transforms, &mut self.points);
                    let Some(b) = p.bounds() else { continue };
                    let v = glass_spec.as_ref().map_or(0.0, |v| v.amount.eval(c).clamp(0.0, 1.0));
                    let lens = glass_spec.as_ref().is_some_and(|v| v.lens.is_true(c));
                    if v * a * layered > GLASS_VISIBLE {
                        self.request_glass(&[p], &clips, lens);
                        if lens {
                            self.glass_marks.push(self.element_count() as u32);
                        }
                    }
                    let k = self.push_shape(p, 0.0);
                    let rgb = color(col);
                    self.element(0.0, [b[0] - 2.0, b[1] - 2.0, b[2] + 2.0, b[3] + 2.0], &clips, |e| {
                        affine.encode(&mut e[44..52]);
                        e[1] = k as f32;
                        e[2] = 1.0;
                        e[3] = a;
                        e[8..11].copy_from_slice(&rgb);
                        e[40] = v;
                        e[41] = bevel_for(b);
                        e[42] = lens as u8 as f32;
                        glass_options(e, glass_spec.as_ref(), b, c);
                    });
                }
                Instr::Image { image, target, alpha, tint, cell, fit } => {
                    let a = alpha.eval(c).clamp(0.0, 1.0) * mult;
                    // What is not seen does not move either: no frame, no waking up.
                    if a <= 0.001 {
                        continue;
                    }
                    let (slot, next) = tip.frame(image.0 as usize, texts, self.clock, self.reduced_motion);
                    let Some(slot) = slot else { continue };
                    if let Some(t) = next {
                        self.wake_at = Some(self.wake_at.map_or(t, |w| w.min(t)));
                    }
                    let d = [target.0.eval(c), target.1.eval(c), target.2.eval(c), target.3.eval(c)];
                    let rgb = tint.as_ref().map(&color);
                    // One picture of a sheet: its piece of what the image takes in the atlas.
                    let mut uv = slot.uv();
                    if let Some((which, columns, rows)) = cell {
                        let (columns, rows) = (columns.eval(c).round().max(1.0), rows.eval(c).round().max(1.0));
                        let which = which.eval(c).floor().rem_euclid(columns * rows);
                        let (x, y) = (which % columns, (which / columns).floor());
                        let (w, h) = ((uv[2] - uv[0]) / columns, (uv[3] - uv[1]) / rows);
                        uv = [uv[0] + x * w, uv[1] + y * h, uv[0] + (x + 1.0) * w, uv[1] + (y + 1.0) * h];
                    }
                    // Not stretched: the picture's shape kept, whole inside its box
                    // (the box made smaller) or covering it (the picture cut).
                    let mut d = d;
                    let (pw, ph) = (uv[2] - uv[0], uv[3] - uv[1]);
                    if *fit > 0 && pw > 0.0 && ph > 0.0 && d[2] > 0.0 && d[3] > 0.0 {
                        let k = (pw / ph) / (d[2] / d[3]);
                        match (*fit, k > 1.0) {
                            (1, true) => (d[1], d[3]) = (d[1] + (d[3] - d[3] / k) * 0.5, d[3] / k),
                            (1, false) => (d[0], d[2]) = (d[0] + (d[2] - d[2] * k) * 0.5, d[2] * k),
                            (_, true) => (uv[0], uv[2]) = (uv[0] + (pw - pw / k) * 0.5, uv[0] + (pw + pw / k) * 0.5),
                            (_, false) => (uv[1], uv[3]) = (uv[1] + (ph - ph * k) * 0.5, uv[1] + (ph + ph * k) * 0.5),
                        }
                    }
                    // Pixel art stays pixels however a group enlarges it.
                    let sharp = tip.is_pixel_art(image.0 as usize);
                    let at = self.element_count();
                    self.sprite(d, uv, a, rgb, false, sharp, affine, &clips);
                    // A picture of its own: its number in the second slot, for the
                    // shader and so a new one is a change, and drawn with it bound.
                    if slot.page > 0 && self.element_count() > at {
                        self.elements[at * PER_ELEMENT + 1] = slot.page as f32;
                        self.picture_marks.push((at as u32, slot.page));
                    }
                }
                Instr::Window { slot, target, alpha, ask } => {
                    let a = alpha.eval(c).clamp(0.0, 1.0) * mult;
                    if a <= 0.001 {
                        continue;
                    }
                    let Some(Some(tex)) = self.window_tex.get(*slot).cloned() else { continue };
                    // Scaled as much as its box is from the size it was asked to
                    // have, from its corner: a window that could not be that
                    // small —a minimum of its own— comes out cut, not squashed.
                    let b = [target.0.eval(c), target.1.eval(c), target.2.eval(c).max(1.0), target.3.eval(c).max(1.0)];
                    // Asked for nothing (`ask: 0, 0`, the size it chooses): as it is.
                    // A picture of it (`ask: -1, -1`): the whole of it, into its box.
                    let g = tex.geometry;
                    let (ax, ay) = (ask.0.eval(c), ask.1.eval(c));
                    let picture = ax < 0.0 && ay < 0.0;
                    let fit = |box_: f32, asked: f32, real: f32| if asked < 0.0 { box_ / real.max(1.0) } else if asked < 0.5 { 1.0 } else { box_ / asked };
                    let (sx, sy) = (fit(b[2], ax, g[2]), fit(b[3], ay, g[3]));
                    let d = [b[0], b[1], (g[2] * sx).max(1.0), (g[3] * sy).max(1.0)];
                    // (A picture is not where the mouse reaches the window: the copy
                    // that lays it out is.)
                    if !picture {
                        self.windows_drawn.push((*slot, d, affine));
                    }
                    // Each piece in its place, and only what falls inside the box is painted.
                    for (layer, uv, r, opaque) in &tex.pieces {
                        let p = [b[0] + (r[0] - g[0]) * sx, b[1] + (r[1] - g[1]) * sy, r[2] * sx, r[3] * sy];
                        let x0 = p[0].max(b[0]);
                        let y0 = p[1].max(b[1]);
                        let x1 = (p[0] + p[2]).min(b[0] + b[2]);
                        let y1 = (p[1] + p[3]).min(b[1] + b[3]);
                        if x1 <= x0 || y1 <= y0 {
                            continue;
                        }
                        let bounds = affine.bounds([x0, y0, x1, y1]);
                        self.element(1.0, bounds, &clips, |e| {
                            affine.encode(&mut e[44..52]);
                            // −1: from the windows' texture, not the atlas (−2 if its
                            // alpha means nothing); which layer, in the second slot.
                            e[1] = *layer as f32;
                            e[2] = if *opaque { -2.0 } else { -1.0 };
                            e[3] = a;
                            e[36..40].copy_from_slice(&p);
                            e[40..44].copy_from_slice(uv);
                        });
                    }
                }
                Instr::Shader { shader, target, corner, alpha, values, colors, time, pointer, behind } => {
                    let a = alpha.eval(c).clamp(0.0, 1.0) * mult;
                    if a <= 0.001 {
                        continue;
                    }
                    let d = [target.0.eval(c), target.1.eval(c), target.2.eval(c).max(0.0), target.3.eval(c).max(0.0)];
                    let b = affine.bounds([d[0], d[1], d[0] + d[2], d[1] + d[3]]);
                    // What is behind it is captured like a lens's: its box.
                    if *behind {
                        self.glass_regions.push((b, Behind::Read));
                        self.glass_marks.push(self.element_count() as u32);
                    }
                    let mut v = [0f32; 8];
                    for (slot, e) in v.iter_mut().zip(values) {
                        *slot = e.eval(c);
                    }
                    let tones: Vec<[f32; 3]> = colors.iter().map(&color).collect();
                    let t = time.as_ref().map_or(0.0, |e| e.eval(c));
                    let (px, py) = pointer.as_ref().map_or((-1e6, -1e6), |(x, y)| (x.eval(c), y.eval(c)));
                    let over = (px >= d[0] && px <= d[0] + d[2] && py >= d[1] && py <= d[1] + d[3]) as u8 as f32;
                    let corner = corner.eval(c).max(0.0);
                    let emitted_before = self.elements.len();
                    self.element(3.0, [b[0] - 1.0, b[1] - 1.0, b[2] + 1.0, b[3] + 1.0], &clips, |e| {
                        affine.encode(&mut e[44..52]);
                        e[1] = *shader as f32;
                        e[3] = a;
                        if let Some(k) = tones.first() {
                            e[8..11].copy_from_slice(k);
                            e[11] = 1.0;
                        }
                        if let Some(k) = tones.get(1) {
                            e[12..15].copy_from_slice(k);
                            e[15] = 1.0;
                        }
                        e[16..24].copy_from_slice(&v);
                        e[24..28].copy_from_slice(&[t, px, py, over]);
                        e[36..40].copy_from_slice(&d);
                        e[40] = corner;
                    });
                    // Its clock goes on only if it is really painted: one that falls on no
                    // surface —in a `screens: each` copy with no monitor, whose `show:`
                    // reads `screen.index`— kept the whole scene awake for nothing.
                    if self.elements.len() > emitted_before {
                        self.timed |= time.is_some();
                    }
                }
                Instr::Field { text, zone, at, width, style, alpha, placeholder, selection, secret, passive } => {
                    let k = text.0 as usize;
                    let real = texts.get(k).map_or("", String::as_str);
                    let empty = real.is_empty();
                    // When secret, what gets painted are dots: one per letter.
                    let dots = if *secret { MASK_DOT.repeat(real.chars().count()) } else { String::new() };
                    let value = if *secret { dots.as_str() } else { real };
                    // When empty, it shows what is expected of it, fainter.
                    let placeholder = content_text(placeholder, c, texts);
                    let m = tip.layout(idx, LayoutKey::new(if empty { &placeholder } else { value }, style, None));
                    let (x0, y0, w) = (at.0.eval(c), at.1.eval(c), width.eval(c));
                    let h = style.px * style.line_height;
                    let mine = field.filter(|v| v.text == k);
                    // The cursor counts bytes of the real text; in dots, they are letters times three.
                    let x_at_byte = |b: usize| {
                        let b = if *secret { real[..b.min(real.len())].chars().count() * MASK_DOT.len() } else { b };
                        if empty { 0.0 } else { m.as_ref().map_or(0.0, |m| m.x_at_byte(b)) }
                    };
                    // If the cursor goes out on the right, the text scrolls.
                    let scroll = mine.map_or(0.0, |v| (x_at_byte(v.cursor) - w + 6.0).max(0.0));
                    self.fields.push(PlacedField { text: k, zone, layout: if empty { None } else { m.clone() }, x0, scroll, secret: *secret, passive: *passive });
                    let a = alpha.eval(c).clamp(0.0, 1.0) * mult;
                    if a <= 0.001 {
                        continue;
                    }
                    // Clipped to its box: what does not fit is not seen.
                    let mut rect = crate::shapes::FlatShape { kind: 1, cx: x0 + w * 0.5, cy: y0 + h * 0.5, mx: w * 0.5, my: h * 0.5 + 2.0, radius: 0.0, rotation: 0.0, ex: 1.0, ey: 1.0, stroke: 0.0, affine };
                    let limit = rect.bounds().unwrap_or([0.0; 4]);
                    let kf = self.push_shape(rect, 0.0);
                    clips.push((kf, limit));
                    let rgb = color(&style.color);
                    if let Some(v) = mine {
                        let (s0, s1) = (x_at_byte(v.cursor.min(v.anchor)), x_at_byte(v.cursor.max(v.anchor)));
                        if s1 > s0 {
                            rect = crate::shapes::FlatShape { cx: x0 - scroll + (s0 + s1) * 0.5, mx: (s1 - s0) * 0.5, my: h * 0.5, ..rect };
                            let (kf, b) = (self.push_shape(rect, 0.0), rect.bounds().unwrap_or([0.0; 4]));
                            let sel = color(selection);
                            self.element(0.0, b, &clips, |e| {
                                affine.encode(&mut e[44..52]);
                                e[1] = kf as f32;
                                e[2] = 1.0;
                                e[3] = a;
                                e[8..11].copy_from_slice(&sel);
                            });
                        }
                    }
                    if let Some(m) = &m {
                        let s = tip_scale;
                        let (tx, ty) = (((x0 - scroll) * s).round() / s, (y0 * s).round() / s);
                        for g in &m.glyphs {
                            let d = [tx + g.rect[0], ty + g.rect[1], g.rect[2], g.rect[3]];
                            self.sprite(d, g.uv, if empty { a * 0.4 } else { a }, if g.colored { None } else { Some(rgb) }, true, false, affine, &clips);
                        }
                    }
                    if let Some(v) = mine.filter(|v| v.visible) {
                        rect = crate::shapes::FlatShape { cx: x0 - scroll + x_at_byte(v.cursor) + 0.5, mx: 0.8, my: h * 0.5 - 1.0, ..rect };
                        let (kf, b) = (self.push_shape(rect, 0.0), rect.bounds().unwrap_or([0.0; 4]));
                        self.element(0.0, [b[0] - 1.0, b[1], b[2] + 1.0, b[3]], &clips, |e| {
                            affine.encode(&mut e[44..52]);
                            e[1] = kf as f32;
                            e[2] = 1.0;
                            e[3] = a;
                            e[8..11].copy_from_slice(&rgb);
                        });
                    }
                    clips.pop();
                }
                Instr::TextFx(fx) => text_fx = Some(fx),
                Instr::Selectable(col) => text_select = Some(col),
                Instr::Text { content, at, anchor, width, style, alpha, measure } => {
                    let fx = text_fx.take();
                    let select = text_select.take();
                    let text = content_text(content, c, texts);
                    // With `markdown:`, its marks go; everything from here on reads it without them.
                    let (text, key) = if style.markdown {
                        let (plain, marks) = crate::markdown::plain(&text);
                        (std::borrow::Cow::Owned(plain.clone()), LayoutKey::marked(plain, marks, style, width.as_ref().map(|w| w.eval(c))))
                    } else {
                        let key = LayoutKey::new(&text, style, width.as_ref().map(|w| w.eval(c)));
                        (text, key)
                    };
                    // A size that changes, at the value it has now (whole quarter
                    // pixels: a size that glides does not shape a new layout each frame).
                    let key = match &style.size {
                        Some(e) => key.sized((e.eval(c) * 4.0).round().max(4.0) / 4.0),
                        None => key,
                    };
                    let key = match &style.weight_of {
                        Some(e) => key.weighted((e.eval(c) / 100.0).round().clamp(1.0, 9.0) as u16 * 100),
                        None => key,
                    };
                    // How many lines, if it changes (`lines: if(open, 12, 2)`): whole ones, and
                    // none or fewer is as many as it has.
                    let key = match &style.lines_of {
                        Some(e) => key.lined(Some(e.eval(c).round()).filter(|n| *n >= 1.0).map(|n| n.min(10_000.0) as usize)),
                        None => key,
                    };
                    let text: &str = &text;
                    // It is ordered even if not visible: so that when it appears, it is already there.
                    let Some(m) = tip.layout(idx, key) else { continue };
                    if let Some((w, h)) = measure {
                        self.measurements.push((*w, m.size.0));
                        self.measurements.push((*h, m.size.1));
                    }
                    let a = alpha.eval(c).clamp(0.0, 1.0) * mult;
                    if a <= 0.001 {
                        continue;
                    }
                    let rgb = color(&style.color);
                    // On real pixels: a text at half a pixel comes out soft.
                    let s = tip_scale;
                    let x0 = ((at.0.eval(c) - m.size.0 * anchor.0) * s).round() / s;
                    let y0 = ((at.1.eval(c) - m.size.1 * anchor.1) * s).round() / s;
                    if self.collect_texts {
                        let b = affine.bounds([x0, y0, x0 + m.size.0, y0 + m.size.1]);
                        let limit = clips.iter().map(|k| k.1).reduce(|a, b| [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]);
                        // Cut out by a clip, it is not seen: a row scrolled out of its list.
                        let seen = limit.is_none_or(|l| b[0] < l[2] && b[2] > l[0] && b[1] < l[3] && b[3] > l[1]);
                        if !text.trim().is_empty() {
                            self.texts_seen.push(SeenText { at: idx, text: text.to_owned(), bounds: b, clipped: !seen });
                        }
                    }
                    if let Some(sel) = select {
                        let limit = clips.iter().map(|k| k.1).reduce(|a, b| [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]);
                        // What is selected of it, behind its letters.
                        if let Some((_, from, to)) = self.selected.filter(|k| k.0 == idx) {
                            let sel = color(sel);
                            for b in m.spans(from.min(to), to.max(from)) {
                                let rect = crate::shapes::FlatShape { kind: 1, cx: x0 + (b[0] + b[2]) * 0.5, cy: y0 + (b[1] + b[3]) * 0.5, mx: (b[2] - b[0]) * 0.5, my: (b[3] - b[1]) * 0.5, radius: 2.0, rotation: 0.0, ex: 1.0, ey: 1.0, stroke: 0.0, affine };
                                let (kf, bounds) = (self.push_shape(rect, 0.0), rect.bounds().unwrap_or([0.0; 4]));
                                self.element(0.0, bounds, &clips, |e| {
                                    affine.encode(&mut e[44..52]);
                                    e[1] = kf as f32;
                                    e[2] = 1.0;
                                    e[3] = a * 0.85;
                                    e[8..11].copy_from_slice(&sel);
                                });
                            }
                        }
                        self.selectable.push(PlacedText { at: idx, text: text.to_owned(), layout: m.clone(), origin: (x0, y0), affine, limit });
                    }
                    let Some(fx) = fx else {
                        for g in &m.glyphs {
                            let d = [x0 + g.rect[0], y0 + g.rect[1], g.rect[2], g.rect[3]];
                            self.sprite(d, g.uv, a, if g.colored { None } else { Some(rgb) }, true, false, affine, &clips);
                        }
                        continue;
                    };
                    // With effects: what is the same for every letter, worked out once.
                    let mut looks = [0f32; 20];
                    if let Some(Paint::Gradient { radial, from, to, stops }) = &fx.gradient {
                        looks[0] = if *radial { 2.0 } else { 1.0 };
                        looks[1..5].copy_from_slice(&[from.0.eval(c), from.1.eval(c), to.0.eval(c), to.1.eval(c)]);
                        looks[5] = (placed_stops.len() / 4) as f32;
                        looks[6] = stops.len() as f32;
                        for (at, col) in stops {
                            let k = color(col);
                            placed_stops.extend_from_slice(&[k[0], k[1], k[2], at.eval(c).clamp(0.0, 1.0)]);
                        }
                    }
                    if let Some((w, col)) = &fx.outline {
                        looks[7] = w.eval(c).max(0.0);
                        looks[8..11].copy_from_slice(&color(col));
                    }
                    if let Some(sh) = &fx.shadow {
                        looks[11..15].copy_from_slice(&[sh.offset.0.eval(c), sh.offset.1.eval(c), sh.blur.eval(c).max(0.0), sh.alpha.eval(c).clamp(0.0, 1.0)]);
                        if let Some(col) = &sh.color {
                            looks[15..18].copy_from_slice(&color(col));
                        }
                    }
                    let pad = looks[7].max(looks[11].abs().max(looks[12].abs()) + looks[13]) + 1.0;
                    let count = m.glyphs.len() as f32;
                    for (k, g) in m.glyphs.iter().enumerate() {
                        // Each letter on its own: `letter` and `letters` are these two while it is worked out.
                        LETTER.with(|l| l.set((k as f32, count)));
                        let (dx, dy) = fx.letter_move.as_ref().map_or((0.0, 0.0), |(x, y)| (x.eval(c), y.eval(c)));
                        let opacity = fx.letter_opacity.as_ref().map_or(1.0, |e| e.eval(c).clamp(0.0, 1.0));
                        let scale = fx.letter_scale.as_ref().map_or(1.0, |e| e.eval(c).max(0.0));
                        let (w, h) = (g.rect[2] * scale, g.rect[3] * scale);
                        let (cxg, cyg) = (x0 + g.rect[0] + g.rect[2] * 0.5 + dx, y0 + g.rect[1] + g.rect[3] * 0.5 + dy);
                        let d = [cxg - w * 0.5, cyg - h * 0.5, w, h];
                        if a * opacity > 0.001 {
                            self.sprite_with_effects(d, g.uv, a * opacity, rgb, g.colored, &looks, pad, affine, &clips);
                        }
                    }
                    LETTER.with(|l| l.set((0.0, 0.0)));
                }
                Instr::Clip(Some((shape, margin))) => {
                    let p = flatten(shape, &transforms, &mut self.points).shrink(*margin);
                    let bounds = p.bounds().unwrap_or([0.0; 4]);
                    let k = self.push_shape(p, 0.0);
                    clips.push((k, [bounds[0] - 1.0, bounds[1] - 1.0, bounds[2] + 1.0, bounds[3] + 1.0]));
                    if clips.len() == 5 && !std::mem::replace(&mut self.clips_warned, true) {
                        eprintln!("render · more than four nested clips: the outer ones clip by their box only, not by their shape");
                    }
                }
                Instr::Clip(None) => {
                    clips.pop();
                }
                Instr::Transform(Some(t)) => {
                    let own = t.affine(c);
                    transforms.push(affine.mul(own));
                }
                Instr::Transform(None) => {
                    transforms.pop();
                }
            }
        }
        if let Some(o) = open_stretch.take() {
            let closed = body.is_none() && text_fx.is_none() && text_select.is_none();
            self.keep(o, c, tip, placed_stops.len(), (transforms.len(), clips.len(), opacity_groups.len()), closed, sequence);
        }
        // What the whole drawing read: each stretch's, the copied ones', and what is between them.
        if let Some(seen) = c.seen {
            seen.pass_to(&self.reads);
            seen.take_from(&self.reads);
        }
        self.copied = (copied, made);
        self.plan = plan;
        for message in [self.shadow.end_frame(true), self.clipping.end_frame(false)].into_iter().flatten() {
            eprintln!("{message}");
        }
        // The silhouettes nobody has used this frame are forgotten; the others
        // start the count again.
        self.glass_cache.retain(|_, (_, used)| std::mem::take(used));
        self.share_layers();
        if hud {
            // The instruments take the bottom strip, which was added for them.
            self.element(9.0, [0.0, size.1 - HUD_HEIGHT, size.0, size.1], &[], |e| Affine::IDENTITY.encode(&mut e[44..52]));
        }
        self.stops = placed_stops;
        // An empty store can be neither bound nor written.
        if self.stops.is_empty() {
            self.stops.resize(4, 0.0);
        }
        if self.points.is_empty() {
            self.points.resize(2, 0.0);
        }
        if self.shapes.is_empty() {
            self.shapes.resize(PER_SHAPE, 0.0);
            self.shape_bounds.push([0.0; 4]);
        }
        if self.elements.is_empty() {
            self.elements.resize(PER_ELEMENT, 0.0);
        }
        tip.end_frame();
    }
}

// ── the device and the sheets ───────────────────────────────────

/// Where a sheet's frames go.
pub enum Target {
    /// A window of the system's compositor: wgpu's own swapchain.
    Surface(wgpu::Surface<'static>),
    /// A monitor driven by the platform itself (pleamar-wm's own session): it
    /// lends a texture to paint each frame in, and shows it once painted.
    Frames(Box<dyn Frames>),
}

/// Frames the platform shows itself. The textures are made on the render's
/// card, so they are asked for with it at hand.
pub trait Frames: Send {
    /// A texture to paint the next frame in, BGRA and the monitor's size, and
    /// which one it is; none if they are all still on screen or on their way.
    /// `modifiers`: the layouts of the card's memory it can paint in.
    fn acquire(&mut self, device: &wgpu::Device, modifiers: &[u64]) -> Option<(usize, wgpu::Texture)>;
    /// That one is painted, in that work: to the monitor once it is done.
    /// `changed`: the piece of it that may differ from the frame presented
    /// before, in its pixels (x, y, w, h); none if all of it may.
    fn present(&mut self, which: usize, done: wgpu::SubmissionIndex, device: &wgpu::Device, queue: &wgpu::Queue, changed: Option<[u32; 4]>);
    /// The same, told each piece that may differ instead of one box round
    /// them all: a bar lit at one end and at the other is not the whole screen.
    fn present_pieces(&mut self, which: usize, done: wgpu::SubmissionIndex, device: &wgpu::Device, queue: &wgpu::Queue, pieces: Option<&[[u32; 4]]>) {
        let round = pieces.map(|c| {
            c.iter().fold(None, |u: Option<[u32; 4]>, b| {
                Some(u.map_or(*b, |u| {
                    let (x0, y0) = (u[0].min(b[0]), u[1].min(b[1]));
                    [x0, y0, (u[0] + u[2]).max(b[0] + b[2]) - x0, (u[1] + u[3]).max(b[1] + b[3]) - y0]
                }))
            }).unwrap_or([0, 0, 0, 0])
        });
        self.present(which, done, device, queue, round);
    }
    /// Whether these frames are a whole monitor of the platform's own
    /// (pleamar-wm): nothing is behind them but what was painted before. A
    /// surface handed to another compositor has its desktop behind.
    fn whole_screen(&self) -> bool {
        true
    }
    /// The surface measures that many pixels now: frames of that size from here on.
    fn resize(&mut self, _px: (u32, u32)) {}
    /// Until the frame presented last has reached the platform: what is
    /// asked of it after —a capture of the screen with that frame on it—
    /// comes after it.
    fn settle(&mut self) {}
    /// They cannot be made (the card or the compositor refused them): the
    /// surface goes back to the system's swapchain.
    fn gave_up(&self) -> bool {
        false
    }
}

/// What the platform hands the render thread for each surface.
pub struct NewSheet {
    pub id: u32,
    pub target: Target,
    pub window: Box<dyn PlatformWindow>,
    pub scale: f32,
    /// The logical size the compositor has given it.
    pub size: (u32, u32),
    /// Millihertz of the monitor; 0 if unknown.
    pub mhz: i32,
    pub name: String,
    pub view: View,
}

/// Which piece of the scene plane a sheet shows, and whose it is. Each surface
/// —and each popup— looks at a different place of the same plane.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Which surface of the scene it is.
    pub surface: usize,
    /// And if it is a popup, which one.
    pub popup: Option<usize>,
    pub origin: (f32, f32),
    pub size: (f32, f32),
}

impl View {
    pub fn bounds(&self) -> [f32; 4] {
        [self.origin.0, self.origin.1, self.origin.0 + self.size.0, self.origin.1 + self.size.1]
    }
}

/// The capture of what is behind that a sheet with a lens has on the way. To
/// unmix the background one has to know **exactly** what we had painted
/// when it was taken: with the wrong canvas the error gets multiplied by
/// α / (1 − α) on each round —×6 with the glass at 86 %— and the lens fills
/// with colours that do not exist. So after presenting the capture is
/// requested, and until it arrives no other frame is presented: it comes the
/// next time the compositor paints, which already carries our frame. And with
/// nothing to paint it watches: the capture arrives when something changes
/// behind, and if something has to be painted before that, it is forgotten.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackdropCapture {
    Idle,
    AfterPresent,
    Watching,
}

/// A Wayland surface as seen from the GPU: its chain of images, at its scale,
/// with its layers and its uniforms.
pub struct Sheet {
    pub id: u32,
    /// For a canvas whose contents are known —lent frames, the lens's two,
    /// the one kept—: a count of the frames painted, which one each canvas
    /// last got, and what changed in the last ones (their boxes, in pixels; `None`, all).
    /// `known_as`: which kind of canvas that is about, and at what size.
    frame_no: u64,
    painted_as: Vec<Option<u64>>,
    damage_log: std::collections::VecDeque<(u64, Option<Vec<[f32; 4]>>)>,
    known_as: (u8, (u32, u32)),
    /// On a surface without a lens: the canvas it is painted on, which stays
    /// from frame to frame and is copied to the screen, so only what changed
    /// is painted again. And how many frames it has gone without that.
    kept: Option<(wgpu::Texture, wgpu::TextureView)>,
    idle_kept_frames: u32,
    pub name: String,
    pub mhz: i32,
    pub scale: f32,
    /// The one that waits for the screen and sets the pace; the others do not block.
    pub drives_pace: bool,
    /// Whether its surface is open now. A closed one does not set the pace
    /// and is not painted more than once, to clear it: the compositor gives no
    /// frames to what is not visible, and waiting for them with vsync stopped
    /// the whole render.
    pub open: bool,
    /// Open, but the compositor stopped answering its frames (covered by
    /// another surface, a lock's blank): it does not set the pace while so.
    pub unseen: bool,
    /// Rounds in a row its last frame went unanswered, waiting for the
    /// screen (`blocks_on_screen`): at three, it is `unseen`.
    pub missed: u32,
    pub cleared: bool,
    pub view: View,
    target: Target,
    /// A surface painted in frames of our own keeps the system's swapchain
    /// unused, to go back to if those cannot be made.
    spare: Option<wgpu::Surface<'static>>,
    window: Box<dyn PlatformWindow>,
    px: (u32, u32),
    uniforms: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
    layer_views: Vec<wgpu::TextureView>,
    layer_group: wgpu::BindGroup,
    /// How many layers it has of the surface's size (0: only the dummy one),
    /// and how many frames it has gone without using any.
    layers: u32,
    idle_layer_frames: u32,
    /// The last thing the compositor was asked to blur, in its coordinates.
    pub blur_rects: Vec<[i32; 4]>,
    /// If it shows glass and what is behind can be seen: its canvas and its background.
    pub lens: Option<crate::lens::Lens>,
    /// What it painted itself under the glass: all there is behind it on a
    /// monitor of pleamar-wm's own, and on a surface what a glass with
    /// something of the scene's under it sees before the background.
    pub own: Option<crate::lens::Own>,
    /// How many frames it has gone without a glass over anything of its own.
    idle_own_frames: u32,
    /// Whether this frame has glass in its piece of the plane.
    pub wants_lens: bool,
    /// Which capture of what is behind is on the way, and since when.
    pub capture: BackdropCapture,
    /// Where there is glass in its piece, with a margin for frosting —in its
    /// own logical pixels—, and the box of the capture that is on the way.
    pub glass_box: Option<[i32; 4]>,
    pub asked_box: [i32; 4],
    pub capture_asked: std::time::Instant,
    /// When it was last painted, for its own `rate:` (it skips the rounds in between).
    pub rate_last: Option<std::time::Instant>,
    /// The last capture: when one was asked after presenting, or when one
    /// that was watching arrived. It sets the pace of both kinds (`lens::Pace`).
    pub capture_taken: std::time::Instant,
    /// It painted —or was held— while it could not capture: what is behind may
    /// have changed unseen, so once the interval is over it captures without
    /// waiting for a change.
    pub capture_owed: bool,
    /// The capture on the way is one of those, asked without presenting: if the
    /// compositor does not answer it, the sheet presents again (see `nudge`).
    pub capture_forced: bool,
    /// What changed in its piece of the plane since it was last painted that
    /// the round painting it will not say: rounds it skipped —waiting for a
    /// capture, or for the compositor to let go of a buffer— and what is
    /// behind its glass, when a capture comes in. Boxes in the scene's plane.
    pub owed_damage: Vec<[f32; 4]>,
    /// Whether it has been painted in this round of the render.
    pub painted_now: bool,
    /// Which piece of the plane and at which scale it has painted, if what it
    /// shows is up to date. With another place, another scale or nothing (just
    /// made, reconfigured), it is painted even if the scene has not changed.
    pub painted: Option<([f32; 4], f32)>,
    /// The input region last given to the compositor, in its coordinates.
    pub input_region: Vec<[i32; 4]>,
    /// Whether it has presented a frame yet. Before that its region is not
    /// sent: sending it is a commit, and a commit with no buffer yet is a
    /// protocol error for a lock screen (ext-session-lock: `null_buffer`).
    pub presented: bool,
    /// The keyboard this surface last asked the compositor for.
    pub keyboard_mode: Option<Keyboard>,
}

/// A moment far enough back that «the last capture» never counts against the
/// first one. Starting at «now», a sheet that painted once and then stayed still
/// —a glass or a shader that does not move— skipped its first capture for being
/// too soon after the last, and never saw what was behind it.
fn long_ago() -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_sub(std::time::Duration::from_secs(10)).unwrap_or(now)
}

pub struct Gpu {
    /// The OpenGL instance, when the card runs on it (see `new`): it has to
    /// outlive the device.
    _gl_instance: Option<wgpu::Instance>,
    /// What unmixes and frosts what is behind the glass.
    lens: crate::lens::Pipelines,
    /// The scene's blur (`blur.rs`), built, and the numbers it is given this frame.
    blur: crate::blur::Built,
    pub blur_sampler: wgpu::Sampler,
    pub blur_values: [f32; 8],
    /// For a sheet without a lens: a one-pixel background nobody reads.
    no_backdrop_group: wgpu::BindGroup,
    /// Whether the screen accepts having a canvas copied to it: without that, there is no lens.
    can_copy: bool,
    /// Some surface is painted in frames of our own: presenting never waits,
    /// so the render sets the pace as with mailbox.
    own_frames: std::cell::Cell<bool>,
    adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    alpha: wgpu::CompositeAlphaMode,
    non_blocking: Option<wgpu::PresentMode>,
    pipeline: wgpu::RenderPipeline,
    /// The particles: the same shader and layout, other entry points, one
    /// instance per particle.
    particles: wgpu::RenderPipeline,
    /// The same elements pipeline with another blend: a group with `mode:
    /// screen` or `mode: multiply` is drawn with one of these. See `BLENDS`.
    screen: wgpu::RenderPipeline,
    multiply: wgpu::RenderPipeline,
    /// Transparent over whatever is there, blending nothing: what empties the
    /// piece painted again before painting it (a clear ignores the scissor).
    erase: wgpu::RenderPipeline,
    pipeline_layout: wgpu::PipelineLayout,
    /// The scene's own shaders, as they went into `pipeline`.
    user_code: String,
    /// The picture the lenses bend instead of a capture (`lens = image "file"`), and whether `pipeline` was built for it.
    lens_image: std::cell::RefCell<Option<crate::lens::FixedSource>>,
    built_for_image: bool,
    /// Where the versions of the pictures start: a new file must not repeat the numbers of the one before.
    lens_epoch: u64,
    shapes_buffer: wgpu::Buffer,
    points_buffer: wgpu::Buffer,
    stops_buffer: wgpu::Buffer,
    elements_buffer: wgpu::Buffer,
    atlas: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    /// The windows of the scene's compositor, one per layer: width, height and layers.
    windows: wgpu::Texture,
    windows_view: wgpu::TextureView,
    windows_dims: (u32, u32, u32),
    /// The programs' buffers already read from their memory on the card, by
    /// buffer: a program cycles through two or three, and each is imported once.
    #[cfg(target_os = "linux")]
    dmabufs: std::collections::HashMap<u64, wgpu::Texture>,
    /// Copies of programs' frames waiting for the next painting: which
    /// buffer, to which layer, how much.
    #[cfg(target_os = "linux")]
    /// Each copy: which buffer, into which layer, how big, and whether it is
    /// video (painted as RGB instead of copied).
    copies: std::cell::RefCell<Vec<(u64, u32, wgpu::Extent3d, bool)>>,
    #[cfg(target_os = "linux")]
    yuv: Option<crate::dmabuf::YuvToRgb>,
    /// The buffers live windows are copied into on the card (`thumbnails.live`),
    /// by the ticket the thumbnails service knows each by: made here, or
    /// another render's read from its fd.
    #[cfg(target_os = "linux")]
    thumbs: std::collections::HashMap<u64, wgpu::Texture>,
    #[cfg(target_os = "linux")]
    fit: Option<crate::dmabuf::FitFrame>,
    #[cfg(target_os = "linux")]
    hash: Option<crate::dmabuf::HashFrame>,
    /// Where the sums go to be waited for: a thread of their own, so the
    /// render never waits for the card to add a frame up.
    #[cfg(target_os = "linux")]
    summer: Option<std::sync::mpsc::Sender<Summing>>,
    last_submission: std::cell::RefCell<Option<Sent>>,
    /// The layouts of the card's memory it can paint BGRA in, for a platform
    /// that lends its own textures (a monitor driven directly).
    render_modifiers: Vec<u64>,
    sampler: wgpu::Sampler,
    /// How many floats fit now in each store, and how many at most on this card.
    capacity: (usize, usize, usize, usize),
    limit: usize,
    limit_warned: bool,
    scene_group: wgpu::BindGroup,
    no_layers_group: wgpu::BindGroup,
    no_picture_view: wgpu::TextureView,
    /// Pictures too big for the atlas, each a texture of its own, by number:
    /// it, its view, and the scene's group with it as binding 7.
    pictures: std::collections::HashMap<u32, (wgpu::Texture, wgpu::TextureView, wgpu::BindGroup)>,
}

/// The card the compositor paints with, by its PCI address (`0000:01:00.0`), if
/// it has said (see `compositor_card`).
static COMPOSITOR_CARD: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The compositor says which card it paints with (`linux-dmabuf`'s main
/// device, a `dev_t`): ours has to be that one. Our frames are handed to it as
/// memory on our card, and on a laptop with two, painted on the frugal one
/// (an AMD inside the processor) for a compositor running on the NVIDIA, it
/// could not read them: Marea was a black rectangle (marea-plm issue 4).
#[cfg(target_os = "linux")]
pub fn compositor_card(dev: u64) {
    let dev = dev as libc::dev_t;
    let path = format!("/sys/dev/char/{}:{}/device", libc::major(dev), libc::minor(dev));
    if let Some(bus) = std::fs::canonicalize(path).ok().and_then(|p| Some(p.file_name()?.to_str()?.to_owned())) {
        println!("render · the compositor paints on the card at {bus}");
        let _ = COMPOSITOR_CARD.set(bus);
    }
}

impl Gpu {
    /// It is created with the first surface that arrives: one is needed to know
    /// which adapter and which format are valid.
    /// With the first sheet's surface, if it has one: the card chosen has to be
    /// able to show on it. Without —a monitor driven by the platform itself—,
    /// the card is whatever there is.
    pub fn new(instance: &wgpu::Instance, first: Option<&wgpu::Surface<'static>>) -> Gpu {
        // `PLEAMAR_GPU=high` asks for the fast card (a laptop's discrete one) instead of the frugal one.
        let asked = std::env::var("PLEAMAR_GPU").ok();
        let power_preference = match asked.as_deref() {
            Some("high") => wgpu::PowerPreference::HighPerformance,
            _ => wgpu::PowerPreference::LowPower,
        };
        // Without anyone asking, the compositor's own card, if it has said which.
        let compositors = first.filter(|_| asked.is_none()).zip(COMPOSITOR_CARD.get()).and_then(|(surface, bus)| {
            pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY)).into_iter().find(|a| a.get_info().device_pci_bus_id == *bus && a.is_surface_supported(surface))
        });
        let primary = match compositors {
            Some(a) => Some(a),
            None => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { compatible_surface: first, power_preference, ..Default::default() })).ok(),
        };
        // A compositor (with no surface of its own: it hands its frames to the
        // monitors itself) has to read other programs' buffers on the card and
        // paint into its monitors' own; Vulkan does that only with
        // VK_EXT_image_drm_format_modifier, which AMD's GFX6 to GFX8 cards (an
        // RX 580) lack until Linux 7.3 and Mesa 26.3. Their OpenGL driver does
        // it through EGL, so then the compositor runs on OpenGL. Programs keep
        // Vulkan: they do not need it. `PLEAMAR_BACKEND=gl` forces it,
        // `PLEAMAR_BACKEND=vulkan` never. (prehonor's idea, pleamar-wm issue 2.)
        let backend = std::env::var("PLEAMAR_BACKEND").unwrap_or_default();
        let lacks = primary.as_ref().is_none_or(|a| a.get_info().backend == wgpu::Backend::Vulkan && !a.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF));
        let gl = first.is_none() && backend != "vulkan" && (backend == "gl" || lacks);
        // (Its instance is kept with the device: dropped, its context went
        // with it, and the device was lost before it was made.)
        let gl_instance = gl.then(|| wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::GL, ..wgpu::InstanceDescriptor::new_without_display_handle() }));
        let gl_adapter = gl_instance.as_ref().and_then(|i| pollster::block_on(i.request_adapter(&wgpu::RequestAdapterOptions { compatible_surface: None, power_preference, ..Default::default() })).ok());
        if gl && gl_adapter.is_none() {
            println!("render · OpenGL was asked for, and there is none: Vulkan");
        }
        // OpenGL first, if it is wanted; and if its device cannot be made (a
        // driver that gives an OpenGL without compute shaders, as NVIDIA's
        // does here), the card as it is, rather than nothing at all.
        let device_of = |adapter: &wgpu::Adapter| {
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                // By default, wgpu reserves blocks of 128 MB on the card and 64 in
                // system memory, thinking of a game. A whole scene fits in less
                // than 20: with blocks of 8, what is reserved is what is used.
                // Asking for memory is rare here —growing a store, a layer—, so
                // what is lost in speed is not noticed.
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                // To read a program's frames straight from its memory on the
                // card (the scene's `windows`), where the card can.
                // Video frames as decoders hand them over (NV12), if the card reads them.
                required_features: adapter.features() & (wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF | wgpu::Features::TEXTURE_FORMAT_NV12),
                ..Default::default()
            }))
        };
        let on_gl = gl_adapter.and_then(|a| match device_of(&a) {
            Ok(dq) => {
                println!("render · {}: on OpenGL, through EGL (its Vulkan driver cannot take buffers by their layout)", a.get_info().name);
                Some((a, dq))
            }
            Err(e) => {
                println!("render · {}: OpenGL could not be used ({e}): Vulkan", a.get_info().name);
                None
            }
        });
        let (adapter, (device, queue)) = match on_gl {
            Some(x) => x,
            None => {
                let adapter = primary.expect("there is no graphics adapter");
                let dq = device_of(&adapter).expect("there is no device");
                (adapter, dq)
            }
        };
        // The card out of memory —a texture too big for what it has free— is
        // said and lived through: what asked for it is not drawn. wgpu's own
        // answer is to panic, and a window manager's whole session went down
        // with it. Every other error still stops the program, as it did.
        device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| match e {
            wgpu::Error::OutOfMemory { .. } => eprintln!("render · the card is out of memory: {e}"),
            other => panic!("wgpu error: {other}"),
        }));
        let (format, alpha, non_blocking) = match first {
            Some(first) => {
                let caps = first.get_capabilities(&adapter);
                // No sRGB: the compositor blends the bytes as they are, and premultiplied
                // alpha only works out if nobody re-encodes them along the way.
                // And eight bits a channel before anything else: a float format
                // (`Rgba16Float`, first on some AMD cards under Hyprland) is read as
                // linear light, so the sRGB values painted into it came out far too
                // light —a pale pink, #ffadfa, as white (#37)—.
                let eight = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm];
                let format = eight
                    .iter()
                    .copied()
                    .find(|f| caps.formats.contains(f))
                    .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb() && !matches!(f, wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgba32Float)))
                    .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
                    .unwrap_or(caps.formats[0]);
                let alpha = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                    wgpu::CompositeAlphaMode::PreMultiplied
                } else {
                    eprintln!("warning: no premultiplied alpha ({:?}); the background will come out opaque", caps.alpha_modes);
                    wgpu::CompositeAlphaMode::Auto
                };
                let non_blocking = [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate].into_iter().find(|m| caps.present_modes.contains(m));
                (format, alpha, non_blocking)
            }
            // A monitor driven directly: BGRA as the monitor takes it (XRGB8888),
            // and the pace set like with mailbox —the platform says when a frame
            // has been shown—.
            None => (wgpu::TextureFormat::Bgra8Unorm, wgpu::CompositeAlphaMode::Opaque, Some(wgpu::PresentMode::Mailbox)),
        };
        let info = adapter.get_info();
        println!("render · {} ({:?}) · {:?} · {:?}", info.name, info.backend, format, alpha);

        let store = |label, floats: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (floats * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let shapes_buffer = store("shapes", INITIAL_SHAPES * PER_SHAPE);
        let points_buffer = store("points", INITIAL_POINTS);
        let stops_buffer = store("stops", INITIAL_STOPS);
        let elements_buffer = store("elements", INITIAL_ELEMENTS * PER_ELEMENT);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // The layout is written out, not taken from the shader: that way a
        // scene that brings its own shaders gets a new pipeline with the same
        // layout, and every bind group already made keeps being valid for it.
        let pipeline_layout = Self::pipeline_layout(&device);
        let base = crate::shaders::generate(&[]);
        let [pipeline, particles, screen, multiply] = Self::build_pipeline(&device, &pipeline_layout, format, &base, false).expect("pleamar's own shader does not compile");
        // A single atlas for glyphs and images. 2048² in RGBA is 16 MB; it
        // grows taller only when what is seen does not fit (`fit_atlas`).
        let (atlas, atlas_view) = Self::atlas_texture(&device, ATLAS_SIZE);
        let (windows, windows_view) = Self::windows_texture(&device, (1, 1, 1));
        // What binding 7 holds while no picture of its own is drawn.
        let (_, no_picture_view) = Self::picture_texture(&device, (1, 1));
        let scene_group = Self::build_scene_group(&device, &pipeline, &shapes_buffer, &elements_buffer, &points_buffer, &stops_buffer, &atlas_view, &sampler, &windows_view, &no_picture_view);
        let limit = device.limits().max_storage_buffer_binding_size as usize / 4;
        let no_layers_group = Self::make_layers(&device, &pipeline, format, 1, 1, 1).1;
        let erase = Self::erase_pipeline(&device, format);
        let lens = crate::lens::Pipelines::new(&device);
        let blur = crate::blur::Built::new(&device, crate::blur::default_plan(), 1).expect("the default blur builds");
        let blur_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let can_copy = first.is_none_or(|f| f.get_capabilities(&adapter).usages.contains(wgpu::TextureUsages::COPY_DST));
        let nothing = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("no backdrop"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        }).create_view(&Default::default());
        let no_backdrop_group = Self::build_backdrop_group(&device, &pipeline, &nothing, &nothing, &sampler);
        #[allow(unused_mut)]
        let mut g = Gpu { _gl_instance: gl_instance, lens, blur, blur_sampler, blur_values: crate::blur::DEFAULT_VALUES, no_backdrop_group, can_copy, own_frames: Default::default(), adapter, device, queue, format, alpha, non_blocking, pipeline, particles, screen, multiply, erase, pipeline_layout, user_code: base, lens_image: Default::default(), built_for_image: false, lens_epoch: 0, shapes_buffer, elements_buffer, points_buffer, stops_buffer, atlas, atlas_view, windows, windows_view, windows_dims: (1, 1, 1), #[cfg(target_os = "linux")] dmabufs: Default::default(), #[cfg(target_os = "linux")] copies: Default::default(), #[cfg(target_os = "linux")] yuv: None, #[cfg(target_os = "linux")] thumbs: Default::default(), #[cfg(target_os = "linux")] fit: None, #[cfg(target_os = "linux")] hash: None, #[cfg(target_os = "linux")] summer: None, last_submission: Default::default(), render_modifiers: Vec::new(), sampler, capacity: (INITIAL_SHAPES * PER_SHAPE, INITIAL_ELEMENTS * PER_ELEMENT, INITIAL_POINTS, INITIAL_STOPS), limit, limit_warned: false, scene_group, no_layers_group, no_picture_view, pictures: Default::default() };
        #[cfg(target_os = "linux")]
        if first.is_none() {
            g.render_modifiers = g.bgra_modifiers(ash::vk::FormatFeatureFlags::COLOR_ATTACHMENT);
        }
        g
    }

    /// The four groups the shapes shader reads, written out. See `shape.wgsl`.
    fn pipeline_layout(d: &wgpu::Device) -> wgpu::PipelineLayout {
        let both = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: both,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let texture = |binding, view_dimension| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: both,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension, multisampled: false },
            count: None,
        };
        let sampler = |binding| wgpu::BindGroupLayoutEntry { binding, visibility: both, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None };
        let group = |label, entries: &[wgpu::BindGroupLayoutEntry]| d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries });
        let scene = group("scene", &[storage(0), storage(1), texture(2, wgpu::TextureViewDimension::D2), sampler(3), storage(4), storage(5), texture(6, wgpu::TextureViewDimension::D2Array), texture(7, wgpu::TextureViewDimension::D2)]);
        let layers = group("layers", &[texture(0, wgpu::TextureViewDimension::D2Array)]);
        let uniforms = group("surface", &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: both,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        }]);
        let backdrop = group("backdrop", &[texture(0, wgpu::TextureViewDimension::D2), texture(1, wgpu::TextureViewDimension::D2), sampler(2)]);
        d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("elements"), bind_group_layouts: &[Some(&scene), Some(&layers), Some(&uniforms), Some(&backdrop)], immediate_size: 0 })
    }

    fn erase_pipeline(d: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        const SOURCE: &str = "
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }
";
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("erase"), source: wgpu::ShaderSource::Wgsl(SOURCE.into()) });
        d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("erase"),
            layout: None,
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// pleamar's shader with the scene's own ones added. An error comes back as
    /// text instead of bringing the program down: they were checked when the
    /// scene was read, but a driver can still say no.
    fn build_pipeline(d: &wgpu::Device, layout: &wgpu::PipelineLayout, format: wgpu::TextureFormat, extra: &str, picture: bool) -> Result<[wgpu::RenderPipeline; 4], String> {
        let scope = d.push_error_scope(wgpu::ErrorFilter::Validation);
        // With a picture for a backdrop (`lens = image`) the glass is all bent picture: the
        // 12 % that lets what is really behind through is cancelled by subtracting the backdrop, which is
        // only right when the backdrop is what is behind.
        let shape = if picture { include_str!("shape.wgsl").replace("const LENS_ALPHA: f32 = 0.88;", "const LENS_ALPHA: f32 = 1.0;") } else { include_str!("shape.wgsl").to_string() };
        let source = format!("{shape}{extra}");
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("shape"), source: wgpu::ShaderSource::Wgsl(source.into()) });
        let make = |label, vs, fs, blend: wgpu::BlendState| {
            d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState { module: &module, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[] },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let over = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;
        let pipelines = [
            make("elements", "vs", "fs", over),
            make("particles", "vs_particle", "fs_particle", over),
            make("screen", "vs", "fs", BLENDS[0]),
            make("multiply", "vs", "fs", BLENDS[1]),
        ];
        match pollster::block_on(scope.pop()) {
            None => Ok(pipelines),
            Some(e) => Err(e.to_string()),
        }
    }

    /// The blur in use.
    pub fn blur(&self) -> &crate::blur::Built {
        &self.blur
    }

    /// The scene's blur, or the default without one: rebuilt only if it changed.
    /// If the driver refuses it, the one in use stays, which is said once.
    pub fn set_blur(&mut self, plan: Option<&crate::blur::Plan>) {
        let plan = plan.cloned().unwrap_or_else(crate::blur::default_plan);
        if plan == self.blur.plan {
            return;
        }
        match crate::blur::Built::new(&self.device, plan, self.blur.generation + 1) {
            Ok(b) => {
                crate::blur::set_reach(b.plan.reach);
                self.blur = b;
            }
            Err(e) => eprintln!("blur · the scene's blur could not be built, and the one in use stays: {e}"),
        }
    }

    /// The scene's own shaders: the pipeline is only remade if they changed.
    /// If the driver refuses them, pleamar's own goes on and the scene's shaders
    /// paint nothing, which is said once.
    pub fn set_shaders(&mut self, shaders: &[crate::shaders::UserShader]) {
        let code = crate::shaders::generate(shaders);
        let picture = self.lens_image.borrow().is_some();
        if code == self.user_code && picture == self.built_for_image {
            return;
        }
        self.built_for_image = picture;
        match Self::build_pipeline(&self.device, &self.pipeline_layout, self.format, &code, picture) {
            Ok([p, q, sc, mu]) => (self.pipeline, self.particles, self.screen, self.multiply) = (p, q, sc, mu),
            Err(e) => {
                eprintln!("shader · the scene's own shaders could not be built, and they are not painted: {e}");
                if let Ok([p, q, sc, mu]) = Self::build_pipeline(&self.device, &self.pipeline_layout, self.format, &crate::shaders::generate(&[]), picture) {
                    (self.pipeline, self.particles, self.screen, self.multiply) = (p, q, sc, mu);
                }
            }
        }
        self.user_code = code;
    }

    /// The picture the scene's lenses bend (`lens = image "file"`), or none: they capture.
    /// Call it before `set_shaders`, which remakes the pipeline if this changed.
    pub fn set_lens_image(&mut self, path: Option<&std::path::Path>) {
        let image = self.lens_image.get_mut();
        if image.as_ref().map(|s| s.path()) == path {
            return;
        }
        self.lens_epoch = image.as_ref().map_or(self.lens_epoch, |s| s.version());
        *image = path.map(|p| crate::lens::FixedSource::new(p.to_path_buf(), self.lens_epoch));
    }

    fn build_backdrop_group(d: &wgpu::Device, pipeline: &wgpu::RenderPipeline, sharp: &wgpu::TextureView, blurred: &wgpu::TextureView, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
        d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("backdrop"),
            layout: &pipeline.get_bind_group_layout(3),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(sharp) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(blurred) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        })
    }

    /// What the shapes shader of a sheet with a lens reads.
    pub fn backdrop_group(&self, sharp: &wgpu::TextureView, blurred: &wgpu::TextureView) -> wgpu::BindGroup {
        Self::build_backdrop_group(&self.device, &self.pipeline, sharp, blurred, &self.sampler)
    }

    /// A capture of what is behind a sheet arrives. `true` if it has to be painted again.
    pub fn receive_backdrop(&self, l: &mut Sheet, d: crate::platform::Backdrop) -> bool {
        let (scale, bounds) = (l.scale, l.asked_box);
        match &mut l.lens {
            Some(lens) => lens.receive(self, d, scale, bounds),
            None => false,
        }
    }

    /// Uploads to the atlas whatever the workshop has painted since last time.
    /// Whether it presents via mailbox and the render sets the pace with its clock.
    ///
    /// With queue vsync (`Fifo`) asking for the next image waits for the
    /// compositor to release one, and here —Hyprland with NVIDIA— sometimes it
    /// does not release it: four frames after waking up from a long rest, 300
    /// ms stuck in `vkAcquireNextImage` in the middle of an animation. With
    /// mailbox nobody is ever waited for; what is lost is being locked to the
    /// refresh, and that is recovered by setting the pace with absolute
    /// deadlines at the monitor's period.
    /// `PLEAMAR_FIFO=1` goes back to the old way, to compare.
    /// Which card, with which driver, and how frames reach the screen: for a report.
    /// What it keeps of the programs' windows, for the timing lines: a number
    /// that only grows over a long session is a leak.
    pub fn kept(&self) -> String {
        let (w, h, n) = self.windows_dims;
        #[cfg(target_os = "linux")]
        let buffers = self.dmabufs.len();
        #[cfg(not(target_os = "linux"))]
        let buffers = 0;
        format!("{buffers} programs' buffers on the card · windows' texture {w}×{h}×{n}")
    }

    pub fn describe(&self) -> String {
        let i = self.adapter.get_info();
        let kind = match i.device_type {
            wgpu::DeviceType::DiscreteGpu => "discrete",
            wgpu::DeviceType::IntegratedGpu => "integrated",
            wgpu::DeviceType::VirtualGpu => "virtual",
            // llvmpipe, lavapipe: no card at all, the CPU paints.
            wgpu::DeviceType::Cpu => "SOFTWARE (the CPU paints: no GPU driver in use)",
            wgpu::DeviceType::Other => "other",
        };
        let present = if self.uses_mailbox() { "mailbox" } else { "fifo" };
        format!("{} · {:?} · {kind} · driver {} {} · {present}", i.name, i.backend, i.driver, i.driver_info)
    }

    /// Whether handing it a frame waits for the compositor to have shown the
    /// last one: a swapchain presented with vsync (FIFO), which the driver
    /// holds until then —for good, if the compositor never answers—.
    pub fn blocks_on_screen(&self, l: &Sheet) -> bool {
        matches!(l.target, Target::Surface(_)) && ((l.drives_pace && !self.uses_mailbox()) || self.non_blocking.is_none())
    }

    pub fn uses_mailbox(&self) -> bool {
        (self.non_blocking == Some(wgpu::PresentMode::Mailbox) || self.own_frames.get()) && std::env::var_os("PLEAMAR_FIFO").is_none()
    }

    /// What was written and then not painted, to the card all the same: a write
    /// is kept in memory until something is submitted, and this submits nothing else.
    pub fn flush(&self) {
        self.queue.submit(None::<wgpu::CommandBuffer>);
    }

    fn atlas_texture(device: &wgpu::Device, height: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d { width: ATLAS_SIZE, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            // Painted in too: a live window copied on the card is made small into it.
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = atlas.create_view(&Default::default());
        (atlas, view)
    }

    /// The tallest atlas this card allows.
    pub fn atlas_most(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d.min(ATLAS_MOST)
    }

    /// The atlas as tall as the workshop paints it, with what it already had:
    /// the pieces keep their place, and their `uv` is in pixels.
    pub fn fit_atlas(&mut self, height: u32) {
        let have = self.atlas.height();
        if height <= have {
            return;
        }
        let (atlas, view) = Self::atlas_texture(&self.device, height);
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_texture(self.atlas.as_image_copy(), atlas.as_image_copy(), wgpu::Extent3d { width: ATLAS_SIZE, height: have, depth_or_array_layers: 1 });
        self.queue.submit([enc.finish()]);
        self.atlas = atlas;
        self.atlas_view = view;
        self.rebind();
        println!("render · the atlas is now {ATLAS_SIZE}×{height}");
    }

    pub fn upload_atlas(&self, pending_upload: &mut Vec<(AtlasSlot, Vec<u8>)>) {
        for (h, rgba) in pending_upload.drain(..) {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &self.atlas, mip_level: 0, origin: wgpu::Origin3d { x: h.x, y: h.y, z: 0 }, aspect: wgpu::TextureAspect::All },
                &rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * h.width), rows_per_image: None },
                wgpu::Extent3d { width: h.width, height: h.height, depth_or_array_layers: 1 },
            );
        }
    }

    /// The layers where groups with opacity are painted separately. While
    /// painting ON one, they cannot be read from, and meanwhile a dummy one is
    /// bound. Each layer is as big as the whole surface: only those needed are requested.
    fn make_layers(device: &wgpu::Device, pipeline: &wgpu::RenderPipeline, format: wgpu::TextureFormat, width: u32, height: u32, n: u32) -> (Vec<wgpu::TextureView>, wgpu::BindGroup) {
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("layers"),
            size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: n.max(1) },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let all = t.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        let each = (0..n.max(1))
            .map(|k| {
                t.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: k,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&all) }],
        });
        (each, group)
    }

    pub fn sheet(&self, n: NewSheet, size: (f32, f32)) -> Sheet {
        let uniforms = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: (N_UNIFORMS * 4) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        let (layer_views, layer_group) = Self::make_layers(&self.device, &self.pipeline, self.format, 1, 1, 1);
        let mut l = Sheet {
            id: n.id, frame_no: 0, painted_as: Vec::new(), damage_log: Default::default(), known_as: (0, (0, 0)), kept: None, idle_kept_frames: 0, name: n.name, mhz: n.mhz, scale: n.scale, drives_pace: true, open: true, unseen: false, missed: 0, cleared: false, view: n.view,
            target: n.target, spare: None, window: n.window, px: (0, 0), uniforms, uniform_group, layer_views, layer_group, layers: 0, idle_layer_frames: 0, blur_rects: Vec::new(), lens: None, own: None, idle_own_frames: 0, wants_lens: false, capture: BackdropCapture::Idle, glass_box: None, asked_box: [0; 4], capture_asked: std::time::Instant::now(), rate_last: None, capture_taken: long_ago(), capture_owed: false, capture_forced: false, owed_damage: Vec::new(), painted_now: false, painted: None, input_region: vec![[-1, -1, -1, -1]], presented: false, keyboard_mode: None,
        };
        // Frames of our own where the platform makes them, so the compositor
        // is told which piece of each changed: a swapchain's are handed over
        // whole, and a bar the size of the screen made it compose and blur all
        // of it for one button lit (#46). They are painted as the scene's
        // pipelines paint, BGRA with premultiplied alpha; `PLEAMAR_SWAPCHAIN=1`
        // keeps the swapchain, to compare.
        let ours = self.format == wgpu::TextureFormat::Bgra8Unorm && self.alpha == wgpu::CompositeAlphaMode::PreMultiplied && std::env::var_os("PLEAMAR_SWAPCHAIN").is_none();
        if ours
            && matches!(l.target, Target::Surface(_))
            && let Some(frames) = l.window.own_frames()
        {
            if let Target::Surface(s) = std::mem::replace(&mut l.target, Target::Frames(frames)) {
                l.spare = Some(s);
            }
            self.own_frames.set(true);
        }
        self.reconfigure(&mut l, size);
        l
    }

    /// When the scale, the size or the role in the pacing changes.
    pub fn reconfigure(&self, l: &mut Sheet, _size: (f32, f32)) {
        let size = l.view.size;
        let px = ((size.0 * l.scale).round().max(1.0) as u32, (size.1 * l.scale).round().max(1.0) as u32);
        self.configure_surface(l, px);
        l.painted = None;
        if px != l.px {
            l.px = px;
            // A canvas of another size is no longer valid: another is made when needed.
            l.lens = None;
            l.kept = None;
            // Whatever there were no longer match the surface: they are requested again when needed.
            self.release_layers(l);
        }
    }

    fn release_layers(&self, l: &mut Sheet) {
        (l.layer_views, l.layer_group) = Self::make_layers(&self.device, &self.pipeline, self.format, 1, 1, 1);
        l.layers = 0;
        l.idle_layer_frames = 0;
    }

    /// That there are at least `n` layers of the surface's size; and if it has
    /// gone a while without needing any, give them back. A group that fades
    /// when a panel opens asks for them for a moment, not forever.
    fn ensure_layers(&self, l: &mut Sheet, n: u32) {
        if n > l.layers {
            (l.layer_views, l.layer_group) = Self::make_layers(&self.device, &self.pipeline, self.format, l.px.0, l.px.1, n);
            l.layers = n;
        }
        if n == 0 && l.layers > 0 {
            l.idle_layer_frames += 1;
            if l.idle_layer_frames > IDLE_LAYER_FRAMES {
                self.release_layers(l);
            }
        } else {
            l.idle_layer_frames = 0;
        }
    }

    fn configure_surface(&self, l: &mut Sheet, px: (u32, u32)) {
        let _ = &self.adapter;
        let surface = match &mut l.target {
            Target::Surface(s) => s,
            Target::Frames(frames) => return frames.resize(px),
        };
        surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                // With COPY_DST the lens canvas can be copied onto it.
                usage: if self.can_copy { wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST } else { wgpu::TextureUsages::RENDER_ATTACHMENT },
                format: self.format,
                view_formats: vec![],
                alpha_mode: self.alpha,
                width: px.0,
                height: px.1,
                desired_maximum_frame_latency: 1,
                // Only one sheet waits for its screen. If they all waited, two
                // monitors at different rates would hold each other back.
                // With mailbox, none waits for the screen: the render sets the
                // pace (see `uses_mailbox`). Without it, the one that sets the pace waits with vsync.
                present_mode: if l.drives_pace && !self.uses_mailbox() { wgpu::PresentMode::Fifo } else { self.non_blocking.unwrap_or(wgpu::PresentMode::Fifo) },
                color_space: wgpu::SurfaceColorSpace::Auto,
            },
        );
    }

    fn windows_texture(device: &wgpu::Device, (w, h, n): (u32, u32, u32)) -> (wgpu::Texture, wgpu::TextureView) {
        // (Its size is checked by whoever grows it: `window_room`.)
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("windows"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: n },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // As the programs hand them over: BGRA, premultiplied.
            format: wgpu::TextureFormat::Bgra8Unorm,
            // Painted into as well: video, made RGB there. And read back: a window shared.
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let v = t.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        (t, v)
    }

    /// Room in the windows' texture for a piece of that size in that layer. If
    /// it does not fit —a bigger window, more of them— the texture grows, and
    /// what the others had is lost: returns true, and the render uploads them again.
    fn window_room(&mut self, layer: u32, (w, h): (u32, u32)) -> bool {
        let max = self.device.limits().max_texture_dimension_2d;
        let (w, h) = (w.min(max), h.min(max));
        let (dw, dh, dn) = self.windows_dims;
        if w <= dw && h <= dh && layer < dn {
            return false;
        }
        // Grown with room to spare, so that a window being stretched does not remake it every frame.
        let grow = |have: u32, want: u32| if want > have { want.div_ceil(256) * 256 } else { have }.min(max);
        let mut dims = (grow(dw.max(1), w), grow(dh.max(1), h), (layer + 1).max(dn));
        // Every layer is as big as the biggest window, so one huge buffer is
        // that, times every layer: a window sent from a phone's monitor to a
        // desk's, its first frame still at the phone's scale of 2.65, asked
        // for gigabytes, the card said no, and the session went down. Past
        // the budget the layers grow less: that window is cut for a frame or
        // two, until it draws at its new size, and nothing else happens.
        let bytes = |d: (u32, u32, u32)| d.0 as u64 * d.1 as u64 * d.2 as u64 * 4;
        if bytes(dims) > windows_budget() {
            let k = (windows_budget() as f64 / bytes(dims) as f64).sqrt();
            let fit = |side: u32, had: u32| ((side as f64 * k) as u32 / 256 * 256).max(had).max(256).min(side);
            dims = (fit(dims.0, dw), fit(dims.1, dh), dims.2);
            // Already as big as it may be: it stays, and the picture is cut.
            if dims == self.windows_dims {
                return false;
            }
        }
        // And if the card cannot make it all the same, the texture there was stays.
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let (t, v) = Self::windows_texture(&self.device, dims);
        if let Some(e) = pollster::block_on(scope.pop()) {
            eprintln!("render · the windows' texture cannot grow to {}×{}×{} ({} MB): {e}; it stays {dw}×{dh}×{dn}", dims.0, dims.1, dims.2, bytes(dims) >> 20);
            return false;
        }
        eprintln!("render · the windows' texture grows to {}×{}×{} ({} MB) for a picture of {w}×{h}", dims.0, dims.1, dims.2, bytes(dims) >> 20);
        self.windows = t;
        self.windows_view = v;
        self.windows_dims = dims;
        self.rebind();
        true
    }

    /// A window's picture, back from the card: its pieces (layer, where in the
    /// picture, in pixels, the part of its layer it shows (x, y, w, h), the
    /// pixels it takes in the picture, and whether it is opaque —XRGB, whose
    /// fourth byte means nothing—), put together over transparent in a
    /// picture of that size. A piece drawn at another density than the
    /// picture's (a browser's page at the monitor's fractional scale under a
    /// window drawn at 2) is stretched to its place, as the screen shows it.
    /// BGRA, premultiplied.
    pub fn read_window(&self, pieces: &[(u32, (i32, i32), [u32; 4], (u32, u32), bool)], size: (u32, u32)) -> Option<Vec<u8>> {
        let (w, h) = size;
        if w == 0 || h == 0 {
            return None;
        }
        let (dw, dh, dn) = self.windows_dims;
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("a window, read") });
        let mut reads = Vec::new();
        for (layer, at, [sx, sy, sw, sh], to, opaque) in pieces {
            let (sx, sy) = ((*sx).min(dw), (*sy).min(dh));
            let (pw, ph) = ((*sw).min(dw - sx), (*sh).min(dh - sy));
            if pw == 0 || ph == 0 || to.0 == 0 || to.1 == 0 || *layer >= dn {
                continue;
            }
            let row = (pw * 4).div_ceil(256) * 256;
            let out = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (row * ph) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo { texture: &self.windows, mip_level: 0, origin: wgpu::Origin3d { x: sx, y: sy, z: *layer }, aspect: wgpu::TextureAspect::All },
                wgpu::TexelCopyBufferInfo { buffer: &out, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None } },
                wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
            );
            reads.push((out, row, *at, (pw, ph), *to, *opaque));
        }
        if reads.is_empty() {
            return None;
        }
        self.queue.submit(Some(encoder.finish()));
        let left = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(reads.len()));
        for (out, ..) in &reads {
            let left = left.clone();
            out.slice(..).map_async(wgpu::MapMode::Read, move |_| {
                left.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            });
        }
        let start = std::time::Instant::now();
        while left.load(std::sync::atomic::Ordering::Acquire) > 0 && start.elapsed() < std::time::Duration::from_secs(1) {
            let _ = self.device.poll(wgpu::PollType::Poll);
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        if left.load(std::sync::atomic::Ordering::Acquire) > 0 {
            return None;
        }
        // Over transparent, each piece over what is under it (premultiplied).
        let mut picture = vec![0u8; (w * h * 4) as usize];
        for (out, row, (ax, ay), (pw, ph), (tw, th), opaque) in &reads {
            let Ok(data) = out.slice(..).get_mapped_range() else { continue };
            let texel = |x: u32, y: u32| {
                let s = (y * row + x * 4) as usize;
                [data[s], data[s + 1], data[s + 2], if *opaque { 255 } else { data[s + 3] }]
            };
            // Its pixel for a pixel of its place: as it is when the sizes
            // agree, else between the four around it.
            let same = (*tw, *th) == (*pw, *ph);
            let at = |x: u32, y: u32| -> [u8; 4] {
                if same {
                    return texel(x, y);
                }
                let fx = ((x as f32 + 0.5) * *pw as f32 / *tw as f32 - 0.5).clamp(0.0, (*pw - 1) as f32);
                let fy = ((y as f32 + 0.5) * *ph as f32 / *th as f32 - 0.5).clamp(0.0, (*ph - 1) as f32);
                let (x0, y0) = (fx as u32, fy as u32);
                let (x1, y1) = ((x0 + 1).min(*pw - 1), (y0 + 1).min(*ph - 1));
                let (u, v) = (fx - x0 as f32, fy - y0 as f32);
                let (a, b, c, d) = (texel(x0, y0), texel(x1, y0), texel(x0, y1), texel(x1, y1));
                std::array::from_fn(|k| {
                    let top = a[k] as f32 * (1.0 - u) + b[k] as f32 * u;
                    let bottom = c[k] as f32 * (1.0 - u) + d[k] as f32 * u;
                    (top * (1.0 - v) + bottom * v).round() as u8
                })
            };
            for y in 0..*th as i32 {
                let ty = ay + y;
                if ty < 0 || ty >= h as i32 {
                    continue;
                }
                for x in 0..*tw as i32 {
                    let tx = ax + x;
                    if tx < 0 || tx >= w as i32 {
                        continue;
                    }
                    let p = at(x as u32, y as u32);
                    let d = ((ty as u32 * w + tx as u32) * 4) as usize;
                    let a = p[3] as u32;
                    if a == 255 {
                        picture[d..d + 4].copy_from_slice(&p);
                    } else if a > 0 {
                        for k in 0..4 {
                            picture[d + k] = (p[k] as u32 + picture[d + k] as u32 * (255 - a) / 255).min(255) as u8;
                        }
                    }
                }
            }
        }
        Some(picture)
    }

    /// What a window of the scene's compositor drew, to its layer. Returns true
    /// if the texture had to grow and the other layers were lost.
    pub fn upload_window(&mut self, layer: u32, size: (u32, u32), pixels: &[u8]) -> bool {
        let remade = self.window_room(layer, size);
        // What the texture holds of it: all of it, unless it could not grow enough.
        let (dw, dh, dn) = self.windows_dims;
        let (w, h) = (size.0.min(dw), size.1.min(dh));
        if layer < dn && w > 0 && h > 0 && pixels.len() >= (size.0 * size.1 * 4) as usize {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &self.windows, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: layer }, aspect: wgpu::TextureAspect::All },
                pixels,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size.0 * 4), rows_per_image: Some(size.1) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        remade
    }

    /// A program's frame that is already on the card, copied to its layer
    /// there without passing through here. Returns whether the texture grew
    /// (as `upload_window`), and the submission the copy went in, for whoever
    /// has to wait for it before handing the buffer back.
    #[cfg(target_os = "linux")]
    /// `buffer` is which of the program's buffers; `fresh`, if it has come
    /// now, what it takes to read it (a buffer already read is not read again).
    pub fn copy_dmabuf(&mut self, layer: u32, size: (u32, u32), buffer: u64, fresh: Option<crate::scene::DmabufPiece>) -> Result<bool, String> {
        if let Some(d) = fresh.filter(|_| !self.dmabufs.contains_key(&buffer)) {
            // ARGB8888 and XRGB8888 are BGRA in memory, like the windows'
            // texture: copied. Video is read as it is, and painted as RGB.
            let (uses, usage) = if crate::dmabuf::copied(d.fourcc) { (wgpu::TextureUses::COPY_SRC, wgpu::TextureUsages::COPY_SRC) } else { (wgpu::TextureUses::RESOURCE, wgpu::TextureUsages::TEXTURE_BINDING) };
            // Its content is the program's, already there: nothing to clear.
            let texture = crate::dmabuf::import(&self.device, d, size, uses, usage, uses)?;
            if texture.format() != wgpu::TextureFormat::Bgra8Unorm && self.yuv.is_none() {
                self.yuv = Some(crate::dmabuf::YuvToRgb::new(&self.device, wgpu::TextureFormat::Bgra8Unorm));
            }
            self.dmabufs.insert(buffer, texture);
        }
        let remade = self.window_room(layer, size);
        if !self.dmabufs.contains_key(&buffer) {
            return Err("that buffer was never read".into());
        }
        let (dw, dh, dn) = self.windows_dims;
        if layer >= dn {
            return Ok(remade);
        }
        let copy = wgpu::Extent3d { width: size.0.min(dw).max(1), height: size.1.min(dh).max(1), depth_or_array_layers: 1 };
        // Not now: it goes in the same work as the next painting, which is
        // what reads it. A submission of its own cost as much as the painting's.
        // Video and RGBA are painted into it; BGRA is copied.
        let video = self.dmabufs.get(&buffer).is_some_and(|t| t.format() != wgpu::TextureFormat::Bgra8Unorm);
        self.copies.borrow_mut().push((buffer, layer, copy, video));
        Ok(remade)
    }

    /// The copies still waiting for a painting, in this encoder.
    #[cfg(target_os = "linux")]
    fn record_copies(&self, encoder: &mut wgpu::CommandEncoder) {
        for (buffer, layer, copy, video) in self.copies.borrow_mut().drain(..) {
            let Some(source) = self.dmabufs.get(&buffer) else { continue };
            if video {
                if let Some(yuv) = &self.yuv {
                    let target = self.windows.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2), base_array_layer: layer, array_layer_count: Some(1), ..Default::default() });
                    yuv.convert(&self.device, encoder, source, &target, (copy.width, copy.height));
                }
                continue;
            }
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo { texture: source, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::TexelCopyTextureInfo { texture: &self.windows, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: layer }, aspect: wgpu::TextureAspect::All },
                copy,
            );
        }
    }

    /// If no painting took the copies this round, they go on their own.
    pub fn flush_copies(&self) {
        #[cfg(target_os = "linux")]
        if !self.copies.borrow().is_empty() {
            let mut encoder = self.device.create_command_encoder(&Default::default());
            self.record_copies(&mut encoder);
            self.queue.submit(Some(encoder.finish()));
            self.last_submission.replace(Some(Sent::after(&self.queue)));
        }
    }

    /// The last work sent to the card: what has to be waited for before
    /// handing a program's buffer back.
    pub fn last_submission(&self) -> Option<Sent> {
        self.last_submission.borrow().clone()
    }

    /// The card as wgpu has it, for a platform that makes its own textures on it.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The layouts it can paint BGRA in, when it was made without a surface.
    pub fn render_modifiers(&self) -> &[u64] {
        &self.render_modifiers
    }

    /// Whether the card has finished that work, without waiting for it.
    pub fn is_done(&self, sent: &Sent) -> bool {
        sent.done(&self.device)
    }

    /// Buffers of programs that no longer exist: what was kept of them goes.
    #[cfg(target_os = "linux")]
    pub fn forget_dmabufs(&mut self, buffers: &[u64]) {
        for b in buffers {
            self.dmabufs.remove(b);
        }
    }

    /// The card's render node and the (fourcc, modifier) pairs of BGRA it can
    /// read from a program's memory: what the scene's compositor offers the
    /// programs, so that they hand over their frames on the card.
    #[cfg(target_os = "linux")]
    pub fn dmabuf_formats(&self) -> Option<(u64, Vec<(u32, u64)>)> {
        use ash::vk;
        use wgpu::hal::api::Vulkan;
        if !self.device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF) {
            return None;
        }
        // SAFETY: only reading properties of the physical device wgpu uses.
        unsafe {
            let hal = self.adapter.as_hal::<Vulkan>()?;
            let instance = hal.shared_instance().raw_instance();
            let pd = hal.raw_physical_device();
            let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
            let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
            instance.get_physical_device_properties2(pd, &mut props);
            if drm.has_render == 0 {
                return None;
            }
            let device = libc::makedev(drm.render_major as u32, drm.render_minor as u32);
            let mut formats = Vec::new();
            // BGRA copied from, in as many planes as its layout has; and video.
            for m in crate::dmabuf::modifiers(&self.adapter, vk::Format::B8G8R8A8_UNORM, vk::FormatFeatureFlags::TRANSFER_SRC) {
                for fourcc in [crate::dmabuf::ARGB, crate::dmabuf::XRGB] {
                    formats.push((fourcc, m));
                }
            }
            for m in crate::dmabuf::modifiers(&self.adapter, vk::Format::R8G8B8A8_UNORM, vk::FormatFeatureFlags::SAMPLED_IMAGE) {
                for fourcc in [crate::dmabuf::ABGR, crate::dmabuf::XBGR] {
                    formats.push((fourcc, m));
                }
            }
            if self.device.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12) {
                for m in crate::dmabuf::modifiers(&self.adapter, vk::Format::G8_B8R8_2PLANE_420_UNORM, vk::FormatFeatureFlags::SAMPLED_IMAGE) {
                    formats.push((crate::dmabuf::NV12, m));
                }
            }
            (!formats.is_empty()).then_some((device, formats))
        }
    }

    /// The card's nodes, primary and render, as `dev_t`s: either is how a
    /// compositor names the card it copies on.
    #[cfg(target_os = "linux")]
    fn drm_nodes(&self) -> Vec<u64> {
        use ash::vk;
        use wgpu::hal::api::Vulkan;
        // SAFETY: only reading properties of the physical device wgpu uses.
        unsafe {
            let Some(hal) = self.adapter.as_hal::<Vulkan>() else { return Vec::new() };
            let instance = hal.shared_instance().raw_instance();
            let pd = hal.raw_physical_device();
            let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
            let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
            instance.get_physical_device_properties2(pd, &mut props);
            let mut nodes = Vec::new();
            if drm.has_primary != 0 {
                nodes.push(libc::makedev(drm.primary_major as u32, drm.primary_minor as u32));
            }
            if drm.has_render != 0 {
                nodes.push(libc::makedev(drm.render_major as u32, drm.render_minor as u32));
            }
            nodes
        }
    }

    /// Makes the buffers the thumbnails service asks for, a live window to be
    /// copied into on the card, and hands it their fds; the reason when it
    /// cannot, and the window goes on being copied in memory. Lets go of
    /// those the service no longer has.
    #[cfg(target_os = "linux")]
    pub fn answer_thumbnail_wants(&mut self) {
        use crate::platform::{thumbnail_cards, thumbnail_offer, thumbnail_wants, ThumbnailOffer};
        for want in thumbnail_wants() {
            let answer = (|| {
                if !self.device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF) {
                    return Err("the card does not share its memory with the compositor".to_string());
                }
                if !self.drm_nodes().contains(&want.device) {
                    return Err("the compositor copies on another card".to_string());
                }
                // With alpha if it can; a window without it is drawn opaque.
                let mut last = "the compositor copies into no format the card reads".to_string();
                for fourcc in [crate::dmabuf::ARGB, crate::dmabuf::XRGB] {
                    let Some((_, mods)) = want.formats.iter().find(|(f, _)| *f == fourcc) else { continue };
                    match crate::dmabuf::export_to_read(&self.device, want.size, mods) {
                        Ok(e) => {
                            self.thumbs.insert(want.ticket, e.texture);
                            return Ok(ThumbnailOffer { fd: e.fd, fourcc, modifier: e.modifier, planes: e.planes });
                        }
                        Err(e) => last = e,
                    }
                }
                Err(last)
            })();
            thumbnail_offer(want.ticket, answer);
        }
        if !self.thumbs.is_empty() {
            let cards = thumbnail_cards();
            self.thumbs.retain(|t, _| cards.contains(t));
        }
    }

    /// Tells the thumbnails service, for each window it has just had copied
    /// into a buffer made here, what that frame's pixels add up to: the same as
    /// last time, and it is not a new picture. 8 bytes each, read back by a
    /// thread of their own (`Summing`): the render submits and goes on.
    #[cfg(target_os = "linux")]
    pub fn check_thumbnails(&mut self) {
        use crate::platform::{thumbnail_checked, thumbnail_checks};
        let mut checks = thumbnail_checks(|t| self.thumbs.contains_key(&t));
        // Being filled again already: nothing to sum, and no answer it would be.
        checks.retain(|t| {
            let claimed = t.on.claim();
            if !claimed {
                thumbnail_checked(t.on.ticket, t.n, None);
            }
            claimed
        });
        if checks.is_empty() {
            return;
        }
        let hash = self.hash.get_or_insert_with(|| crate::dmabuf::HashFrame::new(&self.device));
        let step = (self.device.limits().min_storage_buffer_offset_alignment as u64).max(8);
        let n = checks.len() as u64;
        let sums = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: step * n, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let out = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 8 * n, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("live windows told apart") });
        encoder.clear_buffer(&sums, 0, None);
        for (i, t) in checks.iter().enumerate() {
            hash.hash(&self.device, &mut encoder, &self.thumbs[&t.on.ticket], t.opaque, (&sums, step * i as u64));
        }
        for i in 0..n {
            encoder.copy_buffer_to_buffer(&sums, step * i, &out, 8 * i, 8);
        }
        let index = self.queue.submit(Some(encoder.finish()));
        // Released once the card has read them, not when the sums are read.
        let read: Vec<_> = checks.iter().map(|t| t.on.clone()).collect();
        self.queue.on_submitted_work_done(move || read.iter().for_each(|on| on.release()));
        let mapped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = mapped.clone();
        out.slice(..).map_async(wgpu::MapMode::Read, move |r| flag.store(r.is_ok(), std::sync::atomic::Ordering::Release));
        let job = Summing { index, out, mapped, checks: checks.into_iter().map(|t| (t.on.ticket, t.n)).collect() };
        // Waited for elsewhere: a still window costs the render a submit, not a wait.
        if self.summer.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<Summing>();
            let device = self.device.clone();
            let _ = std::thread::Builder::new().name("sums".into()).spawn(move || {
                for job in rx {
                    job.read(&device);
                }
            });
            self.summer = Some(tx);
        }
        if let Some(Err(std::sync::mpsc::SendError(job))) = self.summer.as_ref().map(|s| s.send(job)) {
            // (The thread is gone: no sums, so each copy is new, as without them.)
            job.unanswered();
        }
    }

    /// Paints each live window copied on the card into its place in the atlas,
    /// made small, and tells the service once the card has read it.
    #[cfg(target_os = "linux")]
    pub fn fit_thumbnails(&mut self, fits: &mut Vec<crate::text::Fit>) {
        if fits.is_empty() {
            return;
        }
        let fit = self.fit.get_or_insert_with(|| crate::dmabuf::FitFrame::new(&self.device, wgpu::TextureFormat::Rgba8Unorm));
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("live windows made small") });
        let mut read = Vec::new();
        for f in fits.drain(..) {
            let Some(on) = f.frame.card.clone() else { continue };
            // Being filled again: the service sends this window again once filled.
            if !on.claim() {
                on.skipped(f.frame.version);
                continue;
            }
            if !self.thumbs.contains_key(&on.ticket) {
                // Another render's: read from its fd.
                let [(offset, stride)] = on.planes[..] else { continue };
                let Ok(fd) = on.fd.try_clone() else { continue };
                match Self::import_dmabuf(&self.device, fd, on.size, on.modifier, stride, offset, wgpu::TextureUses::RESOURCE, wgpu::TextureUsages::TEXTURE_BINDING, wgpu::TextureUses::RESOURCE) {
                    Ok(t) => {
                        self.thumbs.insert(on.ticket, t);
                    }
                    Err(_) => {
                        on.release();
                        continue;
                    }
                }
            }
            let source = &self.thumbs[&on.ticket];
            let r = |s: &AtlasSlot| [s.x, s.y, s.width, s.height];
            fit.fit(&self.device, &mut encoder, source, &self.atlas_view, (r(&f.padded), r(&f.inner)), f.frame.opaque);
            read.push((on, f.frame.version));
        }
        self.queue.submit(Some(encoder.finish()));
        // Only then may the compositor copy into it again.
        self.queue.on_submitted_work_done(move || {
            for (on, version) in read {
                on.release();
                on.read(version);
            }
        });
    }

    /// The single-plane layouts (modifiers) of BGRA the card can do that with.
    #[cfg(target_os = "linux")]
    fn bgra_modifiers(&self, need: ash::vk::FormatFeatureFlags) -> Vec<u64> {
        use ash::vk;
        use wgpu::hal::api::Vulkan;
        // SAFETY: only reading properties of the physical device wgpu uses.
        unsafe {
            let Some(hal) = self.adapter.as_hal::<Vulkan>() else { return Vec::new() };
            let instance = hal.shared_instance().raw_instance();
            let pd = hal.raw_physical_device();
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
            let mut fp = vk::FormatProperties2::default().push_next(&mut list);
            instance.get_physical_device_format_properties2(pd, vk::Format::B8G8R8A8_UNORM, &mut fp);
            let n = list.drm_format_modifier_count as usize;
            let mut mods = vec![vk::DrmFormatModifierPropertiesEXT::default(); n];
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut mods);
            let mut fp = vk::FormatProperties2::default().push_next(&mut list);
            instance.get_physical_device_format_properties2(pd, vk::Format::B8G8R8A8_UNORM, &mut fp);
            mods.iter().filter(|m| m.drm_format_modifier_plane_count == 1 && m.drm_format_modifier_tiling_features.contains(need)).map(|m| m.drm_format_modifier).collect()
        }
    }

    /// A program's —or the monitor's— buffer on the card as a texture of this
    /// device, for the uses asked. Single plane.
    #[cfg(target_os = "linux")]
    /// `initial`: what its content is taken to be ready for (`UNINITIALIZED` if nothing of it matters).
    #[allow(clippy::too_many_arguments)]
    pub fn import_dmabuf(device: &wgpu::Device, fd: std::os::fd::OwnedFd, size: (u32, u32), modifier: u64, stride: u32, offset: u32, uses: wgpu::TextureUses, usage: wgpu::TextureUsages, initial: wgpu::TextureUses) -> Result<wgpu::Texture, String> {
        use wgpu::hal::api::Vulkan;
        // On OpenGL (see `Gpu::new`), a monitor's buffer, through EGL.
        // SAFETY: only asking which side the device runs on.
        if unsafe { device.as_hal::<wgpu::hal::api::Gles>() }.is_some() {
            return crate::dmabuf::import_gl(device, fd, size, crate::dmabuf::XRGB, modifier, stride, offset);
        }
        let extent = wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 };
        let hal_desc = wgpu::hal::TextureDescriptor {
            label: Some("a buffer on the card"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: uses,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        // SAFETY: the fd is a dmabuf of that size, BGRA, and that layout, as
        // whoever made it says; Vulkan takes it.
        let hal_texture = unsafe {
            let hal = device.as_hal::<Vulkan>().ok_or("the card is not driven with Vulkan")?;
            hal.texture_from_dmabuf_fd(fd, &hal_desc, modifier, stride as u64, offset as u64).map_err(|e| format!("{e:?}"))?
        };
        let desc = wgpu::TextureDescriptor {
            label: Some("a buffer on the card"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage,
            view_formats: &[],
        };
        // SAFETY: made on this device, as `desc` says.
        Ok(unsafe { device.create_texture_from_hal::<Vulkan>(hal_texture, &desc, initial) })
    }

    /// How big the windows' texture is: what a window's corners are measured against.
    pub fn windows_dims(&self) -> (u32, u32) {
        (self.windows_dims.0, self.windows_dims.1)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_scene_group(device: &wgpu::Device, pipeline: &wgpu::RenderPipeline, shapes: &wgpu::Buffer, elements: &wgpu::Buffer, points: &wgpu::Buffer, stops: &wgpu::Buffer, atlas: &wgpu::TextureView, sampler: &wgpu::Sampler, windows: &wgpu::TextureView, picture: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: shapes.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: elements.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(atlas) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(sampler) },
                wgpu::BindGroupEntry { binding: 4, resource: points.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: stops.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(windows) },
                wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(picture) },
            ],
        })
    }

    /// The scene's group, and one for each picture of its own: remade
    /// whenever something they bind is.
    fn rebind(&mut self) {
        let group = |picture: &wgpu::TextureView| Self::build_scene_group(&self.device, &self.pipeline, &self.shapes_buffer, &self.elements_buffer, &self.points_buffer, &self.stops_buffer, &self.atlas_view, &self.sampler, &self.windows_view, picture);
        self.scene_group = group(&self.no_picture_view);
        let groups: Vec<(u32, wgpu::BindGroup)> = self.pictures.iter().map(|(page, (_, view, _))| (*page, group(view))).collect();
        for (page, g) in groups {
            if let Some(p) = self.pictures.get_mut(&page) {
                p.2 = g;
            }
        }
    }

    fn picture_texture(device: &wgpu::Device, (w, h): (u32, u32)) -> (wgpu::Texture, wgpu::TextureView) {
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("picture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let v = t.create_view(&Default::default());
        (t, v)
    }

    /// The pictures of their own the workshop has let go, and those it has
    /// painted: each to its texture, made anew only if it changed size.
    pub fn upload_pictures(&mut self, pending: &mut Vec<(u32, (u32, u32), Vec<u8>)>, dropped: &mut Vec<u32>) {
        for page in dropped.drain(..) {
            self.pictures.remove(&page);
        }
        for (page, (w, h), rgba) in pending.drain(..) {
            if self.pictures.get(&page).is_none_or(|(t, ..)| (t.width(), t.height()) != (w, h)) {
                let (t, v) = Self::picture_texture(&self.device, (w, h));
                let g = Self::build_scene_group(&self.device, &self.pipeline, &self.shapes_buffer, &self.elements_buffer, &self.points_buffer, &self.stops_buffer, &self.atlas_view, &self.sampler, &self.windows_view, &v);
                self.pictures.insert(page, (t, v, g));
            }
            let (t, ..) = &self.pictures[&page];
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: None },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
    }

    /// What is going to be painted, to the card. If it does not fit, the
    /// stores grow to double —as many times as needed— and never shrink again.
    pub fn upload(&mut self, d: &DrawList) {
        let wants = (d.shapes.len(), d.elements.len(), d.points.len(), d.stops.len());
        if wants.0 > self.capacity.0 || wants.1 > self.capacity.1 || wants.2 > self.capacity.2 || wants.3 > self.capacity.3 {
            let grow = |fits: usize, wants: usize| if wants > fits { wants.next_power_of_two() } else { fits }.min(self.limit);
            let new = (grow(self.capacity.0, wants.0) / PER_SHAPE * PER_SHAPE, grow(self.capacity.1, wants.1) / PER_ELEMENT * PER_ELEMENT, grow(self.capacity.2, wants.2) / 2 * 2, grow(self.capacity.3, wants.3) / 4 * 4);
            if new != self.capacity {
                let store = |label, floats: usize| self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: (floats * 4) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                if new.0 != self.capacity.0 { self.shapes_buffer = store("shapes", new.0) }
                if new.1 != self.capacity.1 { self.elements_buffer = store("elements", new.1) }
                if new.2 != self.capacity.2 { self.points_buffer = store("points", new.2) }
                if new.3 != self.capacity.3 { self.stops_buffer = store("stops", new.3) }
                self.rebind();
                self.capacity = new;
                println!("render · the scene has grown: now {} shapes and {} elements fit", new.0 / PER_SHAPE, new.1 / PER_ELEMENT);
            }
            if (wants.0 > self.capacity.0 || wants.1 > self.capacity.1) && !std::mem::replace(&mut self.limit_warned, true) {
                eprintln!("render · this card cannot take more than {} shapes and {} elements: the rest is not painted", self.capacity.0 / PER_SHAPE, self.capacity.1 / PER_ELEMENT);
            }
        }
        let (f, e, pt, pa) = (wants.0.min(self.capacity.0), wants.1.min(self.capacity.1), wants.2.min(self.capacity.2), wants.3.min(self.capacity.3));
        self.queue.write_buffer(&self.shapes_buffer, 0, bytemuck::cast_slice(&d.shapes[..f]));
        self.queue.write_buffer(&self.elements_buffer, 0, bytemuck::cast_slice(&d.elements[..e]));
        self.queue.write_buffer(&self.points_buffer, 0, bytemuck::cast_slice(&d.points[..pt]));
        self.queue.write_buffer(&self.stops_buffer, 0, bytemuck::cast_slice(&d.stops[..pa]));
    }

    /// Paints the draw list on a sheet. Returns whether it got to be presented.
    /// `damage`: what has changed since the last frame, in the scene's plane
    /// (`None`, everything). A sheet whose frames are lent by the platform
    /// knows what each of them holds, and only what changed since that one was
    /// last painted is painted again; a swapchain's are unknown, and are painted whole.
    pub fn paint(&self, l: &mut Sheet, d: &DrawList, uniforms: &[f32], request_frame: bool, damage: Option<&[[f32; 4]]>) -> bool {
        // Only the layers of the groups that fall on this surface: the one
        // fading on another monitor costs this one neither memory nor a pass.
        // What blends the layer takes up the union of what is inside, so if
        // that does not touch the view, the layer is not read.
        let v = l.view.bounds();
        let needed = d.offscreen_groups.iter().filter(|(t, _)| d.touches_view(t, v)).map(|(_, c)| *c as u32 + 1).max().unwrap_or(0);
        self.ensure_layers(l, needed);
        // The lens, if it shows glass and the screen lets a canvas be copied onto it.
        // A monitor of pleamar-wm's own has nothing behind it to capture: what
        // is behind its glass is what it painted itself before it.
        // The picture was taken away (`lens = image ... while`): a lens made for it captures nothing, so it is made again.
        if self.lens_image.borrow().is_none() && l.lens.as_ref().is_some_and(|x| x.fixed.is_some()) {
            l.lens = None;
            l.painted_as.clear();
            l.damage_log.clear();
        }
        let whole_screen = matches!(&l.target, Target::Frames(f) if f.whole_screen());
        if !l.wants_lens || !self.can_copy || whole_screen {
            l.lens = None;
        } else if l.lens.as_ref().is_none_or(|x| x.blur_generation() != self.blur.generation) {
            l.lens = Some(crate::lens::Lens::new(&self.device, self.format, l.px, &self.blur));
            // New canvases: nothing is known of what they hold.
            l.painted_as.clear();
            l.damage_log.clear();
        }
        // `lens = image "file"`: the lens bends a picture and never captures.
        let fixed = self.lens_image.borrow_mut().as_mut().and_then(|s| s.poll());
        if let (Some(lens), Some((version, picture))) = (&mut l.lens, fixed) {
            if lens.fixed != Some(version) {
                lens.set_fixed(self, version, &picture);
                l.painted_as.clear();
                l.damage_log.clear();
            }
        }
        // A surface has what is behind it, but a glass of its with something
        // of the scene's own under it —a picture, a text, another shape— has
        // that in front of what is behind: it is taken the same way, over the
        // unmixed background. It needs the canvas, so only with a lens; and
        // only while some glass is over something, so a glass with nothing of
        // ours under it costs what it did.
        let glass = l.wants_lens && !d.glass_marks.is_empty();
        let over_own = glass && !whole_screen && l.lens.is_some() && d.glass_over_own(v);
        if glass && (whole_screen || over_own) {
            l.idle_own_frames = 0;
            if l.own.as_ref().is_none_or(|o| o.px != l.px || o.blur_generation() != self.blur.generation) {
                l.own = Some(crate::lens::Own::new(self, l.px));
            }
        } else if glass && l.own.is_some() && l.idle_own_frames < IDLE_OWN_FRAMES {
            // Kept a while: something passing under a glass now and then
            // would otherwise remake it each time.
            l.idle_own_frames += 1;
        } else {
            l.own = None;
        }
        let with_own = whole_screen || over_own;
        let mut u = uniforms.to_vec();
        u[3] = l.scale;
        u[128] = (l.lens.as_ref().is_some_and(|x| x.ready) || (with_own && l.own.is_some())) as u8 as f32;
        let (v, o) = (l.view.size, l.view.origin);
        (u[0], u[1], u[4], u[7]) = (v.0, v.1, o.0, o.1);
        self.queue.write_buffer(&l.uniforms, 0, bytemuck::cast_slice(&u));
        // With `PLEAMAR_TIMING=1`, how much goes into asking for the screen's
        // slot and how much into sending the work to the card. It is the
        // figure that says whether a frame costs because of what it draws or
        // because of waiting for the monitor.
        let timing = timing_enabled();
        let t0 = timing.then(std::time::Instant::now);
        let frame = match &mut l.target {
            Target::Surface(surface) => match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Frame::Surface(t),
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    self.configure_surface(l, l.px);
                    return false;
                }
                _ => return false,
            },
            Target::Frames(frames) => match frames.acquire(&self.device, &self.render_modifiers) {
                Some((which, texture)) => Frame::Lent(which, texture),
                // Our own frames could not be made: back to the swapchain, from the next round.
                None if frames.gave_up() => {
                    if let Some(s) = l.spare.take() {
                        l.target = Target::Surface(s);
                        self.configure_surface(l, l.px);
                    }
                    return false;
                }
                None => return false,
            },
        };
        let frame_texture = match &frame {
            Frame::Surface(f) => &f.texture,
            Frame::Lent(_, t) => t,
        };
        // A surface without a lens paints on a canvas of its own that stays,
        // while what changed is known: then only that is painted again, and
        // the canvas is copied whole to the screen —a copy costs the card far
        // less than painting a glass the size of the monitor—. Given back
        // after a while of everything changing at once.
        // Frames of our own handed to a compositor are three: painting in
        // each what changed since it was last painted repainted what changed
        // in the last three frames. They are brought up to date from the kept
        // canvas instead, by copies of those pieces.
        let full_repaint = std::env::var_os("PLEAMAR_FULL_REPAINT").is_some();
        let keeps_canvas = (matches!(frame, Frame::Surface(_)) || !whole_screen) && l.lens.is_none();
        let mut kept_fresh = false;
        if keeps_canvas && self.can_copy && damage.is_some() && l.open && !full_repaint {
            l.idle_kept_frames = 0;
            if l.kept.is_none() {
                l.kept = Some(crate::lens::texture(&self.device, "kept", l.px, self.format, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC));
                l.painted_as.clear();
                l.damage_log.clear();
                kept_fresh = true;
            }
        } else if l.kept.is_some() {
            l.idle_kept_frames += 1;
            if !keeps_canvas || l.idle_kept_frames > IDLE_KEPT_FRAMES {
                l.kept = None;
            }
        }
        // Which canvas this frame goes on, if what it holds is known: one
        // the platform lends, one of the lens's two, or the kept one.
        let known: Option<(u8, usize)> = match (&frame, &l.lens, &l.kept) {
            (_, Some(lens), _) => Some((2, lens.next())),
            (Frame::Lent(which, _), None, _) => Some((1, *which)),
            (Frame::Surface(_), None, Some(_)) => Some((3, 0)),
            (Frame::Surface(_), None, None) => None,
        };
        if known.map(|k| (k.0, l.px)) != Some(l.known_as) {
            l.painted_as.clear();
            l.damage_log.clear();
            l.known_as = known.map_or((0, (0, 0)), |k| (k.0, l.px));
        }
        // Which pieces of it have to be painted again —what changed now, and
        // since that canvas was last painted— in pixels: a few boxes, not
        // one around them all, which for a bar round the edge of the screen
        // that lights up at one end and at the other is the whole screen.
        // And what changed in this frame alone, the same way (`None`, all of it).
        let mut this_frame: Option<Vec<[u32; 4]>> = None;
        let cuts: Option<Vec<[u32; 4]>> = match known {
            None => None,
            // `PLEAMAR_FULL_REPAINT=1`: every frame whole, to compare.
            Some(_) if full_repaint => None,
            // Closed, it is cleared whole: a piece left of what it showed stayed
            // in one of its frames, and the monitor alternated it with the empty one.
            Some(_) if !l.open => {
                l.painted_as.clear();
                l.damage_log.clear();
                None
            }
            Some((_, which)) => {
                let v = l.view.bounds();
                let (pw, ph) = (l.px.0 as f32, l.px.1 as f32);
                let now: Option<Vec<[f32; 4]>> = damage.map(|rects| {
                    merge_cuts(rects.iter().filter(|b| b[0] < v[2] && b[2] > v[0] && b[1] < v[3] && b[3] > v[1]).map(|b| {
                        [((b[0] - v[0]) * l.scale).floor().max(0.0), ((b[1] - v[1]) * l.scale).floor().max(0.0), ((b[2] - v[0]) * l.scale).ceil().min(pw), ((b[3] - v[1]) * l.scale).ceil().min(ph)]
                    }))
                });
                let as_px = |r: &Vec<[f32; 4]>| (r.iter().map(|b| (b[2] - b[0]) * (b[3] - b[1])).sum::<f32>() < 0.7 * pw * ph).then(|| r.iter().map(|b| [b[0] as u32, b[1] as u32, (b[2] - b[0]) as u32, (b[3] - b[1]) as u32]).collect());
                this_frame = now.as_ref().and_then(as_px);
                let frame_no = l.frame_no;
                l.frame_no += 1;
                if l.painted_as.len() <= which {
                    l.painted_as.resize(which + 1, None);
                }
                let since = l.painted_as[which];
                l.painted_as[which] = Some(frame_no);
                l.damage_log.push_back((frame_no, now));
                while l.damage_log.len() > 8 {
                    l.damage_log.pop_front();
                }
                // Everything, unless every frame since that canvas was painted is known.
                let region = since.and_then(|p| {
                    let complete = l.damage_log.front().is_some_and(|(n, _)| *n <= p + 1);
                    let mut all = Vec::new();
                    for (_, b) in l.damage_log.iter().filter(|(n, _)| *n > p) {
                        all.extend_from_slice(b.as_ref()?);
                    }
                    complete.then(|| merge_cuts(all.into_iter()))
                });
                // Most of it changed: it is painted whole, in one go.
                region.as_ref().and_then(as_px)
            }
        };
        // Our own frame brought up to date from the kept canvas: on that one,
        // only what changed now (all of it if it was just made).
        let from_kept = matches!(frame, Frame::Lent(..)) && keeps_canvas && l.kept.is_some();
        let (cuts, copies) = if from_kept { (if kept_fresh { None } else { this_frame }, cuts) } else { (cuts, None) };
        // A glass that reads what the scene painted under it (`own`) reads it
        // past its box, as far as its frosting reaches: the piece painted again
        // grows to all of that, so what it reads is this frame's. Cut to the
        // piece, `own` kept what an earlier frame had taken there, and the
        // edges of a glass over glass came out one way in whole frames and
        // another in partial ones, flickering (issue 48).
        let cuts = match cuts {
            Some(c) if with_own && l.own.is_some() => grown_for_glass(c, d, l.view.bounds(), l.scale, l.px),
            c => c,
        };
        let view = frame_texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        #[cfg(target_os = "linux")]
        self.record_copies(&mut encoder);
        // What the last capture of what is behind left, before painting with it.
        let scale = l.scale;
        if let Some(lens) = &mut l.lens {
            lens.prepare(self, &self.lens, &mut encoder, scale);
        }
        let backdrop_group = match l.own.as_ref().filter(|_| with_own) {
            Some(own) => &own.group,
            None => l.lens.as_ref().and_then(|x| x.group.as_ref()).unwrap_or(&self.no_backdrop_group),
        };
        let pass_to = |encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, layers: &wgpu::BindGroup, spans: &[Range<u32>], keep: bool, scissor: Option<[u32; 4]>, erase: bool| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: if keep { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) }, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some([x, y, w, h]) = scissor {
                pass.set_scissor_rect(x, y, w.max(1), h.max(1));
            }
            // The piece painted again starts empty, as a whole frame does:
            // drawing its translucent parts on top of what was there adds them
            // up frame after frame (the edge of Marea, flickering).
            if erase {
                pass.set_pipeline(&self.erase);
                pass.draw(0..3, 0..1);
            }
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.scene_group, &[]);
            pass.set_bind_group(1, layers, &[]);
            pass.set_bind_group(2, &l.uniform_group, &[]);
            pass.set_bind_group(3, backdrop_group, &[]);
            // Never beyond what was uploaded (only happens if the card could not take it all).
            let uploaded = (self.capacity.1 / PER_ELEMENT) as u32;
            for t in spans.iter().map(|t| t.start.min(uploaded)..t.end.min(uploaded)).filter(|t| !t.is_empty()) {
                // The particles go where their emitter is among the elements: the
                // stretch is cut there, and they are drawn with their own pipeline.
                // And the groups that blend another way, with their own pipeline.
                let mut from = t.start;
                let mut special: Vec<(u32, u32, u8)> = d.particle_marks.iter().filter(|(at, _)| t.contains(at)).map(|&(at, n)| (at, n, 0)).collect();
                special.extend(d.blend_marks.iter().filter(|(at, _)| t.contains(at)).map(|&(at, m)| (at, 1, m)));
                special.extend(d.picture_marks.iter().filter(|(at, _)| t.contains(at)).map(|&(at, page)| (at, page, 4)));
                special.sort_by_key(|s| s.0);
                for (at, count, how) in special {
                    if at > from {
                        pass.draw(0..6, from..at);
                    }
                    match how {
                        0 => {
                            pass.set_pipeline(&self.particles);
                            pass.draw(0..6, at * PARTICLE_SLOTS..at * PARTICLE_SLOTS + count);
                        }
                        // A picture of its own (`count`: which), with its texture bound.
                        4 => {
                            if let Some((.., group)) = self.pictures.get(&count) {
                                pass.set_bind_group(0, group, &[]);
                                pass.draw(0..6, at..at + 1);
                                pass.set_bind_group(0, &self.scene_group, &[]);
                            }
                        }
                        m => {
                            pass.set_pipeline(if m == 2 { &self.screen } else { &self.multiply });
                            pass.draw(0..6, at..at + 1);
                        }
                    }
                    pass.set_pipeline(&self.pipeline);
                    from = at + 1;
                }
                if t.end > from {
                    pass.draw(0..6, from..t.end);
                }
            }
        };
        // Everything in order, and each group with opacity or effects painted
        // into its layer right before the stretch that blends it: they share
        // a few, so there can be as many as the scene wants and they cost the
        // memory of at most `LAYER_SLOTS`. The target is cleared once, at the
        // start; the following stretches paint on top of it.
        let target = match (&l.lens, &l.kept) {
            // With a lens, on its canvas, which is then copied to the screen: one
            // has to know exactly what was painted to unmix what is behind.
            (Some(lens), _) => lens.canvas(),
            (None, Some(kept)) if keeps_canvas => &kept.1,
            _ => &view,
        };
        // Closed, it is cleared: transparent and nothing on top. Painting what
        // is in the draw list will not do, because it closes in the middle of
        // a frame —the draw list was composed while it was still open— and
        // that frame stayed forever: in Marea, a piece of the little ball about
        // to come out through the edge.
        // Each piece painted again on its own, cut to its box; or all of it, once.
        let pieces: Vec<Option<[u32; 4]>> = match &cuts {
            Some(c) if !c.is_empty() => c.iter().copied().map(Some).collect(),
            Some(_) => vec![Some([0, 0, 0, 0])],
            None => vec![None],
        };
        for scissor in pieces {
            let total = if l.open { d.element_count() as u32 } else { 0 };
            let groups: &[(Range<u32>, usize)] = if l.open { &d.offscreen_groups } else { &[] };
            let mut from = 0u32;
            // Painting a piece of it, the rest of what it held stays.
            let mut keep = scissor.is_some();
            // The first pass onto the target empties the piece it paints.
            let mut erase = keep;
            // Nothing to paint again: what that buffer holds is already this frame.
            let (total, groups): (u32, &[(Range<u32>, usize)]) = if scissor == Some([0, 0, 0, 0]) { (0, &[]) } else { (total, groups) };
            // Where it is painted, in the scene's plane: the piece that changed if
            // only that is painted again, else all of the view. A group that does
            // not touch it needs no layer: whatever of it could show is cut away.
            let v = l.view.bounds();
            let region = match scissor {
                Some([x, y, w, h]) => [v[0] + x as f32 / l.scale, v[1] + y as f32 / l.scale, v[0] + (x + w) as f32 / l.scale, v[1] + (y + h) as f32 / l.scale],
                None => v,
            };
            // What goes straight onto the target is gathered into as few passes as
            // there are layers painted in between: a pass costs wgpu more than its draws.
            let mut onto: Vec<Range<u32>> = Vec::new();
            // On a monitor of pleamar-wm's own, right before each glass, what has
            // been painted so far is taken as what is behind it: only where there
            // is glass, and only in the piece painted again. A few times a frame at
            // most; past that, a glass bends what the one before it saw.
            // On a surface whose glass is over something of its own, the same,
            // over what the capture says is behind the surface.
            // Where a glass is, in real pixels of the sheet, with the margin its
            // frosting reads: only that is taken for it.
            let own = l.own.as_ref().filter(|_| with_own);
            let behind = l.lens.as_ref().and_then(|x| x.background());
            let (view_origin, sc) = (l.view.bounds(), l.scale);
            let own_region = |at: &mut dyn Iterator<Item = u32>| -> Option<(u32, u32, u32, u32)> {
                let own = own?;
                let (pw, ph) = own.px;
                let m = crate::lens::margin();
                let b = at.filter(|k| (*k as usize) < d.element_count()).map(|k| {
                    let e = &d.elements[k as usize * PER_ELEMENT + 4..k as usize * PER_ELEMENT + 8];
                    [e[0] - view_origin[0] - m, e[1] - view_origin[1] - m, e[2] - view_origin[0] + m, e[3] - view_origin[1] + m]
                }).reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])?;
                let mut r = [((b[0] * sc).floor().max(0.0) as u32).min(pw), ((b[1] * sc).floor().max(0.0) as u32).min(ph), ((b[2] * sc).ceil().max(0.0) as u32).min(pw), ((b[3] * sc).ceil().max(0.0) as u32).min(ph)];
                if let Some([x, y, w, h]) = scissor {
                    r = [r[0].max(x), r[1].max(y), r[2].min(x + w), r[3].min(y + h)];
                }
                (r[2] > r[0] && r[3] > r[1]).then(|| (r[0], r[1], r[2] - r[0], r[3] - r[1]))
            };
            let mut marks: Vec<u32> = if own.is_some() { d.glass_marks.clone() } else { Vec::new() };
            marks.sort_unstable();
            marks.dedup();
            // Whether something was painted on the target since it was last taken, and how many times it was.
            let (mut dirty, mut taken) = (true, 0u32);
            const MOST_TAKEN: u32 = 6;
            // `then_take`: and after it, take it (a group with glass inside comes next).
            let mut flush = |encoder: &mut wgpu::CommandEncoder, ranges: &[Range<u32>], keep: &mut bool, erase: &mut bool, then_take: Option<&Range<u32>>| {
                let mut batch: Vec<Range<u32>> = Vec::new();
                let paint = |encoder: &mut wgpu::CommandEncoder, batch: &mut Vec<Range<u32>>, keep: &mut bool, erase: &mut bool, dirty: &mut bool| {
                    if !batch.is_empty() || !*keep {
                        pass_to(encoder, target, &l.layer_group, batch, *keep, scissor, *erase);
                        *dirty |= !batch.is_empty();
                        (*keep, *erase) = (true, false);
                        batch.clear();
                    }
                };
                for r in ranges {
                    let mut start = r.start;
                    for &m in marks.iter().filter(|m| **m >= r.start && **m < r.end) {
                        if m > start {
                            batch.push(start..m);
                            start = m;
                        }
                        // The last time it can, it takes what every glass still to come is on.
                        let last = taken + 1 == MOST_TAKEN;
                        let mut these = marks.iter().copied().filter(|k| *k == m || (last && *k > m));
                        if let (Some(own), Some(region)) = (own.filter(|_| taken < MOST_TAKEN), own_region(&mut these)) {
                            paint(encoder, &mut batch, keep, erase, &mut dirty);
                            own.take(self, &self.lens, encoder, target, behind, region, l.scale);
                            (dirty, taken) = (false, taken + 1);
                        }
                    }
                    if r.end > start {
                        batch.push(start..r.end);
                    }
                }
                paint(encoder, &mut batch, keep, erase, &mut dirty);
                if let Some(span) = then_take.filter(|_| taken < MOST_TAKEN) {
                    let last = taken + 1 == MOST_TAKEN;
                    if let (Some(own), Some(region)) = (own, own_region(&mut marks.iter().copied().filter(|m| span.contains(m) || (last && *m >= span.start)))) {
                        own.take(self, &self.lens, encoder, target, behind, region, l.scale);
                        (dirty, taken) = (false, taken + 1);
                    }
                }
            };
            // A batch at a time (see `DrawList::share_layers`): what comes before
            // it onto the target, then its groups onto their layers, one pass a
            // layer; what is between them and their blends go onto the target
            // with whatever comes next.
            let batches: &[usize] = if groups.is_empty() { &[] } else { &d.layer_batches };
            for (b, &first) in batches.iter().enumerate() {
                let members = &groups[first..batches.get(b + 1).copied().unwrap_or(groups.len())];
                // The element that blends each comes right after its span: if that one
                // falls in the region, the layer is needed, even if what is inside does not.
                let painted: Vec<&(Range<u32>, usize)> = members.iter().filter(|(span, layer)| l.layers as usize > *layer && d.blends(span) && d.touches_view(&(span.start..(span.end + 1).min(total)), region)).collect();
                onto.push(from..members[0].0.start);
                if !painted.is_empty() {
                    onto.retain(|r| !r.is_empty());
                    // A glass inside one (alone in its batch) bends what is on the target under the group.
                    let glass_inside = painted.iter().find(|(span, _)| marks.iter().any(|m| span.contains(m))).map(|(span, _)| span);
                    if !onto.is_empty() || !keep || glass_inside.is_some() {
                        flush(&mut encoder, &onto, &mut keep, &mut erase, glass_inside);
                        onto.clear();
                    }
                    for (k, view) in l.layer_views.iter().enumerate().take(l.layers as usize) {
                        let spans: Vec<Range<u32>> = painted.iter().filter(|(_, layer)| *layer == k).map(|(span, _)| span.clone()).collect();
                        if !spans.is_empty() {
                            // Painting a piece of the target, the layer is needed only
                            // there, and as far as a blur or a glow reaches around it.
                            // A shader of the scene's may read the group anywhere: whole.
                            let cut = scissor.and_then(|[x, y, w, h]| {
                                let mut reach = 0.0f32;
                                for span in &spans {
                                    let at = span.end as usize * PER_ELEMENT;
                                    let e = d.elements.get(at..at + PER_ELEMENT)?;
                                    if e[40] > 0.5 {
                                        return None;
                                    }
                                    reach = reach.max(e[20].max(e[21]));
                                }
                                let m = (reach * l.scale).ceil() as u32 + 2;
                                let (x0, y0) = (x.saturating_sub(m), y.saturating_sub(m));
                                let (x1, y1) = ((x + w + m).min(l.px.0), (y + h + m).min(l.px.1));
                                Some([x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)])
                            });
                            pass_to(&mut encoder, view, &self.no_layers_group, &spans, false, cut, false);
                        }
                    }
                }
                for pair in members.windows(2) {
                    onto.push(pair[0].0.end..pair[1].0.start);
                }
                from = members[members.len() - 1].0.end;
            }
            onto.push(from..total);
            onto.retain(|r| !r.is_empty());
            flush(&mut encoder, &onto, &mut keep, &mut erase, None);
        }
        if let Some(lens) = &mut l.lens {
            lens.copy_to(&mut encoder, frame_texture);
        } else if let Some(kept) = l.kept.as_ref().filter(|_| keeps_canvas) {
            let (w, h) = (l.px.0.min(frame_texture.width()), l.px.1.min(frame_texture.height()));
            match copies.as_ref().filter(|_| from_kept) {
                // Our own frame: only what it lacks.
                Some(pieces) => {
                    for &[x, y, pw, ph] in pieces {
                        let (pw, ph) = (pw.min(w.saturating_sub(x)), ph.min(h.saturating_sub(y)));
                        if pw > 0 && ph > 0 {
                            let at = wgpu::Origin3d { x, y, z: 0 };
                            encoder.copy_texture_to_texture(
                                wgpu::TexelCopyTextureInfo { origin: at, ..kept.0.as_image_copy() },
                                wgpu::TexelCopyTextureInfo { origin: at, ..frame_texture.as_image_copy() },
                                wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
                            );
                        }
                    }
                }
                None => encoder.copy_texture_to_texture(kept.0.as_image_copy(), frame_texture.as_image_copy(), wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }),
            }
        }
        let t1 = timing.then(std::time::Instant::now);
        let commands = encoder.finish();
        let t2 = timing.then(std::time::Instant::now);
        let index = self.queue.submit(Some(commands));
        let sent = Sent::after(&self.queue);
        self.last_submission.replace(Some(sent.clone()));
        let t3 = timing.then(std::time::Instant::now);
        // `PLEAMAR_GPU_TIME=1`: how long the card takes to paint it, waiting for it
        // (only to measure: it holds the render meanwhile).
        if gpu_time_enabled() {
            sent.wait(&self.device, std::time::Duration::from_secs(1));
            let ms = t3.map_or(0.0, |t| t.elapsed().as_secs_f32() * 1000.0);
            GPU_TIME.with(|c| {
                let (sum, n) = c.get();
                let (sum, n) = (sum + ms, n + 1);
                if n >= 120 {
                    println!("timing · the card takes {:.2} ms to paint a frame of {}×{}", sum / n as f32, l.px.0, l.px.1);
                    c.set((0.0, 0));
                } else {
                    c.set((sum, n));
                }
            });
        }
        match frame {
            Frame::Surface(frame) => {
                if request_frame {
                    l.window.request_frame();
                }
                self.queue.present(frame);
            }
            Frame::Lent(which, _) => {
                if request_frame {
                    l.window.request_frame();
                }
                // What the compositor is told changed: those pieces, none if
                // nothing did, all of it if they are not known.
                if let Target::Frames(frames) = &mut l.target {
                    frames.present_pieces(which, index, &self.device, &self.queue, cuts.as_deref());
                }
            }
        }
        if let (Some(t0), Some(t1), Some(t2), Some(t3)) = (t0, t1, t2, t3) {
            let ms = |a: std::time::Instant, b: std::time::Instant| b.duration_since(a).as_secs_f32() * 1000.0;
            TIMING.with(|c| {
                let mut v = c.get();
                v.0 += ms(t0, t1);
                v.1 += ms(t1, t2);
                v.2 += ms(t2, t3);
                v.3 += t3.elapsed().as_secs_f32() * 1000.0;
                v.4 += 1.0;
                // How much of the surface was painted again: a piece, or all of it.
                v.5 += cuts.as_ref().map_or(1.0, |c| c.iter().map(|[_, _, w, h]| *w as f32 * *h as f32).sum::<f32>() / (l.px.0 as f32 * l.px.1 as f32).max(1.0));
                if v.4 >= 300.0 {
                    println!("timing · acquire {:.2} · record {:.2} · finish {:.2} · submit+present {:.2} ms · painted again {:.0} % of the pixels", v.0 / v.4, v.1 / v.4, v.2 / v.4, v.3 / v.4, 100.0 * v.5 / v.4);
                    // What it takes up on the card: what is really requested, not the blocks the allocator reserves.
                    if let Some(r) = self.device.generate_allocator_report() {
                        println!("timing · card memory: {:.1} MB in use ({:.1} MB reserved)", r.total_allocated_bytes as f64 / 1e6, r.total_reserved_bytes as f64 / 1e6);
                    }
                    v = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
                }
                c.set(v);
            });
        }
        true
    }
}

/// At most this many pieces are painted again in a frame: each one is a
/// pass over the draw list.
const MOST_CUTS: usize = 4;

/// The boxes that changed (x0, y0, x1, y1), as a few: those that overlap or
/// almost —less than a 64 × 64 square is painted for nothing by joining
/// them— become one, and while there are more than `MOST_CUTS`, the two that
/// cost least to join are joined.
/// The pieces painted again (`[x, y, w, h]`, real pixels of the sheet), each
/// grown to the whole of every glass it touches and as far as that glass reads
/// around it (`lens::MARGIN`), until none grows more; and merged where they
/// meet. `None`: most of the sheet, painted whole.
///
/// Before each glass, what has been painted so far is taken (`own`), in the
/// piece painted again, and the glass reads it (sharp and frosted) past the
/// piece too, where `own` holds what an earlier frame took, at another point
/// of the frame. A glass painted again whole, with all it reads, reads only
/// this frame's. (Growing a piece only by the glasses drawn over the first one
/// around it was tried, and still flickered.)
fn grown_for_glass(cuts: Vec<[u32; 4]>, d: &DrawList, view: [f32; 4], scale: f32, px: (u32, u32)) -> Option<Vec<[u32; 4]>> {
    let m = crate::lens::margin();
    let (pw, ph) = (px.0 as f32, px.1 as f32);
    let mut marks: Vec<usize> = d.glass_marks.iter().map(|k| *k as usize).filter(|k| *k < d.element_count()).collect();
    marks.sort_unstable();
    marks.dedup();
    // In the order they are drawn.
    let reach: Vec<[f32; 4]> = marks
        .iter()
        .map(|&k| {
            let e = &d.elements[k * PER_ELEMENT + 4..k * PER_ELEMENT + 8];
            [((e[0] - view[0] - m) * scale).floor().max(0.0), ((e[1] - view[1] - m) * scale).floor().max(0.0), ((e[2] - view[0] + m) * scale).ceil().min(pw), ((e[3] - view[1] + m) * scale).ceil().min(ph)]
        })
        .filter(|g| g[2] > g[0] && g[3] > g[1])
        .collect();

    let meet = |a: &[f32; 4], b: &[f32; 4]| a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
    let union = |a: [f32; 4], b: &[f32; 4]| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])];
    let mut boxes: Vec<[f32; 4]> = cuts.iter().map(|c| [c[0] as f32, c[1] as f32, (c[0] + c[2]) as f32, (c[1] + c[3]) as f32]).collect();
    // A box that grows may reach another glass, or another box: again, a
    // few times at most (each time something joins, there is one less).
    for _ in 0..16 {
        let mut grew = false;
        for b in boxes.iter_mut() {
            for g in &reach {
                if meet(b, g) && !(g[0] >= b[0] && g[1] >= b[1] && g[2] <= b[2] && g[3] <= b[3]) {
                    *b = union(*b, g);
                    grew = true;
                }
            }
        }
        let mut merged: Vec<[f32; 4]> = Vec::with_capacity(boxes.len());
        for b in boxes {
            match merged.iter_mut().find(|o| meet(o, &b)) {
                Some(o) => {
                    *o = union(*o, &b);
                    grew = true;
                }
                None => merged.push(b),
            }
        }
        boxes = merged;
        if !grew {
            break;
        }
    }
    let area: f32 = boxes.iter().map(|b| (b[2] - b[0]) * (b[3] - b[1])).sum();
    (area < 0.7 * pw * ph).then(|| boxes.iter().map(|b| [b[0] as u32, b[1] as u32, (b[2] - b[0]) as u32, (b[3] - b[1]) as u32]).collect())
}

fn merge_cuts(rects: impl Iterator<Item = [f32; 4]>) -> Vec<[f32; 4]> {
    let area = |b: &[f32; 4]| (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    let join = |a: &[f32; 4], b: &[f32; 4]| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])];
    let mut out: Vec<[f32; 4]> = Vec::new();
    for r in rects.filter(|b| b[2] > b[0] && b[3] > b[1]) {
        out.push(r);
        loop {
            let mut best: Option<(f32, usize, usize)> = None;
            for i in 0..out.len() {
                for j in i + 1..out.len() {
                    let cost = area(&join(&out[i], &out[j])) - area(&out[i]) - area(&out[j]);
                    if best.is_none_or(|b| cost < b.0) {
                        best = Some((cost, i, j));
                    }
                }
            }
            match best {
                Some((cost, i, j)) if cost < 4096.0 || out.len() > MOST_CUTS => {
                    out[i] = join(&out[i], &out[j]);
                    out.swap_remove(j);
                }
                _ => break,
            }
        }
    }
    out
}

/// The texture a frame is painted in: the swapchain's, or one lent by the platform.
enum Frame {
    Surface(wgpu::SurfaceTexture),
    Lent(usize, wgpu::Texture),
}

fn gpu_time_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("PLEAMAR_GPU_TIME").is_some() && timing_enabled())
}

thread_local! {
    static GPU_TIME: std::cell::Cell<(f32, u32)> = const { std::cell::Cell::new((0.0, 0)) };
}

/// `PLEAMAR_DEBUG_STRETCHES`: read once.
fn debug_stretches() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("PLEAMAR_DEBUG_STRETCHES").is_some())
}

/// `PLEAMAR_TIMING=1`: read once, not on every frame of every sheet.
pub fn timing_enabled() -> bool {
    static TIMING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TIMING.get_or_init(|| std::env::var_os("PLEAMAR_TIMING").is_some())
}

thread_local! {
    static TIMING: std::cell::Cell<(f32, f32, f32, f32, f32, f32)> = const { std::cell::Cell::new((0.0, 0.0, 0.0, 0.0, 0.0, 0.0)) };
}

impl Sheet {
    /// Its monitor's refresh in mHz, for pacing: 60 Hz where the compositor
    /// does not tell it (a headless output says 0, and a period of 1 mHz put
    /// the render to sleep for a thousand seconds after its first frame).
    pub fn pace_mhz(&self) -> i32 {
        if self.mhz > 0 { self.mhz } else { 60_000 }
    }

    /// Its size in pixels.
    pub fn px(&self) -> (u32, u32) {
        self.px
    }

    /// Where the mouse gets in. The rest of the surface, even if it is its
    /// own, lets the click through to what is underneath.
    pub fn update_input_region(&self, rects: &[[i32; 4]]) {
        self.window.update_input_region(rects);
    }

    pub fn cursor(&self, c: Cursor) {
        self.window.cursor(c);
    }

    /// Request the capture of what is behind: after presenting, the one from
    /// the next time the compositor paints; otherwise, the one from when
    /// something changes behind.
    /// On which monitor it is and where on it, if the system says.
    pub fn desktop_place(&self) -> Option<(String, (i32, i32))> {
        self.window.desktop_place()
    }

    pub fn request_backdrop(&mut self, on_change: bool) {
        // A lens that bends a picture (`lens = image`) has nothing to capture.
        if self.lens.as_ref().is_some_and(|x| x.fixed.is_some()) {
            return;
        }
        let Some(bounds) = self.glass_box else { return };
        if let Target::Frames(frames) = &mut self.target {
            frames.settle();
        }
        if self.window.capture_backdrop(bounds, on_change) {
            self.asked_box = bounds;
            self.capture_forced = false;
            self.capture = if on_change { BackdropCapture::Watching } else { BackdropCapture::AfterPresent };
            self.capture_asked = std::time::Instant::now();
            if !on_change {
                self.capture_taken = self.capture_asked;
            }
        }
    }

    pub fn cancel_backdrop(&mut self) {
        self.window.cancel_backdrop();
        self.capture = BackdropCapture::Idle;
        self.capture_forced = false;
    }

    pub fn update_blur_region(&self, rects: &[[i32; 4]]) {
        self.window.update_blur_region(rects);
    }

    pub fn keyboard(&self, t: Keyboard) {
        self.window.keyboard(t);
    }
}

/// Header, frame history and, at the end, the lens: whether there is a background to show.
pub const N_UNIFORMS: usize = 8 + 120 + 4 + 4;

#[cfg(test)]
mod tests {
    #[test]
    fn a_press_lights_only_the_glass_that_catches_it() {
        // Issue 109: a press made every open sheet paint whole, glass or not.
        let el = |b: [f32; 4], glass: f32, ripple: f32, highlight: f32| {
            let mut e = vec![0.0f32; PER_ELEMENT];
            e[4..8].copy_from_slice(&b);
            (e[40], e[57], e[63]) = (glass, ripple, highlight);
            e
        };
        let mut d = DrawList::default();
        d.elements.extend(el([0.0, 0.0, 1920.0, 40.0], 0.0, 0.0, 0.0));
        assert_eq!(d.lit_by_a_press(true), Some(vec![]), "no glass: nothing to light, nothing to paint");
        d.elements.extend(el([10.0, 5.0, 110.0, 35.0], 1.0, 0.0, 0.0));
        assert_eq!(d.lit_by_a_press(true), Some(vec![]), "glass without highlight or ripple catches nothing");
        d.elements.extend(el([200.0, 5.0, 300.0, 35.0], 1.0, 0.0, 1.0));
        d.elements.extend(el([400.0, 5.0, 500.0, 35.0], 1.0, 1.0, 0.0));
        assert_eq!(d.lit_by_a_press(false), Some(vec![[198.0, 3.0, 302.0, 37.0]]), "the light: the glass with highlight");
        assert_eq!(d.lit_by_a_press(true), Some(vec![[198.0, 3.0, 302.0, 37.0], [398.0, 3.0, 502.0, 37.0]]), "and the ripple: the one that ripples too");
        d.offscreen_groups.push((2..3, 0));
        assert_eq!(d.lit_by_a_press(true), None, "on a layer of its own: all of it");
    }

    use super::*;
    use crate::shapes::{Affine, FlatShape};

    /// A bar round the screen lit at its left end and at its right end is
    /// two pieces, not the screen; what overlaps is one; and never more than four.
    #[test]
    fn cuts_stay_few_and_apart() {
        let left = [0.0, 0.0, 64.0, 1080.0];
        let right = [1910.0, 0.0, 1920.0, 1080.0];
        assert_eq!(merge_cuts([left, right].into_iter()), vec![left, right]);
        assert_eq!(merge_cuts([[0.0, 0.0, 100.0, 100.0], [50.0, 50.0, 150.0, 150.0]].into_iter()), vec![[0.0, 0.0, 150.0, 150.0]]);
        let many: Vec<[f32; 4]> = (0..9).map(|k| [k as f32 * 200.0, 0.0, k as f32 * 200.0 + 20.0, 20.0]).collect();
        let cuts = merge_cuts(many.iter().copied());
        assert!(cuts.len() <= MOST_CUTS);
        for b in &many {
            assert!(cuts.iter().any(|c| c[0] <= b[0] && c[1] <= b[1] && c[2] >= b[2] && c[3] >= b[3]), "{b:?} is left out");
        }
        assert!(merge_cuts([[5.0, 5.0, 5.0, 9.0]].into_iter()).is_empty());
    }

    fn area(f: &[[f32; 4]]) -> f32 {
        f.iter().map(|r| (r[2] - r[0]) * (r[3] - r[1])).sum()
    }

    fn flat(kind: u8, mx: f32, my: f32, radius: f32, ex: f32, ey: f32, scale: f32) -> FlatShape {
        FlatShape { kind, cx: 0.0, cy: 0.0, mx, my, radius, rotation: 0.0, ex, ey, stroke: 0.0, affine: Affine { m: [scale, 0.0, 0.0, scale], t: [0.0, 0.0] } }
    }

    /// The blur's rows never reach outside the shape, wherever it sits between pixels, and they leave
    /// nothing out more than a pixel and a half inside its edge. Vitreus's Island at 70 % (a 425 × 38 box
    /// with 13 px corners, its top at y = 8 and its middle at a fraction of a pixel) showed the blur up to
    /// a pixel and a half past its corners, with the strips measured at the origin and rounded outwards.
    #[test]
    fn rows_stay_inside_and_cover() {
        let shapes = [flat(1, 212.6, 18.9, 13.2, 1.0, 1.0, 1.0), flat(1, 303.75, 27.0, 18.9, 1.0, 1.0, 1.0), flat(0, 0.0, 0.0, 30.0, 1.0, 0.4, 1.5), flat(1, 60.0, 20.0, 20.0, 1.0, 1.0, 2.0)];
        for p in shapes {
            for (ox, oy) in [(960.0, 26.9), (960.37, 35.0), (100.5, 100.5), (33.25, 71.8)] {
                let rows = rows_on_pixels(&p, ox, oy).expect("a rounded box or an ellipse has a formula");
                let d = |x: f32, y: f32| p.distance_with(x - ox, y - oy, &[]);
                let covered = |x: f32, y: f32| rows.iter().any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]);
                for r in &rows {
                    assert!(r.iter().all(|v| v.fract() == 0.0), "{r:?} is not on whole pixels");
                    let mut y = r[1];
                    while y < r[3] {
                        let mut x = r[0];
                        while x < r[2] {
                            for (cx, cy) in [(x, y), (x + 1.0, y), (x, y + 1.0), (x + 1.0, y + 1.0)] {
                                assert!(d(cx, cy) < 1e-3, "pixel {x},{y} of {r:?} reaches outside (kind {}, at {ox},{oy})", p.kind);
                            }
                            x += 1.0;
                        }
                        y += 1.0;
                    }
                }
                let (w, h) = (p.mx.max(p.radius * p.ex) * p.affine.m[0], p.my.max(p.radius * p.ey) * p.affine.m[3]);
                let mut y = (oy - h).floor();
                while y < oy + h {
                    let mut x = (ox - w).floor();
                    while x < ox + w {
                        if d(x + 0.5, y + 0.5) < -1.5 {
                            assert!(covered(x, y), "pixel {x},{y} is well inside and not blurred (kind {}, at {ox},{oy})", p.kind);
                        }
                        x += 1.0;
                    }
                    y += 1.0;
                }
            }
        }
    }

    #[test]
    fn strip_formula_matches_search() {
        for p in [flat(1, 210.0, 110.0, 36.0, 1.0, 1.0, 1.0), flat(0, 0.0, 0.0, 46.0, 1.0, 1.0, 1.0), flat(0, 0.0, 0.0, 30.0, 1.0, 0.4, 1.5), flat(1, 60.0, 20.0, 20.0, 1.0, 1.0, 2.0)] {
            let exact = strips_of(&p, &[]);
            // A rotation that does not rotate: forces searching for the edge by bisection.
            let mut rotated = p;
            rotated.rotation = 1e-7;
            let searched = strips_of(&rotated, &[]);
            // Against the real area: the formula falls a hair inside (the
            // narrowest of each strip) and the search a hair outside.
            let s = p.affine.m[0] * p.affine.m[3];
            let real = s * if p.kind == 1 {
                4.0 * p.mx * p.my - (4.0 - std::f32::consts::PI) * p.radius * p.radius
            } else {
                std::f32::consts::PI * p.radius * p.radius * p.ex * p.ey
            };
            for (which, a) in [("formula", area(&exact)), ("search", area(&searched))] {
                assert!((a - real).abs() / real < 0.04, "kind {}, {which}: {a} versus {real}", p.kind);
            }
            // And the straight sides merge: a box is not a hundred strips.
            if p.kind == 1 {
                assert!(exact.len() < searched.len() + 4, "{} versus {}", exact.len(), searched.len());
            }
        }
    }
}

/// Work sent to the card, and whether it has finished. Asked with a plain
/// poll, never by waiting for a submission's number: wgpu checks that number
/// against its fence in a way that breaks when two threads poll at once (the
/// render, and each monitor of pleamar-wm), and the render went down with it.
#[derive(Clone, Debug)]
pub struct Sent(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Sent {
    /// Everything sent to that queue until now.
    pub fn after(queue: &wgpu::Queue) -> Sent {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let set = flag.clone();
        queue.on_submitted_work_done(move || set.store(true, std::sync::atomic::Ordering::Release));
        Sent(flag)
    }

    pub fn done(&self, device: &wgpu::Device) -> bool {
        if !self.finished() {
            let _ = device.poll(wgpu::PollType::Poll);
        }
        self.finished()
    }

    /// Whether it is known to have finished, without asking the card: the
    /// notice is set by whichever thread asks next (each monitor does, every frame).
    pub fn finished(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Until it has finished, or that long. Asking costs (some 20 µs each
    /// time), so first it sleeps most of what the waits of this thread
    /// usually take, and only then asks, every quarter of a millisecond: with
    /// the card slow, asking every 0.2 ms from the start was the monitor's
    /// thread's biggest cost.
    pub fn wait(&self, device: &wgpu::Device, most: std::time::Duration) {
        let start = std::time::Instant::now();
        let usual = WAIT_USUALLY.with(|u| u.get());
        let first = (usual * 0.8 - 0.3).clamp(0.0, most.as_secs_f64() * 1000.0);
        if first > 0.0 && !self.finished() {
            std::thread::sleep(std::time::Duration::from_secs_f64(first / 1000.0));
        }
        while !self.done(device) && start.elapsed() < most {
            std::thread::sleep(std::time::Duration::from_micros(250));
        }
        let took = start.elapsed().as_secs_f64() * 1000.0;
        WAIT_USUALLY.with(|u| u.set(u.get() * 0.9 + took * 0.1));
    }
}

thread_local! {
    /// How long this thread's waits for the card usually take, in ms.
    static WAIT_USUALLY: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// Sums of live windows' frames on the card, submitted, for the thread that
/// waits for them (`Gpu::check_thumbnails`): the submission, where they are
/// read back, whether that mapping came, and which copy of which buffer each is.
#[cfg(target_os = "linux")]
pub struct Summing {
    index: wgpu::SubmissionIndex,
    out: wgpu::Buffer,
    mapped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    checks: Vec<(u64, u64)>,
}

#[cfg(target_os = "linux")]
impl Summing {
    /// Waits for the card, at most 50 ms, and tells the thumbnails service
    /// each sum; one not read in time is a new picture, as without the sum.
    fn read(self, device: &wgpu::Device) {
        use std::sync::atomic::Ordering;
        let done = device.poll(wgpu::PollType::Wait { submission_index: Some(self.index.clone()), timeout: Some(std::time::Duration::from_millis(50)) }).is_ok();
        // (The mapping is finished by a poll after the work it waited for.)
        if done && !self.mapped.load(Ordering::Acquire) {
            let _ = device.poll(wgpu::PollType::Poll);
        }
        if !done || !self.mapped.load(Ordering::Acquire) {
            return self.unanswered();
        }
        let sums: Vec<u64> = match self.out.slice(..).get_mapped_range() {
            Ok(d) => d.chunks_exact(8).map(|b| u64::from_ne_bytes(b.try_into().unwrap())).collect(),
            Err(_) => return self.unanswered(),
        };
        self.out.unmap();
        for ((ticket, n), sum) in self.checks.iter().zip(sums) {
            crate::platform::thumbnail_checked(*ticket, *n, Some(sum));
        }
    }

    fn unanswered(self) {
        for (ticket, n) in self.checks {
            crate::platform::thumbnail_checked(ticket, n, None);
        }
    }
}
