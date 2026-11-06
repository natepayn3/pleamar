//! Text and images: everything that ends up being a piece of the atlas.
//!
//! Text is shaped with `cosmic-text` —ligatures, right-to-left
//! languages, emoji, fallback fonts— and each glyph is painted once, at the
//! scale of the finest sheet, into an atlas it shares with the images. The
//! three pieces (`cosmic-text`, `image`, `resvg`) are pure Rust and exist on
//! Linux, Windows and macOS; the only thing that depends on the system is finding an
//! icon by its name, and the platform answers that.

use crate::scene::{ToRender, TextAlign, Style, ImageSource};
use std::collections::HashSet;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use cosmic_text::{Align, Attrs, Buffer, CacheKey, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Weight, Wrap};
use std::collections::HashMap;

/// The atlas's width, and the height it starts with.
pub const ATLAS_SIZE: u32 = 2048;
/// The tallest it grows to when what is seen does not fit: 2048×8192, 64 MB.
pub const ATLAS_MOST: u32 = 8192;

/// A piece of the atlas, in pixels. With `page` above 0, not the atlas but
/// a picture of its own: one too big for the atlas, the whole of that texture.
#[derive(Clone, Copy, Debug)]
pub struct AtlasSlot {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub page: u32,
}

impl AtlasSlot {
    /// Where it is, in the atlas's pixels: the shader divides by the size the
    /// atlas has, so a piece keeps its `uv` when the atlas grows.
    pub fn uv(&self) -> [f32; 4] {
        [self.x as f32, self.y as f32, (self.x + self.width) as f32, (self.y + self.height) as f32]
    }
}

/// A glyph already placed, relative to the corner of its text and in logical pixels.
#[derive(Clone, Copy)]
pub struct PlacedGlyph {
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    /// An emoji brings its own color; a letter is a mask that gets tinted.
    pub colored: bool,
}

pub struct Layout {
    pub glyphs: Vec<PlacedGlyph>,
    pub size: (f32, f32),
    /// Where the text cursor falls before each letter: (byte, x). Only for the
    /// first line: it is what a field to type into needs.
    pub cursors: Vec<(usize, f32)>,
    /// The same for every line, with where the line is: what selecting a
    /// text with the mouse needs.
    pub lines: Vec<TextLine>,
}

/// A line of a laid-out text: its top and bottom, and where the cursor falls
/// before each letter of it (byte of the whole text, x), the end included.
pub struct TextLine {
    pub top: f32,
    pub bottom: f32,
    pub cursors: Vec<(usize, f32)>,
}

impl Layout {
    /// The x of the cursor in front of byte `b`.
    pub fn x_at_byte(&self, b: usize) -> f32 {
        self.cursors.iter().rev().find(|(k, _)| *k <= b).or(self.cursors.first()).map_or(0.0, |c| c.1)
    }

    /// The byte closest to an x: where a click falls.
    pub fn byte_at_x(&self, x: f32) -> usize {
        self.cursors.iter().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map_or(0, |c| c.0)
    }

    /// The byte closest to a point, on any line: above the first one is the
    /// start, below the last one the end.
    pub fn byte_at(&self, x: f32, y: f32) -> usize {
        let Some(first) = self.lines.first() else { return 0 };
        if y < first.top {
            return first.cursors.first().map_or(0, |c| c.0);
        }
        let line = self.lines.iter().find(|l| y < l.bottom).unwrap_or_else(|| self.lines.last().unwrap());
        if y >= line.bottom {
            return line.cursors.last().map_or(0, |c| c.0);
        }
        line.cursors.iter().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map_or(0, |c| c.0)
    }

    /// The boxes (x0, y0, x1, y1) that cover the bytes from `a` to `b`, one
    /// per line they touch.
    pub fn spans(&self, a: usize, b: usize) -> Vec<[f32; 4]> {
        let at = |l: &TextLine, k: usize| l.cursors.iter().rev().find(|c| c.0 <= k).or(l.cursors.first()).map_or(0.0, |c| c.1);
        let mut out = Vec::new();
        for l in &self.lines {
            let (Some(s), Some(e)) = (l.cursors.first(), l.cursors.last()) else { continue };
            if b <= s.0 || a > e.0 || (a == e.0 && e.0 > s.0) {
                continue;
            }
            let (x0, x1) = (at(l, a.max(s.0)), at(l, b.min(e.0)));
            if x1 > x0 {
                out.push([x0, l.top, x1, l.bottom]);
            }
        }
        out
    }
}

/// Everything that decides how a text turns out. Two texts with the same key are
/// the same layout, and it is enough to make it: it is what travels to the workshop.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct LayoutKey {
    text: String,
    family: Option<&'static str>,
    px: u32,
    weight: u16,
    line_height: u32,
    width: Option<u32>,
    align: TextAlign,
    max_lines: Option<usize>,
    /// As wide as its longest line, not its `width` (`Style::tight`).
    tight: bool,
    /// Stretches of it (byte ranges) bold, italic or code: `crate::markdown`.
    marks: Vec<(u32, u32, u8)>,
}

impl LayoutKey {
    pub fn new(text: &str, e: &Style, width: Option<f32>) -> LayoutKey {
        LayoutKey {
            text: text.to_owned(), family: e.family, px: e.px.to_bits(), weight: e.weight, line_height: e.line_height.to_bits(),
            width: width.map(|a| a.round().max(1.0) as u32), align: e.align, max_lines: e.max_lines, tight: e.tight, marks: Vec::new(),
        }
    }

    /// The same, at another size: a text whose size is worked out each frame.
    pub fn sized(mut self, px: f32) -> LayoutKey {
        self.px = px.to_bits();
        self
    }

    /// The same, in another weight: a text whose weight is worked out each frame.
    /// The same, in another family (`None`: the system sans).
    pub fn familied(mut self, family: Option<&'static str>) -> LayoutKey {
        self.family = family;
        self
    }

    pub fn weighted(mut self, weight: u16) -> LayoutKey {
        self.weight = weight;
        self
    }

    /// The same, with another most of lines (`None`: as many as it has).
    pub fn lined(mut self, lines: Option<usize>) -> LayoutKey {
        self.max_lines = lines;
        self
    }

    /// A text already without its marks, and what they marked.
    pub fn marked(text: String, marks: Vec<(u32, u32, u8)>, e: &Style, width: Option<f32>) -> LayoutKey {
        LayoutKey { text, marks, ..LayoutKey::new("", e, width) }
    }
}

/// Divides the atlas into shelves: rows as tall as the first thing that fell into them.
struct Shelves {
    rows: Vec<(u32, u32, u32)>, // y, height, how far it is filled
    next_y: u32,
    /// How tall the atlas is, and the most the card lets it grow to.
    height: u32,
    most: u32,
    /// That something that could fit found no room: a taller atlas would help.
    short: bool,
}

impl Shelves {
    fn new() -> Self {
        Shelves { rows: Vec::new(), next_y: 1, height: ATLAS_SIZE, most: ATLAS_SIZE, short: false }
    }

    /// Empty again, as tall as it was: an atlas never shrinks.
    fn emptied(&self) -> Self {
        Shelves { rows: Vec::new(), next_y: 1, height: self.height, most: self.most, short: false }
    }

    /// Twice as tall, if it may be and it would help.
    fn grow(&mut self) -> bool {
        if !self.short || self.height >= self.most {
            return false;
        }
        self.height = (self.height * 2).min(self.most);
        true
    }

    fn request(&mut self, width: u32, height: u32) -> Option<AtlasSlot> {
        if width == 0 || height == 0 || width > ATLAS_SIZE - 2 || height > ATLAS_SIZE - 2 {
            return None;
        }
        let (w, h) = (width + 1, height + 1); // one pixel of air so filtering does not bleed
        let row = self.rows.iter_mut().find(|(_, fh, fx)| *fh >= h && *fh <= h + h / 3 + 2 && fx + w <= ATLAS_SIZE);
        let (y, x) = match row {
            Some((y, _, fx)) => {
                let x = *fx;
                *fx += w;
                (*y, x)
            }
            None => {
                if self.next_y + h > self.height || w > ATLAS_SIZE {
                    self.short = true;
                    return None;
                }
                let y = self.next_y;
                self.next_y += h;
                self.rows.push((y, h, 1 + w));
                (y, 1)
            }
        };
        Some(AtlasSlot { x, y, width, height, page: 0 })
    }
}

/// Whoever shapes and paints. It lives on its own thread —the workshop—: reading the system
/// fonts, shaping a paragraph or decoding a PNG can take a while, and
/// none of that may cost the render a frame.
struct Typesetter {
    fonts: FontSystem,
    swash: SwashCache,
    shelves: Shelves,
    glyphs: HashMap<CacheKey, Option<(AtlasSlot, i32, i32, bool)>>,
    /// What has been painted since the last delivery: where and what (premultiplied RGBA).
    pending_upload: Vec<(AtlasSlot, Vec<u8>)>,
    /// Live windows' frames on the card, for the render to make small into the atlas.
    pending_fit: Vec<Fit>,
    /// Pictures too big for the atlas, each to a texture of its own: which, how big, what.
    pending_pictures: Vec<(u32, (u32, u32), Vec<u8>)>,
    /// The last picture number handed out; 0 is the atlas.
    last_picture: u32,
    /// How many real pixels per logical pixel it is painted at: that of the finest sheet.
    scale: f32,
    exhausted: bool,
    /// The last frame of each live window drawn (`thumbnails:3`, at that size):
    /// which version, and where. Asked for again with nothing new, it is left as it is.
    frames: HashMap<LiveKey, (u64, AtlasSlot)>,
    /// The scenes' own fonts already read.
    fonts_read: HashSet<std::path::PathBuf>,
}

