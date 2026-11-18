//! `--layout SCENE`: how a scene is laid out once it is painted, for whoever
//! checks it without looking —a desktop's «create with AI», a test—. The scene
//! is filmed for a moment with no screen (as `--film`), its texts gathered as
//! painted (the agents' `describe` already gathers them), and the last frame
//! looked at. The last line on stdout is one JSON object:
//!
//! `{"type":"layout","ok":…,"canvas":[x,y,w,h],"surfaces":[[x0,y0,x1,y1]…],
//!   "texts":[{"text","bounds":[x0,y0,x1,y1],"contrast"}…],"problems":[{"code","message","bounds"?}…]}`
//!
//! Problems: `text_overlap` (two texts over each other), `text_outside` (a
//! text past the edge of its surface), `low_contrast` (letters hard to tell
//! from what is behind them), `shadow_cut` and `drawing_cut` (the render's own
//! warnings: a shadow or a shape that needs more room than its surface has).
//! And `warnings`, nearly right but not quite: `misaligned` (texts whose left
//! edges are 0.5–2.5 px apart; only texts put by their left edge, not centred
//! ones), `uneven_spacing` (a gap in a column of texts 1–3 px off the others'),
//! `tight_margin` (a text less than 8 px from an edge of its surface, unless
//! that edge is the screen's) and `lines_overlap` (a text that may take more
//! lines, as its `lines:` allows, and would then reach the text below it).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ON: AtomicBool = AtomicBool::new(false);

