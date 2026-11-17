//! `--film OUT`: the scene filmed, with no screen and no compositor, on a
//! virtual clock (`crate::clock`). Each round of the render is one frame, a
//! 60th of a second (`--fps`), however long it took to paint; the `--mouse`
//! script is played on that clock too. So the same scene and script give the
//! same frames on any machine, and ten seconds are filmed as fast as the card
//! paints them.
//!
//! Every surface of the scene is painted in textures of our own and read back;
//! each frame puts them together where they are on the scene's plane, and
//! draws on top what the script does: the keys pressed, as keycaps, the
//! clicks, as a ring, and where the pointer is. OUT is a folder (a PNG a frame)
//! or a video (`.mp4`, `.webm`, `.mkv`, `.mov`, `.gif`, made by `ffmpeg`).

use crate::scene::{Surface, ToRender};
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// What fills the frame where no surface is.
const BACKGROUND: [u8; 3] = [0x16, 0x18, 0x1c];
/// A surface as wide (or as tall) as the monitor, on a monitor of this size.
const MONITOR: (u32, u32) = (1280, 720);
/// How long a keycap stays, and the last part of it fading.
const CAP_FOR: f32 = 1.4;
const CAP_FADE: f32 = 0.35;
const RING_FOR: f32 = 0.45;

struct Film {
    out: PathBuf,
    fps: u32,
    /// The script still to play: when, and the step.
    script: Vec<(Duration, String)>,
    until: Duration,
    /// Each sheet: its place on the plane, its size in pixels, its last frame (BGRA, premultiplied).
    sheets: Vec<Shot>,
    /// The box of the plane filmed, once the first frame is written: x, y, width, height.
    canvas: Option<(f32, f32, u32, u32)>,
    writer: Option<Writer>,
    written: u64,
    /// What the script did, to draw: keycaps (what, when), clicks (where, when), the pointer.
    caps: Vec<(String, f32)>,
    rings: Vec<((f32, f32), f32)>,
    pointer: Option<(f32, f32)>,
    fonts: Option<(FontSystem, SwashCache)>,
    failed: Option<String>,
}

struct Shot {
    id: u32,
    origin: (f32, f32),
    px: (u32, u32),
    pixels: Option<Vec<u8>>,
}

enum Writer {
    Pictures(PathBuf),
    Video(std::process::Child, (u32, u32)),
}

static FILM: OnceLock<Mutex<Film>> = OnceLock::new();

/// Filming from now on: the clock is virtual. `script` is a `--mouse` script.
pub fn start(out: &str, fps: u32, script: Option<&str>, seconds: Option<f64>) -> Result<(), String> {
    let mut steps = Vec::new();
    for step in script.unwrap_or("").split_whitespace() {
        // The time is after the last `@`: a step can type one (`type:@Chief@600`).
        let (what, when) = step.rsplit_once('@').ok_or_else(|| format!("--mouse: «{step}» has no @ms"))?;
        let ms: u64 = when.parse().map_err(|_| format!("--mouse: «{when}» is not milliseconds"))?;
        steps.push((Duration::from_millis(ms), what.to_owned()));
    }
    steps.sort_by_key(|s| s.0);
    // As long as it is told, or a second past the script's last step (five without one).
    let until = match seconds {
        Some(s) => Duration::from_secs_f64(s),
        None => steps.last().map_or(Duration::from_secs(5), |s| s.0 + Duration::from_secs(1)),
    };
    let film = Film { out: PathBuf::from(out), fps: fps.clamp(1, 240), script: steps, until, sheets: Vec::new(), canvas: None, writer: None, written: 0, caps: Vec::new(), rings: Vec::new(), pointer: None, fonts: None, failed: None };
    FILM.set(Mutex::new(film)).map_err(|_| "already filming".to_owned())?;
    crate::clock::make_virtual();
    Ok(())
}

pub fn on() -> bool {
    FILM.get().is_some()
}