impl Typesetter {
    fn new() -> Self {
        let t0 = std::time::Instant::now();
        let fonts = FontSystem::new();
        println!("text   · {} system fonts in {} ms", fonts.db().len(), t0.elapsed().as_millis());
        Typesetter { fonts, swash: SwashCache::new(), shelves: Shelves::new(), glyphs: HashMap::new(), pending_upload: Vec::new(), pending_fit: Vec::new(), pending_pictures: Vec::new(), last_picture: 0, scale: 1.0, exhausted: false, frames: HashMap::new(), fonts_read: HashSet::new() }
    }

    /// When the scale changes, everything painted stops being valid.
    fn clear(&mut self, scale: f32) {
        self.scale = scale;
        self.shelves = self.shelves.emptied();
        self.glyphs.clear();
        self.pending_upload.clear();
        self.pending_fit.clear();
        self.pending_pictures.clear();
        self.exhausted = false;
        self.frames.clear();
    }

    fn lay_out(&mut self, c: &LayoutKey) -> Layout {
        let (px, line_height, width) = (f32::from_bits(c.px), f32::from_bits(c.line_height), c.width.map(|a| a as f32));
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(px, px * line_height));
        buffer.set_wrap(if width.is_some() { Wrap::WordOrGlyph } else { Wrap::None });
        if let Some(n) = c.max_lines {
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(n)));
        }
        buffer.set_size(width, None);
        let attrs = Attrs::new().family(c.family.map_or(Family::SansSerif, Family::Name)).weight(Weight(c.weight));
        let align = match c.align {
            TextAlign::Left => Align::Left,
            TextAlign::Center => Align::Center,
            TextAlign::Right => Align::Right,
        };
        if c.marks.is_empty() {
            buffer.set_text(&c.text, &attrs, Shaping::Advanced, Some(align));
        } else {
            // The marked stretches with their looks, and what is between them plain.
            let look = |m: u8| {
                let mut a = attrs.clone();
                if m & crate::markdown::BOLD != 0 {
                    a = a.weight(Weight((c.weight + 300).min(900)));
                }
                if m & crate::markdown::ITALIC != 0 {
                    a = a.style(cosmic_text::Style::Italic);
                }
                if m & crate::markdown::CODE != 0 {
                    a = a.family(Family::Monospace);
                }
                a
            };
            let mut spans = Vec::with_capacity(c.marks.len() * 2 + 1);
            let mut at = 0usize;
            for &(from, to, m) in &c.marks {
                let (from, to) = (from as usize, to as usize);
                if from > at {
                    spans.push((&c.text[at..from], attrs.clone()));
                }
                spans.push((&c.text[from..to], look(m)));
                at = to;
            }
            if at < c.text.len() {
                spans.push((&c.text[at..], attrs.clone()));
            }
            buffer.set_rich_text(spans, &attrs, Shaping::Advanced, Some(align));
        }
        buffer.shape_until_scroll(&mut self.fonts, false);

        let s = self.scale;
        let mut glyphs = Vec::new();
        let (mut w, mut h) = (0f32, 0f32);
        let mut full = false;
        let mut cursors: Vec<(usize, f32)> = Vec::new();
        let mut lines: Vec<TextLine> = Vec::new();
        // A letter's place counts from the start of its paragraph: where each
        // one starts in the whole text, split as the buffer split it.
        let starts: Vec<usize> = cosmic_text::LineIter::new(&c.text).map(|(r, _)| r.start).collect();
        for run in buffer.layout_runs() {
            let from = starts.get(run.line_i).copied().unwrap_or(0);
            let mut line: Vec<(usize, f32)> = run.glyphs.iter().map(|g| (from + g.start, g.x)).collect();
            let end = run.glyphs.iter().map(|g| (from + g.end, g.x + g.w)).max_by_key(|e| e.0);
            line.push(end.unwrap_or((from, 0.0)));
            line.sort_by_key(|k| k.0);
            lines.push(TextLine { top: run.line_top, bottom: run.line_top + run.line_height, cursors: line });
            if run.line_i == 0 && cursors.is_empty() {
                cursors = run.glyphs.iter().map(|g| (g.start, g.x)).collect();
                cursors.push((c.text.len(), run.line_w));
                cursors.sort_by_key(|k| k.0);
            }
            w = w.max(run.line_w);
            h = h.max(run.line_top + run.line_height);
            for g in run.glyphs {
                let f = g.physical((0.0, 0.0), s);
                let Some((slot, left, top, colored)) = self.glyph(f.cache_key, &mut full) else { continue };
                let x = (f.x + left) as f32;
                let y = ((run.line_y * s).round() as i32 + f.y - top) as f32;
                glyphs.push(PlacedGlyph { rect: [x / s, y / s, slot.width as f32 / s, slot.height as f32 / s], uv: slot.uv(), colored });
            }
        }
        self.exhausted |= full;
        // Without a fixed width, the alignment is relative to whatever the text itself measures.
        if cursors.is_empty() {
            cursors.push((0, 0.0));
        }
        // Tight, left-aligned: what its longest line takes, up to its width (its letters are already where they go).
        let across = match width {
            Some(fixed) if c.tight && c.align == TextAlign::Left => w.min(fixed),
            Some(fixed) => fixed,
            None => w,
        };
        Layout { glyphs, size: (across, h), cursors, lines }
    }

    fn glyph(&mut self, key: CacheKey, full: &mut bool) -> Option<(AtlasSlot, i32, i32, bool)> {
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let mut no_space = false;
        let painted = self.swash.get_image_uncached(&mut self.fonts, key).and_then(|img| {
            let (w, h) = (img.placement.width, img.placement.height);
            if w == 0 || h == 0 {
                return None;
            }
            let rgba: Vec<u8> = match img.content {
                // A mask: premultiplied white, which the shader will tint.
                SwashContent::Mask => img.data.iter().flat_map(|a| [*a, *a, *a, *a]).collect(),
                SwashContent::Color => img.data.chunks_exact(4).flat_map(|p| {
                    let a = p[3] as u16;
                    [(p[0] as u16 * a / 255) as u8, (p[1] as u16 * a / 255) as u8, (p[2] as u16 * a / 255) as u8, p[3]]
                }).collect(),
                SwashContent::SubpixelMask => return None,
            };
            let Some(slot) = self.shelves.request(w, h) else {
                *full = true;
                no_space = true;
                return None;
            };
            self.upload(slot, rgba);
            Some((slot, img.placement.left, img.placement.top, matches!(img.content, SwashContent::Color)))
        });
        // A space has no ink; a letter waiting for atlas space does. Only the
        // former can be cached as absent.
        if !no_space {
            self.glyphs.insert(key, painted);
        }
        painted
    }

    fn reserve(&mut self, width: u32, height: u32) -> Option<AtlasSlot> {
        let slot = self.shelves.request(width, height);
        self.exhausted |= slot.is_none();
        slot
    }

    fn upload(&mut self, slot: AtlasSlot, rgba: Vec<u8>) {
        // Reused atlas pixels need fresh transparent gutters too; otherwise
        // linear filtering picks up ink from the previous generation.
        let padded = AtlasSlot { x: slot.x - 1, y: slot.y - 1, width: slot.width + 2, height: slot.height + 2, page: 0 };
        let stride = padded.width as usize * 4;
        let mut pixels = vec![0; stride * padded.height as usize];
        for (y, row) in rgba.chunks_exact(slot.width as usize * 4).enumerate() {
            let start = (y + 1) * stride + 4;
            pixels[start..start + row.len()].copy_from_slice(row);
        }
        self.pending_upload.push((padded, pixels));
    }

    /// A picture too big for the atlas, to a texture of its own. With `into`,
    /// a new version of one: into the same texture, if it is as big.
    fn picture(&mut self, px: (u32, u32), rgba: Vec<u8>, into: Option<AtlasSlot>) -> AtlasSlot {
        let page = match into {
            Some(s) if s.page > 0 && (s.width, s.height) == px => s.page,
            _ => {
                self.last_picture += 1;
                self.last_picture
            }
        };
        self.pending_pictures.push((page, px, rgba));
        AtlasSlot { x: 0, y: 0, width: px.0, height: px.1, page }
    }

    /// The real pixels an image asks for, and whether that is too big for the
    /// atlas: then it is a picture of its own (`picture_px`). Declared `0, 0`,
    /// the file's own: its pixels, or an SVG's size times the scale.
    fn image_px(&self, path: Option<&std::path::Path>, (w, h): (u32, u32), budget: u64) -> ((u32, u32), bool) {
        let px = if (w, h) == (0, 0) {
            fit_within(path.and_then(|p| self.own_px(p)).unwrap_or((1, 1)), self.shelves.most)
        } else {
            (((w as f32) * self.scale).round().max(1.0) as u32, ((h as f32) * self.scale).round().max(1.0) as u32)
        };
        let big = px.0 > ATLAS_SIZE - 2 || px.1 > ATLAS_SIZE - 2 || px.0 as u64 * px.1 as u64 > budget;
        (px, big)
    }

    fn own_px(&self, path: &std::path::Path) -> Option<(u32, u32)> {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
            let tree = resvg::usvg::Tree::from_data(&std::fs::read(path).ok()?, &resvg::usvg::Options::default()).ok()?;
            let s = tree.size();
            return Some(((s.width() * self.scale).round().max(1.0) as u32, (s.height() * self.scale).round().max(1.0) as u32));
        }
        image::image_dimensions(path).ok().filter(|&(w, h)| w > 0 && h > 0)
    }

    /// A picture of its own keeps the file's pixels: its box, the shape it
    /// was declared with, as big as the picture fits in it at one pixel per
    /// pixel. Never made bigger here (filtering on the card does that as
    /// well), only smaller if it is bigger than it is ever painted, and no
    /// bigger than the card allows. An SVG has no pixels: painted at `want`.
    fn picture_px(&self, path: &std::path::Path, want: (u32, u32)) -> (u32, u32) {
        let svg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        let px = match image::image_dimensions(path) {
            Ok((w, h)) if !svg => {
                let k = (w as f32 / want.0 as f32).min(h as f32 / want.1 as f32).min(1.0);
                (((want.0 as f32) * k).round().max(1.0) as u32, ((want.1 as f32) * k).round().max(1.0) as u32)
            }
            _ => want,
        };
        fit_within(px, self.shelves.most)
    }

    /// A scene's images, at the logical size they ask for times the scale. An
    /// SVG is painted at that exact size; a PNG is scaled down if it is too big.
    fn load_images(&mut self, images: &[(ImageSource, (u32, u32))]) -> (Vec<Option<AtlasSlot>>, Vec<Option<Animation>>) {
        let mut animations = Vec::with_capacity(images.len());
        let slots = images
            .iter()
            .map(|(source, size)| {
                let path = match source {
                    ImageSource::File(r) | ImageSource::Pixels(r) => Some(r.clone()),
                    ImageSource::Icon(name) => crate::platform::icon(name),
                    // It is requested once it is known what the text says: `live_image`.
                    ImageSource::Live(_) => {
                        animations.push(None);
                        return None;
                    }
                };
                let (big_px, big) = self.image_px(path.as_deref(), *size, u64::MAX);
                let px = fit_side(big_px);
                // A file that moves —a GIF, an animated PNG or WebP—: each frame to
                // the atlas, with the moment it ends.
                let sharp = matches!(source, ImageSource::Pixels(_));
                if let Some(frames) = path.as_ref().and_then(|r| animation_frames(r, px, sharp)) {
                    let mut slots = Vec::new();
                    let mut ends = Vec::new();
                    let mut t = 0.0;
                    for (rgba, delay) in frames {
                        let Some(slot) = self.reserve(px.0, px.1) else { break };
                        self.upload(slot, rgba);
                        t += delay;
                        slots.push(slot);
                        ends.push(t);
                    }
                    let first = slots.first().copied();
                    animations.push((slots.len() > 1).then_some(Animation { frames: slots, ends }));
                    return first;
                }
                animations.push(None);
                let px = match (big, &path) {
                    (true, Some(r)) => self.picture_px(r, big_px),
                    _ => px,
                };
                // One that does not fit is not reported here: the atlas is rebuilt,
                // taller if it may be, and only what still does not fit is said.
                let Some(rgba) = path.as_ref().and_then(|r| rasterize_image(r, px, sharp)) else {
                    eprintln!("image  · cannot load {source:?}");
                    return None;
                };
                if big {
                    return Some(self.picture(px, rgba, None));
                }
                let slot = self.reserve(px.0, px.1)?;
                self.upload(slot, rgba);
                Some(slot)
            })
            .collect();
        (slots, animations)
    }

    /// How many real pixels a live window's frame takes in the atlas: its size
    /// times the scale, but never more than a quarter of it.
    fn live_px(&self, (w, h): (u32, u32)) -> (u32, u32) {
        let (w, h) = (((w as f32) * self.scale).max(1.0), ((h as f32) * self.scale).max(1.0));
        let edge = (ATLAS_SIZE - 2) as f32;
        let k = ((LIVE_BUDGET as f32) / (w * h)).sqrt().min(edge / w).min(edge / h).min(1.0);
        ((w * k).round().max(1.0) as u32, (h * k).round().max(1.0) as u32)
    }

    /// An image some data has asked for: an icon by its name, or a path.
    /// With `into`, a new version of one already loaded, painted over it.
    /// One bigger than a quarter of the atlas (a wallpaper at a monitor's
    /// size) is a picture of its own.
    fn load_live(&mut self, name: &str, size: (u32, u32), into: Option<AtlasSlot>) -> Option<AtlasSlot> {
        if name.starts_with("thumbnails:") || name.starts_with("capture:") {
            return self.load_frame(name, size, self.live_px(size), into);
        }
        let path = if std::path::Path::new(name).is_absolute() { Some(std::path::PathBuf::from(name)) } else { crate::platform::icon(name) }?;
        let (px, big) = self.image_px(Some(&path), size, LIVE_BUDGET);
        let px = if big { self.picture_px(&path, px) } else { px };
        let rgba = rasterize_image(&path, px, false)?;
        if big {
            return Some(self.picture(px, rgba, into));
        }
        let slot = match into {
            Some(s) if s.page == 0 && (s.width, s.height) == px => s,
            _ => self.reserve(px.0, px.1)?,
        };
        self.upload(slot, rgba);
        Some(slot)
    }

    /// A live window's frame (`thumbnails.live`) or a frozen monitor
    /// (`capture.freeze`): from memory, no file, made
    /// the size it is drawn at. Several versions asked for while one was being
    /// made are one: what is kept is always the last, so the rest find it drawn.
    fn load_frame(&mut self, name: &str, size: (u32, u32), px: (u32, u32), into: Option<AtlasSlot>) -> Option<AtlasSlot> {
        let frame = crate::platform::thumbnail_frame(name)?;
        let key = (name.to_owned(), size);
        let slot = match into {
            Some(s) if (s.width, s.height) == px => s,
            _ => self.reserve(px.0, px.1)?,
        };
        let place = |s: AtlasSlot| (s.x, s.y, s.width, s.height);
        if self.frames.get(&key).is_some_and(|&(v, s)| v == frame.version && place(s) == place(slot)) {
            return Some(slot);
        }
        if frame.card.is_some() {
            self.pending_fit.push(Fit::new(slot, frame.clone(), px));
        } else {
            self.upload(slot, fit_frame(&frame, px));
        }
        self.frames.insert(key, (frame.version, slot));
        Some(slot)
    }

    /// Everything that is seen, into an empty atlas. If it does not fit, into
    /// one twice as tall, while the card allows it: what is seen grows the
    /// atlas, not what was seen once and is gone.
    fn rebuild(&mut self, working: &WorkingSet, images: &[(ImageSource, (u32, u32))]) -> Snapshot {
        loop {
            let snapshot = self.rebuild_once(working, images);
            if !self.exhausted || !self.shelves.grow() {
                return snapshot;
            }
            println!("text   · what is seen does not fit the atlas: it grows to {ATLAS_SIZE}×{}", self.shelves.height);
        }
    }

    fn rebuild_once(&mut self, working: &WorkingSet, images: &[(ImageSource, (u32, u32))]) -> Snapshot {
        self.clear(self.scale);
        // Text gets space before pictures. Old covers, search results and
        // animated font sizes are not part of the new generation.
        let layouts = working.layouts.iter().map(|key| (key.clone(), Arc::new(self.lay_out(key)))).collect();
        let (images, animations) = self.load_images(images);
        let live = working.live.iter().map(|(name, size)| {
            let (base, version) = live_name(name);
            let slot = self.load_live(base, *size, None);
            ((base.to_owned(), *size), (slot, version.to_owned()))
        }).collect();
        Snapshot { layouts, images, animations, live }
    }

    fn work(&mut self, job: Job) -> Option<Parcel> {
        self.exhausted = false;
        let delivery = match job {
            Job::Clear(scale) => {
                self.clear(scale);
                return None;
            }
            Job::Fonts(list) => {
                for path in list {
                    if self.fonts_read.insert(path.clone()) {
                        let before = self.fonts.db().len();
                        if let Err(e) = self.fonts.db_mut().load_font_file(&path) {
                            eprintln!("text   · {}: {e}", path.display());
                        } else if self.fonts.db().len() == before {
                            eprintln!("text   · {}: no font in it", path.display());
                        }
                    }
                }
                return None;
            }
            Job::Limit(most) => {
                self.shelves.most = most.clamp(ATLAS_SIZE, ATLAS_MOST);
                return None;
            }
            Job::Layout(key, generation) => {
                let layout = Arc::new(self.lay_out(&key));
                Delivery::Layout { key, layout, generation }
            }
            Job::Images(list, generation) => {
                let (slots, animations) = self.load_images(&list);
                Delivery::Images { slots, animations, generation }
            }
            Job::Live(name, size, generation) => {
                let slot = self.load_live(&name, size, None);
                Delivery::Live { name, size, slot, generation }
            }
            Job::Reload(name, size, into, generation) => {
                let slot = self.load_live(&name, size, Some(into));
                Delivery::Live { name, size, slot, generation }
            }
            Job::Rebuild(working, images, generation) => {
                let snapshot = self.rebuild(&working, &images);
                Delivery::Rebuilt { snapshot, working, generation }
            }
        };
        Some(Parcel { delivery, pending_upload: std::mem::take(&mut self.pending_upload), pending_fit: std::mem::take(&mut self.pending_fit), pending_pictures: std::mem::take(&mut self.pending_pictures), exhausted: self.exhausted, height: self.shelves.height, most: self.shelves.most })
    }
}

