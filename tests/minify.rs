//! Drawn smaller than they are, images are averaged over each screen pixel
//! (`docs/minify.md`), not sampled once: fine detail turns into its average
//! instead of a moiré. Filmed (`--film`, a virtual clock) with and without it
//! (`PLEAMAR_MINIFY=off`): at its own size an image is the same bit for bit,
//! made smaller its stripes come out an even grey, and a picture made very
//! small takes nothing from its neighbour in the atlas.
//! Without a card to paint with, it says so and passes: `cargo test --test minify -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn has_card() -> bool {
    let instance = wgpu::Instance::default();
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).is_ok()
}

/// One-pixel stripes, black and white: what a moiré is made of.
fn stripes(path: &Path) {
    image::RgbaImage::from_fn(128, 128, |x, _| if x % 2 == 0 { image::Rgba([0, 0, 0, 255]) } else { image::Rgba([255, 255, 255, 255]) }).save(path).unwrap();
}
fn plain(path: &Path, rgb: [u8; 3]) {
    image::RgbaImage::from_pixel(128, 128, image::Rgba([rgb[0], rgb[1], rgb[2], 255])).save(path).unwrap();
}

const SCENE: &str = r#"language 0.1
scene Minify {
    surface { size: 320, 140 }
    image stripes = file "stripes.png", 128, 128
    image red = file "red.png", 128, 128
    image blue = file "blue.png", 128, 128
    box { from: 0, 0; size: 320, 140; color: #000000 }
    image stripes { at: 0, 0; size: 128, 128 }
    image stripes { at: 140, 0; size: 41, 41 }
    image red { at: 200, 0; size: 12, 12 }
    image blue { at: 212, 0; size: 12, 12 }
}
"#;

fn film(minify: bool) -> image::RgbaImage {
    let dir: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("minify-{}-{minify}", std::process::id()));
    let home = dir.join("home");
    for d in [".config", ".cache", ".local/share", ".local/state", "run"] {
        std::fs::create_dir_all(home.join(d)).unwrap();
    }
    stripes(&dir.join("stripes.png"));
    plain(&dir.join("red.png"), [255, 0, 0]);
    plain(&dir.join("blue.png"), [0, 0, 255]);
    std::fs::write(dir.join("minify.plm"), SCENE).unwrap();
    let out = dir.join("frames");
    std::fs::create_dir_all(&out).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_pleamar"));
    c.env_clear();
    for keep in ["PATH", "LD_LIBRARY_PATH", "VK_ICD_FILENAMES", "VK_DRIVER_FILES", "LIBGL_ALWAYS_SOFTWARE", "WGPU_BACKEND", "WGPU_ADAPTER_NAME"] {
        if let Some(v) = std::env::var_os(keep) {
            c.env(keep, v);
        }
    }
    c.env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("PLEAMAR_SOCKETS", home.join("run"))
        .env("PLEAMAR_NO_SKILL_REFRESH", "1");
    if !minify {
        c.env("PLEAMAR_MINIFY", "off");
    }
    c.arg("--scene").arg(dir.join("minify.plm")).arg("--film").arg(&out).args(["--seconds", "1"]);
    let done = c.output().expect("pleamar runs");
    assert!(done.status.success(), "the film failed: {}", String::from_utf8_lossy(&done.stderr));
    let mut names: Vec<PathBuf> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "png")).collect();
    names.sort();
    let last = image::open(names.last().expect("frames")).unwrap().to_rgba8();
    let _ = std::fs::remove_dir_all(&dir);
    last
}

fn crop(img: &image::RgbaImage, [x, y, w, h]: [u32; 4]) -> Vec<[u8; 4]> {
    let mut v = Vec::new();
    for yy in y..y + h {
        for xx in x..x + w {
            v.push(img.get_pixel(xx, yy).0);
        }
    }
    v
}

/// How far the greys of a box stray from their mean.
fn spread(px: &[[u8; 4]]) -> f64 {
    let g: Vec<f64> = px.iter().map(|p| p[1] as f64).collect();
    let mean = g.iter().sum::<f64>() / g.len() as f64;
    (g.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / g.len() as f64).sqrt()
}

#[test]
#[ignore = "films with a graphics card; run with --ignored"]
fn smaller_is_averaged_and_its_own_size_unchanged() {
    if !has_card() {
        eprintln!("no graphics card to paint with: nothing to see");
        return;
    }
    let on = film(true);
    let off = film(false);
    // At its own size: the one sample of always, bit for bit.
    assert_eq!(crop(&on, [0, 0, 128, 128]), crop(&off, [0, 0, 128, 128]), "an image at its own size changed");
    // 128 → 41: the stripes' average, an even grey, instead of a moiré.
    let small = [142, 2, 37, 37];
    let (s_on, s_off) = (spread(&crop(&on, small)), spread(&crop(&off, small)));
    eprintln!("stripes made smaller: spread {s_on:.1} with it, {s_off:.1} without");
    // (One-pixel stripes are the worst there is: 4 × 4 bilinear samples leave a faint beat
    // a full box would not; measured 18 against 70 read once.)
    assert!(s_on < 25.0 && s_on < s_off * 0.35, "made smaller, the stripes are not averaged: {s_on:.1} (without: {s_off:.1})");
    // Very small (128 → 12), side by side in the atlas: red takes no blue, blue no red.
    for p in crop(&on, [201, 1, 10, 10]) {
        assert!(p[2] < 8, "the red one took blue from a neighbour: {p:?}");
    }
    for p in crop(&on, [213, 1, 10, 10]) {
        assert!(p[0] < 8, "the blue one took red from a neighbour: {p:?}");
    }
}