/// At the start of each round of the render: the last round's frame is
/// written and the clock goes on one frame (once there is a frame), and the
/// script's steps whose moment has come are told to the render. True when
/// the film is over.
pub fn round(to_render: &Sender<ToRender>) -> bool {
    let Some(f) = FILM.get() else { return false };
    let mut f = f.lock().unwrap();
    if f.failed.is_some() {
        return true;
    }
    if f.sheets.iter().any(|s| s.pixels.is_some()) {
        f.write_frame();
        crate::clock::advance(Duration::from_secs_f64(1.0 / f.fps as f64));
    }
    let gone = crate::clock::gone();
    while f.script.first().is_some_and(|s| s.0 <= gone) {
        let (_, what) = f.script.remove(0);
        f.note(&what, gone.as_secs_f32());
        for m in crate::mouse_step(&what) {
            let _ = to_render.send(m);
        }
    }
    gone >= f.until
}

/// The film is closed: the video is finished. Said what was written, or what failed.
pub fn finish() {
    let Some(f) = FILM.get() else { return };
    let mut f = f.lock().unwrap();
    let written = f.written;
    let out = f.out.display().to_string();
    match f.writer.take() {
        Some(Writer::Video(mut child, _)) => {
            drop(child.stdin.take());
            match child.wait() {
                Ok(s) if s.success() => println!("film   · {written} frames written to {out}"),
                Ok(s) => eprintln!("film   · ffmpeg ended with {s}: {out} may be incomplete"),
                Err(e) => eprintln!("film   · ffmpeg: {e}"),
            }
        }
        Some(Writer::Pictures(_)) => println!("film   · {written} frames written in {out}"),
        None => eprintln!("film   · nothing was painted: no frame written"),
    }
    if let Some(m) = &f.failed {
        eprintln!("film   · {m}");
    }
}