// ── the workshop and its counter ────────────────────────────────

type LiveKey = (String, (u32, u32));

/// One thing a stretch of the drawing noted in the working set: a text's layout or a live picture.
#[derive(Clone)]
pub enum WorkingKey {
    Layout(LayoutKey),
    Live(LiveKey),
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct WorkingSet {
    layouts: HashSet<LayoutKey>,
    live: HashSet<LiveKey>,
}

pub struct Snapshot {
    layouts: HashMap<LayoutKey, Arc<Layout>>,
    images: Vec<Option<AtlasSlot>>,
    animations: Vec<Option<Animation>>,
    live: HashMap<LiveKey, (Option<AtlasSlot>, String)>,
}

fn live_name(name: &str) -> (&str, &str) {
    match name.rsplit_once('?') {
        Some((base, version)) if !base.is_empty() && !version.is_empty() && version.bytes().all(|c| c.is_ascii_digit()) => (base, version),
        _ => (name, ""),
    }
}

enum Job {
    Layout(LayoutKey, u32),
    Images(Vec<(ImageSource, (u32, u32))>, u32),
    Live(String, (u32, u32), u32),
    /// A new version of a live image, over the place the old one had.
    Reload(String, (u32, u32), AtlasSlot, u32),
    /// New atlas, at this scale. Everything from previous generations is thrown away.
    Clear(f32),
    /// The tallest the card lets the atlas be.
    Limit(u32),
    Rebuild(WorkingSet, Vec<(ImageSource, (u32, u32))>, u32),
    /// Fonts a scene brings with it: read once, and kept for the next scenes.
    Fonts(Vec<std::path::PathBuf>),
}

/// What the workshop hands back, with whatever it painted along the way.
pub enum Delivery {
    Layout { key: LayoutKey, layout: Arc<Layout>, generation: u32 },
    Images { slots: Vec<Option<AtlasSlot>>, animations: Vec<Option<Animation>>, generation: u32 },
    Live { name: String, size: (u32, u32), slot: Option<AtlasSlot>, generation: u32 },
    Rebuilt { snapshot: Snapshot, working: WorkingSet, generation: u32 },
}

pub struct Parcel {
    pub delivery: Delivery,
    pub pending_upload: Vec<(AtlasSlot, Vec<u8>)>,
    pub pending_fit: Vec<Fit>,
    pub pending_pictures: Vec<(u32, (u32, u32), Vec<u8>)>,
    pub exhausted: bool,
    /// How tall the atlas the workshop paints into is, and the most it let it grow.
    pub height: u32,
    pub most: u32,
}

/// The render's side: it asks, does not wait, and meanwhile shows what it had.
pub struct Texts {
    to_workshop: Sender<Job>,
    layouts: HashMap<LayoutKey, Arc<Layout>>,
    requested: HashSet<LayoutKey>,
    /// The last layout shown at each place in the scene: it is what is
    /// seen while the new one arrives, instead of a gap.
    last: HashMap<usize, Arc<Layout>>,
    images: Vec<Option<AtlasSlot>>,
    /// The ones that move: their frames and when each one ends.
    animations: Vec<Option<Animation>>,
    /// Where each image of the scene comes from, to know which ones depend on a text.
    sources: Vec<(ImageSource, (u32, u32))>,
    /// The ones the data have asked for, by name and size. `None`: it was looked for and does not exist.
    live: HashMap<(String, (u32, u32)), Option<AtlasSlot>>,
    live_requested: HashSet<(String, (u32, u32))>,
    /// Which version of each live image was asked for: what follows the last
    /// `?` of its name, when it is a number (`/tmp/cover.jpg?3`).
    live_version: HashMap<(String, (u32, u32)), String>,
    /// The last thing each live image showed, while the new one arrives.
    last_live: HashMap<usize, AtlasSlot>,
    pub pending_upload: Vec<(AtlasSlot, Vec<u8>)>,
    pub pending_fit: Vec<Fit>,
    /// Pictures of their own to make or paint again on the card, and those to let go.
    pub pending_pictures: Vec<(u32, (u32, u32), Vec<u8>)>,
    pub dropped_pictures: Vec<u32>,
    /// The pictures of their own the card has been given and not told to let go.
    pictures: HashSet<u32>,
    /// How many frames in a row each live image that is a picture of its own has not been seen.
    unseen: HashMap<LiveKey, u32>,
    generation: u32,
    scale: f32,
    working: WorkingSet,
    /// While a stretch of the drawing is being put together to be kept
    /// (`DrawList::compose`): what it notes in the working set, to note it again
    /// in the frames it is copied instead of being put together.
    recording: Option<Vec<WorkingKey>>,
    exhausted: bool,
    rebuilding: bool,
    /// An oversized working set is reported once, not rebuilt every frame.
    too_large: Option<WorkingSet>,
    /// How tall the atlas has to be on the card, and the most the card allows.
    height: u32,
    most: u32,
}

impl Texts {
    /// Starts the workshop. The first thing it does is read the system fonts,
    /// so it is best to call it as early as possible.
    pub fn open(to_render: Sender<ToRender>) -> Texts {
        let (to_workshop, jobs) = channel::<Job>();
        std::thread::Builder::new()
            .name("workshop".into())
            .spawn(move || {
                let mut t = Typesetter::new();
                for job in jobs {
                    let Some(parcel) = t.work(job) else { continue };
                    // Through the usual channel, which also wakes the render up if it was asleep.
                    if to_render.send(ToRender::Workshop(Box::new(parcel))).is_err() {
                        return;
                    }
                }
            })
            .unwrap();
        Self::new(to_workshop)
    }

