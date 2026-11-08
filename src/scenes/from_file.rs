//! A scene that comes from a file in the language, and that reloads by itself
//! when the file changes. The logic that comes with it is the minimum: it listens.

use crate::scene::*;
use crate::logic::{Context, Script};
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Reads and compiles a scene. The error already comes with its file, its line and its arrow.
/// A scene compiled a moment ago to look at it (`script_for`) is not compiled again.
pub fn read(path: &str) -> Result<Scene, String> {
    let ready = READY.lock().unwrap().take();
    match ready {
        Some((p, e)) if p == path => Ok(e),
        _ => compile(path),
    }
}

/// A big scene costs hundreds of MB to compile (pleamar-wm's session): at
/// start it was compiled three times —to see whether it had logic, to show
/// it, and to know which files to watch—, two of them at once.
static READY: std::sync::Mutex<Option<(String, Scene)>> = std::sync::Mutex::new(None);
/// The files the last compilation of each scene read: what to watch.
static FILES: std::sync::Mutex<Option<(String, Vec<std::path::PathBuf>)>> = std::sync::Mutex::new(None);
/// The logic files of the plugins of the last compilation of each scene: what the
/// logic's watcher watches besides the scene's own `.luau`. Kept here so that it does
/// not compile the whole scene again to find them out (a big shell's start compiled it
/// twice, the second time while the first frame was being made).
static PLUGIN_LOGIC: std::sync::Mutex<Option<(String, Vec<String>)>> = std::sync::Mutex::new(None);

fn note_plugins(path: &str, e: &Scene) {
    *PLUGIN_LOGIC.lock().unwrap() = Some((path.to_owned(), e.plugins.iter().map(|p| p.logic.to_string_lossy().into_owned()).collect()));
}

fn compile(path: &str) -> Result<Scene, String> {
    let (e, mut files) = crate::language::read_file(path)?;
    files.extend(e.attachments.iter().cloned());
    *FILES.lock().unwrap() = Some((path.to_owned(), files));
    note_plugins(path, &e);
    Ok(e)
}

pub struct FromFile {
    path: String,
    /// What it takes to become a Luau script, and the one it became: a scene
    /// that started with no logic, plugins or services gets them when a reload
    /// brings them, instead of only when it starts again.
    #[allow(dead_code)]
    grow: Option<(Sender<ToRender>, Sender<Event>, std::sync::Arc<std::sync::atomic::AtomicBool>)>,
    grown: Option<Box<dyn Script>>,
}

impl FromFile {
    pub fn new(path: &str) -> Self {
        FromFile { path: path.to_owned(), grow: None, grown: None }
    }

    /// When a reload brings it what only a logic does: started there and then,
    /// with the scene as it is now.
    #[cfg(feature = "luau")]
    fn grow_if_needed(&mut self, e: &Event, c: &mut Context) -> bool {
        let logic = logic_path_for(&self.path);
        let wants = match e {
            Event::NewScene(_, _, _, _, _, plugins, _, services, _) => !plugins.is_empty() || !services.is_empty() || std::path::Path::new(&logic).is_file(),
            Event::ReloadLogic => std::path::Path::new(&logic).is_file(),
            _ => false,
        };
        if !wants {
            return false;
        }
        let Some((tx, to_logic, blocked)) = self.grow.take() else { return false };
        println!("logic  · the scene brings logic now: it starts");
        let mut script = crate::logic_luau::LuauScript::new(&self.path, &logic, tx, to_logic, blocked);
        let _ = script.scene();
        script.on_event(Event::Alarm("start"), c);
        self.grown = Some(Box::new(script));
        true
    }
}

impl Script for FromFile {
    fn scene(&mut self) -> Scene {
        read(&self.path).unwrap_or_else(|m| {
            eprintln!("{m}");
            std::process::exit(1)
        })
    }

    fn on_event(&mut self, e: Event, c: &mut Context) {
        if let Some(grown) = &mut self.grown {
            return grown.on_event(e, c);
        }
        #[cfg(feature = "luau")]
        if self.grow_if_needed(&e, c) {
            return;
        }
        match e {
            // Without a mouse, `--demo` fires the "demo" signal: the scene will say what it does with it.
            Event::Demo => c.signal("demo"),
            Event::Layer(_, _) => c.work(),
            Event::Text(n, v) => println!("logic  · '{n}' now says: {v:?}"),
            Event::Submit(n, v) => println!("logic  · enter in '{n}': {v:?}"),
            Event::Received(z, kind, d) => println!("logic  · something was dropped on '{z}', a {kind}: {d:?}"),
            Event::Signal(s, None) => println!("logic  · '{s}' happened"),
            Event::Signal(s, Some(v)) => println!("logic  · '{s}' happened, with {v:.2}"),
            _ => {}
        }
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.grown.as_ref().and_then(|g| g.next_deadline())
    }

