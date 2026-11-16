//! pleamar#157: the rows of a list with `view:` that have scrolled out of it are not drawn,
//! and they no longer take the presses meant for what is above or below the list. The
//! reporter's scene and mouse script, filmed (`--film`, on a virtual clock) with what was
//! pressed read frame by frame (`--record`). Without a card to paint with, it says so and
//! passes, like `list_inertia`: `cargo test --test list_view_presses -- --ignored`
//! (on a machine without one, with software Vulkan).

use std::path::{Path, PathBuf};
use std::process::Command;

fn has_card() -> bool {
    let instance = wgpu::Instance::default();
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).is_ok()
}

/// The last frame's `hit_btn`, `hit_card` and `hit_x`.
fn film(script: &str) -> (f32, f32, f32) {
    let dir: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("list-view-presses-{}", std::process::id()));
    // A home of its own: nothing of the person's (skills, sockets, settings) is read or written.
    let home = dir.join("home");
    for d in [".config", ".cache", ".local/share", ".local/state", "run"] {
        std::fs::create_dir_all(home.join(d)).unwrap();
    }
    let out = dir.join("frames");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let scene = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/list-view-presses.plm");
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
    c.arg("--scene").arg(&scene).arg("--film").arg(&out).args(["--no-hud", "--seconds", "4", "--record", "hit_btn,hit_card,hit_x", "--mouse", script]);
    let done = c.output().expect("pleamar runs");
    assert!(done.status.success(), "the film failed: {}", String::from_utf8_lossy(&done.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&done.stdout)
        .lines()
        .filter_map(|l| {
            let mut v = l.split('\t').skip(1).map(|x| x.parse::<f32>());
            Some((v.next()?.ok()?, v.next()?.ok()?, v.next()?.ok()?))
        })
        .last()
        .expect("frames recorded")
}

#[test]
#[ignore = "films with a graphics card (a minute in debug on a software one); run with --ignored"]
fn a_row_scrolled_out_of_its_list_does_not_take_a_press() {
    if !has_card() {
        eprintln!("no graphics card: not filmed");
        return;
    }
    // The list scrolled six notches down, then two presses on the button above it: one
    // where a row's corner button was before the scroll, one where the row was.
    let pressed = film("100,150@800 wheel-@1000 wheel-@1100 wheel-@1200 wheel-@1300 wheel-@1400 wheel-@1500 175,15@2200 click@2500 100,20@2800 click@3000");
    assert_eq!(pressed, (2.0, 0.0, 0.0), "(hit_btn, hit_card, hit_x): both presses are the button's");
    // And a row in sight still takes its own: a press inside the list, after the scroll.
    let pressed = film("100,150@800 wheel-@1000 wheel-@1100 wheel-@1200 100,200@2200 click@2500");
    assert_eq!(pressed.1, 1.0, "a row in sight is pressed");
}
