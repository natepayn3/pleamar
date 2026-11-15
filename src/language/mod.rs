//! The scene language. A text file goes in and a `Scene` comes out —the same
//! one that until now was written in Rust—, or an error that says where and
//! why. Everything declared here is run by the render, on its own.
//!
//! Three steps: `tokens` chops, `tree` groups into nodes and properties without
//! knowing what they mean, and `compiler` gives them meaning and checks the names.

mod compiler;
mod figure;
mod tokens;
mod tree;
pub mod vocabulary;

use crate::scene::Scene;
use std::path::{Path, PathBuf};

/// A scene can be made of several files (`import`), and an error has to say
/// which one. So as not to load every token with a name, the line number
/// carries the file's inside it: line 12 of the third one is 2 000 012.
pub(crate) const PER_FILE: usize = 1_000_000;

/// The version of the language this program understands. The first number changes
/// when something written stops being valid; the second, when something is added. A file
/// can say which one it needs (`language 0.1`) and find out on load, not halfway through.
pub const VERSION: (u32, u32) = (0, 10);

#[derive(Debug)]
pub struct CompileError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl CompileError {
    pub fn at(line: usize, col: usize, message: impl Into<String>) -> CompileError {
        CompileError { line, col, message: message.into() }
    }

    /// With the file's line and an arrow underneath, the way a compiler shows it.
    fn with_source(&self, files: &[(PathBuf, String)]) -> String {
        let (path, source) = files.get(self.line / PER_FILE).or(files.first()).map_or((String::new(), ""), |(r, f)| {
            // Relative to where we are, if possible: a whole path gets in the way more than it helps.
            let short = std::env::current_dir().ok().and_then(|here| r.strip_prefix(here).ok().map(Path::to_owned)).unwrap_or_else(|| r.clone());
            (short.display().to_string(), f.as_str())
        });
        let n = self.line % PER_FILE;
        let line = source.lines().nth(n.saturating_sub(1)).unwrap_or("");
        let margin = format!("{n:>4} | ");
        format!("{path}:{n}:{}: {}\n{margin}{line}\n{}^", self.col, self.message, " ".repeat(margin.len() + self.col.saturating_sub(1)))
    }
}

/// "file:line" of something that was declared, to say where the thing being overridden was.
pub fn location(names: &[String], line: usize) -> String {
    format!("{}:{}", names.get(line / PER_FILE).map_or("", String::as_str), line % PER_FILE)
}

/// What has been read so far: each file once, with its text to show the errors.
#[derive(Default)]
struct Reader {
    files: Vec<(PathBuf, String)>,
    /// The ones being read right now, one inside another: to spot cycles.
    open_stack: Vec<PathBuf>,
    /// Which files are `strict` libraries, by their number.
    strict: Vec<usize>,
    /// Each library: its file number, its name, and its logic if it has a `.luau` next to it.
    libraries: Vec<compiler::Library>,
    /// What an editor has open and has not saved yet: it wins over the disk.
    unsaved: Vec<(PathBuf, String)>,
    /// The name each library was imported with, `as menu`, if any.
    surnames: Vec<(PathBuf, Option<String>)>,
    /// The `import`s written in a part's file, before its `part`: the scene's, as if written
    /// at its top, with their paths already from where they were written.
    hoisted: Vec<tree::Entry>,
    /// The scenes brought into this one (`import scene "bar.plm" as Bar`), and what they
    /// write, already under their own surface: it goes into the scene's body at the end.
    brought_scenes: Vec<compiler::BroughtScene>,
    piece_entries: Vec<tree::Entry>,
    /// While a scene is being brought in: the libraries it imports, its own and the ones they
    /// import, even when another had read them first (issue 149).
    scene_libs: Option<Vec<PathBuf>>,
    /// `uses scene "bar.plm" as Bar`, as they were read: the name, the file, which file
    /// says it and where. Settled once every scene brought in is known (`settle_uses`).
    uses: Vec<(String, PathBuf, PathBuf, usize, usize)>,
}

impl Reader {
    /// Tokenizes and groups a file, with its number put into the lines.
    fn open(&mut self, path: &Path) -> Result<Vec<tree::Entry>, CompileError> {
        let k = self.files.len();
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        let source = match self.unsaved.iter().find(|(r, _)| *r == canonical) {
            Some((_, t)) => t.clone(),
            None => std::fs::read_to_string(path).map_err(|e| CompileError::at(0, 0, format!("cannot read {}: {e}", path.display())))?,
        };
        self.files.push((path.to_owned(), source));
        let here = |mut f: CompileError| { f.line += k * PER_FILE; f };
        let mut tokens = tokens::tokenize(&self.files[k].1).map_err(here)?;
        for f in &mut tokens {
            f.line += k * PER_FILE;
        }
        // Every file says which version it needs, a library too.
        check_version(tree::parse(&tokens).map_err(|f| if f.line < PER_FILE { here(f) } else { f })?)
    }