    fn tick(&mut self, c: &mut Context) {
        if let Some(grown) = &mut self.grown {
            grown.tick(c);
        }
    }
}

/// The script for a file scene: if next to it there is a `.luau` with the same
/// name, that is its logic; if not, one that only listens.
pub fn script_for(path: &str, tx: Sender<ToRender>, to_logic: Sender<Event>, blocked: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Box<dyn Script> {
    let logic = logic_path_for(path);
    // There is logic if the scene has its `.luau`, if any of its plugins has its own,
    // or if the scene asks for some service: the script also sets that up and hands it out,
    // even if there is not a single line of Luau.
    #[cfg(feature = "luau")]
    {
        if std::path::Path::new(&logic).is_file() {
            return Box::new(crate::logic_luau::LuauScript::new(path, &logic, tx, to_logic, blocked));
        }
        // Compiled to look at it, and kept for the script to show it.
        if let Ok(e) = compile(path) {
            let wants_logic = !e.plugins.is_empty() || !e.services.is_empty();
            *READY.lock().unwrap() = Some((path.to_owned(), e));
            if wants_logic {
                return Box::new(crate::logic_luau::LuauScript::new(path, &logic, tx, to_logic, blocked));
            }
        }
    }
    let _ = logic;
    let mut plain = FromFile::new(path);
    plain.grow = Some((tx, to_logic, blocked));
    Box::new(plain)
}

fn logic_path_for(path: &str) -> String {
    std::path::Path::new(path).with_extension("luau").to_string_lossy().into_owned()
}

/// Hot reload: it checks the file's date four times per second —which
/// works the same on any system— and, if it changed, reads it again. If it is
/// fine, the new scene replaces the old one without losing what was moving; if not,
/// it says why and the old one stays.
/// The program itself: if the binary changes —you just recompiled—, it relaunches
/// with the same arguments.
///
/// A live process keeps the compiler it was born with, so a
/// scene that uses something new in the language will not compile for it however many times it is
/// saved: it keeps the last good one and says so, which is correct, but
/// it looks as if hot reload does not work. This closes that hole.
///
/// `PLEAMAR_NO_RELAUNCH=1` turns it off, for whoever does not want updating the
/// package to restart their bar.
fn watch_binary() {
    if std::env::var_os("PLEAMAR_NO_RELAUNCH").is_some() || super::STAY_ON_UPDATE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Ok(me) = std::env::current_exe() else { return };
    let date = |r: &std::path::Path| std::fs::metadata(r).and_then(|m| m.modified()).ok();
    let Some(mut last) = date(&me) else { return };
    let _ = std::thread::Builder::new().name("relaunch".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_millis(400));
        let Some(now) = date(&me) else { continue };
        if now == last {
            continue;
        }
        last = now;
        // Writing a binary is not instantaneous: it waits for it to stay still,
        // or it would relaunch with half a file.
        loop {
            std::thread::sleep(Duration::from_millis(150));
            match date(&me) {
                Some(d) if d == last => break,
                Some(d) => last = d,
                None => continue,
            }
        }
        println!("pleamar · the program changed on disk: starting again");
        let args: Vec<String> = std::env::args().skip(1).collect();
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // With the name it was started with: started as `pleamar` from the PATH, it stays `pleamar`, not
            // `/home/…/.local/bin/pleamar`, so whatever finds it by its command line (`pgrep -f '^pleamar --scene …'`,
            // a keeper deciding whether to start another) still finds it after an update.
            let mut command = std::process::Command::new(&me);
            if let Some(name) = std::env::args_os().next() {
                command.arg0(name);
            }
            let error = command.args(&args).exec();
            eprintln!("pleamar · could not start again: {error}");
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new(&me).args(&args).spawn();
            std::process::exit(0);
        }
    });
}