struct Seen {
    texts: Vec<(String, [f32; 4])>,
    /// For each text: whether it is put by its left edge, and the room its `lines:` give it.
    left: Vec<bool>,
    room: Vec<[f32; 4]>,
    surfaces: Vec<[f32; 4]>,
    /// For each surface: it covers its monitor, so its edges are the screen's.
    full: Vec<bool>,
    notes: Vec<(&'static str, String)>,
    /// The last frame filmed: RGBA, its size and where it sits on the plane.
    frame: Option<(Vec<u8>, u32, u32, f32, f32)>,
    dir: Option<std::path::PathBuf>,
}

static SEEN: Mutex<Seen> = Mutex::new(Seen { texts: Vec::new(), left: Vec::new(), room: Vec::new(), surfaces: Vec::new(), full: Vec::new(), notes: Vec::new(), frame: None, dir: None });

/// Asked for: where the film goes (a folder of its own, gone at the end).
pub fn start() -> String {
    ON.store(true, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("pleamar-layout-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    SEEN.lock().unwrap().dir = Some(dir.clone());
    dir.display().to_string()
}

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Each round the render paints: the texts as painted (not those a list scrolled
/// out of sight) and where its surfaces are, each with whether it covers its monitor.
pub fn texts(seen: &[crate::gpu::SeenText], surfaces: Vec<([f32; 4], bool)>) {
    let mut s = SEEN.lock().unwrap();
    // Where its letters are, not the box it was given: a word in a wide `width` is
    // that word, for what it covers, where it ends and how it stands out.
    let shown: Vec<&crate::gpu::SeenText> = seen.iter().filter(|t| !t.clipped && !t.text.trim().is_empty()).collect();
    s.texts = shown.iter().map(|t| (t.text.clone(), t.ink)).collect();
    s.left = shown.iter().map(|t| t.left).collect();
    s.room = shown.iter().map(|t| t.room).collect();
    s.surfaces = surfaces.iter().map(|x| x.0).collect();
    s.full = surfaces.iter().map(|x| x.1).collect();
}

/// One of the render's own warnings (a shadow or a drawing its surface cuts).
pub fn note(code: &'static str, message: String) {
    if !on() {
        return;
    }
    let mut s = SEEN.lock().unwrap();
    if !s.notes.iter().any(|(_, m)| *m == message) {
        s.notes.push((code, message));
    }
}

/// The film's frame, as written (RGB over its background, alpha 255).
pub fn frame(rgba: &[u8], w: u32, h: u32, x0: f32, y0: f32) {
    if on() {
        SEEN.lock().unwrap().frame = Some((rgba.to_vec(), w, h, x0, y0));
    }
}

fn luminance(p: &[u8]) -> f32 {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(p[0]) + 0.7152 * lin(p[1]) + 0.0722 * lin(p[2])
}

/// How well the letters stand out, from the frame: the box's darkest and
/// lightest pixels (the 5th and 95th of a hundred), as a WCAG ratio. A box with
/// letters is mostly what is behind them, so one end is the background and the
/// other the letters; antialiasing only makes it a little lower.
fn contrast(frame: &(Vec<u8>, u32, u32, f32, f32), b: [f32; 4]) -> Option<f32> {
    let (px, w, h, x0, y0) = frame;
    let (a, c) = (((b[0] - x0).floor().max(0.0)) as u32, ((b[2] - x0).ceil().max(0.0) as u32).min(*w));
    let (d, e) = (((b[1] - y0).floor().max(0.0)) as u32, ((b[3] - y0).ceil().max(0.0) as u32).min(*h));
    if c <= a + 1 || e <= d + 1 {
        return None;
    }
    let mut l: Vec<f32> = Vec::with_capacity(((c - a) * (e - d)) as usize);
    for y in d..e {
        for x in a..c {
            let i = ((y * w + x) * 4) as usize;
            l.push(luminance(&px[i..i + 3]));
        }
    }
    l.sort_by(f32::total_cmp);
    let lo = l[l.len() * 5 / 100];
    let hi = l[(l.len() * 95 / 100).min(l.len() - 1)];
    Some((hi + 0.05) / (lo + 0.05))
}

fn area(b: [f32; 4]) -> f32 {
    (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0)
}

fn meet(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

/// What is wrong in what was seen: the problems, each with its words.
pub fn problems(texts: &[(String, [f32; 4])], surfaces: &[[f32; 4]], contrasts: &[Option<f32>]) -> Vec<serde_json::Value> {
    use serde_json::json;
    let mut out = Vec::new();
    let short = |t: &str| -> String {
        let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.chars().count() > 30 { format!("{}…", t.chars().take(29).collect::<String>()) } else { t }
    };
    for i in 0..texts.len() {
        for j in i + 1..texts.len() {
            let (a, b) = (texts[i].1, texts[j].1);
            let m = meet(a, b);
            // Letters over letters: more than a sliver (two pixels each way) and a fifth of the smaller.
            if m[2] - m[0] > 2.0 && m[3] - m[1] > 2.0 && area(m) > 0.2 * area(a).min(area(b)) {
                out.push(json!({ "code": "text_overlap", "message": format!("«{}» and «{}» are drawn over each other", short(&texts[i].0), short(&texts[j].0)), "bounds": m }));
            }
        }
    }
    for (t, b) in texts {
        let centre = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        let Some(s) = surfaces.iter().find(|s| centre.0 >= s[0] && centre.0 <= s[2] && centre.1 >= s[1] && centre.1 <= s[3]) else { continue };
        let past = [s[0] - b[0], s[1] - b[1], b[2] - s[2], b[3] - s[3]];
        if past.iter().any(|p| *p > 1.0) {
            out.push(json!({ "code": "text_outside", "message": format!("«{}» goes past the edge of its surface", short(t)), "bounds": b }));
        }
    }
    for ((t, b), c) in texts.iter().zip(contrasts) {
        if let Some(c) = c.filter(|c| *c < 3.0) {
            out.push(json!({ "code": "low_contrast", "message": format!("«{}» is hard to read: its letters and what is behind them are {c:.1}:1 apart (3:1 at least, 4.5:1 for small text)", short(t)), "bounds": b }));
        }
    }
    out
}

/// What is nearly right but not quite, as warnings: texts whose left edges
/// are almost on the same line (0.5 to 2.5 px apart: meant to be aligned, and
/// are not), and, in a column of three or more texts sharing a left edge, a
/// gap between two of them 1 to 3 px off the others'.
pub fn alignment(texts: &[(String, [f32; 4])], left: &[bool]) -> Vec<serde_json::Value> {
    use serde_json::json;
    let mut out = Vec::new();
    let short = |t: &str| -> String { t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(30).collect() };
    let mut told: Vec<(usize, usize)> = Vec::new();
    for i in 0..texts.len() {
        for j in i + 1..texts.len() {
            // A centred text starts where its words make it start: its left edge says nothing.
            if !left.get(i).copied().unwrap_or(true) || !left.get(j).copied().unwrap_or(true) {
                continue;
            }
            let d = (texts[i].1[0] - texts[j].1[0]).abs();
            if d > 0.5 && d <= 2.5 && !told.iter().any(|&(a, b)| a == i || b == j || a == j || b == i) {
                told.push((i, j));
                out.push(json!({ "code": "misaligned", "level": "warning", "message": format!("«{}» and «{}» start {d:.1} px apart: put them on the same edge", short(&texts[i].0), short(&texts[j].0)) }));
            }
        }
    }
    // Columns: texts whose left edges match within half a pixel, top to bottom.
    let mut done = vec![false; texts.len()];
    for i in 0..texts.len() {
        if done[i] {
            continue;
        }
        let mut col: Vec<usize> = (0..texts.len()).filter(|&j| left.get(j).copied().unwrap_or(true) && (texts[j].1[0] - texts[i].1[0]).abs() <= 0.5).collect();
        for &j in &col {
            done[j] = true;
        }
        if col.len() < 3 {
            continue;
        }
        col.sort_by(|&a, &b| texts[a].1[1].total_cmp(&texts[b].1[1]));
        let gaps: Vec<f32> = col.windows(2).map(|w| texts[w[1]].1[1] - texts[w[0]].1[3]).collect();
        if gaps.iter().any(|g| *g < 0.0) {
            continue; // over each other: text_overlap says it
        }
        let mut sorted = gaps.clone();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[sorted.len() / 2];
        for (k, g) in gaps.iter().enumerate() {
            let off = (g - median).abs();
            if off >= 1.0 && off <= 3.0 {
                out.push(json!({ "code": "uneven_spacing", "level": "warning", "message": format!("between «{}» and «{}» there are {g:.0} px, and {median:.0} between the others in that column", short(&texts[col[k]].0), short(&texts[col[k + 1]].0)) }));
            }
        }
    }
    out
}

/// The room round texts, as warnings: a text closer than 8 px to an edge of its
/// surface (not past it: that is `text_outside`), except an edge that is the
/// screen's (a menu bar's words sit near the top of the monitor, and should).
pub fn margins(texts: &[(String, [f32; 4])], surfaces: &[[f32; 4]], full: &[bool]) -> Vec<serde_json::Value> {
    use serde_json::json;
    const ROOM: f32 = 8.0;
    let short = |t: &str| -> String { t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(30).collect() };
    let mut out = Vec::new();
    for (t, b) in texts {
        let centre = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        let Some(k) = surfaces.iter().position(|s| centre.0 >= s[0] && centre.0 <= s[2] && centre.1 >= s[1] && centre.1 <= s[3]) else { continue };
        if full.get(k).copied().unwrap_or(false) {
            continue;
        }
        let s = surfaces[k];
        let gaps = [b[0] - s[0], b[1] - s[1], s[2] - b[2], s[3] - b[3]];
        if gaps.iter().any(|g| *g < -1.0) {
            continue; // past the edge: text_outside says it
        }
        let sides = ["left", "top", "right", "bottom"];
        let near: Vec<String> = (0..4).filter(|&i| gaps[i] < ROOM).map(|i| format!("{:.0} px from the {}", gaps[i].max(0.0), sides[i])).collect();
        if !near.is_empty() {
            out.push(json!({ "code": "tight_margin", "level": "warning", "message": format!("«{}» is {} of its surface: leave {ROOM:.0} px at least", short(t), near.join(" and ")), "bounds": b }));
        }
    }
    out
}

/// A text on fewer lines than its `lines:` allow, with another under it inside
/// the room the rest would take: as soon as its words grow, they are drawn over
/// it. A warning, since today they are not.
pub fn growth(texts: &[(String, [f32; 4])], room: &[[f32; 4]]) -> Vec<serde_json::Value> {
    use serde_json::json;
    let short = |t: &str| -> String { t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(30).collect() };
    let mut out = Vec::new();
    for (i, (t, b)) in texts.iter().enumerate() {
        let Some(r) = room.get(i).copied().filter(|r| r[3] > b[3] + 0.5) else { continue };
        for (j, (u, c)) in texts.iter().enumerate() {
            // Below it (starting under its first line), side by side with its room, and inside it.
            let across = r[2].min(c[2]) - r[0].max(c[0]);
            if j != i && c[1] > b[1] + 1.0 && across > 2.0 && c[1] < r[3] - 2.0 {
                out.push(json!({ "code": "lines_overlap", "level": "warning", "message": format!("«{}» may grow {:.0} px more (its `lines:` allow it) and would then be drawn over «{}»", short(t), r[3] - b[3], short(u)), "bounds": meet(r, *c) }));
            }
        }
    }
    out
}

/// At the end: the report, as the last line on stdout, and the film gone.
pub fn finish() {
    use serde_json::json;
    if !on() {
        return;
    }
    let s = SEEN.lock().unwrap();
    let contrasts: Vec<Option<f32>> = s.texts.iter().map(|(_, b)| s.frame.as_ref().and_then(|f| contrast(f, *b))).collect();
    let mut problems = problems(&s.texts, &s.surfaces, &contrasts);
    for (code, m) in &s.notes {
        problems.push(json!({ "code": code, "message": m.trim_start_matches("render · ") }));
    }
    let warnings = [alignment(&s.texts, &s.left), margins(&s.texts, &s.surfaces, &s.full), growth(&s.texts, &s.room)].concat();
    let report = json!({
        "type": "layout",
        "ok": problems.is_empty(),
        "canvas": s.frame.as_ref().map(|f| json!([f.3, f.4, f.1, f.2])),
        "surfaces": s.surfaces,
        "texts": s.texts.iter().zip(&contrasts).map(|((t, b), c)| json!({ "text": t, "bounds": b, "contrast": c.map(|c| (c as f64 * 10.0).round() / 10.0) })).collect::<Vec<_>>(),
        "problems": problems,
        "warnings": warnings,
    });
    if let Some(d) = &s.dir {
        let _ = std::fs::remove_dir_all(d);
    }
    println!("{report}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_over_each_other_are_found_and_neighbours_are_not() {
        let t = |s: &str, b: [f32; 4]| (s.to_owned(), b);
        let texts = [t("21°", [20.0, 20.0, 80.0, 60.0]), t("Cloudy", [60.0, 30.0, 140.0, 50.0]), t("Max 21°", [20.0, 64.0, 100.0, 80.0])];
        let p = problems(&texts, &[[0.0, 0.0, 344.0, 164.0]], &[None, None, None]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert_eq!(p[0]["code"], "text_overlap");
        assert!(p[0]["message"].as_str().unwrap().contains("Cloudy"));
    }

    #[test]
    fn a_text_past_its_surface_and_a_faint_one_are_said() {
        let texts = [("Wide words".to_owned(), [300.0, 10.0, 360.0, 30.0]), ("Faint".to_owned(), [10.0, 40.0, 60.0, 60.0])];
        let p = problems(&texts, &[[0.0, 0.0, 344.0, 164.0]], &[Some(9.0), Some(1.6)]);
        let codes: Vec<&str> = p.iter().map(|x| x["code"].as_str().unwrap()).collect();
        assert_eq!(codes, ["text_outside", "low_contrast"]);
    }

    #[test]
    fn nearly_aligned_and_uneven_are_warned_not_exact_ones() {
        let t = |s: &str, b: [f32; 4]| (s.to_owned(), b);
        // A column at 16 with one text at 17.5, and gaps of 8, 8 and 10.
        let texts = [
            t("Title", [16.0, 10.0, 90.0, 26.0]),
            t("One", [16.0, 34.0, 60.0, 50.0]),
            t("Two", [16.0, 58.0, 60.0, 74.0]),
            t("Three", [16.0, 84.0, 60.0, 100.0]),
            t("Off", [17.5, 120.0, 60.0, 136.0]),
        ];
        let w = alignment(&texts, &[true; 5]);
        let codes: Vec<&str> = w.iter().map(|x| x["code"].as_str().unwrap()).collect();
        assert_eq!(codes, ["misaligned", "uneven_spacing"], "{w:?}");
        assert!(w[1]["message"].as_str().unwrap().contains("Three"));
        // Exactly aligned and evenly spaced: nothing.
        let fine = [t("A", [16.0, 10.0, 40.0, 26.0]), t("B", [16.0, 34.0, 40.0, 50.0]), t("C", [16.0, 58.0, 40.0, 74.0]), t("Far", [120.0, 10.0, 160.0, 26.0])];
        assert!(alignment(&fine, &[true; 4]).is_empty());
        // Centred texts start where their words make them: 1.5 px apart is not a fault.
        let centred = [t("Small", [40.0, 10.0, 80.0, 26.0]), t("Medium", [41.5, 40.0, 79.0, 56.0])];
        assert!(alignment(&centred, &[false, false]).is_empty());
        assert_eq!(alignment(&centred, &[true, true]).len(), 1);
    }

    #[test]
    fn a_text_against_its_edge_is_warned_unless_the_edge_is_the_screens() {
        let t = |s: &str, b: [f32; 4]| (s.to_owned(), b);
        // A widget: «RED» at 0, 1; «Fine» 16 px in.
        let texts = [t("RED", [0.0, 1.0, 22.0, 15.0]), t("Fine", [16.0, 40.0, 60.0, 56.0])];
        let w = margins(&texts, &[[0.0, 0.0, 344.0, 164.0]], &[false]);
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0]["code"], "tight_margin");
        assert!(w[0]["message"].as_str().unwrap().contains("0 px from the left and 1 px from the top"), "{w:?}");
        // A menu bar on a surface that covers the monitor: its words near the top are meant.
        assert!(margins(&[t("Window", [346.0, 7.0, 397.0, 24.0])], &[[0.0, 0.0, 1280.0, 720.0]], &[true]).is_empty());
        // Past the edge is text_outside's, not this.
        assert!(margins(&[t("Wide", [300.0, 10.0, 360.0, 30.0])], &[[0.0, 0.0, 344.0, 164.0]], &[false]).is_empty());
    }

    #[test]
    fn a_text_that_may_grow_over_the_one_below_is_warned() {
        let t = |s: &str, b: [f32; 4]| (s.to_owned(), b);
        // Two lines of 17 px of three allowed (room to 90.7), and a label at 72: e08.
        let texts = [t("Long", [16.0, 40.0, 327.0, 73.8]), t("Below", [16.0, 72.0, 168.0, 88.9])];
        let room = [[16.0, 40.0, 328.0, 90.7], texts[1].1];
        let w = growth(&texts, &room);
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0]["code"], "lines_overlap");
        // With all its lines (its room is itself) or the other well below: nothing.
        assert!(growth(&texts, &[texts[0].1, texts[1].1]).is_empty());
        let far = [texts[0].clone(), t("Below", [16.0, 100.0, 168.0, 116.0])];
        assert!(growth(&far, &room).is_empty());
    }

    #[test]
    fn contrast_is_read_from_the_pixels() {
        // A white box with a black stroke in it: high; grey on grey: low.
        let mut px = vec![255u8; 20 * 10 * 4];
        for x in 5..9 {
            for y in 2..8 {
                let i = ((y * 20 + x) * 4) as usize;
                px[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        let f = (px, 20, 10, 0.0, 0.0);
        assert!(contrast(&f, [0.0, 0.0, 20.0, 10.0]).unwrap() > 15.0);
        let g = (vec![128u8; 20 * 10 * 4], 20, 10, 0.0, 0.0);
        assert!(contrast(&g, [0.0, 0.0, 20.0, 10.0]).unwrap() < 1.1);
    }
}
