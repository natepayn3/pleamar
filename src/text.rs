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
    /// The byte of the text it starts at: what a mark of colour asks after.
    pub at: u32,
}

pub struct Layout {
    pub glyphs: Vec<PlacedGlyph>,
    pub size: (f32, f32),
    /// Where the text cursor falls before each letter: (byte, x). Only for the
    /// first line: it is what a field to type into needs.
    pub cursors: Vec<(usize, f32)>,
    /// The same for every line, with where the line is: what selecting a
    /// text with the mouse needs. In byte order: a table's cells side by side
    /// are one after another, each with its own lines.
    pub lines: Vec<TextLine>,
    /// Its markdown tables, where they are, for their lines to be drawn.
    pub tables: Vec<TableBox>,
}

/// Where a markdown table was laid out: the x of its left edge, of each line
/// between columns and of its right edge, and the y of its top, of each line
/// between rows and of its bottom; and whether its first row is its head (a
/// row shown as a card of its own has none).
#[derive(Clone, Debug, Default)]
pub struct TableBox {
    pub cols: Vec<f32>,
    pub rows: Vec<f32>,
    pub head: bool,
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
        // Of the lines at that height (a table's cells are side by side), the nearest across.
        let away = |l: &TextLine| {
            let (a, b) = (l.cursors.first().map_or(0.0, |c| c.1), l.cursors.last().map_or(0.0, |c| c.1));
            if x < a { a - x } else if x > b { x - b } else { 0.0 }
        };
        let level = self.lines.iter().filter(|l| y >= l.top && y < l.bottom).min_by(|a, b| away(a).total_cmp(&away(b)));
        let line = level.or_else(|| self.lines.iter().find(|l| y < l.bottom)).unwrap_or_else(|| self.lines.last().unwrap());
        if y >= line.bottom {
            return line.cursors.last().map_or(0, |c| c.0);
        }
        line.cursors.iter().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map_or(0, |c| c.0)
    }

    /// Which line the cursor in front of byte `b` is on: the last one that
    /// starts at or before it (at a wrap, the cursor goes to the line below).
    pub fn line_of(&self, b: usize) -> usize {
        self.lines.iter().rposition(|l| l.cursors.first().is_some_and(|c| c.0 <= b)).unwrap_or(0)
    }

    /// Where the cursor in front of byte `b` is drawn, on any line: x, and the
    /// top and bottom of its line. Before anything is laid out, at the start.
    pub fn caret_at(&self, b: usize) -> (f32, f32, f32) {
        match self.lines.get(self.line_of(b)) {
            Some(l) => (l.cursors.iter().rev().find(|c| c.0 <= b).or(l.cursors.first()).map_or(0.0, |c| c.1), l.top, l.bottom),
            None => (self.x_at_byte(b), 0.0, self.size.1),
        }
    }

    /// The byte a line up (`-1`) or down (`1`) from byte `b`, as near its x as
    /// there is one; past the first line, the start, and past the last, the end.
    pub fn byte_a_line_away(&self, b: usize, by: i32, end: usize) -> usize {
        let (x, _, _) = self.caret_at(b);
        let k = self.line_of(b) as i32 + by;
        if k < 0 {
            return 0;
        }
        match self.lines.get(k as usize) {
            Some(l) => l.cursors.iter().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map_or(0, |c| c.0),
            None => end,
        }
    }

    /// Where the line of byte `b` starts and ends, in bytes.
    pub fn line_ends(&self, b: usize) -> (usize, usize) {
        self.lines.get(self.line_of(b)).map_or((0, b), |l| (l.cursors.first().map_or(0, |c| c.0), l.cursors.last().map_or(b, |c| c.0)))
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

/// The widest run of letters without a space in a laid-out piece of `text`.
fn longest_word(text: &str, m: &Layout) -> f32 {
    let mut most = 0f32;
    for l in &m.lines {
        let mut start: Option<f32> = None;
        for &(b, x) in &l.cursors {
            let space = text.get(b..).and_then(|t| t.chars().next()).is_none_or(char::is_whitespace);
            match (start, space) {
                (None, false) => start = Some(x),
                (Some(s), true) => {
                    most = most.max(x - s);
                    start = None;
                }
                _ => {}
            }
        }
    }
    most
}

/// A letter's colour: that of the last stretch (byte range) it falls in, or the text's.
pub fn colour_of(g: &PlacedGlyph, paints: &[(u32, u32, [f32; 3])], text: [f32; 3]) -> [f32; 3] {
    paints.iter().rev().find(|p| g.at >= p.0 && g.at < p.1).map_or(text, |p| p.2)
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
    /// `paragraph:`: how tall a blank line between blocks is (an f32's bits); its
    /// lines that are a lone space are that tall (see `crate::markdown::Options`).
    paragraph: Option<u32>,
    /// Stretches of it (byte ranges) bold, italic or code: `crate::markdown`.
    marks: Vec<(u32, u32, u8)>,
    /// The points of its lists and its tables, placed by the layout itself.
    blocks: Vec<crate::markdown::Block>,
}

impl LayoutKey {
    pub fn new(text: &str, e: &Style, width: Option<f32>) -> LayoutKey {
        LayoutKey {
            text: text.to_owned(), family: e.family, px: e.px.to_bits(), weight: e.weight, line_height: e.line_height.to_bits(),
            width: width.map(|a| a.round().max(1.0) as u32), align: e.align, max_lines: e.max_lines, tight: e.tight, paragraph: None, marks: Vec::new(), blocks: Vec::new(),
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

    /// The same, with its blank lines that tall (`paragraph:` on a markdown text).
    pub fn paragraphed(mut self, h: Option<f32>) -> LayoutKey {
        self.paragraph = h.map(f32::to_bits);
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

    /// The same, with the points of its lists and its tables (`crate::markdown::Block`).
    pub fn blocked(mut self, blocks: Vec<crate::markdown::Block>) -> LayoutKey {
        self.blocks = blocks;
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

    /// How many rows of the atlas hold something (or are kept for what a shelf may still take):
    /// what a texture on the card has to be tall enough for. Rows below are never touched.
    fn used(&self) -> u32 {
        self.next_y
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
pub(crate) struct Typesetter {
    fonts: FontSystem,
    swash: SwashCache,
    /// For the letters hinted here (`hinted_mask`).
    scale_context: swash::scale::ScaleContext,
    shelves: Shelves,
    /// The shelves of the letters' own atlas, one channel (a letter is only how much it covers,
    /// which the shader paints in the text's colour): four times less card for text than in RGBA.
    masks: Shelves,
    glyphs: HashMap<CacheKey, Option<(AtlasSlot, i32, i32, bool)>>,
    /// What has been painted since the last delivery: where and what (premultiplied RGBA).
    pending_upload: Vec<(AtlasSlot, Vec<u8>)>,
    /// The same for the letters' atlas: one byte a pixel.
    pending_masks: Vec<(AtlasSlot, Vec<u8>)>,
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
    pub(crate) fn new() -> Self {
        let t0 = std::time::Instant::now();
        let fonts = crate::fonts::font_system();
        let (_, parsed, cached) = crate::fonts::last_read();
        println!("text   · {} system fonts in {} ms ({parsed} files read, {cached} from the cache)", fonts.db().len(), t0.elapsed().as_millis());
        Typesetter { fonts, swash: SwashCache::new(), scale_context: swash::scale::ScaleContext::new(), shelves: Shelves::new(), masks: Shelves::new(), glyphs: HashMap::new(), pending_upload: Vec::new(), pending_masks: Vec::new(), pending_fit: Vec::new(), pending_pictures: Vec::new(), last_picture: 0, scale: 1.0, exhausted: false, frames: HashMap::new(), fonts_read: HashSet::new() }
    }

    /// When the scale changes, everything painted stops being valid.
    fn clear(&mut self, scale: f32) {
        self.scale = scale;
        self.shelves = self.shelves.emptied();
        self.masks = self.masks.emptied();
        self.glyphs.clear();
        self.pending_upload.clear();
        self.pending_masks.clear();
        self.pending_fit.clear();
        self.pending_pictures.clear();
        self.exhausted = false;
        self.frames.clear();
    }

    pub(crate) fn lay_out(&mut self, c: &LayoutKey) -> Layout {
        let m = self.lay_out_once(c);
        // `lines: n` counts the whole text's lines. cosmic-text cuts each paragraph at n on
        // its own, so a text with line breaks showed all of its paragraphs and ran out of
        // its card. Past the n-th line it is laid out again cut there, with its «…».
        let Some(n) = c.max_lines.filter(|n| *n >= 1 && m.lines.len() > *n) else { return m };
        let Some(end) = m.lines[n - 1].cursors.last().map(|k| k.0.min(c.text.len())) else { return m };
        let mut cut = c.clone();
        cut.text = format!("{}…", c.text[..end].trim_end());
        cut.marks.retain(|k| (k.0 as usize) < end);
        cut.blocks.retain(|b| (b.ends().1 as usize) <= end);
        for k in &mut cut.marks {
            k.1 = k.1.min(end as u32);
        }
        let mut m = self.lay_out_once(&cut);
        // The «…» may have made its line wrap: never more than n.
        if m.lines.len() > n {
            let bottom = m.lines[n - 1].bottom;
            m.lines.truncate(n);
            m.glyphs.retain(|g| g.rect[1] + g.rect[3] * 0.5 < bottom);
            m.size.1 = bottom;
        }
        m
    }

    fn lay_out_once(&mut self, c: &LayoutKey) -> Layout {
        if !c.blocks.is_empty() {
            return self.compose(c);
        }
        let (px, line_height, width) = (f32::from_bits(c.px), f32::from_bits(c.line_height), c.width.map(|a| a as f32));
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(px, px * line_height));
        // Hinted (as fontconfig says, see `Hinting`): each letter starts on a
        // whole pixel and advances by whole pixels, as Pango does with hinted
        // metrics; its stems then fall on one column, not across two. (It
        // rounds the scene's units: only at a whole scale are they whole pixels.)
        if hinting() != Hinting::None && self.scale.fract() == 0.0 {
            buffer.set_hinting(cosmic_text::Hinting::Enabled);
        }
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
        let paragraph = c.paragraph.map(f32::from_bits);
        if c.marks.is_empty() && paragraph.is_none() {
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
            let mut spans: Vec<(usize, usize, Attrs)> = Vec::with_capacity(c.marks.len() * 2 + 1);
            let mut at = 0usize;
            for &(from, to, m) in &c.marks {
                let (from, to) = (from as usize, to as usize);
                if from > at {
                    spans.push((at, from, attrs.clone()));
                }
                spans.push((from, to, look(m)));
                at = to;
            }
            if at < c.text.len() {
                spans.push((at, c.text.len(), attrs.clone()));
            }
            // `paragraph:`: a line that is a lone space (a blank line between blocks) is
            // that tall, not a whole line: its space is cut out of the span it is in.
            if let Some(h) = paragraph {
                let short = Metrics::new(h.min(px).max(1.0), h.max(1.0));
                let mut start = 0usize;
                let mut blanks = Vec::new();
                for line in c.text.split('\n') {
                    if line == " " {
                        blanks.push((start, start + 1));
                    }
                    start += line.len() + 1;
                }
                for (a, b) in blanks {
                    if let Some(k) = spans.iter().position(|s| s.0 <= a && b <= s.1) {
                        let (s0, s1, at) = spans.remove(k);
                        let mut parts = Vec::new();
                        if s0 < a {
                            parts.push((s0, a, at.clone()));
                        }
                        parts.push((a, b, at.clone().metrics(short)));
                        if b < s1 {
                            parts.push((b, s1, at));
                        }
                        for (i, p) in parts.into_iter().enumerate() {
                            spans.insert(k + i, p);
                        }
                    }
                }
            }
            buffer.set_rich_text(spans.iter().map(|(a, b, at)| (&c.text[*a..*b], at.clone())), &attrs, Shaping::Advanced, Some(align));
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
                glyphs.push(PlacedGlyph { rect: [x / s, y / s, slot.width as f32 / s, slot.height as f32 / s], uv: slot.uv(), colored, at: (from + g.start) as u32 });
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
        Layout { glyphs, size: (across, h), cursors, lines, tables: Vec::new() }
    }

    /// The bytes from `a` to `b` of a text laid out on their own, `width` wide
    /// (or as wide as they are), with their marks: a piece of a text with blocks.
    fn piece(&mut self, c: &LayoutKey, a: usize, b: usize, width: Option<f32>) -> Layout {
        let marks = c.marks.iter().filter(|m| (m.1 as usize) > a && (m.0 as usize) < b).map(|m| ((m.0 as usize).max(a) as u32 - a as u32, (m.1 as usize).min(b) as u32 - a as u32, m.2)).collect();
        let key = LayoutKey { text: c.text[a..b].to_owned(), marks, width: width.map(|w| w.ceil().max(1.0) as u32), max_lines: None, tight: true, blocks: Vec::new(), ..c.clone() };
        self.lay_out_once(&key)
    }

    /// A text with blocks (`crate::markdown::Block`): what is between them laid out
    /// as text is, each point of a list with its words hanging past its mark, each
    /// table its cells in columns; one under another.
    fn compose(&mut self, c: &LayoutKey) -> Layout {
        let width = c.width.map(|w| w as f32);
        let px = f32::from_bits(c.px);
        let s = self.scale;
        let mut out = Layout { glyphs: Vec::new(), size: (0.0, 0.0), cursors: Vec::new(), lines: Vec::new(), tables: Vec::new() };
        let (mut y, mut right) = (0f32, 0f32);
        // A piece where it goes: its letters and lines moved there, its bytes counted from `from`.
        let put = |out: &mut Layout, m: &Layout, dx: f32, dy: f32, from: usize, right: &mut f32| {
            let (dx, dy) = ((dx * s).round() / s, (dy * s).round() / s);
            out.glyphs.extend(m.glyphs.iter().map(|g| PlacedGlyph { rect: [g.rect[0] + dx, g.rect[1] + dy, g.rect[2], g.rect[3]], at: g.at + from as u32, ..*g }));
            out.lines.extend(m.lines.iter().map(|l| TextLine { top: l.top + dy, bottom: l.bottom + dy, cursors: l.cursors.iter().map(|k| (k.0 + from, k.1 + dx)).collect() }));
            *right = right.max(dx + m.size.0);
        };
        let text = c.text.as_str();
        let mut blocks = c.blocks.clone();
        blocks.sort_by_key(|b| b.ends().0);
        let mut at = 0usize;
        let mut after_block = false;
        for b in &blocks {
            let (from, to) = (b.ends().0 as usize, b.ends().1 as usize);
            if from < at || to > text.len() || !text.is_char_boundary(from) || !text.is_char_boundary(to) {
                continue;
            }
            if from > at {
                // Up to the line break before the block.
                let end = if text[..from].ends_with('\n') { from - 1 } else { from };
                let m = self.piece(c, at, end, width);
                put(&mut out, &m, 0.0, y, at, &mut right);
                y += m.size.1;
            }
            match b {
                crate::markdown::Block::Hang { body, .. } => {
                    let body = (*body as usize).clamp(from, to);
                    let mark = self.piece(c, from, body, None);
                    let hang = mark.size.0;
                    let words = self.piece(c, body, to, width.map(|w| (w - hang).max(px * 4.0)));
                    let first = out.lines.len();
                    put(&mut out, &words, hang, y, body, &mut right);
                    // The mark goes on the first line of its words, before them.
                    let dy = (y * s).round() / s;
                    out.glyphs.extend(mark.glyphs.iter().map(|g| PlacedGlyph { rect: [g.rect[0], g.rect[1] + dy, g.rect[2], g.rect[3]], at: g.at + from as u32, ..*g }));
                    if let (Some(l), Some(m)) = (out.lines.get_mut(first), mark.lines.first()) {
                        let mut cursors: Vec<(usize, f32)> = m.cursors.iter().filter(|k| k.0 < body - from).map(|k| (k.0 + from, k.1)).collect();
                        cursors.append(&mut l.cursors);
                        l.cursors = cursors;
                        l.bottom = l.bottom.max(m.bottom + dy);
                    }
                    y += words.size.1.max(mark.size.1);
                }
                crate::markdown::Block::Table { rows, .. } => self.table(c, rows, width, &mut y, &mut out, &mut right),
            }
            at = to;
            after_block = true;
            if text[at..].starts_with('\n') {
                at += 1;
                after_block = at == text.len();
            }
        }
        if at < text.len() || after_block && at == text.len() && at > 0 && text.ends_with('\n') {
            let m = self.piece(c, at, text.len(), width);
            put(&mut out, &m, 0.0, y, at, &mut right);
            y += m.size.1;
        }
        out.cursors = out.lines.first().map_or_else(|| vec![(0, 0.0)], |l| l.cursors.clone());
        let across = match width {
            Some(fixed) if c.tight && c.align == TextAlign::Left => right.min(fixed),
            Some(fixed) => fixed.max(right),
            None => right,
        };
        out.size = (across, y);
        out
    }

    /// A markdown table. When not even its words fit across (a dozen columns in a
    /// narrow text), each row is a card of its own instead: the head's cells down
    /// one side, the row's beside them.
    fn table(&mut self, c: &LayoutKey, rows: &[Vec<(u32, u32)>], width: Option<f32>, y: &mut f32, out: &mut Layout, right: &mut f32) {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if let (Some(w), true) = (width, rows.len() > 1 && columns > 2) {
            let pad_x = (f32::from_bits(c.px) * 0.55).round();
            let mut least = vec![0f32; columns];
            for row in rows {
                for (k, &(a, b)) in row.iter().enumerate() {
                    let m = self.piece(c, a as usize, b as usize, None);
                    least[k] = least[k].max(longest_word(&c.text[a as usize..b as usize], &m).ceil());
                }
            }
            if least.iter().sum::<f32>() > w - columns as f32 * pad_x * 2.0 - 1.0 {
                for row in &rows[1..] {
                    let card: Vec<Vec<(u32, u32)>> = row.iter().enumerate().map(|(k, &cell)| vec![rows[0].get(k).copied().unwrap_or((cell.0, cell.0)), cell]).collect();
                    self.grid(c, &card, width, y, out, right, false);
                }
                return;
            }
        }
        self.grid(c, rows, width, y, out, right, true);
    }

    /// Cells in rows and columns: each column as wide as its cells ask, up to what
    /// the text's width leaves (then its cells wrap, the widest giving most), each
    /// cell laid out on its own in its place.
    #[allow(clippy::too_many_arguments)]
    fn grid(&mut self, c: &LayoutKey, rows: &[Vec<(u32, u32)>], width: Option<f32>, y: &mut f32, out: &mut Layout, right: &mut f32, head: bool) {
        let px = f32::from_bits(c.px);
        let s = self.scale;
        let (pad_x, pad_y, margin) = ((px * 0.55).round(), (px * 0.4).round(), (px * 0.35).round());
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        // What each column takes on one line, and its longest word: as narrow as it can be.
        let (mut most, mut least) = (vec![0f32; columns], vec![0f32; columns]);
        for row in rows {
            for (k, &(a, b)) in row.iter().enumerate() {
                let m = self.piece(c, a as usize, b as usize, None);
                most[k] = most[k].max(m.size.0.ceil());
                least[k] = least[k].max(longest_word(&c.text[a as usize..b as usize], &m).ceil());
            }
        }
        let room = width.map(|w| (w - columns as f32 * pad_x * 2.0 - 1.0).max(columns as f32));
        let (sum_most, sum_least): (f32, f32) = (most.iter().sum(), least.iter().sum());
        let widths: Vec<f32> = match room {
            Some(r) if sum_most > r && sum_least <= r => {
                // Each its longest word, and what is left shared out as each wants more.
                let k = (r - sum_least) / (sum_most - sum_least).max(1.0);
                most.iter().zip(&least).map(|(m, l)| (l + (m - l) * k).floor()).collect()
            }
            // Not even its words fit: each a share as its words ask, and they break.
            Some(r) if sum_most > r => least.iter().map(|l| (r * l / sum_least.max(1.0)).floor().max(1.0)).collect(),
            _ => most,
        };
        let mut cols = vec![0f32];
        for w in &widths {
            cols.push(cols.last().unwrap() + w + pad_x * 2.0);
        }
        let top = ((*y + margin) * s).round() / s;
        let mut table_rows = vec![top];
        let mut ry = top;
        for row in rows {
            let mut tallest = 0f32;
            let laid: Vec<(usize, Layout)> = row.iter().enumerate().map(|(k, &(a, b))| (a as usize, self.piece(c, a as usize, b as usize, Some(widths[k])))).collect();
            for (k, (a, m)) in laid.iter().enumerate() {
                tallest = tallest.max(m.size.1);
                let (dx, dy) = (((cols[k] + pad_x) * s).round() / s, ((ry + pad_y) * s).round() / s);
                out.glyphs.extend(m.glyphs.iter().map(|g| PlacedGlyph { rect: [g.rect[0] + dx, g.rect[1] + dy, g.rect[2], g.rect[3]], at: g.at + *a as u32, ..*g }));
                out.lines.extend(m.lines.iter().map(|l| TextLine { top: l.top + dy, bottom: l.bottom + dy, cursors: l.cursors.iter().map(|q| (q.0 + a, q.1 + dx)).collect() }));
            }
            ry += tallest + pad_y * 2.0;
            ry = (ry * s).round() / s;
            table_rows.push(ry);
        }
        *right = right.max(*cols.last().unwrap());
        out.tables.push(TableBox { cols, rows: table_rows, head });
        *y = ry + margin;
    }

    fn glyph(&mut self, key: CacheKey, full: &mut bool) -> Option<(AtlasSlot, i32, i32, bool)> {
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let mut no_space = false;
        let image = match self.hinted_mask(key) {
            Some(img) => Some(img),
            None => self.swash.get_image_uncached(&mut self.fonts, key),
        };
        let painted = image.and_then(|img| {
            let (w, h) = (img.placement.width, img.placement.height);
            if w == 0 || h == 0 {
                return None;
            }
            let rgba: Vec<u8> = match img.content {
                // A mask is how much of each pixel the letter covers: one byte, in the letters' own
                // atlas, where the shader will tint it.
                SwashContent::Mask => {
                    let Some(slot) = self.masks.request(w, h) else {
                        *full = true;
                        no_space = true;
                        return None;
                    };
                    self.upload_mask(slot, img.data);
                    return Some((slot, img.placement.left, img.placement.top, false));
                }
                // An emoji brings its own colours, and goes in the RGBA atlas with the images.
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

    /// A letter as a mask, hinted as fontconfig asks (see `Hinting`): its
    /// baseline, its x-height and its capitals land on whole pixels, the rest
    /// of it stretched between them, and what overshoots them by less than
    /// half a pixel (the round of an «o») flat on the same line. It is what
    /// FreeType's light hinting does —GTK's and Chrome's with `hintstyle
    /// slight`—, and what a font without instructions of its own (Inter
    /// Variable) never had here: its tops and bottoms fell across two rows,
    /// a grey row above and below every letter. Across, nothing moves: the
    /// letters keep their shapes and their places at quarters of a pixel.
    /// None: not hinted here (hinting off, a colour or pixel font), and
    /// cosmic-text's own rasterizer paints it.
    fn hinted_mask(&mut self, key: CacheKey) -> Option<cosmic_text::SwashImage> {
        use cosmic_text::CacheKeyFlags;
        use swash::zeno::{Format, Mask, Origin, Transform, Angle, Vector};
        let hinting = hinting();
        if hinting == Hinting::None || key.flags.contains(CacheKeyFlags::PIXEL_FONT) {
            return None;
        }
        let font = self.fonts.get_font(key.font_id, key.font_weight)?;
        let face = font.as_swash();
        let size = f32::from_bits(key.font_size_bits);
        // The weight of a variable font, as cosmic-text sets it.
        let coords: Vec<swash::NormalizedCoord> = match face.variations().find_by_tag(swash::Tag::from_be_bytes(*b"wght")) {
            Some(v) => face.variations().normalized_coords([(swash::Tag::from_be_bytes(*b"wght"), f32::from(key.font_weight.0).clamp(v.min_value(), v.max_value()))]).collect(),
            None => Vec::new(),
        };
        let metrics = face.metrics(&coords).scale(size);
        let mut scaler = self.scale_context.builder(face).size(size).hint(hinting == Hinting::Full).normalized_coords(coords.iter()).build();
        if scaler.has_color_outlines() || scaler.has_color_bitmaps() || !scaler.has_outlines() {
            return None;
        }
        let mut outline = scaler.scale_outline(key.glyph_id)?;
        let lines = BlueLines::new(metrics.x_height, metrics.cap_height, size);
        for p in outline.points_mut() {
            p.y = lines.snap(p.y);
        }
        if key.flags.contains(CacheKeyFlags::FAKE_ITALIC) {
            outline.transform(&Transform::skew(Angle::from_degrees(14.0), Angle::from_degrees(0.0)));
        }
        let offset = Vector::new(key.x_bin.as_float(), key.y_bin.as_float());
        // (As swash does it: `inspect` works the size out before the placement,
        // which `render()` alone gets wrong with the origin at the bottom.)
        let mut img = cosmic_text::SwashImage::new();
        let mut data = Vec::new();
        img.placement = Mask::new(outline.path())
            .format(Format::Alpha)
            .origin(Origin::BottomLeft)
            .offset(offset)
            .render_offset(offset)
            .inspect(|fmt, w, h| data.resize(fmt.buffer_size(w, h), 0))
            .render_into(&mut data[..], None);
        img.data = data;
        img.content = cosmic_text::SwashContent::Mask;
        Some(img)
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

    /// A letter's coverage, one byte a pixel, into the letters' atlas with its gutter of nothing.
    fn upload_mask(&mut self, slot: AtlasSlot, coverage: Vec<u8>) {
        let padded = AtlasSlot { x: slot.x - 1, y: slot.y - 1, width: slot.width + 2, height: slot.height + 2, page: 0 };
        let stride = padded.width as usize;
        let mut pixels = vec![0; stride * padded.height as usize];
        for (y, row) in coverage.chunks_exact(slot.width as usize).enumerate() {
            let start = (y + 1) * stride + 1;
            pixels[start..start + row.len()].copy_from_slice(row);
        }
        self.pending_masks.push((padded, pixels));
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
        if crate::alloc_report::on() {
            eprintln!("image  · a picture of its own, {}×{}: {:.1} MB on the card", px.0, px.1, px.0 as f64 * px.1 as f64 * 4.0 / 1e6);
        }
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
                // Read later, the size of the box it is drawn in (`Texts::boxed_image`).
                if *size == BOXED {
                    animations.push(None);
                    return None;
                }
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
        // `image me = data "me"`: the bytes the logic handed over, from memory.
        if let Some(key) = name.strip_prefix("data:") {
            let data = data_image(key)?;
            let (px, _) = self.image_px(None, size, LIVE_BUDGET);
            let rgba = rasterize_bytes(&data, looks_like_svg(&data), px, false)?;
            let slot = match into {
                Some(s) if s.page == 0 && (s.width, s.height) == px => s,
                _ => self.reserve(px.0, px.1)?,
            };
            self.upload(slot, rgba);
            return Some(slot);
        }
        let path = if std::path::Path::new(name).is_absolute() { Some(std::path::PathBuf::from(name)) } else { scene_picture(name).or_else(|| crate::platform::icon(name)) }?;
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
            // (Both: `|`, not `||`; whichever was short of room grows.)
            if !self.exhausted || !(self.shelves.grow() | self.masks.grow()) {
                return snapshot;
            }
            println!("text   · what is seen does not fit the atlas: it grows to {ATLAS_SIZE}×{} (letters {ATLAS_SIZE}×{})", self.shelves.height, self.masks.height);
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
                self.masks.most = self.shelves.most;
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
        Some(Parcel { delivery, pending_upload: std::mem::take(&mut self.pending_upload), pending_masks: std::mem::take(&mut self.pending_masks), used: self.shelves.used(), mask_used: self.masks.used(), pending_fit: std::mem::take(&mut self.pending_fit), pending_pictures: std::mem::take(&mut self.pending_pictures), exhausted: self.exhausted, height: self.shelves.height, most: self.shelves.most })
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
    /// The letters', one byte a pixel, and how many rows of each atlas hold something: what
    /// the card's textures have to be tall enough for (they are made as tall as that, not as the atlas may be).
    pub pending_masks: Vec<(AtlasSlot, Vec<u8>)>,
    pub used: u32,
    pub mask_used: u32,
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
    /// The letters' atlas's (one byte a pixel), and the rows of each atlas the card has to have.
    pub pending_masks: Vec<(AtlasSlot, Vec<u8>)>,
    used: u32,
    mask_used: u32,
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
    /// The scene's images made the size of the box they are drawn in (#161): by their number.
    boxed: HashMap<usize, Boxed>,
}

/// An image declared `0, 0` that is only drawn with `fit: cover` or `fit: contain` (#161): it is not
/// read whole when the scene loads, but the size of its box the first time it is drawn —never bigger
/// than the file—, as a live image of that size. Its box may grow (another monitor, another scale, a
/// `size:` that grows): read again for the bigger one, twice; a third time it is taken to be an image
/// the scene enlarges on purpose (a zoom, a map) and read whole, as its own pixels.
#[derive(Clone, Debug)]
struct Boxed {
    /// What it is read from, and whether it covers its box (else it is contained in it).
    path: std::path::PathBuf,
    cover: bool,
    /// The file's own pixels, once looked at; `None` before, and for one that cannot be read (then whole).
    own: Option<Option<(u32, u32)>>,
    /// The size (logical, as a live image is asked for) it was last read at, and how many times its box grew.
    size: Option<(u32, u32)>,
    grew: u32,
}

/// The real pixels a picture of `own` pixels needs to fill a box of `want` real pixels: covering it (its
/// shape kept, what overflows cut) or contained in it. Never more than its own (#161).
pub fn boxed_px(own: (u32, u32), want: (f32, f32), cover: bool) -> (u32, u32) {
    let (kx, ky) = (want.0 / own.0.max(1) as f32, want.1 / own.1.max(1) as f32);
    let k = if cover { kx.max(ky) } else { kx.min(ky) }.min(1.0);
    if !(k > 0.0) {
        return (1, 1);
    }
    (((own.0 as f32 * k).ceil() as u32).clamp(1, own.0), ((own.1 as f32 * k).ceil() as u32).clamp(1, own.1))
}

/// The scene's images read the size of the box they are drawn in (#161), by number, and whether one of
/// its drawings covers its box: declared `0, 0`, from a file of pixels (not pixel art, an icon or a live
/// text), JPEG, or PNG or WebP that do not move, and drawn only with `fit: contain` or `fit: cover` and
/// no `cell:`. One drawn with `fit: fill` (or none) somewhere keeps the file's own pixels, as before.
pub fn boxed_images(scene: &crate::scene::Scene) -> Vec<(usize, bool)> {
    use crate::scene::Instr;
    // For each image: drawn at all, only into a box, and covering one.
    let mut how: Vec<(bool, bool, bool)> = vec![(false, true, false); scene.images.len()];
    for i in &scene.instrs {
        if let Instr::Image { image, cell, fit, .. } = i {
            if let Some(h) = how.get_mut(image.0 as usize) {
                h.0 = true;
                h.1 &= *fit > 0 && cell.is_none();
                h.2 |= *fit == 2;
            }
        }
    }
    let still = |path: &std::path::Path| {
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        let open = || std::fs::File::open(path).ok().map(std::io::BufReader::new);
        match ext.as_str() {
            "jpg" | "jpeg" => true,
            "png" => open().and_then(|r| image::codecs::png::PngDecoder::new(r).ok()).and_then(|d| d.is_apng().ok()).is_some_and(|apng| !apng),
            "webp" => open().and_then(|r| image::codecs::webp::WebPDecoder::new(r).ok()).is_some_and(|d| !d.has_animation()),
            _ => false,
        }
    };
    scene
        .images
        .iter()
        .enumerate()
        .filter_map(|(k, (source, size))| {
            let (drawn, boxed, cover) = how[k];
            match source {
                ImageSource::File(path) if *size == (0, 0) && drawn && boxed && still(path) => Some((k, cover)),
                _ => None,
            }
        })
        .collect()
}

/// What the workshop is asked for in place of an image drawn the size of its box: nothing, yet.
const BOXED: (u32, u32) = (u32::MAX, u32::MAX);

/// How many jobs the workshop has been given and how many it has finished:
/// a film (`--film`) waits for it to be done before each frame, so a text laid
/// out or an image read always reaches the same frame.
static JOBS_SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static JOBS_DONE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Waits (up to `most`) until the workshop has finished every job it was given;
/// what it made is then already on its way to the render. False if it did not.
pub fn workshop_idle(most: std::time::Duration) -> bool {
    let until = std::time::Instant::now() + most;
    loop {
        if JOBS_DONE.load(std::sync::atomic::Ordering::Acquire) >= JOBS_SENT.load(std::sync::atomic::Ordering::Acquire) {
            return true;
        }
        if std::time::Instant::now() >= until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
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
                    let parcel = t.work(job);
                    // Through the usual channel, which also wakes the render up if it was asleep.
                    if let Some(parcel) = parcel
                        && to_render.send(ToRender::Workshop(Box::new(parcel))).is_err()
                    {
                        return;
                    }
                    JOBS_DONE.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                }
            })
            .unwrap();
        Self::new(to_workshop)
    }

    fn new(to_workshop: Sender<Job>) -> Self {
        Texts { to_workshop, layouts: HashMap::new(), requested: HashSet::new(), last: HashMap::new(), images: Vec::new(), animations: Vec::new(), sources: Vec::new(), live: HashMap::new(), live_requested: HashSet::new(), live_version: HashMap::new(), last_live: HashMap::new(), pending_upload: Vec::new(), pending_masks: Vec::new(), used: 1, mask_used: 1, pending_fit: Vec::new(), pending_pictures: Vec::new(), dropped_pictures: Vec::new(), pictures: HashSet::new(), unseen: HashMap::new(), generation: 0, scale: 1.0, working: WorkingSet::default(), recording: None, exhausted: false, rebuilding: false, too_large: None, height: ATLAS_SIZE, most: ATLAS_SIZE, boxed: HashMap::new() }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// A job for the workshop, counted (see `workshop_idle`).
    fn to_workshop_send(&self, job: Job) -> Result<(), std::sync::mpsc::SendError<Job>> {
        JOBS_SENT.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.to_workshop.send(job)
    }

    /// How tall the atlas has to be on the card: it is never made smaller.
    pub fn atlas_height(&self) -> u32 {
        self.height
    }

    /// How many rows of the RGBA atlas hold something, and of the letters' one: the card's textures
    /// are as tall as that (a power of two at least), and grow as it does.
    pub fn rows_used(&self) -> (u32, u32) {
        (self.used, self.mask_used)
    }

    /// The tallest atlas the card allows. What did not fit before is tried again.
    pub fn allow_atlas(&mut self, most: u32) {
        if most == self.most {
            return;
        }
        self.most = most;
        let _ = self.to_workshop_send(Job::Limit(most));
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
        self.reset_boxed(scale, images, fonts, &[]);
    }

    /// `reset`, with the images (by number, and whether they cover their box) that are read the size of
    /// the box they are drawn in, not whole (#161: see `Boxed`).
    pub fn reset_boxed(&mut self, scale: f32, images: &[(ImageSource, (u32, u32))], fonts: &[std::path::PathBuf], boxed: &[(usize, bool)]) {
        self.boxed = boxed
            .iter()
            .filter_map(|&(k, cover)| match images.get(k) {
                Some((ImageSource::File(path), (0, 0))) => Some((k, Boxed { path: path.clone(), cover, own: None, size: None, grew: 0 })),
                _ => None,
            })
            .collect();
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
        self.pending_masks.clear();
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
            let _ = self.to_workshop_send(Job::Fonts(fonts.to_vec()));
        }
        let _ = self.to_workshop_send(Job::Clear(scale));
        if !images.is_empty() {
            let _ = self.to_workshop_send(Job::Images(self.to_load(), self.generation));
        }
    }

    /// The scene's images as the workshop loads them: those drawn the size of their box, not yet (see `Boxed`).
    fn to_load(&self) -> Vec<(ImageSource, (u32, u32))> {
        self.sources.iter().enumerate().map(|(k, (s, size))| (s.clone(), if self.boxed.contains_key(&k) { BOXED } else { *size })).collect()
    }

    /// Image `k` drawn in a box of `want` (logical: the atlas's scale is put on here), if it is one read the size of its box (#161);
    /// `None` if it is not (then `frame`). The slot is that of the size asked for, or the one before
    /// while it arrives.
    pub fn boxed_image(&mut self, k: usize, want: (f32, f32)) -> Option<Option<AtlasSlot>> {
        let scale = self.scale.max(0.01);
        let b = self.boxed.get_mut(&k)?;
        let own = *b.own.get_or_insert_with(|| image::image_dimensions(&b.path).ok().filter(|&(w, h)| w > 0 && h > 0));
        let full = |own: (u32, u32)| (((own.0 as f32) / scale).floor().max(1.0) as u32, ((own.1 as f32) / scale).floor().max(1.0) as u32);
        let size = match own {
            // Unreadable: as before, whole (and the workshop says why).
            None => (0, 0),
            Some(own) if b.grew > 2 => full(own),
            Some(own) => {
                let px = boxed_px(own, (want.0 * scale, want.1 * scale), b.cover);
                // As a live image is asked for: logical, the scale taken off; never past the file's own.
                let need = (((px.0 as f32) / scale).ceil() as u32, ((px.1 as f32) / scale).ceil() as u32);
                let (fw, fh) = full(own);
                let need = (need.0.min(fw.max(1)), need.1.min(fh.max(1)));
                match b.size {
                    None => need,
                    // Bigger by more than a hair: read again for it; a box that keeps growing is a zoom.
                    Some(was) if need.0 as f32 > was.0 as f32 * 1.02 || need.1 as f32 > was.1 as f32 * 1.02 => {
                        b.grew += 1;
                        if b.grew > 2 { full(own) } else { (need.0.max(was.0), need.1.max(was.1)) }
                    }
                    Some(was) => was,
                }
            }
        };
        b.size = Some(size);
        let name = b.path.to_string_lossy().into_owned();
        Some(self.live_image(k, &name, size))
    }

    pub fn receive(&mut self, p: Parcel) {
        // Even from an old generation: the workshop's atlas is that tall now.
        self.height = self.height.max(p.height);
        self.used = self.used.max(p.used);
        self.mask_used = self.mask_used.max(p.mask_used);
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
            self.pending_masks.clear();
            self.pending_fit.clear();
        }
        self.pending_upload.extend(p.pending_upload);
        self.pending_masks.extend(p.pending_masks);
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
            let _ = self.to_workshop_send(Job::Rebuild(self.working.clone(), self.to_load(), self.generation));
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
            let _ = self.to_workshop_send(Job::Layout(key, self.generation));
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
        let (name, size) = (texts.get(t.0 as usize).filter(|n| !n.is_empty())?.clone(), *size);
        self.live_image(k, &name, size)
    }

    /// A picture by what a live image would say —an absolute path, an icon's
    /// name, `data:name`—, at that size, with no image of the scene behind it:
    /// a markdown text's `![…](…)` (see `images:`).
    pub fn named_image(&mut self, name: &str, size: (u32, u32)) -> Option<AtlasSlot> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (name, size).hash(&mut h);
        // Its own place among the ones kept while a new one arrives: far past the scene's images.
        let k = usize::MAX / 2 + (h.finish() as usize % (usize::MAX / 4));
        self.live_image(k, name, size)
    }

    fn live_image(&mut self, k: usize, name: &str, size: (u32, u32)) -> Option<AtlasSlot> {
        let name = &name.to_owned();
        let size = &size;
        if let Some(r) = &mut self.recording {
            r.push(WorkingKey::Live((name.clone(), *size)));
        }
        self.working.live.insert((name.clone(), *size));
        let (base, version) = live_name(name);
        let key = (base.to_string(), *size);
        let known = self.live.get(&key).copied();
        if !self.rebuilding && known.is_some() && self.live_version.get(&key).map(String::as_str) != Some(version) {
            self.live_version.insert(key.clone(), version.to_string());
            let _ = self.to_workshop_send(match known {
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
                    let _ = self.to_workshop_send(Job::Live(key.0, key.1, self.generation));
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
    fit_decoded(image::DynamicImage::ImageRgba8(img), px, sharp)
}

/// `fit`, from an image as it was decoded, with as few whole copies of it at once as can be (#161): made
/// smaller first, in its own kind of pixels (a JPEG's three bytes, not four), the decoded one let go
/// before it is turned into RGBA (no copy if it already is), put on a canvas only to be centred in a box
/// of another shape, and premultiplied where it is. A photo of 8084 × 6264 had four of 200 MB at once.
fn fit_decoded(img: image::DynamicImage, px: (u32, u32), sharp: bool) -> Vec<u8> {
    let k = (px.0 as f32 / img.width() as f32).min(px.1 as f32 / img.height() as f32);
    let (w, h) = (((img.width() as f32 * k).round() as u32).clamp(1, px.0), ((img.height() as f32 * k).round() as u32).clamp(1, px.1));
    let img = if (w, h) == (img.width(), img.height()) { img } else { img.resize_exact(w, h, if sharp { image::imageops::FilterType::Nearest } else { image::imageops::FilterType::Triangle }) };
    let mut rgba = img.into_rgba8();
    if (w, h) != px {
        let mut canvas = image::RgbaImage::new(px.0, px.1);
        image::imageops::overlay(&mut canvas, &rgba, ((px.0 - w) / 2) as i64, ((px.1 - h) / 2) as i64);
        rgba = canvas;
    }
    for p in rgba.pixels_mut() {
        let a = p[3] as u16;
        if a < 255 {
            *p = image::Rgba([(p[0] as u16 * a / 255) as u8, (p[1] as u16 * a / 255) as u8, (p[2] as u16 * a / 255) as u8, p[3]]);
        }
    }
    rgba.into_raw()
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

/// The scene's own pictures by name (`image chief = file "avatars/chief.png"`), and
/// its folder: a live image —a model's `image` field— that says `chief`, or a path
/// relative to the scene, is that picture (C7). It could only be an icon's name, an
/// absolute path or `data:`, so a row with an agent's avatar drew all of them with `show:`.
static SCENE_PICTURES: std::sync::Mutex<Option<(HashMap<String, std::path::PathBuf>, Option<std::path::PathBuf>)>> = std::sync::Mutex::new(None);

/// The compiler's: a picture of the scene by its name (a file's, resolved).
pub fn name_scene_picture(name: &str, path: std::path::PathBuf) {
    SCENE_PICTURES.lock().unwrap().get_or_insert_with(Default::default).0.insert(name.to_owned(), path);
}

/// The compiler's: the scene's folder, which relative paths start from.
pub fn set_scene_folder(dir: std::path::PathBuf) {
    SCENE_PICTURES.lock().unwrap().get_or_insert_with(Default::default).1 = Some(dir);
}

/// What a live image's name is as a file: a picture of the scene's by its name, or a
/// path relative to the scene's folder that is there; else None (an icon's name).
fn scene_picture(name: &str) -> Option<std::path::PathBuf> {
    let held = SCENE_PICTURES.lock().unwrap();
    let (named, folder) = held.as_ref()?;
    if let Some(p) = named.get(name) {
        return Some(p.clone());
    }
    let relative = folder.as_ref()?.join(name);
    (name.contains('/') || name.contains('.')).then_some(relative).filter(|p| p.is_file())
}

/// The pictures the logic hands over as bytes (`sys.call("image.data", name, bytes)`
/// for an `image x = data "name"`), by name: painted from memory, never written to disk.
static DATA_IMAGES: std::sync::Mutex<Option<HashMap<String, Arc<Vec<u8>>>>> = std::sync::Mutex::new(None);

/// Keeps the bytes of the picture `name`, and says what version it is now: the
/// live text of the image becomes `data:name?version`, so a new one is painted
/// where the old one was (see `Typesetter::image`).
pub fn put_data_image(name: &str, bytes: Vec<u8>) -> u64 {
    static VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    DATA_IMAGES.lock().unwrap().get_or_insert_with(HashMap::new).insert(name.to_owned(), Arc::new(bytes));
    VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

fn data_image(name: &str) -> Option<Arc<Vec<u8>>> {
    DATA_IMAGES.lock().unwrap().as_ref()?.get(name).cloned()
}

fn looks_like_svg(data: &[u8]) -> bool {
    let start = String::from_utf8_lossy(&data[..data.len().min(256)]).trim_start().to_ascii_lowercase();
    start.starts_with("<svg") || start.starts_with("<?xml")
}

/// What the logic hands over: the picture's own bytes, or the same in base64
/// (a `data:image/png;base64,` before it is let go).
pub fn picture_bytes(given: &[u8]) -> Vec<u8> {
    let raw = given.starts_with(b"\x89PNG") || given.starts_with(&[0xff, 0xd8]) || given.starts_with(b"GIF8") || given.starts_with(b"RIFF") || looks_like_svg(given);
    if raw {
        return given.to_vec();
    }
    let text = String::from_utf8_lossy(given);
    let body = text.split_once(";base64,").map_or(text.as_ref(), |(_, b)| b);
    base64(body).unwrap_or_else(|| given.to_vec())
}

/// Plain base64 (with or without its `=`, spaces and line breaks ignored).
fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        acc = (acc << 6) | value(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn rasterize_image(path: &std::path::Path, px: (u32, u32), sharp: bool) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    let is_svg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    rasterize_bytes(&data, is_svg, px, sharp)
}

fn rasterize_bytes(data: &[u8], is_svg: bool, px: (u32, u32), sharp: bool) -> Option<Vec<u8>> {
    if is_svg {
        let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
        let mut canvas = resvg::tiny_skia::Pixmap::new(px.0, px.1)?;
        let t = tree.size();
        let k = (px.0 as f32 / t.width()).min(px.1 as f32 / t.height());
        let (dx, dy) = ((px.0 as f32 - t.width() * k) * 0.5, (px.1 as f32 - t.height() * k) * 0.5);
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(k, k).post_translate(dx, dy), &mut canvas.as_mut());
        return Some(canvas.take()); // tiny-skia already premultiplies
    }
    Some(fit_decoded(image::load_from_memory(data).ok()?, px, sharp))
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
        assert!(t.masks.request(ATLAS_SIZE - 2, ATLAS_SIZE - 2).is_some());
        assert!(t.lay_out(&key).glyphs.is_empty());
        t.masks = Shelves::new();
        assert_eq!(t.lay_out(&key).glyphs.len(), expected, "atlas exhaustion must not cache letters as nonexistent");
    }

    #[test]
    fn a_live_image_can_name_the_scenes_picture() {
        // `image chief = file "…"` in the scene, and a model's field that says `chief` (C7).
        let dir = std::env::temp_dir().join(format!("pleamar-c7-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("avatars")).unwrap();
        let file = dir.join("avatars/golio.png");
        std::fs::write(&file, b"png").unwrap();
        set_scene_folder(dir.clone());
        name_scene_picture("chief", dir.join("avatars/chief.png"));
        assert_eq!(scene_picture("chief"), Some(dir.join("avatars/chief.png")));
        assert_eq!(scene_picture("avatars/golio.png"), Some(file), "relative to the scene's folder");
        assert_eq!(scene_picture("firefox"), None, "an icon's name stays one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_from_memory() {
        // A 2 × 2 PNG, as bytes and as base64: the same picture, and no file.
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert_eq!(picture_bytes(&png), png);
        let b64: String = {
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            png.chunks(3).flat_map(|c| {
                let n = c.iter().enumerate().fold(0u32, |a, (i, b)| a | (*b as u32) << (16 - 8 * i));
                (0..4).map(move |k| if k <= c.len() { A[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' })
            }).collect()
        };
        assert_eq!(picture_bytes(format!("data:image/png;base64,{b64}").as_bytes()), png);
        let v = put_data_image("test-dot", png.clone());
        assert!(v > 0);
        let rgba = rasterize_bytes(&data_image("test-dot").unwrap(), false, (2, 2), false).unwrap();
        assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn lines_count_the_whole_text_not_each_paragraph() {
        // A notification's body with line breaks and `lines: 2`: two lines, not two a paragraph.
        let mut t = Typesetter::new();
        let mut style = Style::new(14.0, crate::scene::color(1.0, 1.0, 1.0));
        style.max_lines = Some(2);
        let m = t.lay_out(&LayoutKey::new("text with multiple lines\ntext with multiple lines\ntext with multiple lines\ntext", &style, Some(300.0)));
        assert_eq!(m.lines.len(), 2, "{} lines", m.lines.len());
        let one = t.lay_out(&LayoutKey::new("one line", &style, Some(300.0)));
        assert!((m.size.1 - one.size.1 * 2.0).abs() < 2.0, "two lines tall: {} vs {}", m.size.1, one.size.1);
    }

    #[test]
    fn two_colours_one_line() {
        let mut t = Typesetter::new();
        let text = "Fix @launch-brief.md and @metrics.csv";
        let m = t.lay_out(&LayoutKey::new(text, &Style::new(14.0, crate::scene::color(1.0, 1.0, 1.0)), None));
        let (blue, green, ink) = ([0.45, 0.73, 1.0], [0.19, 0.81, 0.46], [0.96, 0.97, 0.96]);
        let (a, b) = (text.find("@launch").unwrap() as u32, text.find("@metrics").unwrap() as u32);
        let paints = [(a, a + 16, blue), (b, b + 12, green)];
        let at = |byte: u32| m.glyphs.iter().find(|g| g.at == byte).map(|g| colour_of(g, &paints, ink));
        assert_eq!(at(a + 1), Some(blue), "l of launch");
        assert_eq!(at(b + 1), Some(green), "m of metrics");
        assert_eq!(at(0), Some(ink), "F of Fix");
        let mut seen: Vec<[f32; 3]> = m.glyphs.iter().map(|g| colour_of(g, &paints, ink)).collect();
        seen.dedup();
        assert_eq!(seen, vec![ink, blue, ink, green], "in that order along the line");
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

    /// A markdown text laid out as a scene lays it out.
    fn markdown(t: &mut Typesetter, source: &str, width: f32) -> (String, Layout) {
        let r = crate::markdown::read(source, crate::markdown::Options::default());
        let laid: Vec<(u32, u32, u8)> = r.marks.iter().filter(|m| m.2 & crate::markdown::LOOKS != 0).map(|m| (m.0, m.1, m.2 & crate::markdown::LOOKS)).collect();
        let key = LayoutKey::marked(r.text.clone(), laid, &Style::new(14.0, crate::scene::color(1.0, 1.0, 1.0)), Some(width)).blocked(r.blocks);
        let m = t.lay_out(&key);
        (r.text, m)
    }

    #[test]
    fn a_wide_table_wraps_in_its_cells() {
        let mut t = Typesetter::new();
        let src = "Plan:\n\n| Who | What they do | When | Notes |\n|---|---|---|---|\n| Ana | Writes the brief for the launch and sends it round | Monday | **Ask** for the `metrics.csv` first |\n| Luis | Checks it | Tuesday | none |\n\nDone.";
        let (text, m) = markdown(&mut t, src, 320.0);
        assert_eq!(m.tables.len(), 1);
        let tb = &m.tables[0];
        assert_eq!((tb.cols.len(), tb.rows.len()), (5, 4), "four columns, a head and two rows: {tb:?}");
        assert!(*tb.cols.last().unwrap() <= 320.0, "within its width: {tb:?}");
        assert!(m.size.0 <= 320.0);
        // Each cell's letters inside its column and its row, and the long ones wrap there.
        let cell = |what: &str| {
            let a = text.find(what).unwrap() as u32;
            m.glyphs.iter().filter(|g| g.at >= a && g.at < a + what.len() as u32).map(|g| g.rect).collect::<Vec<_>>()
        };
        let brief = cell("Writes the brief for the launch and sends it round");
        let (x0, x1) = (brief.iter().map(|r| r[0]).fold(f32::MAX, f32::min), brief.iter().map(|r| r[0] + r[2]).fold(0.0, f32::max));
        assert!(x0 >= tb.cols[1] && x1 <= tb.cols[2] + 1.0, "{x0}..{x1} in {tb:?}");
        let tops: std::collections::BTreeSet<i32> = brief.iter().map(|r| r[1] as i32 / 4).collect();
        assert!(tops.len() >= 2, "it wraps in its cell");
        let ana = cell("Ana");
        assert!(ana.iter().all(|r| r[1] >= tb.rows[1] && r[1] < tb.rows[2]), "in the second row: {tb:?}");
        // The text after it comes after the table.
        assert!(cell("Done.").iter().all(|r| r[1] >= *tb.rows.last().unwrap()));
        // A press on a cell falls on its words: the table's lines are side by side.
        let at = text.find("Monday").unwrap();
        let l = m.lines.iter().find(|l| l.cursors.first().is_some_and(|c| c.0 == at)).unwrap();
        assert_eq!(m.byte_at(l.cursors[0].1 + 1.0, (l.top + l.bottom) / 2.0), at);
        // Narrow enough, a table as wide as it is.
        let (_, small) = markdown(&mut t, "| a | b |\n|---|---|\n| 1 | 2 |", 320.0);
        assert!(*small.tables[0].cols.last().unwrap() < 120.0, "{:?}", small.tables[0]);
        assert!(small.tables[0].head);
        // So many columns that not even their words fit: each row a card, its head down the side.
        let head: Vec<String> = (1..=12).map(|k| format!("Column{k}")).collect();
        let src = format!("|{}|\n|{}|\n|{}|\n|{}|", head.join("|"), vec!["---"; 12].join("|"), vec!["uno"; 12].join("|"), vec!["dos"; 12].join("|"));
        let (_, many) = markdown(&mut t, &src, 300.0);
        assert_eq!(many.tables.len(), 2, "two rows, two cards");
        assert!(many.tables.iter().all(|t| t.cols.len() == 3 && t.rows.len() == 13 && !t.head && *t.cols.last().unwrap() <= 300.0), "{:?}", many.tables[0]);
    }

    #[test]
    fn a_point_hangs_past_its_mark() {
        let mut t = Typesetter::new();
        let src = "- a long point of a list that goes on and on and wraps at least once here\n1. **Estefanía**:\n   \"Buenos días,\n   un saludo\"\nafter";
        let (text, m) = markdown(&mut t, src, 220.0);
        let x_of = |what: &str| {
            let a = text.find(what).unwrap() as u32;
            m.glyphs.iter().find(|g| g.at == a).map(|g| g.rect[0]).unwrap()
        };
        let line_starts: Vec<f32> = m.lines.iter().map(|l| l.cursors.first().unwrap().1).collect();
        // The bullet at the edge, its words past it, and the wrapped line under its words.
        assert!(x_of("•") < 2.0);
        let words = x_of("a long");
        assert!(words > 4.0);
        assert!(m.lines.len() >= 6, "{line_starts:?}");
        assert!((line_starts[1] - words).abs() < 1.5, "the second line under the words: {line_starts:?} vs {words}");
        // The lines under «1.» are under its words, and the text after at the edge.
        let quote = x_of("\"Buenos");
        assert!((quote - x_of("Estefanía")).abs() < 1.5 && quote > 4.0, "{quote}");
        assert!((x_of("un saludo") - quote).abs() < 1.5);
        assert!(x_of("after") < 2.0);
        // Its lines go down in order, one each.
        let tops: Vec<f32> = m.lines.iter().map(|l| l.top).collect();
        assert!(tops.windows(2).all(|w| w[1] > w[0]), "{tops:?}");
        // Selecting from the bullet to the end of the point covers its lines.
        assert!(m.spans(0, text.find("\n1.").unwrap()).len() >= 2);
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
        texts.receive(Parcel { delivery: Delivery::Live { name: "x".into(), size: (1, 1), slot: None, generation: 4 }, pending_upload: Vec::new(), pending_masks: Vec::new(), used: 1, mask_used: 1, pending_fit: Vec::new(), pending_pictures: Vec::new(), exhausted: false, height: 4096, most: ATLAS_SIZE });
        assert_eq!(texts.atlas_height(), 4096);
        texts.too_large = Some(WorkingSet::default());
        texts.allow_atlas(8192);
        assert!(texts.exhausted && texts.too_large.is_none(), "what did not fit is tried again in the taller atlas");
        // A rebuild the workshop made before it heard of the card's limit: tried again, not given up on.
        texts.exhausted = false;
        texts.rebuilding = true;
        let snapshot = Snapshot { layouts: HashMap::new(), images: Vec::new(), animations: Vec::new(), live: HashMap::new() };
        let generation = texts.generation;
        texts.receive(Parcel { delivery: Delivery::Rebuilt { snapshot, working: WorkingSet::default(), generation }, pending_upload: Vec::new(), pending_masks: Vec::new(), used: 1, mask_used: 1, pending_fit: Vec::new(), pending_pictures: Vec::new(), exhausted: true, height: ATLAS_SIZE, most: ATLAS_SIZE });
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
    fn letters_go_in_their_own_one_byte_atlas_and_emoji_stay_in_the_rgba_one() {
        let mut t = Typesetter::new();
        let key = spanish();
        let expected = t.lay_out(&key).glyphs.len();
        assert!(expected > 30, "system font did not shape the Spanish fixture");
        // Every letter is a mask, one byte a pixel, with its gutter: nothing of it in the RGBA atlas.
        assert!(t.pending_upload.is_empty(), "a letter went into the RGBA atlas");
        assert!(!t.pending_masks.is_empty());
        for (padded, bytes) in &t.pending_masks {
            assert_eq!(bytes.len(), (padded.width * padded.height) as usize);
            assert!(bytes[..padded.width as usize].iter().all(|b| *b == 0), "the gutter above it is empty");
        }
        // The rows of each atlas a card's texture has to hold: the letters', not the images'.
        assert!(t.masks.used() > 1 && t.shelves.used() == 1, "rows used: {} of letters, {} of images", t.masks.used(), t.shelves.used());
        assert!(t.masks.used() < 200, "a line of text takes a few rows: {}", t.masks.used());
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
        t.masks.rows.clear();
        t.masks.next_y = ATLAS_SIZE;
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
                texts.pending_masks.clear();
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
    fn the_size_a_picture_is_read_at_for_its_box() {
        // #161: covering a 1920 × 1080 box, an 8084 × 6264 photo is needed at 1920 × 1488 (the box's
        // width rules), contained at 1394 × 1080; never more than its own pixels.
        assert_eq!(boxed_px((8084, 6264), (1920.0, 1080.0), true), (1920, 1488));
        assert_eq!(boxed_px((8084, 6264), (1920.0, 1080.0), false), (1394, 1080));
        assert_eq!(boxed_px((800, 600), (3840.0, 2160.0), true), (800, 600), "never made bigger");
        assert_eq!(boxed_px((800, 600), (0.0, 0.0), true), (1, 1));
    }

    #[test]
    fn decoding_with_fewer_copies_gives_the_same_pixels() {
        // #161: made smaller in its own kind of pixels (RGB) and premultiplied in place, a photo comes out
        // byte for byte as when it was turned into RGBA first; letterboxed or not, and one with alpha too.
        let rgb = image::RgbImage::from_fn(317, 211, |x, y| image::Rgb([(x * 7 % 256) as u8, (y * 13 % 256) as u8, ((x ^ y) % 256) as u8]));
        let rgba = image::RgbaImage::from_fn(317, 211, |x, y| image::Rgba([(x * 3 % 256) as u8, (y * 5 % 256) as u8, 90, ((x + y) % 256) as u8]));
        for px in [(100, 66), (100, 100), (317, 211), (400, 300)] {
            let before = fit(image::DynamicImage::ImageRgb8(rgb.clone()).to_rgba8(), px, false);
            assert_eq!(fit_decoded(image::DynamicImage::ImageRgb8(rgb.clone()), px, false), before, "rgb at {px:?}");
            assert_eq!(fit_decoded(image::DynamicImage::ImageRgba8(rgba.clone()), px, false), fit(rgba.clone(), px, false), "rgba at {px:?}");
        }
    }

    #[test]
    fn which_images_are_read_the_size_of_their_box() {
        let dir = std::env::temp_dir().join(format!("pleamar-boxed-scene-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(64, 48).save(dir.join("photo.jpg")).unwrap();
        image::RgbImage::new(64, 48).save(dir.join("still.png")).unwrap();
        std::fs::write(dir.join("s.plm"), r#"scene Boxed {
    surface { size: 400, 300 }
    image covered = file "photo.jpg", 0, 0
    image contained = file "still.png", 0, 0
    image stretched = file "photo.jpg", 0, 0
    image declared = file "photo.jpg", 64, 48
    image both = file "photo.jpg", 0, 0
    image art = pixels "still.png", 0, 0
    image unused = file "photo.jpg", 0, 0
    image covered { at: 0, 0; size: 400, 300; fit: cover }
    image contained { at: 0, 0; size: 100, 100; fit: contain }
    image stretched { at: 0, 0; size: 400, 300 }
    image declared { at: 0, 0; size: 400, 300; fit: cover }
    image both { at: 0, 0; size: 400, 300; fit: cover }
    image both { at: 0, 0; size: 40, 30; fit: fill }
    image art { at: 0, 0; size: 400, 300; fit: cover }
}
"#).unwrap();
        let (scene, _) = crate::language::read_file(dir.join("s.plm").to_str().unwrap()).unwrap();
        // Only the `0, 0` ones from a file, drawn only into a box: covering, or contained.
        let boxed = boxed_images(&scene);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(boxed, [(0, true), (1, false)]);
    }

    #[test]
    fn a_photo_drawn_to_cover_its_box_is_read_the_size_of_the_box() {
        // #161: an image declared `0, 0` and drawn with `fit: cover` is not read whole when the scene loads
        // but the size of its box when drawn; read again when the box grows; whole once it has grown three
        // times (a scene that enlarges it on purpose).
        let dir = std::env::temp_dir().join(format!("pleamar-boxed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("photo.jpg");
        image::RgbImage::from_fn(1600, 1200, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])).save(&path).unwrap();
        let (send, jobs) = channel();
        let mut texts = Texts::new(send);
        let mut t = Typesetter::new();
        t.work(Job::Limit(8192));
        texts.reset_boxed(1.0, &[(ImageSource::File(path.clone()), (0, 0))], &[], &[(0, true)]);
        deliver(&mut t, &mut texts, &jobs);
        assert!(texts.image(0, &[]).is_none() && texts.pending_pictures.is_empty(), "nothing read when the scene loads");
        let mut frame = |texts: &mut Texts, want: (f32, f32)| {
            texts.begin_frame();
            texts.boxed_image(0, want).expect("a boxed image");
            texts.end_frame();
            deliver(&mut t, texts, &jobs);
            texts.begin_frame();
            let slot = texts.boxed_image(0, want).expect("a boxed image").expect("read");
            texts.end_frame();
            (slot.width, slot.height)
        };
        assert_eq!(frame(&mut texts, (400.0, 200.0)), (400, 300), "the size that covers the box, its shape kept");
        assert_eq!(frame(&mut texts, (100.0, 100.0)), (400, 300), "a smaller box: not read again");
        assert_eq!(frame(&mut texts, (800.0, 400.0)), (800, 600), "a bigger one: read again");
        assert_eq!(frame(&mut texts, (1000.0, 400.0)), (1000, 750));
        assert_eq!(frame(&mut texts, (1100.0, 400.0)), (1600, 1200), "growing again and again: whole");
        assert_eq!(frame(&mut texts, (200.0, 100.0)), (1600, 1200));
        let _ = std::fs::remove_dir_all(&dir);
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

/// How letters are hinted: what fontconfig says for the system's sans-serif
/// (`hintstyle`, `hinting`), as GTK and Chrome read it; `PLEAMAR_FONT_HINTING`
/// (`none`, `slight`, `full`) says it over that. `slight` and `medium` are
/// FreeType's light hinting —the lines a letter sits on, on whole pixels—;
/// `full` also runs the font's own instructions, where it has them. Without
/// fontconfig (Windows, macOS, or no `fc-match`), slight on Linux and none
/// elsewhere, as those systems draw their letters.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hinting {
    None,
    Slight,
    Full,
}

pub fn hinting() -> Hinting {
    static IT: std::sync::OnceLock<Hinting> = std::sync::OnceLock::new();
    *IT.get_or_init(|| {
        let said = std::env::var("PLEAMAR_FONT_HINTING").ok().and_then(|v| parse_hinting(&v));
        let h = said.or_else(fontconfig_hinting).unwrap_or(if cfg!(target_os = "linux") { Hinting::Slight } else { Hinting::None });
        if std::env::var_os("PLEAMAR_DEBUG_TEXT").is_some() {
            eprintln!("text   · letters hinted: {h:?}");
        }
        h
    })
}

fn parse_hinting(v: &str) -> Option<Hinting> {
    match v.trim().to_ascii_lowercase().as_str() {
        "none" | "0" | "off" | "false" | "hintnone" => Some(Hinting::None),
        "slight" | "1" | "light" | "medium" | "2" | "hintslight" | "hintmedium" => Some(Hinting::Slight),
        "full" | "3" | "hintfull" => Some(Hinting::Full),
        _ => None,
    }
}

/// `fc-match -f '%{hinting}|%{hintstyle}' sans-serif`: «True|1» is slight.
fn fontconfig_hinting() -> Option<Hinting> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{hinting}|%{hintstyle}", "sans-serif"]).stderr(std::process::Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let (on, style) = said.split_once('|')?;
    if on.trim().eq_ignore_ascii_case("false") {
        return Some(Hinting::None);
    }
    // No hintstyle said, with hinting on: fontconfig's own default is slight.
    parse_hinting(style).or(Some(Hinting::Slight))
}

/// The lines a letter sits on, at the size it is drawn (y upwards, in pixels):
/// the baseline, the x-height and the capitals, each on a whole pixel.
struct BlueLines {
    /// (where the font has it, where it goes)
    lines: [(f32, f32); 3],
    /// How far past a line a curve still belongs to it (its overshoot).
    band: f32,
}

impl BlueLines {
    fn new(x_height: f32, cap_height: f32, size: f32) -> BlueLines {
        // A font that does not say them (an icon font): the baseline alone.
        let x = if x_height > 0.5 { x_height } else { 0.0 };
        let cap = if cap_height > x + 0.5 { cap_height } else { x };
        // The x-height is rounded with a lean upwards, as FreeType does: a
        // smaller one reads worse than a taller one. The capitals, as they come.
        let up = |v: f32, lean: f32| if v <= 0.0 { 0.0 } else { (v + lean).round().max(1.0) };
        BlueLines { lines: [(0.0, 0.0), (x, up(x, 0.1)), (cap, up(cap, 0.0))], band: (size * 0.04).clamp(0.25, 0.5) }
    }

    fn snap(&self, y: f32) -> f32 {
        let [(b, _), (x, xs), (c, cs)] = self.lines;
        // On a line, or past it by its overshoot: on it.
        if y <= b && y >= b - self.band {
            return 0.0;
        }
        if x > 0.0 && y >= x && y <= x + self.band {
            return xs;
        }
        if c > x && y >= c && y <= c + self.band {
            return cs;
        }
        // Between them, stretched; outside them, moved as the nearest one moved.
        if y < b {
            y
        } else if x > 0.0 && y < x {
            y * xs / x
        } else if c > x && y < c {
            xs + (y - x) * (cs - xs) / (c - x)
        } else if c > 0.0 {
            y + (cs - c)
        } else {
            y
        }
    }
}

#[cfg(test)]
mod hinting_tests {
    use super::{parse_hinting, BlueLines, Hinting};

    /// The lines a letter sits on land on whole pixels, and what overshoots
    /// them by a little (the round of an «o») lands on them too.
    #[test]
    fn the_x_height_and_the_baseline_fall_on_whole_pixels() {
        // Inter at 13 px: x-height 7.08, capitals 9.45.
        let b = BlueLines::new(7.08, 9.45, 13.0);
        assert_eq!(b.snap(0.0), 0.0);
        assert_eq!(b.snap(-0.15), 0.0, "an overshoot below the baseline sits on it");
        assert_eq!(b.snap(7.08), 7.0);
        assert_eq!(b.snap(7.25), 7.0, "an overshoot above the x-height sits on it");
        assert_eq!(b.snap(9.45), 9.0);
        // Halfway up, stretched in proportion.
        assert!((b.snap(3.54) - 3.5).abs() < 1e-4);
        // A descender keeps its depth; an ascender moves as the capitals did.
        assert_eq!(b.snap(-3.0), -3.0);
        assert!((b.snap(10.0) - (10.0 - 0.45)).abs() < 1e-4);
    }

    #[test]
    fn fontconfig_words_for_hinting() {
        assert_eq!(parse_hinting("1"), Some(Hinting::Slight));
        assert_eq!(parse_hinting("hintslight"), Some(Hinting::Slight));
        assert_eq!(parse_hinting("medium"), Some(Hinting::Slight));
        assert_eq!(parse_hinting("0"), Some(Hinting::None));
        assert_eq!(parse_hinting("full"), Some(Hinting::Full));
        assert_eq!(parse_hinting("what"), None);
    }
}