pub fn watch(path: String, to_render: Sender<ToRender>, to_logic: Sender<Event>) {
    watch_binary();
    // The logic reloads too: same trick, another file.
    let logic = logic_path_for(&path);
    let to_the_logic = to_logic.clone();
    let scene_for_logic = path.clone();
    std::thread::Builder::new()
        .name("reload-logic".into())
        .spawn(move || {
            // The scene's and its plugins': touching any of them reloads them.
            let all = |scene: &str| -> Vec<String> {
                let mut v = vec![logic.clone()];
                let known = PLUGIN_LOGIC.lock().unwrap().as_ref().filter(|(p, _)| p == scene).map(|(_, l)| l.clone());
                match known {
                    Some(l) => v.extend(l),
                    None => {
                        if let Ok(e) = read(scene) {
                            v.extend(e.plugins.iter().map(|p| p.logic.to_string_lossy().into_owned()));
                        }
                    }
                }
                v
            };
            let date = |v: &[String]| v.iter().map(|r| std::fs::metadata(r).and_then(|m| m.modified()).ok()).collect::<Vec<_>>();
            // And the modules it loads with `require`, as it loads them.
            let with_modules = |v: &[String]| -> Vec<String> {
                #[allow(unused_mut)]
                let mut v = v.to_vec();
                #[cfg(feature = "luau")]
                v.extend(crate::logic_luau::REQUIRED.lock().unwrap().iter().map(|p| p.to_string_lossy().into_owned()));
                v
            };
            let mut watched = all(&scene_for_logic);
            let mut last = date(&with_modules(&watched));
            loop {
                std::thread::sleep(Duration::from_millis(250));
                let now = date(&with_modules(&watched));
                // A module required for the first time: watched from here on.
                if now.len() != last.len() {
                    last = now;
                    continue;
                }
                if now != last && now.iter().any(Option::is_some) {
                    std::thread::sleep(Duration::from_millis(80));
                    watched = all(&scene_for_logic);
                    last = date(&with_modules(&watched));
                    if to_logic.send(Event::ReloadLogic).is_err() {
                        return;
                    }
                }
            }
        })
        .unwrap();
    std::thread::Builder::new()
        .name("reload".into())
        .spawn(move || {
            // The scene and whatever it imports: touching a library also reloads it.
            // The scene, its libraries and whatever they bring attached —the SVGs of their
            // figures—: you draw the hat, save, and it is on.
            // Those the scene's compilation read, if it has already been: not compiled again for this.
            let known = FILES.lock().unwrap().take().filter(|(p, _)| *p == path).map(|(_, f)| f);
            let mut watched: Vec<std::path::PathBuf> = known.unwrap_or_else(|| {
                crate::language::read_file(&path).map_or_else(
                    |_| vec![path.clone().into()],
                    |(e, mut f)| {
                        f.extend(e.attachments.iter().cloned());
                        f
                    },
                )
            });
            let date = |v: &[std::path::PathBuf]| v.iter().map(|r| std::fs::metadata(r).and_then(|m| m.modified()).ok()).collect::<Vec<_>>();
            let mut last = date(&watched);
            loop {
                std::thread::sleep(Duration::from_millis(250));
                let mut now = date(&watched);
                if now == last || now.iter().all(Option::is_none) {
                    continue;
                }
                // An editor saves in several steps —truncates, writes, sometimes renames—:
                // it waits for the file to have been still for a moment and not be empty.
                loop {
                    std::thread::sleep(Duration::from_millis(60));
                    let after = date(&watched);
                    // Only what is on disk: a library of pleamar's own
                    // (`pleamar:ui`) has no file, and taking it for one half
                    // saved left this waiting forever —no reload ever again—.
                    let empty = watched.iter().any(|r| std::fs::metadata(r).is_ok_and(|m| m.len() == 0));
                    if after == now && !empty {
                        break;
                    }
                    now = after;
                }
                let t0 = std::time::Instant::now();
                match crate::language::read_file(&path) {
                    Ok((e, files)) => {
                        note_plugins(&path, &e);
                        println!("reload · {path} read in {:.1} ms{}", t0.elapsed().as_secs_f32() * 1000.0, if files.len() > 1 { format!(" · with {} more files", files.len() - 1) } else { String::new() });
                        // It may import other things now.
                        watched = files;
                        watched.extend(e.attachments.iter().cloned());
                        let _ = to_render.send(ToRender::ReloadError(None));
                        let _ = to_the_logic.send(Event::NewScene(e.facts.clone(), e.texts.clone(), e.permissions.clone(), e.models.clone(), e.types.clone(), e.plugins.clone(), e.signals.iter().map(|s| s.0).collect(), e.services.clone(), e.translations.clone()));
                        if to_render.send(ToRender::Scene(e)).is_err() {
                            return;
                        }
                    }
                    Err(m) => {
                        eprintln!("reload · the scene stays as it was:\n{m}");
                        let _ = to_render.send(ToRender::ReloadError(Some(m)));
                    }
                }
                last = date(&watched);
            }
        })
        .unwrap();
}

#[cfg(test)]
mod tests {
    use crate::scene::{Rule, Trigger};