impl Film {
    /// What a step of the script is seen as.
    fn note(&mut self, what: &str, t: f32) {
        let cap = |s: &str| -> String {
            let s = match s {
                "Return" => "Enter",
                "BackSpace" => "Backspace",
                "Escape" => "Esc",
                "space" => "Space",
                "Prior" => "PgUp",
                "Next" => "PgDn",
                other => other,
            };
            // `Ctrl+z` → `Ctrl+Z`: a letter on a key is a capital.
            match s.rsplit_once('+') {
                Some((mods, k)) if k.chars().count() == 1 => format!("{mods}+{}", k.to_uppercase()),
                _ => s.to_owned(),
            }
        };
        match what {
            "click" | "down" | "right" => {
                if let Some(p) = self.pointer {
                    self.rings.push((p, t));
                }
            }
            "out" => self.pointer = None,
            "wheel+" => self.caps.push(("Wheel ↑".into(), t)),
            "wheel-" => self.caps.push(("Wheel ↓".into(), t)),
            w if w.starts_with("key:") => self.caps.push((cap(&w[4..]), t)),
            w if w.starts_with("keydown:") => self.caps.push((cap(&w[8..]), t)),
            w if w.starts_with("type:") => {
                let typed = w[5..].replace('_', " ");
                // Typing on from what was typed a moment ago: the same cap grows.
                match self.caps.last_mut() {
                    Some((last, at)) if last.starts_with('“') && t - *at < CAP_FOR => {
                        last.pop();
                        last.push_str(&typed);
                        last.push('”');
                        *at = t;
                    }
                    _ => self.caps.push((format!("“{typed}”"), t)),
                }
            }
            w => {
                if let Some((x, y)) = w.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                    self.pointer = Some((x, y));
                }
            }
        }
        let now = t;
        self.caps.retain(|c| now - c.1 < CAP_FOR);
        self.rings.retain(|r| now - r.1 < RING_FOR);
    }

    fn write_frame(&mut self) {
        let (x0, y0, w, h) = *self.canvas.get_or_insert_with(|| {
            let (mut a, mut b, mut c, mut d) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for s in &self.sheets {
                a = a.min(s.origin.0);
                b = b.min(s.origin.1);
                c = c.max(s.origin.0 + s.px.0 as f32);
                d = d.max(s.origin.1 + s.px.1 as f32);
            }
            // Even, for the video's colour format.
            let (w, h) = (((c - a).ceil() as u32 + 1) & !1, ((d - b).ceil() as u32 + 1) & !1);
            (a, b, w.max(2), h.max(2))
        });
        let mut frame = vec![0u8; (w * h * 4) as usize];
        for p in frame.chunks_exact_mut(4) {
            p.copy_from_slice(&[BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 255]);
        }
        for s in &self.sheets {
            let Some(px) = &s.pixels else { continue };
            let (ox, oy) = ((s.origin.0 - x0).round() as i64, (s.origin.1 - y0).round() as i64);
            for y in 0..s.px.1 as i64 {
                let fy = oy + y;
                if fy < 0 || fy >= h as i64 {
                    continue;
                }
                for x in 0..s.px.0 as i64 {
                    let fx = ox + x;
                    if fx < 0 || fx >= w as i64 {
                        continue;
                    }
                    let i = ((y * s.px.0 as i64 + x) * 4) as usize;
                    let (b, g, r, a) = (px[i] as u32, px[i + 1] as u32, px[i + 2] as u32, px[i + 3] as u32);
                    let o = ((fy * w as i64 + fx) * 4) as usize;
                    // Premultiplied over what is there.
                    for (k, c) in [r, g, b].into_iter().enumerate() {
                        frame[o + k] = (c + frame[o + k] as u32 * (255 - a) / 255).min(255) as u8;
                    }
                }
            }
        }
        crate::layout::frame(&frame, w, h, x0, y0);
        let t = crate::clock::gone().as_secs_f32();
        self.draw_marks(&mut frame, (w, h), (x0, y0), t);
        if let Err(e) = self.put(&frame, (w, h)) {
            self.failed = Some(e);
        }
        self.written += 1;
    }

    /// The pointer, the clicks and the keycaps, on the frame.
    fn draw_marks(&mut self, frame: &mut [u8], size: (u32, u32), origin: (f32, f32), t: f32) {
        let at = |p: (f32, f32)| (p.0 - origin.0, p.1 - origin.1);
        for &(p, when) in &self.rings {
            let k = ((t - when) / RING_FOR).clamp(0.0, 1.0);
            if k < 1.0 {
                let (x, y) = at(p);
                ring(frame, size, (x, y), 9.0 + 18.0 * k, 3.0, [255, 255, 255], 0.9 * (1.0 - k));
            }
        }
        if let Some(p) = self.pointer {
            let (x, y) = at(p);
            disc(frame, size, (x, y), 7.5, [20, 20, 24], 0.85);
            disc(frame, size, (x, y), 5.5, [255, 255, 255], 0.95);
        }
        let shown: Vec<(String, f32)> = self.caps.iter().filter(|c| t - c.1 < CAP_FOR && t >= c.1).cloned().collect();
        if shown.is_empty() {
            return;
        }
        let (fonts, swash) = self.fonts.get_or_insert_with(|| (crate::fonts::font_system(), SwashCache::new()));
        let size_px = (size.1 as f32 / 22.0).clamp(15.0, 30.0);
        let pad = size_px * 0.6;
        let laid: Vec<(Buffer, f32, f32)> = shown
            .iter()
            .map(|(text, when)| {
                let mut b = Buffer::new(fonts, Metrics::new(size_px, size_px * 1.25));
                b.set_size(None, None);
                b.set_text(text, &Attrs::new().family(Family::SansSerif).weight(Weight::SEMIBOLD), Shaping::Advanced, None);
                b.shape_until_scroll(fonts, false);
                let wide = b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
                let left = CAP_FOR - (t - when);
                (b, wide, (left / CAP_FADE).clamp(0.0, 1.0))
            })
            .collect();
        let gap = size_px * 0.5;
        let total: f32 = laid.iter().map(|l| l.1 + 2.0 * pad).sum::<f32>() + gap * (laid.len() as f32 - 1.0);
        let mut x = (size.0 as f32 - total) / 2.0;
        let y = size.1 as f32 - size_px * 1.25 - 2.0 * pad * 0.6 - size_px;
        for (mut b, wide, fade) in laid {
            let (bw, bh) = (wide + 2.0 * pad, size_px * 1.25 + 1.2 * pad);
            rounded(frame, size, [x, y, bw, bh], bh * 0.28, [24, 26, 32], 0.88 * fade);
            rounded_edge(frame, size, [x, y, bw, bh], bh * 0.28, [255, 255, 255], 0.22 * fade);
            let (tx, ty) = (x + pad, y + 0.6 * pad);
            b.draw(fonts, swash, Color::rgb(255, 255, 255), |gx, gy, gw, gh, c| {
                let a = c.a() as f32 / 255.0 * fade;
                for yy in 0..gh as i32 {
                    for xx in 0..gw as i32 {
                        blend(frame, size, (tx as i32 + gx + xx, ty as i32 + gy + yy), [c.r(), c.g(), c.b()], a);
                    }
                }
            });
            x += bw + gap;
        }
    }

    fn put(&mut self, frame: &[u8], size: (u32, u32)) -> Result<(), String> {
        if self.writer.is_none() {
            let out = self.out.clone();
            let video = out.extension().and_then(|e| e.to_str()).is_some_and(|e| ["mp4", "webm", "mkv", "mov", "gif"].contains(&e.to_ascii_lowercase().as_str()));
            self.writer = Some(if video {
                let mut c = std::process::Command::new("ffmpeg");
                c.args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{}x{}", size.0, size.1), "-r", &self.fps.to_string(), "-i", "-"]);
                match out.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
                    Some("gif") => {}
                    Some("webm") => {
                        c.args(["-c:v", "libvpx-vp9", "-b:v", "0", "-crf", "30", "-pix_fmt", "yuv420p"]);
                    }
                    _ => {
                        c.args(["-c:v", "libx264", "-preset", "medium", "-crf", "16", "-pix_fmt", "yuv420p"]);
                    }
                }
                let child = c.arg(&out).stdin(std::process::Stdio::piped()).spawn().map_err(|e| format!("ffmpeg could not be started ({e}): to film a video it has to be installed; a folder gets PNG frames without it"))?;
                Writer::Video(child, size)
            } else {
                std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
                Writer::Pictures(out)
            });
        }
        match self.writer.as_mut().unwrap() {
            Writer::Video(child, s) => {
                if *s != size {
                    return Err("the frame changed size while filming".into());
                }
                child.stdin.as_mut().unwrap().write_all(frame).map_err(|e| format!("ffmpeg stopped taking frames: {e}"))
            }
            Writer::Pictures(dir) => {
                let path = dir.join(format!("frame-{:05}.png", self.written + 1));
                image::save_buffer(&path, frame, size.0, size.1, image::ExtendedColorType::Rgba8).map_err(|e| format!("{}: {e}", path.display()))
            }
        }
    }
}

