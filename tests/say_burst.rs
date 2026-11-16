//! pleamar#159: a burst of `--say x "text x.v <value>" "emit x"`. At each `x` the logic has to read the
//! text of ITS call, not that of a later one: the `text` goes to the logic straight from the socket, and
//! the `emit` used to go through the render and arrive later, after the following texts. Filmed
//! (`--film`, a virtual clock) with the orders said from outside, as the reporter does. Without a card
//! to paint with it says so and passes, like `list_view_presses`: `cargo test --test say_burst -- --ignored`
//! (on a machine without one, with software Vulkan).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn has_card() -> bool {
    let instance = wgpu::Instance::default();
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).is_ok()
}

/// The program, with a home and a sockets directory of its own: nothing of the person's is read or written.
fn pleamar(home: &Path) -> Command {
    // `PLEAMAR_UNDER_TEST`: another build of the program to try the same test on (the one before a fix).
    let mut c = Command::new(std::env::var_os("PLEAMAR_UNDER_TEST").unwrap_or_else(|| env!("CARGO_BIN_EXE_pleamar").into()));
    c.env_clear();
    for keep in ["PATH", "LD_LIBRARY_PATH", "VK_ICD_FILENAMES", "VK_DRIVER_FILES", "LIBGL_ALWAYS_SOFTWARE", "WGPU_BACKEND", "WGPU_ADAPTER_NAME"] {
        if let Some(v) = std::env::var_os(keep) {
            c.env(keep, v);
        }
    }
    c.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("PLEAMAR_SOCKETS", home.join("run"))
        .env("PLEAMAR_NO_SKILL_REFRESH", "1");
    c
}

struct Running {
    home: PathBuf,
    dir: PathBuf,
    child: Child,
}

impl Running {
    fn start(name: &str) -> Running {
        let dir: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        for d in [".config", ".cache", ".local/share", ".local/state", "run"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        let out = dir.join("frames");
        std::fs::create_dir_all(&out).unwrap();
        let scene = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/say-burst.plm");
        // A virtual clock: four hundred seconds pass in a few real ones, and the test ends it before.
        let child = pleamar(&home).arg("--scene").arg(&scene).arg("--film").arg(&out).args(["--no-hud", "--seconds", "400"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("pleamar runs");
        let me = Running { home, dir, child };
        let until = Instant::now() + Duration::from_secs(60);
        while !me.say(&["get out"]).0 {
            assert!(Instant::now() < until, "the scene never listened");
            std::thread::sleep(Duration::from_millis(200));
        }
        me
    }
    /// `--say x <lines>`, as a program would: all down one connection. Whether it worked, and what it answered.
    fn say(&self, lines: &[&str]) -> (bool, String) {
        let done = pleamar(&self.home).args(["--say", "say-burst"]).args(lines).output().expect("--say runs");
        (done.status.success(), String::from_utf8_lossy(&done.stdout).trim().to_owned())
    }
    fn get(&self, name: &str) -> String {
        self.say(&[&format!("get {name}")]).1
    }
    /// Waits until `name` says something that holds `done`, or gives it up after a while.
    fn read(&self, name: &str, done: impl Fn(&str) -> bool) -> String {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let v = self.get(name);
            if done(&v) || Instant::now() > until {
                return v;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.say(&["quit"]);
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn values(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("v{i:02}")).collect()
}

#[test]
#[ignore = "films with a graphics card (software Vulkan on a machine without one); run with --ignored"]
fn a_burst_of_text_and_emit_reads_each_its_own_text() {
    if !has_card() {
        eprintln!("no graphics card: not filmed");
        return;
    }
    let run = Running::start("say-burst");
    let vs = values(20);
    let lines: Vec<String> = vs.iter().flat_map(|v| [format!("text x.v {v}"), "emit x".to_owned()]).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert!(run.say(&refs).0, "the burst was said");
    let want = vs.join("|");
    let out = run.read("out", |o| o.split('|').count() >= 20);
    assert_eq!(run.get("heard"), want, "`on(\"text:x.v\")` heard each one, in order");
    assert_eq!(out, want, "each `x` read the text of its own call");
}