    /// A zone made in a `repeat` of a scene with one copy per monitor: each copy
    /// has its own, and each copy's rule presses its own. Both used to be one
    /// name, the second copy's, and the first monitor's zone was deaf.
    #[test]
    fn repeated_zones_belong_to_their_screen_copy() {
        let dir = std::env::temp_dir().join(format!("pleamar-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("copies.plm");
        std::fs::write(&path, "scene Copies {
    surface { size: 200, 40; anchor: top; screens: each max 2 }
    fact hit = 0
    repeat k in 1..3 {
        prop glow.$k = 0 ~140ms
        zone box glow.$k { at: 40 * k, 20; size: 30, 30 }
        on press glow.$k { hit = k; glow.$k: 1 }
    }
}
").unwrap();
        let scene = super::read(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let ids: Vec<&str> = scene.zones.iter().map(|z| z.id).collect();
        for name in ["glow.1#screen0", "glow.1#screen1", "glow.2#screen0", "glow.2#screen1"] {
            assert!(ids.contains(&name), "no zone {name} in {ids:?}");
        }
        let pressed: Vec<&str> = scene.rules.iter().filter_map(|r: &Rule| match r.when {
            Trigger::Press(z) => Some(scene.zones[z.0 as usize].id),
            _ => None,
        }).collect();
        for name in ["glow.1#screen0", "glow.1#screen1"] {
            assert!(pressed.contains(&name), "no rule presses {name}: {pressed:?}");
        }
    }

    /// A `for` inside a `grid`: a cell past the end of its list is neither
    /// seen nor pressed, as in a row or a column. It used to keep showing the
    /// records of the last longer list.
    #[test]
    fn a_grid_only_has_the_cells_its_list_reaches() {
        let dir = std::env::temp_dir().join(format!("pleamar-grid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grid.plm");
        std::fs::write(&path, "scene Cells {
    surface { size: 200, 100 }
    model rows max 4 { title: text }
    grid {
        at: 0, 0; columns: 2; width: 200; gap: 10; row: 40
        for r in rows {
            group {
                size: 95, 40
                zone box hit { from: 0, 0; size: 95, 40 }
                text r.title { at: 4, 20; size: 12; color: #fff }
            }
        }
    }
}
").unwrap();
        let scene = super::read(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let mut facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        let count = scene.facts.iter().position(|f| f.0 == "rows.count").expect("rows.count");
        let active = |facts: &[f32]| scene.zones.iter().filter(|z| z.active.is_true(crate::scene::Ctx { props: &[], facts, seen: None })).count();
        assert_eq!(scene.zones.len(), 4);
        facts[count] = 4.0;
        assert_eq!(active(&facts), 4);
        facts[count] = 1.0;
        assert_eq!(active(&facts), 1, "a cell the list does not reach can still be pressed");
        facts[count] = 0.0;
        assert_eq!(active(&facts), 0);
    }

    /// A rule that names nothing of its copy is the same rule once per monitor:
    /// only the first one may act, or a count goes up twice per monitor.
    #[test]
    fn a_shared_rule_acts_once_whatever_the_copies() {
        let dir = std::env::temp_dir().join(format!("pleamar-twins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("twins.plm");
        std::fs::write(&path, "scene Twins {
    surface { size: 200, 40; anchor: top; screens: each max 2 }
    event landed
    fact n = 0
    on landed { n = n + 1 }
    zone box hit { from: 0, 0; size: 20, 20 }
    on press hit { n = 0 }
}
").unwrap();
        let scene = super::read(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let twins: Vec<usize> = (0..scene.rules.len()).filter(|&k| scene.twin_of[k] != k).collect();
        // The count is one rule written twice; the press is each copy's own zone.
        assert_eq!(twins.len(), 1, "twins: {:?}", scene.twin_of);
        assert!(matches!(scene.rules[twins[0]].when, crate::scene::Trigger::On(_)));
    }

    /// A big `let` is computed in a property: read in every monitor's copy,
    /// it is one property, or the rules naming it differ per copy and each
    /// copy's acts —a `toggle` undoes itself—.
    #[test]
    fn a_big_let_is_one_property_whatever_the_copies() {
        let dir = std::env::temp_dir().join(format!("pleamar-twin-lets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lets.plm");
        std::fs::write(&path, "scene Lets {
    surface { size: 200, 40; anchor: top; screens: each max 2 }
    event go
    fact a = 0
    fact b = 0
    fact t = 0
    repeat i in 0..1 {
        let big = max(a, b) + (a - b) * 3 + min(a * 2, b) + abs(a)
        on go { t = if(big > 1, 1 - t, t) }
    }
}
").unwrap();
        let scene = super::read(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let bound = scene.behaviors.iter().filter(|b| matches!(b, crate::scene::Behavior::Bind { .. })).count();
        assert_eq!(bound, 1, "one property for the let, not one per copy");
        let twins: Vec<usize> = (0..scene.rules.len()).filter(|&k| scene.twin_of[k] != k).collect();
        assert_eq!(twins.len(), 1, "twins: {:?}", scene.twin_of);
    }

    /// What each fact a rule sets would be set to, with the facts as they
    /// start (or as given) and every big `let` computed once, in order.
    fn set_by_rules(source: &str, given: &[(&str, f32)]) -> std::collections::HashMap<String, Vec<f32>> {
        let dir = std::env::temp_dir().join(format!("pleamar-set-{}-{}", std::process::id(), source.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("set.plm");
        std::fs::write(&path, source).unwrap();
        let scene = super::read(path.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let scene = scene.unwrap_or_else(|e| panic!("{e}"));
        let facts: Vec<f32> = scene.facts.iter().map(|(n, v)| given.iter().find(|(g, _)| g == n).map_or(*v, |g| g.1)).collect();
        let mut props: Vec<crate::scene::Animated> = scene.props.iter().map(|(_, x, s)| crate::scene::Animated::at(*x, *s)).collect();
        for b in &scene.behaviors {
            if let crate::scene::Behavior::Bind { prop, to } = b {
                props[prop.0 as usize].x = to.eval(crate::scene::Ctx { props: &props, facts: &facts, seen: None });
            } else if let crate::scene::Behavior::Sort { keys, ord, pos } = b {
                crate::scene::run_sort(keys, ord, pos, &mut props, &facts, |_| {});
            }
        }
        let mut out = std::collections::HashMap::new();
        for r in &scene.rules {
            for e in &r.effects {
                if let crate::scene::Effect::Fact(h, e) = e {
                    let v = e.eval(crate::scene::Ctx { props: &props, facts: &facts, seen: None });
                    out.entry(scene.facts[h.0 as usize].0.to_owned()).or_insert_with(Vec::new).push(v);
                } else if let crate::scene::Effect::FactAt(hs, i, e) = e {
                    let c = crate::scene::Ctx { props: &props, facts: &facts, seen: None };
                    if let Some(h) = Some(i.eval(c).round()).filter(|k| *k >= 0.0).and_then(|k| hs.get(k as usize)) {
                        out.entry(scene.facts[h.0 as usize].0.to_owned()).or_insert_with(Vec::new).push(e.eval(c));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn an_array_is_read_by_a_place_known_or_not() {
        let set = set_by_rules("scene Arrays {
    surface { size: 200, 40 }
    fact k = 2
    fact far = 9
    let sq[4]
    repeat i in 0..4 {
        let twice = i * 2
        sq[i] = twice * twice + k * 0
    }
    let last = sq[3]
    on demo { a = sq[k]; b = last; c = sq[far]; d = sq[k - 3]; e = sq[1] }
    fact a = 0
    fact b = 0
    fact c = 0
    fact d = 0
    fact e = 0
}
", &[]);
        assert_eq!(set["a"][0], 16.0);
        assert_eq!(set["b"][0], 36.0);
        assert_eq!(set["c"][0], -1.0, "past the last element");
        assert_eq!(set["d"][0], -1.0, "before the first");
        assert_eq!(set["e"][0], 4.0);
    }

    #[test]
    fn an_index_reads_names_with_numbers_in_them() {
        let set = set_by_rules("scene Windows {
    surface { size: 200, 40 }
    windows win max 4
    fact s = 1
    let n[win.max]
    repeat i in 0..win.max {
        n[i] = win[i].screen * 10 + i
    }
    on demo { a = n[s + 1]; b = win.reserved[s].top; c = win[s].screen }
    fact a = 0
    fact b = 0
    fact c = 0
}
", &[("win.2.screen", 3.0), ("win.reserved.1.top", 44.0), ("win.1.screen", 1.0)]);
        assert_eq!(set["a"][0], 32.0);
        assert_eq!(set["b"][0], 44.0);
        assert_eq!(set["c"][0], 1.0);
    }

    #[test]
    fn an_element_that_reads_its_monitor_is_each_copys_own() {
        let set = set_by_rules("scene Copies {
    surface { size: full, 40; anchor: top; screens: each max 2 }
    let half[2]
    repeat i in 0..2 {
        half[i] = screen.width / 2 + i
    }
    let both = half[0] + 1000
    on demo { a = half[0]; b = both }
    fact a = 0
    fact b = 0
}
", &[("screen.0.width", 100.0), ("screen.1.width", 300.0)]);
        let mut a = set["a"].clone();
        a.sort_by(f32::total_cmp);
        assert_eq!(a, vec![50.0, 150.0]);
        let mut b = set["b"].clone();
        b.sort_by(f32::total_cmp);
        assert_eq!(b, vec![1050.0, 1150.0]);
    }

    #[test]
    fn a_named_surface_beside_the_copies_reads_the_copy_on_its_monitor() {
        // A panel of its own (`screens: on`) sent to the second monitor said copy 0's
        // `screen.index` and `screen.width`, so a `let` it shared with the copies
        // —"the panel is on this copy"— was false there.
        let set = set_by_rules("scene Beside {
    surface { size: full, 40; anchor: top; screens: each max 2 }
    text on = \"B\"
    fact panel_on = 1
    let mine = if(panel_on == screen.index, 1, 0)
    surface sheet {
        size: 200, 200; anchor: top; screens: on
        on demo { a = mine; b = screen.width; c = screen.index }
    }
    fact a = 0
    fact b = 0
    fact c = 0
}
", &[("screen.0.width", 100.0), ("screen.1.width", 300.0), ("sheet.screen.index", 1.0), ("sheet.screen.width", 300.0)]);
        assert_eq!(set["a"], vec![1.0]);
        assert_eq!(set["b"], vec![300.0]);
        assert_eq!(set["c"], vec![1.0]);
    }

    #[test]
    fn each_copy_of_a_surface_measures_its_own_monitor() {
        // A phone's monitor of 442 came last and said `bar.width` for every copy: the bar on
        // the 1920 one was laid out for 442, and stayed so when the phone went.
        let dir = std::env::temp_dir().join(format!("pleamar-measures-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.plm");
        std::fs::write(&path, "scene Measures {
    surface bar {
        size: full, 40
        anchor: top
        screens: each max 2
        on demo { a = bar.width / 2 }
    }
    fact a = 0
}
").unwrap();
        let scene = super::read(path.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let scene = scene.unwrap_or_else(|e| panic!("{e}"));
        let sizes: Vec<_> = scene.surfaces.iter().filter(|s| s.name == "bar").map(|s| s.size_props.unwrap().0).collect();
        assert_eq!(sizes.len(), 2);
        assert_ne!(sizes[0], sizes[1], "each copy, its own width");
        let facts: Vec<f32> = scene.facts.iter().map(|(_, v)| *v).collect();
        let mut props: Vec<crate::scene::Animated> = scene.props.iter().map(|(_, x, s)| crate::scene::Animated::at(*x, *s)).collect();
        props[sizes[0].0 as usize].x = 1920.0;
        props[sizes[1].0 as usize].x = 442.0;
        let mut a = Vec::new();
        for r in &scene.rules {
            for e in &r.effects {
                if let crate::scene::Effect::Fact(_, e) = e {
                    a.push(e.eval(crate::scene::Ctx { props: &props, facts: &facts, seen: None }));
                }
            }
        }
        a.sort_by(f32::total_cmp);
        assert_eq!(a, vec![221.0, 960.0]);
    }

    #[test]
    fn a_fold_adds_up_across_a_repeat() {
        let set = set_by_rules("scene Folds {
    surface { size: 200, 40 }
    fact w.0 = 30
    fact w.1 = 10
    fact w.2 = 50
    fact w.3 = 20
    let total = 0
    let best = 99999
    let at = -1
    let left[4]
    repeat i in 0..4 {
        left[i] = total
        total += w.$i
        at = if(w.$i < best, i, at)
        best = min(best, w.$i)
    }
    on demo { a = total; b = at; c = best; d = left[2]; e = left[3] }
    fact a = 0
    fact b = 0
    fact c = 0
    fact d = 0
    fact e = 0
}
", &[]);
        assert_eq!(set["a"][0], 110.0);
        assert_eq!(set["b"][0], 1.0, "the smallest is the second");
        assert_eq!(set["c"][0], 10.0);
        assert_eq!(set["d"][0], 40.0, "what the ones before it add up to");
        assert_eq!(set["e"][0], 90.0);
    }

    #[test]
    fn a_turn_reads_a_fold_as_it_was_then() {
        let set = set_by_rules("scene Turns {
    surface { size: 200, 40 }
    let x = 0
    repeat i in 0..3 {
        x += 10
        on demo { r.$i = x }
        fact r.$i = 0
    }
    on demo { after = x }
    fact after = 0
}
", &[]);
        assert_eq!(set["r.0"][0], 10.0);
        assert_eq!(set["r.2"][0], 30.0);
        assert_eq!(set["after"][0], 30.0);
    }

    #[test]
    fn a_fold_that_reads_its_monitor_is_each_copys_own() {
        let set = set_by_rules("scene Copies {
    surface { size: full, 40; anchor: top; screens: each max 2 }
    let room = 0
    repeat i in 0..3 {
        room += screen.width / 10
    }
    on demo { a = room }
    fact a = 0
}
", &[("screen.0.width", 100.0), ("screen.1.width", 300.0)]);
        let mut a = set["a"].clone();
        a.sort_by(f32::total_cmp);
        assert_eq!(a, vec![30.0, 90.0]);
    }

    #[test]
    fn a_sort_orders_the_slots_by_their_keys() {
        let set = set_by_rules("scene Sorted {
    surface { size: 200, 40 }
    windows win max 4
    fact row.0 = 1
    fact row.1 = 0
    fact row.2 = 1
    fact row.3 = 0
    fact col.0 = 5
    fact col.1 = 2
    fact col.2 = 5
    fact col.3 = 1
    let ord[win.max] = sort(i in win by row.$i, col.$i)
    // Where each one starts in its row: the widths of the ones before it there.
    let x = 0
    let at[win.max]
    repeat p in 0..win.max {
        let s = ord[p]
        x = if(row[s] != row[ord[p - 1]], 0, x)
        at[p] = x
        x += 10 + s
    }
    on demo { o0 = ord[0]; o1 = ord[1]; o2 = ord[2]; o3 = ord[3]; p1 = ord.pos[1]; left2 = at[ord.pos[2]] }
    fact o0 = 0
    fact o1 = 0
    fact o2 = 0
    fact o3 = 0
    fact p1 = 0
    fact left2 = 0
}
", &[]);
        let order: Vec<f32> = ["o0", "o1", "o2", "o3"].iter().map(|k| set[*k][0]).collect();
        assert_eq!(order, vec![3.0, 1.0, 0.0, 2.0], "by row, then by column, ties by slot");
        assert_eq!(set["p1"][0], 1.0);
        assert_eq!(set["left2"][0], 10.0, "slot 2 comes after slot 0 in its row");
    }

    #[test]
    fn a_repeat_in_a_rule_is_its_effects_in_turn() {
        let set = set_by_rules("scene Effects {
    surface { size: 200, 40 }
    repeat k in 0..3 { fact v.$k = 0 }
    on demo {
        repeat k in 0..3 {
            v.$k = 10 * k + 1
        }
    }
}
", &[]);
        assert_eq!([set["v.0"][0], set["v.1"][0], set["v.2"][0]], [1.0, 11.0, 21.0]);
        assert_eq!(set.values().map(Vec::len).sum::<usize>(), 3, "one rule, three effects");
    }

    #[test]
    fn a_rule_writes_the_fact_at_a_place() {
        let set = set_by_rules("scene Writes {
    surface { size: 200, 40 }
    fact at = 2
    repeat k in 0..4 {
        fact x.$k = 0
        fact b.$k.y = 0
    }
    on demo { x[at] = 7; b[at - 1].y = at * 10; x[0] = 5 }
}
", &[]);
        assert_eq!(set["x.2"], vec![7.0]);
        assert_eq!(set["b.1.y"], vec![20.0]);
        assert_eq!(set["x.0"], vec![5.0], "a place known when loading is that fact");
    }

    #[test]
    fn a_text_is_read_by_its_place() {
        let dir = std::env::temp_dir().join(format!("pleamar-texts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("texts.plm");
        std::fs::write(&path, "scene Texts {
    surface { size: 200, 100 }
    windows win max 3
    fact at = 1
    text win[at].title { at: 0, 0; size: 12; color: #fff }
    text \"{win[at + 1].app} and {upper(win[0].title)} and {win[at].title}\" { at: 0, 20; size: 12; color: #fff }
    text win[2].app { at: 0, 40; size: 12; color: #fff }
}
").unwrap();
        let scene = super::read(path.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let scene = scene.unwrap_or_else(|e| panic!("{e}"));
        let mut texts: Vec<String> = scene.texts.iter().map(|t| t.1.clone()).collect();
        for (name, value) in [("win.0.title", "zero"), ("win.1.title", "one"), ("win.2.app", "foot")] {
            texts[scene.texts.iter().position(|t| t.0 == name).unwrap()] = value.to_owned();
        }
        let facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        let props: Vec<crate::scene::Animated> = Vec::new();
        let c = crate::scene::Ctx { props: &props, facts: &facts, seen: None };
        let shown: Vec<String> = scene.instrs.iter().filter_map(|i| match i {
            crate::scene::Instr::Text { content, .. } => Some(crate::gpu::content_text(content, c, &texts).into_owned()),
            _ => None,
        }).collect();
        assert_eq!(shown, ["one", "foot and ZERO and one", "foot"]);
    }

    #[test]
    fn a_fold_inside_a_turn_is_that_turns_own() {
        let set = set_by_rules("scene Local {
    surface { size: 200, 40 }
    fact w.0 = 10
    fact w.1 = 20
    fact w.2 = 30
    repeat i in 0..3 {
        fact r.$i = 0
        fact first.$i = 0
        // What the others weigh, without itself; and the first heavier one.
        let others = 0
        let heavier = -1
        repeat j in 0..3 {
            others += (j != i) * w.$j
            heavier = if(heavier < 0 and w.$j > w.$i, j, heavier)
        }
        on demo { r.$i = others; first.$i = heavier }
    }
}
", &[]);
        assert_eq!([set["r.0"][0], set["r.1"][0], set["r.2"][0]], [50.0, 40.0, 30.0]);
        assert_eq!([set["first.0"][0], set["first.1"][0], set["first.2"][0]], [1.0, 2.0, -1.0]);
    }

    #[test]
    fn a_blocks_fold_is_what_its_rules_read() {
        let set = set_by_rules("scene Free {
    surface { size: 200, 40 }
    fact used.0 = 1
    fact used.1 = 0
    fact used.2 = 0
    fact got = -1
    repeat q in 0..1 {
        let free = -1
        repeat j in 0..3 {
            free = if(free < 0 and used.$j < 0.5, j, free)
        }
        repeat j in 0..3 {
            on demo while free == j { got = j }
        }
    }
}
", &[]);
        assert_eq!(set["got"], vec![0.0, 1.0, 2.0], "each rule's value");
        let guards = set_by_rules_guards("scene Free {
    surface { size: 200, 40 }
    fact used.0 = 1
    fact used.1 = 0
    fact used.2 = 0
    fact got = -1
    repeat q in 0..1 {
        let free = -1
        repeat j in 0..3 {
            free = if(free < 0 and used.$j < 0.5, j, free)
        }
        repeat j in 0..3 {
            on demo while free == j { got = j }
        }
    }
}
");
        assert_eq!(guards, vec![false, true, false], "only the rule of the first free one fires");
    }

    /// Whether each rule's guard holds, with the facts as they start.
    fn set_by_rules_guards(source: &str) -> Vec<bool> {
        let dir = std::env::temp_dir().join(format!("pleamar-guards-{}-{}", std::process::id(), source.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("guards.plm");
        std::fs::write(&path, source).unwrap();
        let scene = super::read(path.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let scene = scene.unwrap_or_else(|e| panic!("{e}"));
        let facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        let mut props: Vec<crate::scene::Animated> = scene.props.iter().map(|(_, x, s)| crate::scene::Animated::at(*x, *s)).collect();
        for b in &scene.behaviors {
            if let crate::scene::Behavior::Bind { prop, to } = b {
                props[prop.0 as usize].x = to.eval(crate::scene::Ctx { props: &props, facts: &facts, seen: None });
            }
        }
        scene.rules.iter().map(|r| r.guard.as_ref().is_none_or(|g| g.eval(crate::scene::Ctx { props: &props, facts: &facts, seen: None }) > 0.5)).collect()
    }

    #[test]
    fn a_table_is_read_by_its_two_places() {
        let set = set_by_rules("scene Table {
    surface { size: 200, 40 }
    fact s = 1
    fact k = 2
    let at[2][3]
    repeat i in 0..2 {
        repeat j in 0..3 {
            at[i][j] = 10 * i + j
        }
    }
    on demo { a = at[s][k]; b = at[1][0]; c = at[s][k + 5]; d = at[s - 2][0] }
    fact a = 0
    fact b = 0
    fact c = 0
    fact d = 0
}
", &[]);
        assert_eq!(set["a"][0], 12.0);
        assert_eq!(set["b"][0], 10.0);
        assert_eq!(set["c"][0], -1.0, "past the last column");
        assert_eq!(set["d"][0], -1.0, "before the first row");
    }

    #[test]
    fn a_known_place_past_the_last_is_outside() {
        let set = set_by_rules("scene Past {
    surface { size: 200, 40 }
    windows win max 4
    on demo { a = win[4].screen; b = win.order[win.max]; c = win[3].screen }
    fact a = 0
    fact b = 0
    fact c = 0
}
", &[("win.3.screen", 2.0)]);
        assert_eq!(set["a"][0], -1.0);
        assert_eq!(set["b"][0], -1.0);
        assert_eq!(set["c"][0], 2.0);
    }

    #[test]
    fn a_turn_reads_a_fold_as_it_is_where_it_reads_it() {
        let set = set_by_rules("scene Where {
    surface { size: 200, 40 }
    fact q.0 = 3
    fact q.1 = 5
    let x = 0
    let left[2]
    repeat i in 0..2 {
        let mine = x
        x += q.$i
        left[i] = mine
        on demo { r.$i = mine; after.$i = x }
        fact r.$i = 0
        fact after.$i = 0
    }
    on demo { a = left[1] }
    fact a = 0
}
", &[]);
        assert_eq!(set["a"][0], 3.0);
        assert_eq!([set["r.0"][0], set["r.1"][0]], [0.0, 3.0], "its rules read what it read before the link");
        assert_eq!([set["after.0"][0], set["after.1"][0]], [3.0, 8.0]);
    }

    #[test]
    fn a_fold_from_a_copys_own_start_ends_in_its_result() {
        let set = set_by_rules("scene Start {
    surface { size: full, 40; anchor: top; screens: each max 2 }
    fact open.0 = 1
    fact open.1 = 0
    let x = screen.width
    repeat i in 0..2 {
        x = if(open.$i, 10 + i, -1)
    }
    on demo { a = x }
    fact a = 0
}
", &[("screen.0.width", 100.0), ("screen.1.width", 300.0)]);
        assert_eq!(set["a"], vec![-1.0, -1.0], "each copy's: the last link, not its monitor's width");
    }

    #[test]
    fn a_sort_ties_minus_zero_with_zero() {
        let set = set_by_rules("scene Zero {
    surface { size: 200, 40 }
    fact sign.0 = 1
    fact sign.1 = -1
    fact sign.2 = 1
    fact sign.3 = -1
    let ord[4] = sort(i in 0..4 by 0 * sign.$i)
    on demo { a = ord[0]; b = ord[1]; c = ord[2]; d = ord[3] }
    fact a = 0
    fact b = 0
    fact c = 0
    fact d = 0
}
", &[]);
        assert_eq!([set["a"][0], set["b"][0], set["c"][0], set["d"][0]], [0.0, 1.0, 2.0, 3.0]);
    }
}