    fn new(to_workshop: Sender<Job>) -> Self {
        Texts { to_workshop, layouts: HashMap::new(), requested: HashSet::new(), last: HashMap::new(), images: Vec::new(), animations: Vec::new(), sources: Vec::new(), live: HashMap::new(), live_requested: HashSet::new(), live_version: HashMap::new(), last_live: HashMap::new(), pending_upload: Vec::new(), pending_fit: Vec::new(), pending_pictures: Vec::new(), dropped_pictures: Vec::new(), pictures: HashSet::new(), unseen: HashMap::new(), generation: 0, scale: 1.0, working: WorkingSet::default(), recording: None, exhausted: false, rebuilding: false, too_large: None, height: ATLAS_SIZE, most: ATLAS_SIZE }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// How tall the atlas has to be on the card: it is never made smaller.
    pub fn atlas_height(&self) -> u32 {
        self.height
    }

    /// The tallest atlas the card allows. What did not fit before is tried again.
    pub fn allow_atlas(&mut self, most: u32) {
        if most == self.most {
            return;
        }
        self.most = most;
        let _ = self.to_workshop.send(Job::Limit(most));
        if self.too_large.take().is_some() {
            self.exhausted = true;
        }
    }

    /// Whether that image is pixel art (`pixels "…"`): drawn bigger, each of
    /// its pixels stays a square.
    pub fn is_pixel_art(&self, k: usize) -> bool {
        matches!(self.sources.get(k), Some((ImageSource::Pixels(_), _)))
    }

    /// New scene or new scale: new atlas. What came before stops being valid, and
    /// with it whatever was being shown.
    pub fn reset(&mut self, scale: f32, images: &[(ImageSource, (u32, u32))], fonts: &[std::path::PathBuf]) {
        self.generation += 1;
        self.scale = scale;
        self.layouts.clear();
        self.requested.clear();
        self.last.clear();
        self.images.clear();
        self.animations.clear();
        self.sources = images.to_vec();
        self.live.clear();
        self.live_requested.clear();
        self.live_version.clear();
        self.last_live.clear();
        self.pending_upload.clear();
        self.pending_fit.clear();
        self.pending_pictures.clear();
        self.unseen.clear();
        self.sweep_pictures();
        self.working = WorkingSet::default();
        self.exhausted = false;
        self.rebuilding = false;
        self.too_large = None;
        // Before anything is laid out with them.
        if !fonts.is_empty() {
            let _ = self.to_workshop.send(Job::Fonts(fonts.to_vec()));
        }
        let _ = self.to_workshop.send(Job::Clear(scale));
        if !images.is_empty() {
            let _ = self.to_workshop.send(Job::Images(images.to_vec(), self.generation));
        }
    }

    pub fn receive(&mut self, p: Parcel) {
        // Even from an old generation: the workshop's atlas is that tall now.
        self.height = self.height.max(p.height);
        let generation = match &p.delivery {
            Delivery::Layout { generation, .. } | Delivery::Images { generation, .. } | Delivery::Live { generation, .. } | Delivery::Rebuilt { generation, .. } => *generation,
        };
        if generation != self.generation {
            return; // from an atlas that no longer exists
        }
        self.exhausted |= p.exhausted;
        if matches!(&p.delivery, Delivery::Rebuilt { .. }) {
            // Old UVs and new atlas pixels become visible together. Until
            // this parcel arrived, the previous frame remained usable.
            self.pending_upload.clear();
            self.pending_fit.clear();
        }
        self.pending_upload.extend(p.pending_upload);
        self.pending_fit.extend(p.pending_fit);
        self.pictures.extend(p.pending_pictures.iter().map(|(page, ..)| *page));
        self.pending_pictures.extend(p.pending_pictures);
        match p.delivery {
            Delivery::Layout { key, layout, .. } => {
                self.requested.remove(&key);
                if p.exhausted {
                    return; // keep the previous complete layout until the rebuild
                }
                if self.layouts.len() > 512 {
                    self.layouts.clear();
                }
                self.layouts.insert(key, layout);
            }
            Delivery::Images { slots, animations, .. } => (self.images, self.animations) = (slots, animations),
            Delivery::Live { name, size, slot, .. } => {
                self.live_requested.remove(&(name.clone(), size));
                self.live.insert((name, size), slot);
            }
            Delivery::Rebuilt { snapshot, working, .. } => {
                self.layouts = snapshot.layouts;
                self.images = snapshot.images;
                self.animations = snapshot.animations;
                self.live.clear();
                self.live_version.clear();
                for (key, (slot, version)) in snapshot.live {
                    self.live.insert(key.clone(), slot);
                    self.live_version.insert(key, version);
                }
                self.requested.clear();
                self.live_requested.clear();
                self.last.clear();
                self.last_live.clear();
                self.rebuilding = false;
                // Rebuilt before the workshop knew how tall the card lets it be:
                // tried again, taller, rather than given up on.
                let retry = p.exhausted && p.most < self.most;
                self.exhausted = retry;
                self.too_large = (p.exhausted && !retry).then_some(working);
                if p.exhausted && !retry {
                    eprintln!("text   · visible text and images exceed the {ATLAS_SIZE}×{} atlas; reduce their size or number", self.height);
                }
            }
        }
        self.sweep_pictures();
    }

    /// The pictures of their own nothing shows any more, nor will while it is
    /// on its way, are let go: each one is megabytes on the card.
    fn sweep_pictures(&mut self) {
        let mut used: HashSet<u32> = self.pending_pictures.iter().map(|(page, ..)| *page).collect();
        used.extend(self.images.iter().chain(self.live.values()).flatten().chain(self.last_live.values()).map(|s| s.page));
        let gone: Vec<u32> = self.pictures.iter().copied().filter(|p| !used.contains(p)).collect();
        for page in gone {
            self.pictures.remove(&page);
            self.dropped_pictures.push(page);
        }
    }

    /// From now on, what is noted in the working set is also kept apart.
    pub fn record(&mut self) {
        self.recording = Some(Vec::new());
    }
    /// What was noted since `record`, and no more keeping apart.
    pub fn recorded(&mut self) -> Vec<WorkingKey> {
        self.recording.take().unwrap_or_default()
    }
    /// Noted again, as a stretch copied from the frame before would have.
    pub fn note_again(&mut self, keys: &[WorkingKey]) {
        for k in keys {
            match k {
                WorkingKey::Layout(l) => {
                    self.working.layouts.insert(l.clone());
                }
                WorkingKey::Live(v) => {
                    self.working.live.insert(v.clone());
                }
            }
        }
    }

    pub fn begin_frame(&mut self) {
        self.working.layouts.clear();
        self.working.live.clear();
    }

    pub fn end_frame(&mut self) {
        // A live picture of its own not seen for a while (the wallpaper that
        // faded out) is forgotten, and asked for again if it comes back.
        let seen: HashSet<LiveKey> = self.working.live.iter().map(|(name, size)| (live_name(name).0.to_owned(), *size)).collect();
        let mut forgotten = Vec::new();
        for (key, slot) in &self.live {
            let Some(page) = slot.filter(|s| s.page > 0).map(|s| s.page) else { continue };
            let n = self.unseen.entry(key.clone()).or_insert(0);
            *n = if seen.contains(key) { 0 } else { *n + 1 };
            if *n > UNSEEN_FRAMES {
                forgotten.push((key.clone(), page));
            }
        }
        let live = &self.live;
        self.unseen.retain(|k, _| live.contains_key(k));
        if !forgotten.is_empty() {
            for (key, page) in forgotten {
                self.live.remove(&key);
                self.live_version.remove(&key);
                self.unseen.remove(&key);
                self.last_live.retain(|_, s| s.page != page);
            }
            self.sweep_pictures();
        }
        if self.exhausted && !self.rebuilding && self.too_large.as_ref() != Some(&self.working) {
            self.generation += 1;
            self.rebuilding = true;
            self.exhausted = false;
            let _ = self.to_workshop.send(Job::Rebuild(self.working.clone(), self.sources.clone(), self.generation));
        }
    }

    /// A text's layout, if it is ready; if not, it is ordered and the last one
    /// seen at that place is returned. `place` is the position in the draw list.
    pub fn layout(&mut self, place: usize, key: LayoutKey) -> Option<Arc<Layout>> {
        if let Some(r) = &mut self.recording {
            r.push(WorkingKey::Layout(key.clone()));
        }
        self.working.layouts.insert(key.clone());
        if let Some(m) = self.layouts.get(&key) {
            self.last.insert(place, m.clone());
            return Some(m.clone());
        }
        if !self.rebuilding && self.requested.insert(key.clone()) {
            let _ = self.to_workshop.send(Job::Layout(key, self.generation));
        }
        self.last.get(&place).cloned()
    }

    /// Image number `k` at `clock` seconds: if it moves, the frame of that
    /// moment, and when it changes to the next one (to wake up then, and not
    /// before). Looping forever; with `still`, the first frame and no change.
    pub fn frame(&mut self, k: usize, texts: &[String], clock: f32, still: bool) -> (Option<AtlasSlot>, Option<f32>) {
        let Some(Some(a)) = self.animations.get(k) else { return (self.image(k, texts), None) };
        if still {
            return (a.frames.first().copied(), None);
        }
        let total = *a.ends.last().unwrap_or(&1.0);
        let lap = (clock / total).floor();
        let t = clock - lap * total;
        let i = a.ends.iter().position(|e| t < *e).unwrap_or(a.ends.len() - 1);
        (Some(a.frames[i]), Some(lap * total + a.ends[i]))
    }

    /// Image number `k` of the scene. If it comes from a live text, the one that
    /// text says now; while it arrives, the one that was there. An empty text is none.
    ///
    /// A name that ends in `?` and a number is a version of the same image: a
    /// file that is written again under the same name. The new version is
    /// painted where the old one was, so changing it costs the atlas nothing.
    pub fn image(&mut self, k: usize, texts: &[String]) -> Option<AtlasSlot> {
        let Some((ImageSource::Live(t), size)) = self.sources.get(k) else {
            return self.images.get(k).copied().flatten();
        };
        let name = texts.get(t.0 as usize).filter(|n| !n.is_empty())?;
        if let Some(r) = &mut self.recording {
            r.push(WorkingKey::Live((name.clone(), *size)));
        }
        self.working.live.insert((name.clone(), *size));
        let (base, version) = live_name(name);
        let key = (base.to_string(), *size);
        let known = self.live.get(&key).copied();
        if !self.rebuilding && known.is_some() && self.live_version.get(&key).map(String::as_str) != Some(version) {
            self.live_version.insert(key.clone(), version.to_string());
            let _ = self.to_workshop.send(match known {
                Some(Some(slot)) => Job::Reload(key.0.clone(), key.1, slot, self.generation),
                _ => Job::Live(key.0.clone(), key.1, self.generation),
            });
        }
        match known {
            Some(Some(slot)) => {
                self.last_live.insert(k, slot);
                Some(slot)
            }
            Some(None) => None,
            None => {
                if self.rebuilding {
                    return self.last_live.get(&k).copied();
                }
                self.live_version.insert(key.clone(), version.to_string());
                if self.live_requested.insert(key.clone()) {
                    let _ = self.to_workshop.send(Job::Live(key.0, key.1, self.generation));
                }
                self.last_live.get(&k).copied()
            }
        }
    }
}

/// An image that moves: its frames in the atlas, and the moment (seconds
/// into a lap) each one ends.
#[derive(Clone)]
pub struct Animation {
    frames: Vec<AtlasSlot>,
    ends: Vec<f32>,
}

/// How much of the atlas one animation can take, in pixels: a quarter of it.
/// One that asks for more keeps every other frame —or one in three…—, each
/// lasting what the ones it stands for lasted, so it runs at the same pace.
const ANIMATION_BUDGET: u64 = (ATLAS_SIZE as u64 * ATLAS_SIZE as u64) / 4;

/// The most one live image takes, in real pixels: a quarter of the atlas too.
const LIVE_BUDGET: u64 = (ATLAS_SIZE as u64 * ATLAS_SIZE as u64) / 4;

/// How many frames a live picture of its own goes unseen before it is let go.
const UNSEEN_FRAMES: u32 = 120;

/// A GIF, an animated PNG or an animated WebP, frame by frame, fitted like
/// `rasterize_image` and premultiplied, each with how long it lasts. `None`
/// for anything else, or for one with a single frame.
fn animation_frames(path: &std::path::Path, px: (u32, u32), sharp: bool) -> Option<Vec<(Vec<u8>, f32)>> {
    use image::AnimationDecoder;
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let data = std::fs::read(path).ok()?;
    let reader = std::io::Cursor::new(&data);
    let frames: Vec<image::Frame> = match ext.as_str() {
        "gif" => image::codecs::gif::GifDecoder::new(reader).ok()?.into_frames().collect_frames().ok()?,
        "webp" => {
            let d = image::codecs::webp::WebPDecoder::new(reader).ok()?;
            if !d.has_animation() {
                return None;
            }
            d.into_frames().collect_frames().ok()?
        }
        "png" | "apng" => {
            let d = image::codecs::png::PngDecoder::new(reader).ok()?;
            if !d.is_apng().ok()? {
                return None;
            }
            d.apng().ok()?.into_frames().collect_frames().ok()?
        }
        _ => return None,
    };
    if frames.len() < 2 {
        return None;
    }
    let per_frame = px.0 as u64 * px.1 as u64;
    let step = ((per_frame * frames.len() as u64).div_ceil(ANIMATION_BUDGET)).max(1) as usize;
    if step > 1 {
        eprintln!("image  · {}: {} frames do not fit at {}×{}: one in {step} is kept", path.display(), frames.len(), px.0, px.1);
    }
    let mut out = Vec::new();
    for chunk in frames.chunks(step) {
        // The chunk's first frame, lasting what the whole chunk lasted.
        let delay: f32 = chunk.iter().map(|f| {
            let (n, d) = f.delay().numer_denom_ms();
            let ms = n as f32 / d.max(1) as f32;
            // Browsers take 0 and 10 ms as «as fast as possible», which means 100 ms.
            if ms <= 10.0 { 0.1 } else { ms / 1000.0 }
        }).sum();
        out.push((fit(chunk[0].buffer().clone(), px, sharp), delay));
    }
    Some(out)
}

/// An RGBA image fitted into `px` without distorting, centred, premultiplied.
/// `sharp`: each of its pixels stays a square, for pixel art made bigger.
fn fit(img: image::RgbaImage, px: (u32, u32), sharp: bool) -> Vec<u8> {
    let k = (px.0 as f32 / img.width() as f32).min(px.1 as f32 / img.height() as f32);
    let (w, h) = (((img.width() as f32 * k).round() as u32).clamp(1, px.0), ((img.height() as f32 * k).round() as u32).clamp(1, px.1));
    let reduced = image::imageops::resize(&img, w, h, if sharp { image::imageops::FilterType::Nearest } else { image::imageops::FilterType::Triangle });
    let mut canvas = image::RgbaImage::new(px.0, px.1);
    image::imageops::overlay(&mut canvas, &reduced, ((px.0 - w) / 2) as i64, ((px.1 - h) / 2) as i64);
    canvas.pixels().flat_map(|p| {
        let a = p[3] as u16;
        [(p[0] as u16 * a / 255) as u8, (p[1] as u16 * a / 255) as u8, (p[2] as u16 * a / 255) as u8, p[3]]
    }).collect()
}

/// A window's frame —premultiplied BGRA, as the compositor copies it— to
/// premultiplied RGBA at exactly `px`, fitted like `fit`. Made smaller by
/// averaging each box of pixels: it reads every pixel once, whatever the size
/// (a frame is megabytes, and comes again many times a second). The rows of a
/// box are summed first, a whole row at a time —a plain add over contiguous
/// bytes, which the compiler turns into vector code—, then each box across
/// that sum. Made bigger, a window smaller than where it is drawn, it is
/// filtered like any image.
/// A live window's frame on the card (`thumbnails.live`), made small by the
/// render the way `fit_frame` does it here: into `inner`, centred in the slot,
/// the rest of `padded` (the slot and its gutter) left transparent.
pub struct Fit {
    pub padded: AtlasSlot,
    pub inner: AtlasSlot,
    pub frame: Arc<crate::platform::ThumbnailFrame>,
}

impl Fit {
    fn new(slot: AtlasSlot, frame: Arc<crate::platform::ThumbnailFrame>, px: (u32, u32)) -> Fit {
        let (fw, fh) = (frame.width.max(1), frame.height.max(1));
        let k = (px.0 as f32 / fw as f32).min(px.1 as f32 / fh as f32);
        let (w, h) = (((fw as f32 * k).round() as u32).clamp(1, px.0), ((fh as f32 * k).round() as u32).clamp(1, px.1));
        let inner = AtlasSlot { x: slot.x + (px.0 - w) / 2, y: slot.y + (px.1 - h) / 2, width: w, height: h, page: 0 };
        Fit { padded: AtlasSlot { x: slot.x - 1, y: slot.y - 1, width: slot.width + 2, height: slot.height + 2, page: 0 }, inner, frame }
    }
}

fn fit_frame(f: &crate::platform::ThumbnailFrame, px: (u32, u32)) -> Vec<u8> {
    let (fw, fh) = (f.width.max(1), f.height.max(1));
    let k = (px.0 as f32 / fw as f32).min(px.1 as f32 / fh as f32);
    let (w, h) = (((fw as f32 * k).round() as u32).clamp(1, px.0), ((fh as f32 * k).round() as u32).clamp(1, px.1));
    let reduced: Vec<u8> = if w <= fw && h <= fh {
        // The source pixels that make output pixel `i` of `n`, along a side of `of`.
        let span = |i: u32, n: u32, of: u32| {
            let a = i * of / n;
            (a as usize, ((i + 1) * of / n).max(a + 1) as usize)
        };
        let boxes: Vec<(usize, usize)> = (0..w).map(|x| span(x, w, fw)).collect();
        let row = fw as usize * 4;
        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
        let mut rows = vec![0u32; row];
        for y in 0..h {
            let (y0, y1) = span(y, h, fh);
            rows.fill(0);
            for line in f.pixels[y0 * row..y1 * row].chunks_exact(row) {
                for (sum, &b) in rows.iter_mut().zip(line) {
                    *sum += b as u32;
                }
            }
            for &(x0, x1) in &boxes {
                let mut sum = [0u32; 4];
                for p in rows[x0 * 4..x1 * 4].chunks_exact(4) {
                    for c in 0..4 {
                        sum[c] += p[c];
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as u32;
                let mean = |c: u32| ((c + n / 2) / n) as u8;
                out.extend_from_slice(&[mean(sum[2]), mean(sum[1]), mean(sum[0]), if f.opaque { 255 } else { mean(sum[3]) }]);
            }
        }
        out
    } else {
        let rgba = |i: usize| {
            let p = &f.pixels[i..i + 4];
            [p[2], p[1], p[0], if f.opaque { 255 } else { p[3] }]
        };
        let full: Vec<u8> = (0..fw as usize * fh as usize).flat_map(|i| rgba(i * 4)).collect();
        let Some(img) = image::RgbaImage::from_raw(fw, fh, full) else { return vec![0; px.0 as usize * px.1 as usize * 4] };
        // Already premultiplied: filtered as it is, and not multiplied again.
        image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle).into_raw()
    };
    let mut canvas = vec![0; px.0 as usize * px.1 as usize * 4];
    let (dx, dy) = (((px.0 - w) / 2) as usize, ((px.1 - h) / 2) as usize);
    for (y, row) in reduced.chunks_exact(w as usize * 4).enumerate() {
        let start = ((dy + y) * px.0 as usize + dx) * 4;
        canvas[start..start + row.len()].copy_from_slice(row);
    }
    canvas
}

/// Premultiplied RGBA, at exactly `px`, fitted without distorting.
/// No side longer than the atlas holds: one that asks for more —a sheet as
/// wide as the atlas, any big picture at a scale of 2— is made smaller, the
/// same shape, and shown a little softer rather than not at all.
fn fit_side(px: (u32, u32)) -> (u32, u32) {
    fit_within(px, ATLAS_SIZE - 2)
}

/// No side longer than `edge`, the same shape.
fn fit_within((w, h): (u32, u32), edge: u32) -> (u32, u32) {
    if w <= edge && h <= edge {
        return (w, h);
    }
    let k = (edge as f32 / w as f32).min(edge as f32 / h as f32);
    (((w as f32 * k).round() as u32).clamp(1, edge), ((h as f32 * k).round() as u32).clamp(1, edge))
}

fn rasterize_image(path: &std::path::Path, px: (u32, u32), sharp: bool) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    let is_svg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    if is_svg {
        let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
        let mut canvas = resvg::tiny_skia::Pixmap::new(px.0, px.1)?;
        let t = tree.size();
        let k = (px.0 as f32 / t.width()).min(px.1 as f32 / t.height());
        let (dx, dy) = ((px.0 as f32 - t.width() * k) * 0.5, (px.1 as f32 - t.height() * k) * 0.5);
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(k, k).post_translate(dx, dy), &mut canvas.as_mut());
        return Some(canvas.take()); // tiny-skia already premultiplies
    }
    Some(fit(image::load_from_memory(&data).ok()?.to_rgba8(), px, sharp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, bgra: [u8; 4], opaque: bool) -> crate::platform::ThumbnailFrame {
        crate::platform::ThumbnailFrame { version: 1, width, height, pixels: bgra.repeat((width * height) as usize), opaque, card: None }
    }

    #[test]
    fn a_frozen_monitor_is_looked_up_without_its_version() {
        // What capture.freeze answers; the base is what capture::frame finds.
        assert_eq!(live_name("capture:HEADLESS-1?12"), ("capture:HEADLESS-1", "12"));
        assert_eq!(live_name("capture:DP-1"), ("capture:DP-1", ""));
        assert_eq!(live_name("capture:DP-1?"), ("capture:DP-1?", ""));
    }

    #[test]
    fn a_live_frame_made_smaller_is_the_mean_of_each_box() {
        // Boxes of uneven sizes (36 into 7, 26 into 5), every pixel different:
        // each output pixel is the rounded mean of exactly the pixels of its box.
        let (fw, fh, w, h) = (36u32, 26u32, 7u32, 5u32);
        let mut n = 7u32;
        let pixels: Vec<u8> = (0..fw * fh * 4).map(|_| { n = n.wrapping_mul(1_103_515_245).wrapping_add(12_345); (n >> 16) as u8 }).collect();
        for opaque in [true, false] {
            let f = crate::platform::ThumbnailFrame { version: 1, width: fw, height: fh, pixels: pixels.clone(), opaque, card: None };
            let out = fit_frame(&f, (w, h));
            for y in 0..h {
                for x in 0..w {
                    let (y0, y1) = (y * fh / h, (y + 1) * fh / h);
                    let (x0, x1) = (x * fw / w, (x + 1) * fw / w);
                    let count = (y1 - y0) * (x1 - x0);
                    let mean = |c: usize| {
                        let sum: u32 = (y0..y1).flat_map(|sy| (x0..x1).map(move |sx| (sy, sx))).map(|(sy, sx)| pixels[((sy * fw + sx) * 4) as usize + c] as u32).sum();
                        ((sum + count / 2) / count) as u8
                    };
                    let want = [mean(2), mean(1), mean(0), if opaque { 255 } else { mean(3) }];
                    let i = ((y * w + x) * 4) as usize;
                    assert_eq!(out[i..i + 4], want, "pixel ({x}, {y}), opaque {opaque}");
                }
            }
        }
    }

    #[test]
    fn a_live_frame_is_fitted_like_any_image_and_turned_to_rgba() {
        // Wider than the place: fitted to its width, centred, the rest left clear.
        let out = fit_frame(&frame(40, 10, [10, 20, 30, 0], true), (20, 20));
        let at = |x: usize, y: usize| &out[(y * 20 + x) * 4..(y * 20 + x) * 4 + 4];
        assert_eq!(at(10, 10), [30, 20, 10, 255], "BGRA to RGBA, and XRGB is opaque whatever its alpha byte says");
        assert_eq!(at(10, 0), [0, 0, 0, 0], "outside the fitted frame, clear");
        // See-through, premultiplied: kept as it is, not multiplied again.
        let out = fit_frame(&frame(8, 8, [0, 0, 64, 128], false), (4, 4));
        assert_eq!(&out[0..4], [64, 0, 0, 128]);
        // Smaller than its place: made bigger, filled.
        let out = fit_frame(&frame(2, 2, [1, 2, 3, 255], false), (8, 8));
        assert_eq!(&out[(4 * 8 + 4) * 4..(4 * 8 + 4) * 4 + 4], [3, 2, 1, 255]);
    }

    fn spanish() -> LayoutKey {
        LayoutKey::new("Hoy · Nada todavía. Captura de pantalla copiada.", &Style::new(14.0, crate::scene::color(1.0, 1.0, 1.0)), None)
    }

    #[test]
    fn a_tight_text_is_as_wide_as_its_longest_line() {
        let mut t = Typesetter::new();
        let mut style = Style::new(14.0, crate::scene::color(1.0, 1.0, 1.0));
        let wide = t.lay_out(&LayoutKey::new("Ey", &style, Some(470.0)));
        assert_eq!(wide.size.0, 470.0, "without tight, its width");
        style.tight = true;
        let short = t.lay_out(&LayoutKey::new("Ey", &style, Some(470.0)));
        assert!(short.size.0 > 5.0 && short.size.0 < 40.0, "{}", short.size.0);
        // A long one breaks its lines at its width, and takes no more than it.
        let long = t.lay_out(&LayoutKey::new(&"palabra ".repeat(80), &style, Some(200.0)));
        assert!(long.size.0 <= 200.0 && long.size.0 > 150.0 && long.size.1 > 40.0, "{:?}", long.size);
    }

    #[test]
    fn glyphs_can_retry_after_atlas_space_is_reclaimed() {
        let mut t = Typesetter::new();
        let key = spanish();
        let expected = t.lay_out(&key).glyphs.len();
        assert!(expected > 30, "system font did not shape the Spanish fixture");
        t.clear(1.0);
        assert!(t.shelves.request(ATLAS_SIZE - 2, ATLAS_SIZE - 2).is_some());
        assert!(t.lay_out(&key).glyphs.is_empty());
        t.shelves = Shelves::new();
        assert_eq!(t.lay_out(&key).glyphs.len(), expected, "atlas exhaustion must not cache letters as nonexistent");
    }

    #[test]
    fn every_line_knows_its_bytes_for_selecting() {
        let mut t = Typesetter::new();
        // Two paragraphs, the second wrapped: bytes count from the start of the whole text.
        let text = "Primera línea\nuna segunda bastante más larga que se parte en dos";
        let m = t.lay_out(&LayoutKey::new(text, &Style::new(14.0, crate::scene::color(1.0, 1.0, 1.0)), Some(180.0)));
        assert!(m.lines.len() >= 3, "the second paragraph should wrap: {} lines", m.lines.len());
        let second = text.find("una").unwrap();
        assert_eq!(m.lines[1].cursors.first().unwrap().0, second);
        assert_eq!(m.lines.last().unwrap().cursors.last().unwrap().0, text.len());
        // A point on the second line falls there; above everything is the start, below the end.
        let l = &m.lines[1];
        assert_eq!(m.byte_at(0.0, (l.top + l.bottom) / 2.0), second);
        assert_eq!(m.byte_at(50.0, -20.0), 0);
        assert_eq!(m.byte_at(0.0, 1000.0), text.len());
        // Selecting across the paragraph break covers both lines, each in its own box.
        let spans = m.spans(2, second + 3);
        assert_eq!(spans.len(), 2);
        assert!(spans[0][1] < spans[1][1]);
    }

    #[test]
    fn atlas_slots_leave_room_for_gutters() {
        let mut shelves = Shelves::new();
        for (w, h) in [(0, 1), (1, 0), (ATLAS_SIZE, 1), (1, ATLAS_SIZE), (u32::MAX, 1)] {
            assert!(shelves.request(w, h).is_none());
        }
        let slot = shelves.request(ATLAS_SIZE - 2, ATLAS_SIZE - 2).unwrap();
        assert_eq!((slot.x + slot.width + 1, slot.y + slot.height + 1), (ATLAS_SIZE, ATLAS_SIZE));
        assert!(shelves.request(1, 1).is_none());
    }

    #[test]
    fn a_working_set_that_does_not_fit_grows_the_atlas_while_the_card_allows() {
        let mut t = Typesetter::new();
        // Two pictures of 2000², which a 2048² atlas cannot hold together.
        let fill = |t: &mut Typesetter| [t.reserve(2000, 2000), t.reserve(2000, 2000)];
        let set = WorkingSet::default();
        t.rebuild(&set, &[]);
        assert!(fill(&mut t)[1].is_none() && t.shelves.short);
        // Rebuilt as a frame that sees them: without the card's say, it stays.
        t.exhausted = true;
        assert!(!t.shelves.grow());
        t.work(Job::Limit(16384));
        assert_eq!(t.shelves.most, ATLAS_MOST);
        assert!(t.shelves.grow());
        assert_eq!(t.shelves.height, 2 * ATLAS_SIZE);
        t.clear(1.0);
        let slots = fill(&mut t);
        let second = slots[1].expect("a 2048×4096 atlas holds both");
        assert_eq!(second.uv()[1], second.y as f32, "uv is in pixels, the same whatever the height");
        // Emptied for a new scene, it keeps its height: the card's texture does not shrink.
        t.clear(1.0);
        assert_eq!(t.shelves.height, 2 * ATLAS_SIZE);
        // A piece wider than the atlas grows nothing.
        t.clear(1.0);
        assert!(t.reserve(ATLAS_SIZE, 10).is_none() && !t.shelves.grow());
    }

    #[test]
    fn the_render_learns_the_height_even_from_an_old_generation() {
        let (send, _jobs) = channel();
        let mut texts = Texts::new(send);
        texts.generation = 5;
        texts.receive(Parcel { delivery: Delivery::Live { name: "x".into(), size: (1, 1), slot: None, generation: 4 }, pending_upload: Vec::new(), pending_fit: Vec::new(), pending_pictures: Vec::new(), exhausted: false, height: 4096, most: ATLAS_SIZE });
        assert_eq!(texts.atlas_height(), 4096);
        texts.too_large = Some(WorkingSet::default());
        texts.allow_atlas(8192);
        assert!(texts.exhausted && texts.too_large.is_none(), "what did not fit is tried again in the taller atlas");
        // A rebuild the workshop made before it heard of the card's limit: tried again, not given up on.
        texts.exhausted = false;
        texts.rebuilding = true;
        let snapshot = Snapshot { layouts: HashMap::new(), images: Vec::new(), animations: Vec::new(), live: HashMap::new() };
        let generation = texts.generation;
        texts.receive(Parcel { delivery: Delivery::Rebuilt { snapshot, working: WorkingSet::default(), generation }, pending_upload: Vec::new(), pending_fit: Vec::new(), pending_pictures: Vec::new(), exhausted: true, height: ATLAS_SIZE, most: ATLAS_SIZE });
        assert!(texts.exhausted && texts.too_large.is_none() && !texts.rebuilding);
    }

    #[test]
    fn a_picture_wider_than_the_atlas_is_made_to_fit() {
        assert_eq!(fit_side((2048, 128)), (2046, 128));
        assert_eq!(fit_side((4032, 936)), (2046, 475));
        assert_eq!(fit_side((300, 200)), (300, 200));
        assert!(Shelves::new().request(2046, 128).is_some());
    }

    #[test]
    fn panoramic_live_images_fit_both_atlas_dimensions() {
        let t = Typesetter::new();
        for size in [(10_000, 1), (1, 10_000), (0, 0), (3840, 2160)] {
            let px = t.live_px(size);
            assert!(px.0 > 0 && px.1 > 0 && px.0 <= ATLAS_SIZE - 2 && px.1 <= ATLAS_SIZE - 2);
            assert!(Shelves::new().request(px.0, px.1).is_some());
        }
    }

    #[test]
    fn reused_pixels_include_transparent_gutters() {
        let mut t = Typesetter::new();
        let slot = t.reserve(2, 2).unwrap();
        t.upload(slot, vec![255; 16]);
        let (padded, rgba) = t.pending_upload.pop().unwrap();
        assert_eq!((padded.x, padded.y, padded.width, padded.height), (0, 0, 4, 4));
        for y in 0..4 {
            for x in 0..4 {
                let expected = if (1..3).contains(&x) && (1..3).contains(&y) { 255 } else { 0 };
                assert_eq!(&rgba[(y * 4 + x) * 4..(y * 4 + x + 1) * 4], &[expected; 4]);
            }
        }
    }

    fn deliver(t: &mut Typesetter, texts: &mut Texts, jobs: &std::sync::mpsc::Receiver<Job>) {
        for job in jobs.try_iter() {
            if let Some(parcel) = t.work(job) {
                texts.receive(parcel);
            }
        }
    }

    #[test]
    fn pressure_rebuild_is_atomic_and_old_deliveries_cannot_overwrite_it() {
        let (send, jobs) = channel();
        let mut texts = Texts::new(send);
        let mut t = Typesetter::new();
        let old = LayoutKey { text: "Previous".into(), ..spanish() };
        texts.layout(0, old.clone());
        deliver(&mut t, &mut texts, &jobs);
        let previous = texts.layout(0, old).unwrap();
        texts.pending_upload.clear();
        t.shelves.rows.clear();
        t.shelves.next_y = ATLAS_SIZE;
        texts.begin_frame();
        texts.layout(0, spanish());
        deliver(&mut t, &mut texts, &jobs);
        texts.end_frame();
        assert!(texts.rebuilding);
        assert!(Arc::ptr_eq(&texts.layout(0, spanish()).unwrap(), &previous));
        let stale = t.work(Job::Layout(spanish(), texts.generation - 1)).unwrap();
        let rebuilt = t.work(jobs.recv().unwrap()).unwrap();
        assert!(!rebuilt.exhausted);
        texts.receive(rebuilt);
        let complete = texts.layout(0, spanish()).unwrap();
        assert!(complete.glyphs.len() > 30);
        let uploads = texts.pending_upload.len();
        texts.receive(stale);
        assert_eq!(texts.pending_upload.len(), uploads);
        assert!(Arc::ptr_eq(&texts.layout(0, spanish()).unwrap(), &complete));
        assert!(!texts.rebuilding);
    }

    #[test]
    fn changing_live_images_reclaims_history_and_keeps_spanish_complete() {
        let dir = std::env::temp_dir().join(format!("pleamar-atlas-{}-imágenes", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let (send, jobs) = channel();
        let mut texts = Texts::new(send);
        let mut t = Typesetter::new();
        let expected = t.lay_out(&spanish()).glyphs.len();
        t.clear(1.0);
        texts.reset(1.0, &[(ImageSource::Live(crate::scene::TextId(0)), (640, 360))], &[]);
        deliver(&mut t, &mut texts, &jobs);
        let initial = texts.generation;
        for i in 0..48 {
            let path = dir.join(format!("cover {i}.svg"));
            std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="36"><rect width="64" height="36" fill="red"/></svg>"#).unwrap();
            let names = [path.to_str().unwrap().to_owned()];
            for _ in 0..4 {
                texts.begin_frame();
                texts.layout(0, spanish());
                texts.image(0, &names);
                texts.end_frame();
                // The real render consumes the previous frame's uploads
                // before receiving any of the jobs queued by that frame.
                texts.pending_upload.clear();
                deliver(&mut t, &mut texts, &jobs);
            }
            assert!(!texts.rebuilding && !texts.exhausted, "cover {i} never recovered");
            assert!(texts.image(0, &names).is_some(), "cover {i} was lost");
            assert_eq!(texts.layout(0, spanish()).unwrap().glyphs.len(), expected);
            assert!(texts.live.len() < 20, "old covers are still resident");
            std::fs::remove_file(path).unwrap();
        }
        assert!(texts.generation >= initial + 3, "fixture did not exhaust the atlas repeatedly");
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn a_wallpaper_is_a_picture_of_its_own_at_full_size_and_let_go_when_unseen() {
        let dir = std::env::temp_dir().join(format!("pleamar-pictures-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let wall = |i: usize| {
            let path = dir.join(format!("wall {i}.svg"));
            std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="9"><rect width="16" height="9" fill="blue"/></svg>"#).unwrap();
            path.to_str().unwrap().to_owned()
        };
        let (send, jobs) = channel();
        let mut texts = Texts::new(send);
        let mut t = Typesetter::new();
        t.work(Job::Limit(8192));
        texts.reset(1.0, &[(ImageSource::Live(crate::scene::TextId(0)), (4128, 2322))], &[]);
        deliver(&mut t, &mut texts, &jobs);
        let frame = |texts: &mut Texts, t: &mut Typesetter, names: &[String]| {
            texts.begin_frame();
            let slot = texts.image(0, names);
            texts.end_frame();
            deliver(t, texts, &jobs);
            slot
        };
        let first = [wall(0)];
        frame(&mut texts, &mut t, &first);
        let slot = frame(&mut texts, &mut t, &first).expect("loaded");
        assert!(slot.page > 0 && (slot.width, slot.height) == (4128, 2322), "the whole of it, not shrunk to the atlas");
        let (page, size, rgba) = texts.pending_pictures.pop().expect("handed to the card");
        assert_eq!((page, size, rgba.len()), (slot.page, (4128, 2322), 4128 * 2322 * 4));
        // Another one in its place: the first is let go once it has gone unseen a while.
        let second = [wall(1)];
        for _ in 0..UNSEEN_FRAMES + 4 {
            frame(&mut texts, &mut t, &second);
        }
        assert!(texts.dropped_pictures.contains(&slot.page));
        assert!(frame(&mut texts, &mut t, &second).is_some_and(|s| s.page > slot.page));
        assert!(texts.atlas_height() == ATLAS_SIZE && !texts.exhausted, "the atlas never saw it");
        // A photo keeps its own pixels: not made bigger to what was declared,
        // and made smaller only to it.
        let photo = dir.join("photo.png");
        image::RgbaImage::new(3840, 2160).save(&photo).unwrap();
        assert_eq!(t.picture_px(&photo, (4128, 2322)), (3840, 2160));
        assert_eq!(t.picture_px(&photo, (2752, 1548)), (2752, 1548));
        assert_eq!(t.picture_px(&photo, (4128, 1728)), (3840, 1607), "the box keeps its shape");
        // Declared `0, 0`: the file's own size and shape, whatever the scale.
        t.clear(1.5);
        assert_eq!(t.image_px(Some(&photo), (0, 0), LIVE_BUDGET), ((3840, 2160), true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scene_reset_discards_an_inflight_rebuild() {
        let (send, jobs) = channel();
        let mut texts = Texts::new(send);
        let mut t = Typesetter::new();
        texts.working.layouts.insert(spanish());
        texts.exhausted = true;
        texts.end_frame();
        let old = t.work(jobs.recv().unwrap()).unwrap();
        texts.reset(1.25, &[], &[]);
        texts.receive(old);
        assert!(texts.layouts.is_empty() && texts.pending_upload.is_empty());
        deliver(&mut t, &mut texts, &jobs);
        assert_eq!(t.scale, 1.25);
        assert!(!texts.rebuilding);
    }

    #[test]
    fn oversized_working_set_does_not_rebuild_on_every_frame() {
        let (send, jobs) = channel();
        let mut texts = Texts::new(send);
        let mut t = Typesetter::new();
        let huge = LayoutKey { px: 4096.0f32.to_bits(), text: "W".into(), ..spanish() };
        texts.begin_frame();
        texts.layout(0, huge.clone());
        deliver(&mut t, &mut texts, &jobs);
        texts.end_frame();
        deliver(&mut t, &mut texts, &jobs);
        assert!(texts.too_large.is_some());
        for _ in 0..60 {
            texts.begin_frame();
            texts.layout(0, huge.clone());
            texts.end_frame();
        }
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn live_images_keep_animation_indices_aligned() {
        let mut t = Typesetter::new();
        let sources = [(ImageSource::Live(crate::scene::TextId(0)), (16, 16)), (ImageSource::Live(crate::scene::TextId(1)), (16, 16))];
        let (images, animations) = t.load_images(&sources);
        assert_eq!(images.len(), sources.len());
        assert_eq!(animations.len(), sources.len());
    }
}