fn blend(frame: &mut [u8], size: (u32, u32), (x, y): (i32, i32), c: [u8; 3], a: f32) {
    if x < 0 || y < 0 || x >= size.0 as i32 || y >= size.1 as i32 || a <= 0.0 {
        return;
    }
    let o = ((y as u32 * size.0 + x as u32) * 4) as usize;
    for k in 0..3 {
        frame[o + k] = (c[k] as f32 * a + frame[o + k] as f32 * (1.0 - a)).round() as u8;
    }
}

/// Shapes by their distance: covered by how much of each pixel falls inside.
fn shape(frame: &mut [u8], size: (u32, u32), bounds: [f32; 4], c: [u8; 3], a: f32, inside: impl Fn(f32, f32) -> f32) {
    let (x0, y0) = (bounds[0].floor() as i32 - 1, bounds[1].floor() as i32 - 1);
    let (x1, y1) = ((bounds[0] + bounds[2]).ceil() as i32 + 1, (bounds[1] + bounds[3]).ceil() as i32 + 1);
    for y in y0.max(0)..y1.min(size.1 as i32) {
        for x in x0.max(0)..x1.min(size.0 as i32) {
            let cover = (0.5 - inside(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0);
            blend(frame, size, (x, y), c, a * cover);
        }
    }
}

fn disc(frame: &mut [u8], size: (u32, u32), c: (f32, f32), r: f32, color: [u8; 3], a: f32) {
    shape(frame, size, [c.0 - r, c.1 - r, 2.0 * r, 2.0 * r], color, a, |x, y| ((x - c.0).hypot(y - c.1)) - r);
}

fn ring(frame: &mut [u8], size: (u32, u32), c: (f32, f32), r: f32, width: f32, color: [u8; 3], a: f32) {
    shape(frame, size, [c.0 - r - width, c.1 - r - width, 2.0 * (r + width), 2.0 * (r + width)], color, a, |x, y| ((x - c.0).hypot(y - c.1) - r).abs() - width / 2.0);
}

fn rounded_distance(b: [f32; 4], r: f32, x: f32, y: f32) -> f32 {
    let (cx, cy) = (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
    let (qx, qy) = ((x - cx).abs() - (b[2] / 2.0 - r), (y - cy).abs() - (b[3] / 2.0 - r));
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

fn rounded(frame: &mut [u8], size: (u32, u32), b: [f32; 4], r: f32, color: [u8; 3], a: f32) {
    shape(frame, size, b, color, a, |x, y| rounded_distance(b, r, x, y));
}

fn rounded_edge(frame: &mut [u8], size: (u32, u32), b: [f32; 4], r: f32, color: [u8; 3], a: f32) {
    shape(frame, size, b, color, a, |x, y| rounded_distance(b, r, x, y).abs() - 0.6);
}

/// The surfaces, put up as sheets painted in our own textures.
pub struct FilmPlatform;

impl super::Platform for FilmPlatform {
    fn run(self: Box<Self>, surfaces: Vec<Surface>, _extra_height: u32, _instance: wgpu::Instance, to_render: Sender<ToRender>) {
        let mut id = 0u32;
        for (which, s) in surfaces.iter().enumerate() {
            // The lock screen only exists while locked, and a copy for another monitor is not filmed.
            if s.lock_screen || s.instance > 0 {
                continue;
            }
            let px = (if s.width == 0 { MONITOR.0 } else { s.width }, if s.height == 0 { MONITOR.1 } else { s.height });
            if let Some(f) = FILM.get() {
                f.lock().unwrap().sheets.push(Shot { id, origin: s.origin, px, pixels: None });
            }
            let frames = FilmFrames { sheet: id, px, textures: Vec::new(), latest: None, to_render: to_render.clone() };
            let _ = to_render.send(ToRender::Sheet(Box::new(crate::gpu::NewSheet {
                id,
                target: crate::gpu::Target::Frames(Box::new(frames)),
                window: Box::new(FilmWindow),
                scale: 1.0,
                size: px,
                mhz: 60_000,
                name: format!("film{}", if s.name.is_empty() { String::new() } else { format!(" · {}", s.name) }),
                view: crate::gpu::View { surface: which, popup: None, origin: s.origin, size: (px.0 as f32, px.1 as f32) },
            })));
            id += 1;
        }
        // Until the render is done: the film ends it (`round`).
        while !crate::RENDER_DONE.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

struct FilmWindow;

impl super::PlatformWindow for FilmWindow {
    fn update_input_region(&self, _: &[[i32; 4]]) {}
    fn cursor(&self, _: crate::scene::Cursor) {}
    fn keyboard(&self, _: crate::scene::Keyboard) {}
}

/// `PLEAMAR_FILM_HANDED=1`: the surfaces filmed as handed to another compositor (`FilmFrames::whole_screen`).
fn film_handed() -> bool {
    std::env::var_os("PLEAMAR_FILM_HANDED").is_some_and(|v| !v.is_empty() && v != "0")
}

struct FilmFrames {
    sheet: u32,
    px: (u32, u32),
    textures: Vec<wgpu::Texture>,
    latest: Option<usize>,
    to_render: Sender<ToRender>,
}

impl crate::gpu::Frames for FilmFrames {
    fn acquire(&mut self, device: &wgpu::Device, _modifiers: &[u64]) -> Option<(usize, wgpu::Texture)> {
        if self.textures.is_empty() {
            for _ in 0..2 {
                self.textures.push(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("a filmed frame"),
                    size: wgpu::Extent3d { width: self.px.0.max(1), height: self.px.1.max(1), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                }));
            }
        }
        let k = self.latest.map_or(0, |l| 1 - l);
        Some((k, self.textures[k].clone()))
    }

    /// Read back at once: a film is not in a hurry, and the frame is what it is for.
    fn present(&mut self, which: usize, _done: wgpu::SubmissionIndex, device: &wgpu::Device, queue: &wgpu::Queue, _changed: Option<[u32; 4]>) {
        self.latest = Some(which);
        let (w, h) = self.px;
        let row = (w * 4).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor { label: Some("a filmed frame, read back"), size: (row * h) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("film") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &self.textures[which], mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let index = queue.submit(Some(encoder.finish()));
        let mapped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = mapped.clone();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| flag.store(r.is_ok(), std::sync::atomic::Ordering::Release));
        let _ = device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(Duration::from_secs(5)) });
        if mapped.load(std::sync::atomic::Ordering::Acquire) {
            let Ok(data) = buffer.slice(..).get_mapped_range() else { return };
            let mut pixels = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                let start = (y * row) as usize;
                pixels.extend_from_slice(&data[start..start + (w * 4) as usize]);
            }
            drop(data);
            buffer.unmap();
            if let Some(f) = FILM.get() {
                if let Some(s) = f.lock().unwrap().sheets.iter_mut().find(|s| s.id == self.sheet) {
                    s.pixels = Some(pixels);
                }
            }
        }
        let _ = self.to_render.send(ToRender::Frame(self.sheet));
    }

    /// A monitor of pleamar-wm's own, unless `PLEAMAR_FILM_HANDED=1`: then each
    /// surface is filmed as one handed to another compositor (Hyprland, sway…),
    /// painted on the canvas it keeps and brought up to date by pieces —what
    /// is behind it is the film's background, with nothing to blur it—.
    fn whole_screen(&self) -> bool {
        !film_handed()
    }

    fn resize(&mut self, px: (u32, u32)) {
        if px != self.px {
            self.px = px;
            self.textures.clear();
            self.latest = None;
            if let Some(f) = FILM.get() {
                if let Some(s) = f.lock().unwrap().sheets.iter_mut().find(|s| s.id == self.sheet) {
                    s.px = px;
                    s.pixels = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn film() -> Film {
        Film { out: PathBuf::new(), fps: 60, script: Vec::new(), until: Duration::ZERO, sheets: Vec::new(), canvas: None, writer: None, written: 0, caps: Vec::new(), rings: Vec::new(), pointer: None, fonts: None, failed: None }
    }

    #[test]
    fn keys_are_drawn_as_they_are_read_on_a_keyboard() {
        let mut f = film();
        f.note("key:Ctrl+z", 0.0);
        f.note("key:Ctrl+Shift+z", 0.1);
        f.note("key:Return", 0.2);
        f.note("keydown:BackSpace", 0.3);
        f.note("wheel-", 0.4);
        let caps: Vec<&str> = f.caps.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(caps, ["Ctrl+Z", "Ctrl+Shift+Z", "Enter", "Backspace", "Wheel ↓"]);
    }

    #[test]
    fn what_is_typed_in_a_row_is_one_cap_and_old_caps_go() {
        let mut f = film();
        f.note("type:hola", 0.0);
        f.note("type:_mundo", 0.5);
        assert_eq!(f.caps.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["“hola mundo”"]);
        // Long after, another one, and the first has gone.
        f.note("type:adiós", 3.0);
        assert_eq!(f.caps.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["“adiós”"]);
    }

    #[test]
    fn a_click_rings_where_the_pointer_is_and_nowhere_without_one() {
        let mut f = film();
        f.note("click", 0.0);
        assert!(f.rings.is_empty());
        f.note("120,40", 0.1);
        f.note("click", 0.2);
        assert_eq!(f.rings, [((120.0, 40.0), 0.2)]);
        f.note("out", 0.3);
        assert_eq!(f.pointer, None);
    }
}