    /// `include "pages/wifi.plm"`: a piece of the scene kept in a file of its
    /// own (`part Wifi { … }`), put where the line is —inside a group, if that
    /// is where it is written—, as if it had been written there: the same
    /// names, the same order, its lines still its own file's, for the errors and
    /// for the paths of what it draws. A part can include others. Inside it, it does
    /// not import: libraries are the scene's business. But its file can have `import`s
    /// before the `part` —a file written by a program that lists plugins and shows them,
    /// `import "weather/weather.plm" as weather` and then `part Plugins { weather.Card() }`—:
    /// they are the scene's, as if written at its top.
    fn resolve_includes(&mut self, path: &Path, entries: &mut Vec<tree::Entry>) -> Result<(), CompileError> {
        use tokens::TokenKind;
        let mut out = Vec::with_capacity(entries.len());
        for mut e in std::mem::take(entries) {
            let tree::Entry::Node(n) = &mut e else {
                out.push(e);
                continue;
            };
            if !matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "include") {
                if let Some(body) = n.body.as_mut() {
                    self.resolve_includes(path, body)?;
                }
                out.push(e);
                continue;
            }
            // `include "pages/settings.plm" while panel == settings keep 30s`: a page that
            // is there only while its condition holds (see `Scene::pages`).
            let usage = "an include is `include \"path/to/the/part.plm\"`: a piece of this scene, in a file of its own. Or, for a page that is there only while something holds, `include \"pages/settings.plm\" while panel == settings`, and after it `keep 30s` or `keep forever`";
            let paged = matches!(n.head.get(2).map(|f| &f.kind), Some(TokenKind::Id(w)) if w == "while");
            let (Some(TokenKind::Str(which)), true, None) = (n.head.get(1).map(|f| &f.kind), n.head.len() == 2 || paged, &n.body) else {
                return Err(CompileError::at(n.line, n.col, usage));
            };
            if paged {
                let keep = n.head.iter().position(|t| matches!(&t.kind, TokenKind::Id(w) if w == "keep"));
                let condition = &n.head[3..keep.unwrap_or(n.head.len())];
                if condition.is_empty() {
                    return Err(CompileError::at(n.line, n.head[2].col, "`while` says when the page is there: `while panel == settings`"));
                }
                if let Some(k) = keep {
                    let fine = match n.head.get(k + 1..).unwrap_or(&[]) {
                        [t] => matches!(&t.kind, TokenKind::Dur(_)) || matches!(&t.kind, TokenKind::Id(w) if w == "forever"),
                        _ => false,
                    };
                    if !fine {
                        return Err(CompileError::at(n.line, n.head[k].col, "`keep` says how long the page stays after its condition stops holding: `keep 30s`, or `keep forever`"));
                    }
                }
            }
            let target = path.parent().unwrap_or(Path::new(".")).join(which);
            let target = target.canonicalize().map_err(|e| CompileError::at(n.line, n.head[1].col, format!("cannot find '{which}' (looking in {}): {e}", target.display())))?;
            if self.open_stack.contains(&target) {
                return Err(CompileError::at(n.line, n.head[1].col, format!("'{which}' ends up including itself: {}", self.open_stack.iter().chain([&target]).map(|p| p.file_name().unwrap_or_default().to_string_lossy()).collect::<Vec<_>>().join(" → "))));
            }
            self.open_stack.push(target.clone());
            let mut theirs = self.open(&target)?;
            // Its `import`s first, then its part.
            let is_import = |e: &tree::Entry| matches!(e, tree::Entry::Node(x) if matches!(x.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "import"));
            let imports = theirs.iter().take_while(|e| is_import(e)).count();
            for mut i in theirs.drain(..imports) {
                if let tree::Entry::Node(x) = &mut i
                    && let Some(TokenKind::Str(which)) = x.head.get_mut(1).map(|f| &mut f.kind)
                    && !which.starts_with("pleamar:")
                {
                    *which = target.parent().unwrap_or(Path::new(".")).join(&*which).to_string_lossy().into_owned();
                }
                self.hoisted.push(i);
            }
            let part = match theirs.as_slice() {
                [tree::Entry::Node(p)] if matches!(p.head.first().map(|f| &f.kind), Some(TokenKind::Id(w)) if w == "part") => p,
                [tree::Entry::Node(p)] if matches!(p.head.first().map(|f| &f.kind), Some(TokenKind::Id(w)) if w == "library") => {
                    return Err(CompileError::at(n.line, n.head[1].col, format!("'{which}' is a library: a library is imported (`import \"{which}\"`), and a part, included")));
                }
                _ => return Err(CompileError::at(n.line, n.head[1].col, format!("'{which}' is not a part: what is included is `part Name {{ … }}`, a piece of a scene (with, before it, the `import`s it uses), and nothing else"))),
            };
            if !matches!((part.head.get(1).map(|f| &f.kind), part.head.len()), (Some(TokenKind::Id(_)), 2)) {
                return Err(CompileError::at(part.line, part.col, "a part has a name, and only that: `part Wifi { … }`"));
            }
            let mut body = part.body.clone().unwrap_or_default();
            if let Some(tree::Entry::Node(i)) = body.iter().find(|d| matches!(d, tree::Entry::Node(x) if matches!(x.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "import"))) {
                return Err(CompileError::at(i.line, i.col, "a part does not import: libraries are imported by the scene, and then the part can use them"));
            }
            self.resolve_includes(&target, &mut body)?;
            self.open_stack.pop();
            // A page goes between two marks: the `include` itself, which carries its
            // condition, and a bare `include` where it ends (no one can write that one).
            if paged {
                out.push(tree::Entry::Node(tree::Node { head: n.head.clone(), body: None, line: n.line, col: n.col }));
            }
            out.extend(body);
            if paged {
                out.push(tree::Entry::Node(tree::Node { head: n.head[..1].to_vec(), body: None, line: n.line, col: n.col }));
            }
        }
        *entries = out;
        Ok(())
    }

    /// What a file's `import`s declare, in order, and what is left of it.
    /// A library imported twice —by two routes— is read once.
    fn resolve_imports(&mut self, path: &Path, entries: Vec<tree::Entry>, brought: &mut Vec<tree::Entry>) -> Result<Vec<tree::Entry>, CompileError> {
        use tokens::TokenKind;
        let mut rest = Vec::new();
        for e in entries {
            let tree::Entry::Node(n) = &e else { rest.push(e); continue };
            // `uses scene "bar.plm" as Bar`: a scene this one talks to, without bringing it in.
            if matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "uses") {
                let usage = "a scene says which one it talks to as `uses scene \"bar.plm\" as Bar`";
                let (Some(TokenKind::Id(w)), Some(TokenKind::Str(which)), Some(TokenKind::Id(a)), Some(TokenKind::Id(name)), 5, None) =
                    (n.head.get(1).map(|f| &f.kind), n.head.get(2).map(|f| &f.kind), n.head.get(3).map(|f| &f.kind), n.head.get(4).map(|f| &f.kind), n.head.len(), &n.body)
                else {
                    return Err(CompileError::at(n.line, n.col, usage));
                };
                if w != "scene" || a != "as" || name.contains('.') || name.contains('$') {
                    return Err(CompileError::at(n.line, n.col, usage));
                }
                let target = path.parent().unwrap_or(Path::new(".")).join(which);
                let target = target.canonicalize().map_err(|e| CompileError::at(n.line, n.head[2].col, format!("cannot find '{which}' (looking in {}): {e}", target.display())))?;
                self.uses.push((name.clone(), target, path.to_owned(), n.line, n.head[2].col));
                continue;
            }
            if !matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "import") {
                rest.push(e);
                continue;
            }
            // `import scene "bar.plm" as Bar`: a whole scene, under its name (G22).
            if matches!(n.head.get(1).map(|f| &f.kind), Some(TokenKind::Id(w)) if w == "scene") {
                self.import_scene(n, brought)?;
                continue;
            }
            // `import "menu.plm" as menu`: its components are `menu.Row`, so two
            // libraries with a `Row` each can live in the same scene.
            let surname = match (n.head.get(2).map(|f| &f.kind), n.head.get(3).map(|f| &f.kind), n.head.len()) {
                (None, _, 2) => None,
                (Some(TokenKind::Id(a)), Some(TokenKind::Id(s)), 4) if a == "as" && !s.contains('.') => Some(s.clone()),
                _ => None,
            };
            let (Some(TokenKind::Str(which)), None, true) = (n.head.get(1).map(|f| &f.kind), &n.body, n.head.len() == 2 || surname.is_some()) else {
                return Err(CompileError::at(n.line, n.col, "an import is `import \"path/to/the/library.plm\"`, or `import \"…\" as name` to call its components `name.Row`"));
            };
            // `pleamar:ui` is one of pleamar's own libraries: it comes inside the
            // program, so it is there wherever the scene is and whoever runs it.
            // Anything else is a path, relative to the importing file, not to
            // where it is launched from.
            let target = if let Some(name) = which.strip_prefix("pleamar:") {
                let Some(source) = builtin_library(name) else {
                    return Err(CompileError::at(n.line, n.head[1].col, format!("pleamar has no library '{name}': there is {}", BUILTIN_LIBRARIES.iter().map(|(n, _)| format!("pleamar:{n}")).collect::<Vec<_>>().join(", "))));
                };
                let t = PathBuf::from(format!("pleamar:{name}.plm"));
                if !self.unsaved.iter().any(|(r, _)| *r == t) {
                    self.unsaved.push((t.clone(), source.to_owned()));
                }
                t
            } else {
                let target = path.parent().unwrap_or(Path::new(".")).join(which);
                target.canonicalize().map_err(|e| CompileError::at(n.line, n.head[1].col, format!("cannot find '{which}' (looking in {}): {e}", target.display())))?
            };
            if self.open_stack.contains(&target) {
                return Err(CompileError::at(n.line, n.head[1].col, format!("'{which}' ends up importing itself: {}", self.open_stack.iter().chain([&target]).map(|p| p.file_name().unwrap_or_default().to_string_lossy()).collect::<Vec<_>>().join(" → "))));
            }
            if let Some((_, before)) = self.surnames.iter().find(|(r, _)| *r == target) {
                if *before != surname {
                    let said = |s: &Option<String>| s.as_ref().map_or("with no name".to_owned(), |s| format!("as {s}"));
                    return Err(CompileError::at(n.line, n.col, format!("'{which}' is already imported {}, and here {}: a library has one name in a scene", said(before), said(&surname))));
                }
            }
            if let Some(libs) = self.scene_libs.as_mut() {
                if !libs.contains(&target) {
                    libs.push(target.clone());
                }
            }
            if self.files.iter().any(|(r, _)| r == &target) {
                continue;
            }
            self.surnames.push((target.clone(), surname.clone()));
            self.open_stack.push(target.clone());
            let number = self.files.len();
            let theirs = self.open(&target)?;
            let theirs = self.resolve_imports(&target, theirs, brought)?;
            self.open_stack.pop();
            let [tree::Entry::Node(b)] = theirs.as_slice() else {
                return Err(CompileError::at(n.line, n.head[1].col, format!("'{which}' is not a library: it has to be `library Name {{ … }}`, and nothing else")));
            };
            if !matches!(b.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "library") {
                return Err(CompileError::at(b.line, b.col, "what you import is a library: `library Name { … }`. A scene is not imported"));
            }
            let Some(tree::Entry::Node(b)) = theirs.into_iter().next() else { unreachable!() };
            let TokenKind::Id(library_name) = &b.head.get(1).map(|f| f.kind.clone()).unwrap_or(TokenKind::Id(String::new())) else {
                return Err(CompileError::at(b.line, b.col, "this library is missing its name: `library Name { … }`"));
            };
            let logic = target.with_extension("luau");
            self.libraries.push(compiler::Library { file: number, name: library_name.clone(), logic: logic.is_file().then_some(logic) });
            // `library Menu strict { … }`: its components only read what they ask for.
            match b.head.get(2).map(|f| &f.kind) {
                None => {}
                Some(TokenKind::Id(p)) if p == "strict" && b.head.len() == 3 => self.strict.push(number),
                Some(_) => return Err(CompileError::at(b.line, b.head[2].col, "after a library name only `strict` can go")),
            }
            let mut body = b.body.unwrap_or_default();
            if let Some(surname) = &surname {
                let own: Vec<String> = body
                    .iter()
                    .filter_map(|d| match d {
                        tree::Entry::Node(x) if matches!(x.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "component") => match x.head.get(1).map(|f| &f.kind) {
                            Some(TokenKind::Id(c)) => Some(c.clone()),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect();
                // Where it is declared and wherever the library itself uses it.
                fn rename(entries: &mut [tree::Entry], own: &[String], surname: &str) {
                    let fix = |tokens: &mut [tokens::Token]| {
                        for t in tokens {
                            if let TokenKind::Id(w) = &mut t.kind {
                                if own.contains(w) {
                                    *w = format!("{surname}.{w}");
                                }
                            }
                        }
                    };
                    for e in entries {
                        match e {
                            tree::Entry::Prop { value, .. } => fix(value),
                            tree::Entry::Node(x) => {
                                fix(&mut x.head);
                                rename(x.body.as_deref_mut().unwrap_or(&mut []), own, surname);
                            }
                        }
                    }
                }
                rename(&mut body, &own, surname);
            }
            for d in body {
                // A library declares; it does not paint, or react, or have a frontier with the logic.
                // `text now = "…"` declares; `text "hello" { … }` paints, and that is not a library's business.
                let ok = matches!(&d, tree::Entry::Node(x) if matches!(x.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if vocabulary::LIBRARY_STATEMENTS.contains(&p.as_str())
                    && (!["text", "image"].contains(&p.as_str()) || matches!(x.head.get(2).map(|f| &f.kind), Some(TokenKind::Sym("="))))));
                if !ok {
                    let (l, c) = match &d { tree::Entry::Node(x) => (x.line, x.col), tree::Entry::Prop { line, col, .. } => (*line, *col) };
                    return Err(CompileError::at(l, c, "a library only declares: `let`, `spring`, `component`, its frontier (`fact`, `text`, `model`, `event`, `image x = …`, `permissions`) and what it moves inside (`prop`, `pose`, `gesture`, `posture`, `layer`). What gets painted, and loose rules, belong to the scene"));
                }
                brought.push(d);
            }
        }
        Ok(rest)
    }

    /// `import scene "bar.plm" as Bar`: a scene that can also be opened alone, brought
    /// into this one with everything of its own under its name (G22). Its libraries are
    /// the scene's, as if imported here; its parts are its own. Its surface without a name
    /// becomes the surface `Bar`, and what it draws loose goes into it.
    fn import_scene(&mut self, n: &tree::Node, brought: &mut Vec<tree::Entry>) -> Result<(), CompileError> {
        use tokens::TokenKind;
        let usage = "a scene is brought into another as `import scene \"bar.plm\" as Bar`";
        let (Some(TokenKind::Str(which)), Some(TokenKind::Id(a)), Some(TokenKind::Id(name)), 5, None) =
            (n.head.get(2).map(|f| &f.kind), n.head.get(3).map(|f| &f.kind), n.head.get(4).map(|f| &f.kind), n.head.len(), &n.body)
        else {
            return Err(CompileError::at(n.line, n.col, usage));
        };
        if a != "as" || name.contains('.') || name.contains('$') {
            return Err(CompileError::at(n.line, n.col, usage));
        }
        // Only the scene that is opened brings others: a scene brought in, or a library, does not.
        if self.open_stack.len() > 1 {
            return Err(CompileError::at(n.line, n.col, "only the scene that is opened brings in others: a scene brought in, or a library, cannot `import scene`"));
        }
        if self.brought_scenes.iter().any(|p| p.name == *name) {
            return Err(CompileError::at(n.line, n.head[4].col, format!("there is already a scene called '{name}' in this one")));
        }
        let here = self.open_stack.first().cloned().unwrap_or_default();
        let target = here.parent().unwrap_or(Path::new(".")).join(which);
        let target = target.canonicalize().map_err(|e| CompileError::at(n.line, n.head[2].col, format!("cannot find '{which}' (looking in {}): {e}", target.display())))?;
        if self.open_stack.contains(&target) {
            return Err(CompileError::at(n.line, n.head[2].col, format!("'{which}' is this same scene: a scene does not bring itself in")));
        }
        if let Some(p) = self.brought_scenes.iter().find(|p| self.files.get(p.files[0]).is_some_and(|(r, _)| *r == target)) {
            return Err(CompileError::at(n.line, n.head[2].col, format!("'{which}' is already brought in as {}: a scene comes in once", p.name)));
        }
        self.open_stack.push(target.clone());
        let first = self.files.len();
        let libraries_before = self.libraries.len();
        let mut theirs = self.open(&target)?;
        self.resolve_includes(&target, &mut theirs)?;
        let hoisted = std::mem::take(&mut self.hoisted);
        theirs.splice(0..0, hoisted);
        let outer = self.scene_libs.replace(Vec::new());
        let rest = self.resolve_imports(&target, theirs, brought)?;
        let imported = std::mem::replace(&mut self.scene_libs, outer).unwrap_or_default();
        self.open_stack.pop();
        let scene = match rest.as_slice() {
            [tree::Entry::Node(s)] if matches!(s.head.first().map(|f| &f.kind), Some(TokenKind::Id(w)) if w == "scene") => s,
            [tree::Entry::Node(s)] if matches!(s.head.first().map(|f| &f.kind), Some(TokenKind::Id(w)) if w == "library") => {
                return Err(CompileError::at(n.line, n.head[2].col, format!("'{which}' is a library: it is imported with `import \"{which}\"`, without `scene`")));
            }
            _ => return Err(CompileError::at(n.line, n.head[2].col, format!("'{which}' is not a scene: what `import scene` brings is `scene Name {{ … }}`"))),
        };
        let libraries: Vec<usize> = self.libraries[libraries_before..].iter().map(|b| b.file).collect();
        let files: Vec<usize> = (first..self.files.len()).filter(|k| !libraries.contains(k)).collect();
        // What its libraries declare with `let` is the scene's too, under its name: they read its facts
        // (`let col = rgb(c_r, 0, 0)`), and two scenes that import one each have their own. So each library's
        // `let`s are read again, in a file that belongs to the scene, before its own lines (issue 149).
        let mut lets = Vec::new();
        let (mut libs, mut lib_lets) = (Vec::new(), Vec::new());
        for t in &imported {
            let Some(original) = self.files.iter().position(|(r, _)| r == t) else { continue };
            libs.push(original);
            let copy = self.files.len();
            let theirs = self.open(t)?;
            let only_lets = |d: &tree::Entry| {
                matches!(d, tree::Entry::Node(x) if matches!(x.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "let")
                    && !matches!(x.head.get(2).map(|f| &f.kind), Some(TokenKind::Sym("["))))
            };
            let mine: Vec<tree::Entry> = theirs
                .into_iter()
                .filter_map(|e| match e {
                    tree::Entry::Node(x) if matches!(x.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "library") => x.body,
                    _ => None,
                })
                .flatten()
                .filter(only_lets)
                .collect();
            if mine.is_empty() {
                // Nothing to read again: the file is not needed.
                self.files.pop();
                continue;
            }
            lib_lets.push((copy, original));
            lets.extend(mine);
        }
        // Its surface without a name, and what it draws loose, become its surface `Bar`. A
        // scene whose surfaces all have a name keeps its loose rules at its level, as alone:
        // a surface `Bar` would be one more on the screen, for nothing.
        let body = scene.body.clone().unwrap_or_default();
        let has_main = body.iter().any(|e| matches!(e, tree::Entry::Node(x) if x.head.len() == 1 && matches!(x.head[0].kind, TokenKind::Id(ref w) if w == "surface")));
        let mut kept = lets;
        let mut own = Vec::new();
        for e in body {
            match e {
                tree::Entry::Node(x) if x.head.len() == 1 && matches!(x.head[0].kind, TokenKind::Id(ref w) if w == "surface") => own.extend(x.body.unwrap_or_default()),
                e if !has_main && compiler::is_rule(&e) => kept.push(e),
                e if compiler::is_loose_repeat(&e) => {
                    let (declared, drawn) = compiler::split_repeat(&e);
                    kept.extend(declared);
                    // Its rules, with nothing drawn, stay at the scene's level too when it has no main surface.
                    match drawn {
                        Some(d) if !has_main && !compiler::draws_anything(&d) => kept.push(d),
                        d => own.extend(d),
                    }
                }
                e if compiler::draws_loose(&e) => own.push(e),
                e => kept.push(e),
            }
        }
        if !own.is_empty() {
            // Its name is written on the `import scene` line: it is the scene's name for it, not one of its own.
            let word = tokens::Token { kind: TokenKind::Id("surface".into()), line: n.line, col: n.col };
            let named = n.head[4].clone();
            kept.push(tree::Entry::Node(tree::Node { head: vec![word, named], body: Some(own), line: n.line, col: n.col }));
        }
        let logic = target.with_extension("luau");
        self.brought_scenes.push(compiler::BroughtScene { name: name.clone(), files, logic: logic.is_file().then_some(logic), absent: false, libs, lib_lets });
        self.piece_entries.extend(kept);
        Ok(())
    }
}

