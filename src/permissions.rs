//! Who has said yes. A plugin declares in its file what its logic wants to
//! touch of the system, but declaring it is not having it: **whoever uses it approves it**, once,
//! and it is stored outside the plugin, together with the fingerprint of its logic and of what
//! it asked for. If the plugin changes —its code, or what it asks for—, the approval no longer holds.
//!
//! The scene you open yourself does not go through here: opening it is already deciding, like
//! running a script. What comes in through an `import` is what has to be looked at.

use crate::scene::{Permissions, Plugin};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `--no-permissions` (and `--layout`, which paints a scene nobody approved
/// yet to say how it is laid out): the scene's logic, its pieces' and its
/// plugins' have none —no `run`, no `spawn`, no service, so no network either—
/// and its terminals start no program. Whatever the scene declares.
static NONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn grant_none() {
    NONE.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn none() -> bool {
    NONE.load(std::sync::atomic::Ordering::Relaxed)
}

/// What the logic is given: what was declared or approved, or nothing at all.
pub fn granted(p: Permissions) -> Permissions {
    granted_if(none(), p)
}

fn granted_if(none: bool, p: Permissions) -> Permissions {
    if none { Permissions::default() } else { p }
}

/// What runs anything it is told: asking for it is asking for everything.
const INTERPRETERS: [&str; 12] = ["sh", "bash", "zsh", "fish", "dash", "python", "python3", "perl", "ruby", "node", "lua", "env"];

fn approvals_file() -> PathBuf {
    let dir = crate::platform::config_dir();
    let file = dir.join("permissions.json");
    // Until 2026-09-25 it was called `permisos.json`: move it once, or every
    // plugin already approved would ask again.
    let old = dir.join("permisos.json");
    if !file.exists() && old.exists() {
        let _ = std::fs::rename(&old, &file);
    }
    file
}

/// Variables that decide which code a program runs, not how it behaves: where a
/// bare name is looked up, what the loader adds to it, what an interpreter loads
/// before its script, and the folders it reads its settings from. With one of them
/// `run: "node tools/usage.js"` runs whatever else: `run` and `spawn` refuse them
/// unless the permissions name them (`env: "PATH"`).
const LOADS_CODE: [&str; 33] = [
    "PATH", "ENV", "BASH_ENV", "BASHOPTS", "SHELLOPTS", "PS4", "PROMPT_COMMAND", "IFS", "ZDOTDIR",
    "NODE_OPTIONS", "NODE_PATH", "PERL5OPT", "PERL5LIB", "PERLLIB", "PERL5DB", "RUBYOPT", "RUBYLIB",
    "JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH", "GCONV_PATH",
    "GTK_MODULES", "GTK_PATH", "GIO_EXTRA_MODULES", "GIO_MODULE_DIR", "QT_PLUGIN_PATH",
    "LESSOPEN", "LESSCLOSE", "PAGER", "EDITOR", "VISUAL", "BROWSER",
];
/// The same, by how they begin: the loader's (`LD_PRELOAD`, `DYLD_INSERT_LIBRARIES`),
/// Python's, Lua's, git's, npm's; and the home and settings folders, where a program
/// finds what to load (`~/.npmrc`, Python's `usercustomize`, git's hooks).
const LOADS_CODE_PREFIX: [&str; 9] = ["LD_", "DYLD_", "PYTHON", "LUA_", "GIT_", "NPM_CONFIG_", "HOME", "XDG_CONFIG_", "XDG_DATA_"];

/// Variables any program can be given: its language, time zone, colours and size.
const HARMLESS: [&str; 9] = ["LANG", "LANGUAGE", "TZ", "NO_COLOR", "FORCE_COLOR", "TERM", "COLORTERM", "COLUMNS", "LINES"];

/// Whether this variable decides which code runs (see `LOADS_CODE`). By its
/// name in capitals: Windows does not tell `Path` from `PATH`.
pub fn loads_code(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    LOADS_CODE.contains(&n.as_str()) || LOADS_CODE_PREFIX.iter().any(|p| n.starts_with(p))
}

/// Why `run` or `spawn` may not set this variable, or nothing if it may. A
/// variable the permissions name always may. One that decides which code runs
/// never may otherwise. And a plugin's logic, which someone else approved
/// reading «node tools/usage.js», may set only the harmless ones.
pub fn env_refused(declared: &[String], plugin: bool, name: &str) -> Option<&'static str> {
    if declared.iter().any(|d| d == name) {
        return None;
    }
    if loads_code(name) {
        return Some("it decides which code the program runs");
    }
    let n = name.to_ascii_uppercase();
    if plugin && !(HARMLESS.contains(&n.as_str()) || n.starts_with("LC_")) {
        return Some("a plugin sets only its language, time zone and colours (LANG, LC_*, TZ, NO_COLOR…) unless it asks for more");
    }
    None
}

/// FNV-1a, 64-bit: small, and the same everywhere and in every version.
fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// What an approval depends on: the plugin's code, what it asks for, and the
/// files inside its folder that its commands name (`node tools/usage.js`):
/// approving that command is approving what `usage.js` says.
pub fn fingerprint(p: &Plugin) -> String {
    let code = std::fs::read(&p.logic).unwrap_or_default();
    let mut asks = format!("|run:{}|services:{}", p.permissions.commands.join(","), p.permissions.services.join(","));
    // Only when it asks for any: what was approved before `env:` existed still holds.
    if !p.permissions.env.is_empty() {
        asks.push_str(&format!("|env:{}", p.permissions.env.join(",")));
    }
    format!("{:016x}", fnv(code.into_iter().chain(asks.bytes()).chain(named_files(p))))
}

/// The files inside the plugin's folder that its commands name, each with its
/// name before it. Never one outside it: `/usr/bin/node` is the system's, not
/// the plugin's, and reading it on every load would cost what it weighs.
fn named_files(p: &Plugin) -> Vec<u8> {
    let Some(dir) = p.logic.parent().and_then(|d| d.canonicalize().ok()) else { return Vec::new() };
    let mut out = Vec::new();
    for word in p.permissions.commands.iter().flat_map(|c| c.split_whitespace()) {
        let Ok(path) = dir.join(word).canonicalize() else { continue };
        if path.starts_with(&dir) && path.is_file() && let Ok(bytes) = std::fs::read(&path) {
            out.extend(format!("|file:{word}:").bytes());
            out.extend(bytes);
        }
    }
    out
}

fn approved() -> BTreeMap<String, String> {
    std::fs::read_to_string(approvals_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn key(p: &Plugin) -> String {
    p.logic.canonicalize().unwrap_or_else(|_| p.logic.clone()).to_string_lossy().into_owned()
}

/// A plugin that asks for nothing does not need anyone to say yes to it; nor does a
/// scene brought into the one opened (G22): opening that one is already deciding.
pub fn is_approved(p: &Plugin) -> bool {
    p.brought || p.permissions == Permissions::default() || approved().get(&key(p)) == Some(&fingerprint(p))
}

/// The permissions it really runs with: its own if they are approved; if not, none.
pub fn effective(p: &Plugin) -> Permissions {
    if is_approved(p) { p.permissions.clone() } else { Permissions::default() }
}

pub fn approve(p: &Plugin) -> Result<(), String> {
    let mut all = approved();
    all.insert(key(p), fingerprint(p));
    let path = approvals_file();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, serde_json::to_string_pretty(&all).unwrap()).map_err(|e| format!("{}: {e}", path.display()))
}

/// What it asks for, said so that it is understood, and warning about what asking for everything is.
pub fn describe(p: &Permissions) -> String {
    let list = |l: &[String], flag: &dyn Fn(&str) -> bool| match l {
        [] => "none".to_owned(),
        _ => l.iter().map(|x| if flag(x) { format!("{x} ⚠") } else { x.clone() }).collect::<Vec<_>>().join(", "),
    };
    // Bare: it runs whatever it is told. With its script (`"node usage.js"`), only that.
    let is_interpreter = |o: &str| !o.contains(' ') && INTERPRETERS.contains(&o.rsplit('/').next().unwrap_or(o));
    // `say:*` tells any scene any of `--say`'s commands, `quit` among them.
    let says_to_all = |x: &str| x == "say:*";
    let mut s = format!("commands: {} · services: {}", list(&p.commands, &is_interpreter), list(&p.services, &says_to_all));
    if !p.env.is_empty() {
        s.push_str(&format!(" · variables: {}", list(&p.env, &|x| loads_code(x))));
    }
    if p.commands.iter().any(|o| is_interpreter(o)) {
        s.push_str(" · ⚠ an interpreter runs whatever it is told: that is asking for everything");
    }
    if p.env.iter().any(|x| loads_code(x)) {
        s.push_str(" · ⚠ a variable that decides which code runs (PATH, LD_PRELOAD, NODE_OPTIONS…) lets its commands run something else: that is asking for everything");
    }
    if p.services.iter().any(|x| says_to_all(x)) {
        s.push_str(" · ⚠ say:* tells any running scene anything --say can, quit included");
    }
    s
}

/// `pleamar --approve scene.plm`: shows what each plugin of that scene asks for, and asks.
/// With `yes_to_all`, it does not ask (for scripts; use it knowing what is being approved).
/// `only`: that plugin alone (`--plugin Name`). `json`: nothing is asked; what each plugin
/// asks for and whether it is approved is written as JSON, after approving if `yes_to_all`.
pub fn ask(scene: &str, yes_to_all: bool, only: Option<&str>, json: bool) -> i32 {
    let e = match crate::scenes::from_file::read(scene) {
        Ok(e) => e,
        Err(m) => {
            eprintln!("{m}");
            return 1;
        }
    };
    let plugins: Vec<&Plugin> = e.plugins.iter().filter(|p| only.is_none_or(|n| p.name == n)).collect();
    if let Some(n) = only
        && plugins.is_empty()
    {
        let names: Vec<&str> = e.plugins.iter().map(|p| p.name.as_str()).collect();
        eprintln!("{scene} has no plugin called '{n}'{}", if names.is_empty() { ": it uses none".to_owned() } else { format!(": it has {}", names.join(", ")) });
        return 1;
    }
    if json {
        for p in plugins.iter().filter(|p| yes_to_all && !is_approved(p)) {
            if let Err(m) = approve(p) {
                eprintln!("could not save the approval of '{}': {m}", p.name);
                return 1;
            }
        }
        let list: Vec<serde_json::Value> = plugins
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "logic": p.logic.display().to_string(),
                    "approved": is_approved(p),
                    "asks_nothing": p.permissions == Permissions::default(),
                    "commands": p.permissions.commands,
                    "services": p.permissions.services,
                    "env": p.permissions.env,
                    "summary": describe(&p.permissions),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "scene": scene, "plugins": list })).unwrap_or_default());
        return 0;
    }
    let pending: Vec<&Plugin> = plugins.iter().copied().filter(|p| !is_approved(p)).collect();
    if e.plugins.is_empty() {
        println!("{scene} uses no plugins: there is nothing to approve.");
        return 0;
    }
    for p in plugins.iter().filter(|p| is_approved(p)) {
        println!("✓ '{}' · {} · {}", p.name, p.logic.display(), if p.permissions == Permissions::default() { "asks for nothing".to_owned() } else { format!("already approved · {}", describe(&p.permissions)) });
    }
    let mut denied = 0;
    for p in pending {
        println!("\n? Plugin '{}' ({}) wants:\n    {}", p.name, p.logic.display(), describe(&p.permissions));
        let yes = yes_to_all || {
            print!("  Approve it? [y/N] ");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            matches!(line.trim().to_lowercase().as_str(), "s" | "si" | "sí" | "y" | "yes")
        };
        if yes {
            match approve(p) {
                Ok(()) => println!("  approved. If its logic, what it asks for or a script its commands name changes, it will have to be approved again."),
                Err(m) => {
                    eprintln!("  could not save it: {m}");
                    return 1;
                }
            }
        } else {
            println!("  not approved: it will run, but unable to touch anything of the system.");
            denied += 1;
        }
    }
    if denied > 0 { 2 } else { 0 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn with_none_granted_nothing_is_given() {
        let p = crate::scene::Permissions { commands: vec!["touch".into()], services: vec!["files.write".into()], env: vec![] };
        assert_eq!(super::granted_if(false, p.clone()), p);
        assert_eq!(super::granted_if(true, p), crate::scene::Permissions::default());
    }

    use super::{Permissions, describe, env_refused};

    #[test]
    fn telling_every_scene_is_flagged_and_telling_one_is_not() {
        let p = |services: &[&str]| Permissions { commands: Vec::new(), services: services.iter().map(|s| s.to_string()).collect(), env: Vec::new() };
        let one = describe(&p(&["say:bar"]));
        assert!(!one.contains('⚠'), "{one}");
        let all = describe(&p(&["audio", "say:*"]));
        assert!(all.contains("say:* ⚠"), "{all}");
        assert!(all.contains("⚠ say:* tells any running scene"), "{all}");
    }

    #[test]
    fn variables_that_load_code_are_refused_unless_declared() {
        let none: Vec<String> = Vec::new();
        // Whoever approved «node tools/usage.js» did not approve another program, a loader or a script.
        for name in ["PATH", "LD_PRELOAD", "LD_LIBRARY_PATH", "NODE_OPTIONS", "PYTHONPATH", "PYTHONSTARTUP", "PERL5OPT", "RUBYOPT", "BASH_ENV", "LUA_INIT", "DYLD_INSERT_LIBRARIES", "HOME", "Path"] {
            assert!(env_refused(&none, true, name).is_some(), "{name}, plugin");
            assert!(env_refused(&none, false, name).is_some(), "{name}, scene");
        }
        // Named in its permissions, it goes through: and `--approve` shows it with a ⚠.
        assert!(env_refused(&["PATH".into()], true, "PATH").is_none());
        assert!(describe(&Permissions { env: vec!["PATH".into()], ..Default::default() }).contains("PATH ⚠"));
        // A plugin sets only the harmless ones; a scene, also its own.
        for name in ["LANG", "LC_ALL", "TZ", "NO_COLOR"] {
            assert!(env_refused(&none, true, name).is_none(), "{name}");
        }
        assert!(env_refused(&none, true, "MY_TOKEN").is_some());
        assert!(env_refused(&["MY_TOKEN".into()], true, "MY_TOKEN").is_none());
        assert!(env_refused(&none, false, "WAYLAND_DISPLAY").is_none());
        assert!(env_refused(&none, false, "CHECKUPDATES_DB").is_none());
    }
}