impl Reader {
    /// The scenes named with `uses scene`, once every one brought in is known: one brought
    /// in under that name has to be that file; one that is not is **absent**. An absent one
    /// brings only what it declares (`fact`, `text … =`, `model`, `event`), under its name, for
    /// the names to be checked: its facts keep their first values, its events never come, and
    /// nothing of it runs. So a scene that talks to another still opens alone.
    fn settle_uses(&mut self) -> Result<(), CompileError> {
        use tokens::TokenKind;
        for (name, target, from, line, col) in std::mem::take(&mut self.uses) {
            let file_of = |r: &Reader, k: usize| r.files.get(k).map(|(p, _)| p.clone());
            if let Some(b) = self.brought_scenes.iter().find(|b| b.name == name) {
                if file_of(self, b.files[0]).as_ref() != Some(&target) {
                    let said = file_of(self, b.files[0]).map_or(String::new(), |p| p.file_name().unwrap_or_default().to_string_lossy().into_owned());
                    return Err(CompileError::at(line, col, format!("'{name}' is {said} here, and this says {}: a name is one scene", target.file_name().unwrap_or_default().to_string_lossy())));
                }
                continue;
            }
            if self.files.first().is_some_and(|(p, _)| *p == target) || from == target {
                return Err(CompileError::at(line, col, "a scene does not use itself: its own names are already its"));
            }
            self.open_stack.push(target.clone());
            let first = self.files.len();
            let mut theirs = self.open(&target)?;
            let hoisted = std::mem::take(&mut self.hoisted);
            self.resolve_includes(&target, &mut theirs)?;
            // What it imports is not needed for its declarations.
            self.hoisted = hoisted;
            self.open_stack.pop();
            let is = |e: &tree::Entry, w: &str| matches!(e, tree::Entry::Node(x) if matches!(x.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == w));
            let Some(tree::Entry::Node(scene)) = theirs.iter().find(|e| is(e, "scene")) else {
                return Err(CompileError::at(line, col, format!("'{}' is not a scene: what `uses scene` names is `scene Name {{ … }}`", target.file_name().unwrap_or_default().to_string_lossy())));
            };
            let declares = |e: &tree::Entry| match e {
                tree::Entry::Node(x) => match x.head.first().map(|f| &f.kind) {
                    Some(TokenKind::Id(w)) => matches!(w.as_str(), "fact" | "model" | "event") || (w == "text" && matches!(x.head.get(2).map(|f| &f.kind), Some(TokenKind::Sym("=")))),
                    _ => false,
                },
                _ => false,
            };
            let kept: Vec<tree::Entry> = scene.body.clone().unwrap_or_default().into_iter().filter(declares).collect();
            let files = (first..self.files.len()).collect();
            self.brought_scenes.push(compiler::BroughtScene { name, files, logic: None, absent: true, libs: Vec::new(), lib_lets: Vec::new() });
            self.piece_entries.extend(kept);
        }
        Ok(())
    }
}

/// pleamar's own libraries, inside the program: `import "pleamar:ui"`.
const BUILTIN_LIBRARIES: &[(&str, &str)] = &[("ui", include_str!("../../lib/ui.plm"))];

fn builtin_library(name: &str) -> Option<&'static str> {
    BUILTIN_LIBRARIES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// `language 0.1`, if present, is the first thing in the file. It is checked and removed.
fn check_version(mut entries: Vec<tree::Entry>) -> Result<Vec<tree::Entry>, CompileError> {
    use tokens::TokenKind;
    let Some(tree::Entry::Node(n)) = entries.first() else { return Ok(entries) };
    if !matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "language") {
        return Ok(entries);
    }
    let (Some(TokenKind::Num(v)), 2, None) = (n.head.get(1).map(|f| &f.kind), n.head.len(), &n.body) else {
        return Err(CompileError::at(n.line, n.col, format!("the version is asked for like this: `language {}.{}`", VERSION.0, VERSION.1)));
    };
    // `0.1` arrives as a number: the integer part, and the decimals as they are
    // written (`0.12` is twelve, not 1.2 rounded to one).
    let minor = format!("{v}").split('.').nth(1).and_then(|d| d.parse().ok()).unwrap_or(0);
    let asked = (v.trunc() as u32, minor);
    if asked.0 != VERSION.0 || asked.1 > VERSION.1 {
        return Err(CompileError::at(n.line, n.head[1].col, format!("this file asks for language {}.{}, and this pleamar understands {}.{}", asked.0, asked.1, VERSION.0, VERSION.1)));
    }
    entries.remove(0);
    Ok(entries)
}

/// Reads a scene from its file, with whatever it imports. Returns the scene and all the
/// files it is made of —to watch them—, or the errors already with their file,
/// their line and their arrow.
pub fn read_file(path: &str) -> Result<(Scene, Vec<PathBuf>), String> {
    read_with(path, Vec::new()).map_err(|(errors, files)| {
        let n = errors.len();
        let text: Vec<String> = errors.iter().map(|f| f.with_source(&files)).collect();
        format!("{}\n{}", text.join("\n\n"), if n == 1 { "one error".to_owned() } else { format!("{n} errors") })
    })
}

/// A diagnostic for whoever is writing, as is: in which file, where, and what is wrong.
/// It is what the language server sends the editor while typing.
pub struct Diagnostic {
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

pub use compiler::Symbol;

/// What is wrong and what there is: both in one pass, which is how the editor wants them.
/// The line of each name already comes without the file number, and with it apart.
pub fn index(path: &str, unsaved: Vec<(PathBuf, String)>) -> (Vec<Diagnostic>, Vec<(PathBuf, Symbol)>) {
    let (r, symbols) = read_with_symbols(path, unsaved, true);
    let files: Vec<PathBuf> = match &r {
        Ok((_, f)) => f.clone(),
        Err((_, f)) => f.iter().map(|(p, _)| p.clone()).collect(),
    };
    let file_of = |line: usize| files.get(line / PER_FILE).cloned().unwrap_or_else(|| PathBuf::from(path));
    let names = symbols
        .into_iter()
        .map(|mut s| {
            let f = file_of(s.line);
            s.line %= PER_FILE;
            (f, s)
        })
        .collect();
    (diagnostics_for(path, r), names)
}

fn diagnostics_for(path: &str, r: Result<(Scene, Vec<PathBuf>), (Vec<CompileError>, Vec<(PathBuf, String)>)>) -> Vec<Diagnostic> {
    match r {
        Ok(_) => Vec::new(),
        Err((errors, files)) => errors
            .into_iter()
            .map(|f| Diagnostic {
                file: files.get(f.line / PER_FILE).map_or_else(|| PathBuf::from(path), |(r, _): &(PathBuf, String)| r.clone()),
                line: f.line % PER_FILE,
                col: f.col,
                message: f.message,
            })
            .collect(),
    }
}

fn read_with(path: &str, unsaved: Vec<(PathBuf, String)>) -> Result<(Scene, Vec<PathBuf>), (Vec<CompileError>, Vec<(PathBuf, String)>)> {
    let (r, _) = read_with_symbols(path, unsaved, false);
    r
}

/// The same, and also the names the scene declares with their location: the editor
/// needs them even when the file is half-written and does not compile.
fn read_with_symbols(path: &str, unsaved: Vec<(PathBuf, String)>, wanted: bool) -> (Result<(Scene, Vec<PathBuf>), (Vec<CompileError>, Vec<(PathBuf, String)>)>, Vec<compiler::Symbol>) {
    let mut l = Reader { unsaved, ..Default::default() };
    let main = Path::new(path).canonicalize().unwrap_or_else(|_| PathBuf::from(path));
    let mut symbols = Vec::new();
    let compiled = (|| {
        l.open_stack.push(main.clone());
        let mut entries = l.open(Path::new(path)).map_err(|f| vec![f])?;
        l.files[0].0 = main.clone();
        l.resolve_includes(&main, &mut entries).map_err(|f| vec![f])?;
        // The `import`s of the parts' files: before the scene's, as if written at its top.
        let hoisted = std::mem::take(&mut l.hoisted);
        entries.splice(0..0, hoisted);
        let mut brought = Vec::new();
        let mut rest = l.resolve_imports(&main, entries, &mut brought).map_err(|f| vec![f])?;
        l.settle_uses().map_err(|f| vec![f])?;
        // What is imported goes before the scene's own, as if it were written there.
        // And the scenes it brings in, after its own (each on surfaces of its own).
        if let [tree::Entry::Node(scene)] = rest.as_mut_slice() {
            if let Some(body) = scene.body.as_mut() {
                brought.append(body);
                brought.append(&mut l.piece_entries);
                *body = brought;
            }
        }
        let names: Vec<String> = l.files.iter().map(|(r, _)| r.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
        let dirs: Vec<PathBuf> = l.files.iter().map(|(r, _)| r.parent().map_or_else(PathBuf::new, Path::to_owned)).collect();
        let (r, s) = compiler::compile(&rest, &names, &dirs, &l.strict, &l.libraries, &l.brought_scenes, wanted);
        symbols = s;
        r
    })();
    // When showing an error, the main file with the path given by whoever opened it.
    if let Some(f) = l.files.first_mut() {
        f.0 = PathBuf::from(path);
    }
    let r = match compiled {
        Ok(e) => Ok((e, l.files.into_iter().map(|(r, _)| r).collect())),
        Err(errors) => Err((errors, l.files)),
    };
    (r, symbols)
}

/// "Did you mean…?": the known name that looks most alike, if it looks alike enough.
pub fn closest_match<'a>(a: &str, known: impl Iterator<Item = &'a String>) -> Option<&'a String> {
    let distance = |x: &str, y: &str| {
        let (x, y): (Vec<char>, Vec<char>) = (x.chars().collect(), y.chars().collect());
        let mut row: Vec<usize> = (0..=y.len()).collect();
        for i in 1..=x.len() {
            let mut prev = row[0];
            row[0] = i;
            for j in 1..=y.len() {
                let saved = row[j];
                row[j] = (prev + (x[i - 1] != y[j - 1]) as usize).min(row[j] + 1).min(row[j - 1] + 1);
                prev = saved;
            }
        }
        row[y.len()]
    };
    known.map(|c| (distance(a, c), c)).filter(|(d, c)| *d <= 1 + c.len() / 4).min_by_key(|(d, _)| *d).map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use crate::scene::{Animated, Behavior, Ctx, Expr, Scene, Spring};
    use crate::shapes::Shape;

    /// The scene's bound properties, computed in their order with these facts and `t` at a half.
    fn settled(scene: &Scene, facts: &[f32]) -> Vec<Animated> {
        let mut props: Vec<Animated> = scene.props.iter().map(|(name, x, _)| Animated::at(if *name == "t" { 0.5 } else { *x }, Spring::LIVELY)).collect();
        for b in &scene.behaviors {
            if let Behavior::Bind { prop, to } = b {
                let v = to.eval(Ctx { props: &props, facts, seen: None });
                props[prop.0 as usize].x = v;
            }
        }
        props
    }

    #[test]
    fn a_page_included_with_while_owns_its_part_of_each_list() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/include-while.plm")).unwrap();
        assert_eq!(scene.pages.len(), 1);
        let p = &scene.pages[0];
        assert_eq!(p.file, "parts/settings-page.plm");
        assert_eq!(p.keep, Some(30.0));
        // Its drawing, its zone, its rule and its `follow`, and nothing of the scene's.
        assert!(!p.instrs.is_empty() && p.instrs.end < scene.instrs.len(), "{:?} of {}", p.instrs, scene.instrs.len());
        let row = scene.zones.iter().find(|z| z.id == "row").expect("its zone");
        let open = scene.zones.iter().find(|z| z.id == "open").expect("the scene's zone");
        // (Not the button just before it, which opens it, nor what comes right after.)
        assert!(p.instrs.contains(&row.at) && !p.instrs.contains(&open.at), "page {:?}, row at {}, open at {}", p.instrs, row.at, open.at);
        assert_eq!(p.rules.len(), 1);
        assert_eq!(p.behaviors.len(), 1);
        // Asleep while `panel` is `none`, there when it is `settings`.
        let mut facts: Vec<f32> = scene.facts.iter().map(|(_, v)| *v).collect();
        let props = settled(&scene, &facts);
        assert_eq!(scene.pages_asleep(Ctx { props: &props, facts: &facts, seen: None }).len(), 1);
        let panel = scene.facts.iter().position(|(n, _)| *n == "panel").unwrap();
        facts[panel] = 1.0;
        assert!(scene.pages_asleep(Ctx { props: &props, facts: &facts, seen: None }).is_empty());
    }

    #[test]
    fn a_scene_brought_in_keeps_its_names_and_surface_apart() {
        let (scene, files) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/brought-scenes.plm")).unwrap();
        assert_eq!(files.len(), 3);
        // Each one on a surface of its own, named as the scene that brings it calls it.
        let surfaces: Vec<&str> = scene.surfaces.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(surfaces, ["", "Bar.panel", "Bar", "Osd"]);
        // The three `volume`s and the three `hit`s, each its own.
        for name in ["volume", "Bar.volume", "Osd.volume"] {
            assert!(scene.facts.iter().any(|(n, _)| *n == name), "no fact {name}");
        }
        assert_eq!(scene.facts.iter().find(|(n, _)| *n == "Bar.volume").unwrap().1, 3.0);
        let zones: Vec<&str> = scene.zones.iter().map(|z| z.id).collect();
        for name in ["hit", "Bar.hit", "Osd.hit"] {
            assert!(zones.contains(&name), "no zone {name}: {zones:?}");
        }
        assert_eq!(scene.rules.len(), 5);
        // Its hover is its own zone's.
        assert!(scene.props.iter().any(|(n, _, _)| *n == "Bar.hit.hover"), "no Bar.hit.hover");
    }

    /// How wide the box of a zone is, with the scene's facts as they start.
    fn zone_width(scene: &Scene, zone: &str) -> f32 {
        let z = scene.zones.iter().find(|z| z.id == zone).unwrap_or_else(|| panic!("no zone {zone}: {:?}", scene.zones.iter().map(|z| z.id).collect::<Vec<_>>()));
        let Shape::Rect { half_size, .. } = &z.shape else { panic!("a box") };
        let facts: Vec<f32> = scene.facts.iter().map(|(_, v)| *v).collect();
        let props = settled(scene, &facts);
        half_size.0.eval(Ctx { props: &props, facts: &facts, seen: None }) * 2.0
    }

    #[test]
    fn the_lets_of_a_scene_brought_in_are_under_its_name() {
        // Issue 149: two scenes that both write `let w`, each keeps its own.
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/brought-lets.plm")).unwrap();
        assert_eq!(zone_width(&scene, "RA.hit"), 20.0);
        assert_eq!(zone_width(&scene, "RB.hit"), 150.0);
    }

    #[test]
    fn a_library_imported_by_a_scene_brought_in_reads_that_scenes_facts() {
        // Issue 149: `let col = rgb(c_r, 0, 0)` in a library reads the `c_r` of the scene that
        // imports it, and a second scene that imports the same library gets its own copy.
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/brought-lets-library.plm")).unwrap();
        for name in ["LibA.c_r", "LibB.c_r"] {
            assert!(scene.facts.iter().any(|(n, _)| *n == name), "no fact {name}");
        }
        assert_eq!(zone_width(&scene, "LibA.hit"), 7.0);
        assert_eq!(zone_width(&scene, "LibB.hit"), 8.0);
    }

    #[test]
    fn a_fold_a_bound_and_an_array_of_a_scene_brought_in_read_its_own_lets() {
        // Issue 149 (Calima's island): a fold reads and leaves its `let` under the scene's name, a `repeat` bounded
        // by a `let` unfolds with the scene's own, and an array reads the `let` of a library the scene imports.
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/brought-lets-fold.plm")).unwrap();
        assert_eq!(zone_width(&scene, "FoldA.hit"), 60.0, "FoldA's fold: four turns of 10 + k * 5");
        assert_eq!(zone_width(&scene, "FoldB.hit"), 21.0, "FoldB's fold: three turns of 7");
        assert_eq!(zone_width(&scene, "FoldA.arr"), 30.0, "FoldA's a[2] is base + 2");
        assert_eq!(zone_width(&scene, "FoldB.arr"), 50.0, "FoldB's a[2] is base * 2 + 2");
        let has = |name: &str| scene.facts.iter().any(|(n, _)| *n == name);
        assert!(has("FoldA.g.1") && !has("FoldA.g.2"), "FoldA unfolds two turns (its n is 2)");
        assert!(has("FoldB.g.2"), "FoldB unfolds three (its n is 3)");
    }

    #[test]
    fn a_scene_whose_surfaces_all_have_a_name_brings_no_surface_of_its_own() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/brought-all-named.plm")).unwrap();
        let surfaces: Vec<&str> = scene.surfaces.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(surfaces, ["Named.panel"]);
        // `wheel` is still the zone called so, and the image reads the text the repeat declared.
        assert!(scene.zones.iter().any(|z| z.id == "wheel"));
        assert!(scene.texts.iter().any(|(n, _)| *n == "Named.ch.0.img"));
    }

    #[test]
    fn each_scene_brought_in_reads_its_own_monitor_text_and_zones() {
        // Issue 144: `screens: mon` in two scenes brought in, each its own `mon`.
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/brought-monitor-text.plm")).unwrap();
        for (surface, text, monitor) in [("A.p", "A.mon", "a-monitor"), ("B.p", "B.mon", "b-monitor")] {
            let s = scene.surfaces.iter().find(|s| s.name == surface).unwrap_or_else(|| panic!("no surface {surface}"));
            let t = s.screens_from.expect("its screens follow a text");
            assert_eq!(scene.texts[t.0 as usize].0, text);
            assert_eq!(s.screens, crate::scene::Screens::Named(vec![monitor.to_owned()]));
        }
        // Issue 143: a zone in a component's children on a `screens: each` surface, one per copy, each with its rules.
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/brought-children-each.plm")).unwrap();
        let (alone, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/brought/children-each.plm")).unwrap();
        assert_eq!(scene.rules.len(), alone.rules.len());
        assert_eq!(scene.zones.len(), alone.zones.len());
        assert!(scene.zones.iter().any(|z| z.id == "P.hit_a#screen1"), "{:?}", scene.zones.iter().map(|z| z.id).collect::<Vec<_>>());
    }

    #[test]
    fn a_zone_named_in_a_repeat_inside_a_for_is_each_rows_own() {
        use crate::scene::Trigger;
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/zone-in-repeat-in-for.plm")).unwrap();
        // Four rows of three ports: twelve zones, and a rule on each (one name
        // for all the rows left twelve rules on the last row's three).
        let pressed: std::collections::HashSet<u32> = scene.rules.iter().filter_map(|r| match &r.when { Trigger::Press(z) => Some(z.0), _ => None }).collect();
        assert_eq!(pressed.len(), 12);
        // …and the row's own rule on `port.0` is on that row's first port.
        let right: std::collections::HashSet<u32> = scene.rules.iter().filter_map(|r| match &r.when { Trigger::PressWith(z, 1) => Some(z.0), _ => None }).collect();
        assert_eq!(right.len(), 4);
        assert!(right.is_subset(&pressed));
    }

    #[test]
    fn mixing_or_choosing_colours_mixes_or_chooses_how_much_of_them_shows() {
        use crate::scene::{Instr, Paint};
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/color-alpha-mix.plm")).unwrap();
        let on = scene.facts.iter().position(|f| f.0 == "on").unwrap();
        let alphas = |v: f32| -> Vec<f32> {
            let mut facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
            facts[on] = v;
            let props = settled(&scene, &facts);
            let c = Ctx { props: &props, facts: &facts, seen: None };
            scene.instrs.iter().filter_map(|i| match i {
                Instr::Fill { paint: Paint::Color(_), alpha, .. } | Instr::Solid { alpha, .. } | Instr::Text { alpha, .. } => Some((alpha.eval(c) * 255.0).round()),
                _ => None,
            }).collect()
        };
        // mix(#…10, #…10) 16 · if(on, #…40, #…80) 128 off, 64 on · the named `veil` (16 + 48) / 2 · two opaque ones mixed: all ·
        // pick(1, …20, …60) 96 · a text a quarter of the way from 0 to 255 · the column's fill chosen 48 / 16 · its text all of it.
        assert_eq!(alphas(0.0), vec![16.0, 128.0, 32.0, 255.0, 96.0, 64.0, 48.0, 255.0]);
        assert_eq!(alphas(1.0), vec![16.0, 64.0, 32.0, 255.0, 96.0, 64.0, 16.0, 255.0]);
    }

    #[test]
    fn a_text_can_take_its_family_from_a_text() {
        use crate::scene::Instr;
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/family-from-a-text.plm")).unwrap();
        let face = scene.texts.iter().position(|t| t.0 == "face").expect("the text face");
        let styles: Vec<(Option<&str>, Option<usize>)> = scene.instrs.iter().filter_map(|i| match i {
            Instr::Text { style, .. } | Instr::Field { style, .. } => Some((style.family, style.family_from.map(|t| t.0 as usize))),
            _ => None,
        }).collect();
        // The text itself follows it; a component given a name in quotes keeps
        // that name, given the text it follows it too; and so does an input.
        assert_eq!(styles, vec![(None, Some(face)), (Some("DejaVu Sans Mono"), None), (None, Some(face)), (None, Some(face))]);
    }

    #[test]
    fn a_surface_can_take_its_monitor_from_a_text() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/screens-from-a-text.plm")).unwrap();
        let main = &scene.surfaces[0];
        assert_eq!(main.screens, crate::scene::Screens::Named(vec!["HDMI-A-1".into()]), "it starts where the text says");
        let t = main.screens_from.expect("it follows the text");
        assert_eq!(scene.texts[t.0 as usize].0, "on_screen");
        // Declared further down, and empty: on no monitor until it says one.
        let panel = scene.surfaces.iter().find(|s| s.name == "panel").unwrap();
        assert_eq!(panel.screens, crate::scene::Screens::Named(Vec::new()));
        assert!(panel.screens_from.is_some());
        // Several, split at commas.
        let both = scene.surfaces.iter().find(|s| s.name == "both").unwrap();
        assert_eq!(both.screens, crate::scene::Screens::Named(vec!["DP-1".into(), "HDMI-A-1".into()]));
    }

    #[test]
    fn a_lock_screen_has_the_keyboard_whatever_the_first_surface_says() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/lock-keyboard-own.plm")).unwrap();
        let lock = scene.surfaces.iter().find(|s| s.lock_screen).unwrap();
        assert_eq!(lock.keyboard, crate::scene::Keyboard::Always);
        assert!(lock.keyboard_when.is_none());
        assert_eq!(scene.surfaces[0].keyboard, crate::scene::Keyboard::Never);
    }

    #[test]
    fn a_list_that_sticks_to_its_end_follows_it_while_it_is_there() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/list-sticks-to-end.plm")).unwrap();
        let pinned = scene.facts.iter().position(|f| f.0 == "list.pinned").expect("list.pinned");
        assert_eq!(scene.facts[pinned].1, 1.0, "it starts at its end");
        // What is inside grows: while pinned, it goes to the end; scrolled up, it stays.
        let follow = scene.rules.iter().find(|r| matches!(r.when, crate::scene::Trigger::Change(_)) && r.guard.is_some()).expect("the rule that follows the end");
        let props: Vec<Animated> = scene.props.iter().map(|(_, x, s)| Animated::at(*x, *s)).collect();
        let mut facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        let guard = follow.guard.as_ref().unwrap();
        assert!(guard.eval(Ctx { props: &props, facts: &facts, seen: None }) > 0.5);
        facts[pinned] = 0.0;
        assert!(guard.eval(Ctx { props: &props, facts: &facts, seen: None }) < 0.5);
        // The wheel and the drag say whether they left it at the end.
        let says = scene.rules.iter().filter(|r| r.effects.iter().any(|e| matches!(e, crate::scene::Effect::Fact(f, _) if f.0 as usize == pinned))).count();
        assert_eq!(says, 2);
    }

    #[test]
    fn how_many_lines_a_text_takes_can_follow_a_fact() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/text-lines-follow.plm")).unwrap();
        let styles: Vec<&crate::scene::Style> = scene.instrs.iter().filter_map(|i| if let crate::scene::Instr::Text { style, .. } = i { Some(&**style) } else { None }).collect();
        let open = scene.facts.iter().position(|f| f.0 == "open").unwrap();
        let mut facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        let lines = styles[0].lines_of.as_ref().expect("worked out at every frame");
        assert_eq!(lines.eval(Ctx { props: &[], facts: &facts, seen: None }), 2.0);
        facts[open] = 1.0;
        assert_eq!(lines.eval(Ctx { props: &[], facts: &facts, seen: None }), 12.0);
        // 0: as many as it has.
        assert!(styles[1].max_lines.is_none() && styles[1].lines_of.is_none());
    }

    #[test]
    fn a_child_placed_goes_to_its_side_of_the_layout() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/place-in-a-column.plm")).unwrap();
        let props: Vec<Animated> = settled(&scene, &scene.facts.iter().map(|f| f.1).collect::<Vec<_>>());
        let facts: Vec<f32> = scene.facts.iter().map(|f| f.1).collect();
        let c = Ctx { props: &props, facts: &facts, seen: None };
        let left = |name: &str| {
            let z = scene.zones.iter().find(|z| z.id == name).unwrap_or_else(|| panic!("{name}"));
            let x: f32 = z.under.iter().map(|t| t.translate.0.eval(c)).sum();
            let Shape::Rect { center, half_size, .. } = &z.shape else { panic!("a box") };
            x + center.0.eval(c) - half_size.0.eval(c)
        };
        // The column is 200 wide: at its start, at its end (200 - 60) and in its middle ((200 - 100) / 2).
        assert_eq!(left("first"), 0.0);
        assert_eq!(left("second"), 140.0);
        assert_eq!(left("third"), 50.0);
    }

    #[test]
    fn where_the_rows_of_a_long_column_fall_is_computed_once() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/long-column.plm")).unwrap();
        let lines: Vec<&crate::scene::Zone> = scene.zones.iter().filter(|z| z.id.starts_with("line#")).collect();
        assert_eq!(lines.len(), 200);
        // What a zone carries of where it is does not grow with the rows before it.
        for z in &lines {
            let nodes: usize = z.under.iter().map(|t| [&t.pivot.0, &t.pivot.1, &t.rotate, &t.scale.0, &t.scale.1, &t.translate.0, &t.translate.1].iter().map(|e| e.node_count()).sum::<usize>()).sum();
            assert!(nodes < 60, "{} carries {nodes} nodes of where it is", z.id);
        }
        // Nor does what it is painted with: the colour is three properties.
        let painted: Vec<usize> = scene.instrs.iter().filter_map(|i| if let crate::scene::Instr::Solid { color, .. } = i { Some(color.iter().map(Expr::node_count).sum()) } else { None }).collect();
        assert_eq!(painted.len(), 200);
        assert!(painted.iter().all(|n| *n == 3), "each box reads its colour from three properties");
        // And each row is where it was: under the ones before it, centred on the fattest one that is there.
        let facts: Vec<f32> = scene.facts.iter().map(|(_, v)| *v).collect();
        let props = settled(&scene, &facts);
        let c = Ctx { props: &props, facts: &facts, seen: None };
        let width = |k: usize| 100.0 + (k % 7) as f32 * 20.0;
        let height = |k: usize| 10.0 + (k % 3) as f32 * 4.0;
        let mut top = 0.0;
        for k in 0..100 {
            let z = lines.iter().find(|z| z.id == format!("line#k{k}")).unwrap();
            let slot = z.under.last().unwrap();
            let (x, y) = (slot.translate.0.eval(c), slot.translate.1.eval(c));
            assert!((y - top).abs() < 1e-2 && (x - (220.0 - width(k)) * 0.5).abs() < 1e-2, "row {k} is at {x}, {y} and it had to be at {}, {top}", (220.0 - width(k)) * 0.5);
            top += height(k) + 2.0;
        }
    }

    #[test]
    fn zones_in_a_component_are_each_copys() {
        // Two copies of a component, each with three zones in a `repeat` (`opt.$i`) and a rule
        // on each: six zones and six rules on six different zones, not three shared (B12).
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/zones-per-copy.plm")).unwrap();
        let zones: Vec<&str> = scene.zones.iter().map(|z| z.id).filter(|id| id.starts_with("opt.")).collect();
        assert_eq!(zones.len(), 6, "{zones:?}");
        let mut pressed: Vec<u32> = scene.rules.iter().filter_map(|r| match r.when { crate::scene::Trigger::Press(z) => Some(z.0), _ => None }).collect();
        assert_eq!(pressed.len(), 6, "a rule a zone");
        pressed.sort_unstable();
        pressed.dedup();
        assert_eq!(pressed.len(), 6, "each copy's rule on its own zone");
        // And each zone has its own springs (`opt.$i.hover`), named as the zone is.
        for z in &zones {
            assert!(scene.props.iter().any(|p| p.0 == format!("{z}.hover")), "{z}.hover");
        }
    }

    #[test]
    fn a_parameter_called_time_does_not_wake_the_clock() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/time-parameter.plm")).unwrap();
        assert!(!scene.props.iter().any(|p| p.0 == "time"), "the scene's clock was not asked for");
    }

    #[test]
    fn a_measure_in_a_component_is_each_copys() {
        // Two copies of a component, each measuring two words in a `repeat` (`measure cell.$i`):
        // four measures, not two shared by both (M20: a table's chips all as long as the last row's word).
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/measure-per-copy.plm")).unwrap();
        let widths: Vec<_> = scene.instrs.iter().filter_map(|i| match i {
            crate::scene::Instr::Text { measure: Some((w, _)), .. } => Some(*w),
            _ => None,
        }).collect();
        assert_eq!(widths.len(), 4, "four texts measured");
        let mut unique = widths.clone();
        unique.sort_by_key(|p| p.0);
        unique.dedup();
        assert_eq!(unique.len(), 4, "each copy measures its own: {widths:?}");
    }

    #[test]
    fn a_chain_of_lets_from_the_screen_is_computed_once_per_copy() {
        let (scene, _) = super::read_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/chained-lets-per-screen.plm")).unwrap();
        // Each link names the previous one twice: substituted, the last one was
        // hundreds of nodes, and here nothing is more than its own line.
        let widths: Vec<(&str, &Expr)> = scene
            .zones
            .iter()
            .map(|z| match &z.shape {
                Shape::Rect { half_size, .. } => (z.id, &half_size.0),
                _ => panic!("the zone is a box"),
            })
            .collect();
        assert_eq!(widths.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ["hit#screen0", "hit#screen1"], "one zone per monitor's copy");
        for (id, w) in &widths {
            assert!(w.node_count() <= 3, "{id} reads its width from a property, not from the whole chain: {w:?}");
        }
        for b in &scene.behaviors {
            if let Behavior::Bind { to, .. } = b {
                assert!(to.node_count() < 30, "a link reads the one before it from its property: {to:?}");
            }
        }
        // And each copy's is its own: a monitor half as tall has the chain half as big.
        let mut facts: Vec<f32> = scene.facts.iter().map(|(_, v)| *v).collect();
        let fact = |name: &str| scene.facts.iter().position(|(n, _)| *n == name).unwrap_or_else(|| panic!("no fact {name}"));
        facts[fact("screen.0.height")] = 1350.0;
        facts[fact("screen.1.height")] = 675.0;
        let props = settled(&scene, &facts);
        let expected = |height: f32| {
            let t = 0.5;
            let sc = (height / 1350.0).clamp(0.55, 1.0) * (1.0 + t * 0.2);
            let mut x = 10.0 * sc + (40.0 * sc - 10.0 * sc) * t;
            for k in [60.0, 80.0, 100.0, 120.0, 140.0] {
                x += (k * sc - x) * t;
            }
            x
        };
        for (id, w) in &widths {
            let height = if id.ends_with("#screen1") { 675.0 } else { 1350.0 };
            let got = w.eval(Ctx { props: &props, facts: &facts, seen: None }) * 2.0;
            assert!((got - expected(height)).abs() < 1e-2, "{id}: {got} and it had to be {}", expected(height));
        }
    }
}
