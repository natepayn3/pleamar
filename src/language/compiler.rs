//! From the tree to the scene: what each node means, and whether the names exist.
//!
//! It is read in four passes, so that the order in which it is written is the one
//! that suits whoever reads it and not the program: first what is declared; then the
//! names (`let`) and the layers; then the drawing, and at the end the rules, which by then
//! can name any shape. Only a `let` has to come before whoever uses it.

use super::tree::{Entry, Node};
use super::tokens::{Token, TokenKind};
use super::vocabulary as vocab;
use super::{closest_match, CompileError};
use crate::scene::*;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

type R<T> = Result<T, CompileError>;

fn interned(s: &str) -> &'static str {
    intern(s)
}

/// A cursor over the tokens of a header or of a value.
struct Cur<'a> {
    tokens: &'a [Token],
    i: usize,
    end: (usize, usize),
    /// A property's value (`properties`): read, it has to be read to its end.
    slot: bool,
}

thread_local! {
    // Where the first property value read but not to its end was left: the
    // statement it belongs to says so once it has been read (issue #58).
    static LEFTOVER: std::cell::RefCell<Vec<(usize, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
    // Those already said: a component or a monitor's copy reads the same value again.
    static LEFTOVER_SAID: std::cell::RefCell<HashSet<(usize, usize)>> = std::cell::RefCell::new(HashSet::new());
}

/// A value read and then let go with something after it would drop that
/// something without a word: `width: 200 200` was a 200. Every slot is
/// checked here, whoever read it, instead of at each of the reads.
impl Drop for Cur<'_> {
    fn drop(&mut self) {
        if self.slot && self.i > 0 && self.i < self.tokens.len() {
            let t = &self.tokens[self.i];
            LEFTOVER.with(|l| l.borrow_mut().push((t.line, t.col)));
        }
    }
}

/// Where a `while` condition ends in a property that can hold several of them
/// (`rate: 60, 20 while a, 5 while b`): at the next comma outside any brackets,
/// so that a comma inside a call —`while mix(a, b) > 0.5`— is not one.
fn condition_end(tokens: &[Token], from: usize) -> usize {
    let mut depth = 0i32;
    for (k, t) in tokens.iter().enumerate().skip(from) {
        if let TokenKind::Sym(x) = &t.kind {
            match *x {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                "," if depth == 0 => return k,
                _ => {}
            }
        }
    }
    tokens.len()
}

impl<'a> Cur<'a> {
    fn new(tokens: &'a [Token], line: usize, col: usize) -> Self {
        let end = tokens.last().map_or((line, col), |u| (u.line, u.col + 1));
        Cur { tokens, i: 0, end, slot: false }
    }
    fn peek(&self) -> Option<&'a TokenKind> {
        // The last thing peeked at is almost always the name about to be resolved: that way
        // whoever resolves names knows where it is without every call having to say so.
        if let Some(f) = self.tokens.get(self.i) {
            PEEKED.with(|m| m.set((f.line, f.col)));
        }
        self.tokens.get(self.i).map(|x| &x.kind)
    }
    /// A name of the kind that gets resolved has just been read: it is what the editor
    /// underlines when asked where something is used. It is noted before advancing.
    fn mark_name(&self) {
        NAMED.with(|n| n.set(PEEKED.with(std::cell::Cell::get)));
    }
    fn pos(&self) -> (usize, usize) {
        self.tokens.get(self.i).map_or(self.end, |x| (x.line, x.col))
    }
    fn error<T>(&self, m: impl Into<String>) -> R<T> {
        let (l, c) = self.pos();
        Err(CompileError::at(l, c, m))
    }
    fn at_end(&self) -> bool {
        self.i >= self.tokens.len()
    }
    fn sym(&mut self, s: &str) -> bool {
        let hit = matches!(self.peek(), Some(TokenKind::Sym(x)) if *x == s);
        self.i += hit as usize;
        hit
    }
    fn word(&mut self, p: &str) -> bool {
        let hit = matches!(self.peek(), Some(TokenKind::Id(x)) if x == p);
        self.i += hit as usize;
        hit
    }
    /// A button's word before a zone (`on press back hit`): only when a name
    /// follows it, so that a zone called `back`, `right`… (`on press back { … }`)
    /// is still that zone.
    fn button(&mut self, p: &str) -> bool {
        let named_after = matches!(self.tokens.get(self.i + 1).map(|t| &t.kind), Some(TokenKind::Id(x)) if x != "while");
        named_after && self.word(p)
    }
    fn expect_sym(&mut self, s: &str) -> R<()> {
        if self.sym(s) { Ok(()) } else { self.error(format!("expected '{s}' here")) }
    }
    fn expect_word(&mut self, p: &str) -> R<()> {
        if self.word(p) { Ok(()) } else { self.error(format!("expected '{p}' here")) }
    }
    fn id(&mut self, what: &str) -> R<String> {
        match self.peek() {
            Some(TokenKind::Id(x)) => {
                // `peek` has just left this name there: it is saved before advancing.
                NAMED.with(|n| n.set(PEEKED.with(std::cell::Cell::get)));
                self.i += 1;
                Ok(x.clone())
            }
            _ => self.error(format!("expected {what} here")),
        }
    }
    fn num(&mut self) -> R<f32> {
        let negative = self.sym("-");
        match self.peek() {
            Some(TokenKind::Num(n) | TokenKind::Angle(n)) => {
                let n = if negative { -n } else { *n };
                self.i += 1;
                // A slot that takes a plain number used to read the first one and
                // drop the rest without a word: `1 * rung` was a 1 (issue #55).
                if matches!(self.peek(), Some(TokenKind::Sym("*" | "/" | "+" | "-" | "%"))) {
                    return self.error("only a plain number goes here: what follows it would be dropped");
                }
                Ok(n)
            }
            _ => self.error("expected a number here"),
        }
    }
    fn dur(&mut self) -> R<Duration> {
        match self.peek() {
            Some(TokenKind::Dur(s)) => {
                self.i += 1;
                Ok(Duration::from_secs_f32(*s))
            }
            _ => self.error("expected a duration here, like 320ms or 14s"),
        }
    }
    fn string(&mut self) -> R<String> {
        match self.peek() {
            Some(TokenKind::Str(s)) => {
                self.i += 1;
                Ok(s.clone())
            }
            _ => self.error("expected a quoted string here"),
        }
    }
    /// A word from a vocabulary list. If it is none of them, it says which ones are valid.
    fn one_of(&mut self, list: &[&str], what: &str) -> R<String> {
        let word = self.id(what)?;
        if list.contains(&word.as_str()) {
            return Ok(word);
        }
        self.i -= 1;
        let all: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        let hint = closest_match(&word, all.iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
        self.error(format!("'{word}' is not valid here: {what} is {}.{hint}", join_or(list)))
    }
    fn expect_end(&self) -> R<()> {
        if self.at_end() { Ok(()) } else { self.error("this is left over here") }
    }
}

thread_local! {
    // Where the last token peeked at is: (line, column).
    static PEEKED: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
    // And where the last NAME that was read was, which is not the same: by the time it
    // is resolved the next token has already been peeked at. It is what the editor underlines.
    static NAMED: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// A shape with a name: it will be a zone if some rule names it, if it was declared
/// with `zone` or if it carries `active`.
struct Candidate {
    name: String,
    /// Where it was declared among the instructions, and in which group with `z:`.
    at: usize,
    zblock: Option<u16>,
    shape: Shape,
    active: Option<Expr>,
    /// If what contains it is not there (`show:`, a record that does not exist), neither is it.
    visible: Option<Expr>,
    under: Vec<Transform>,
    forced: bool,
    cursor: Cursor,
    /// `cursor:` with something that changes: worked out every frame instead.
    cursor_when: Option<Expr>,
    /// What it carries when dragged out to another program (`carries:`).
    carries: Option<Content>,
    /// `carry_icon:`: the picture under the pointer while it is dragged out.
    carry_icon: Option<Content>,
    /// `label:` and `agent:`: what it is, said in words, and who may use it.
    label: Option<Content>,
    reach: Reach,
    /// The stack with `view:` it scrolls inside, if any: the zone of a row
    /// that can be scrolled out of sight.
    within: Option<String>,
    /// `role:`, `value:`, `checked:`, `selected:`.
    told: Option<Box<Told>>,
    /// `handle:`: the window it carries, and by which edges.
    handle: Option<Box<crate::scene::Handle>>,
    /// `yields:`: it only counts a press that nothing below it takes (see `Zone::yields`).
    yields: Option<Expr>,
}

/// What holds inside a component or an iteration of `repeat`: its
/// parameters, and the names it declares, which outside go by another name
/// so that two copies do not step on each other.
#[derive(Clone, Default)]
struct Scope {
    exprs: HashMap<String, Expr>,
    colors: HashMap<String, Color>,
    strings: HashMap<String, String>,
    /// The strings with holes, already resolved where they were written: `Chip("{n.app}")`.
    contents: HashMap<String, Content>,
    alias: HashMap<String, String>,
    /// Of those names, the ones that have parts: `label.width` belongs to the measure `label`.
    with_parts: std::collections::HashSet<String>,
    suffix: String,
    /// Inside a `for`: this iteration only exists if its record exists.
    visible: Option<Expr>,
    /// If it is the copy of a component: which one, and on which line it was placed.
    instance: Option<(String, usize)>,
    /// The springs that arrived as a parameter: `~bounce`.
    springs: HashMap<String, Spring>,
    /// Mark of a child that comes from outside the component (`children`): it is read with
    /// the names of whoever wrote it, not with the ones inside.
    outer_scope: Option<Vec<Rc<Scope>>>,
    /// The copy of a library component: from which file.
    library: Option<usize>,
    /// From a `strict` library: in here only what is asked for, what is
    /// declared and what belongs to the library itself are valid.
    strict: bool,
    /// Its `let`s written outside any group, and on which line.
    loose: HashMap<String, usize>,
    /// The values of the scene's folds a `repeat` keeps (see `fold`): not a turn's own `let`s.
    folds: bool,
}

struct Component<'a> {
    /// From a `strict` library.
    strict: bool,
    /// The slots for children it has: `children` ("") and `children header`.
    slots: Vec<String>,
    params: Vec<Param>,
    node: &'a Node,
}

/// An imported library: which file it comes from, what it is called, and its logic if it has any.
pub struct Library {
    pub file: usize,
    pub name: String,
    pub logic: Option<std::path::PathBuf>,
}

/// What a copy brings for the slots of its component, and the scope of whoever wrote it.
struct InstanceChildren<'a> {
    outer: Vec<Rc<Scope>>,
    /// Per slot ("" is the one with no name): what goes inside, and whether it has already been placed.
    slots: Vec<(String, Vec<&'a Entry>, bool)>,
}

/// What a component asks of whoever uses it. With a type, whoever uses it finds out on
/// the spot what is missing or what is left over; without one, it is guessed from what is passed.
#[derive(Clone)]
struct Param {
    name: String,
    kind: Option<String>,
    /// What it is worth if it is not passed: as it was written, to be read where it is used.
    fallback: Option<Vec<Token>>,
}

struct Compiler<'a> {
    e: Scene,
    /// Where each fact's type is in `e.types`, by its name: the first one, as a search
    /// from the start finds it. Brought up to date as it is asked (`e.types` only grows).
    /// Every bare name in a sum or a comparison asks whether it is an enum, and searching
    /// the list each time was a third of the compilation of a big scene.
    type_index: std::cell::RefCell<(usize, HashMap<String, usize>)>,
    /// Synthesized syntax must survive the deferred passes, but not the load.
    generated: &'a typed_arena::Arena<Node>,
    /// Stacks that scroll: their zone, their property, how far, how much per notch and with which spring.
    scrolls: Vec<(String, PropId, Expr, Expr, Spring, FactId, Option<FactId>)>,
    /// Of the stacks that scroll, which ones are rows: they are dragged sideways.
    row_scrolls: std::collections::HashSet<String>,
    /// Surfaces whose `open:` is resolved at the end: (which one, the fact, where it is written).
    pending_surfaces: Vec<(usize, &'a [Token], (usize, usize))>,
    /// The zones the scene names with `.hover` or `.pressed`, as written
    /// (`tile.$t`), and the springs made for them, to tie to their zones at the end.
    hover_mentions: std::collections::HashSet<String>,
    zone_springs: Vec<(String, PropId, PropId)>,
    /// `level: top, overlay while …`: read at the end, when every fact is known.
    pending_levels: Vec<(usize, Level, &'a [Token], (usize, usize))>,
    /// `reserve: n while expr`: read at the end, like the levels.
    pending_reserves: Vec<(usize, &'a [Token], (usize, usize))>,
    /// Each surface's own `keyboard: … while` condition, read at the end.
    pending_keyboards: Vec<(usize, &'a [Token], (usize, usize))>,
    /// `rate: 60, 20 while …`: the surface, which step it is, the lower rate and
    /// its condition. The step is kept because a copy of the surface reads the same
    /// value again, and a list must not grow twice over.
    pending_rates: Vec<(usize, usize, u32, &'a [Token], (usize, usize))>,
    /// `anchor: corner`, with `corner` a fact: which surface, which name and where.
    pending_anchors: Vec<(usize, String, (usize, usize))>,
    /// `screens: on_screen`: a text fact, looked for at the end like an anchor's.
    pending_screens: Vec<(usize, String, (usize, usize))>,
    /// The names of the files it is made of, to say where something is.
    files: &'a [String],
    /// Which files, by their number, are `strict` libraries.
    strict_files: &'a [usize],
    libraries: &'a [Library],
    /// The folder of each file: a relative path is relative to the file that writes it.
    dirs: &'a [std::path::PathBuf],
    /// The boundary each library declares, by the number of its file: what it is
    /// called inside (`now`) and what it is really called (`Clock.now`).
    boundary_of: HashMap<usize, HashMap<String, String>>,
    /// And the permissions it asks for its logic.
    permissions_of: HashMap<usize, Permissions>,
    /// Which pass the reading is on: `surface` reads its properties in 0 and its drawing in 2.
    pass: u8,
    /// Each surface and each popup look at a different piece of the same plane.
    next_origin: f32,
    /// The names of the enum values, as numbers: `critical` is 2.
    values: HashMap<String, f32>,
    /// The ones that are in two enums with different numbers: on their own they say nothing.
    ambiguous: std::collections::HashSet<String>,
    /// What each copy in progress brings for the `children` of its component: the
    /// nodes, the scope of whoever wrote them, and whether they have already been placed.
    instance_children: Vec<InstanceChildren<'a>>,
    /// What the libraries declare: a `strict` component can read it.
    from_library: std::collections::HashSet<String>,
    /// What a `strict` component has read from the scene without asking for it: (component, name).
    unrequested: std::cell::RefCell<Vec<(String, String, (usize, usize))>>,
    unwatched: std::cell::Cell<bool>,
    /// The lines of `let`s already said to hide a fact or a prop (once, not once per monitor).
    hiding_noted: std::cell::RefCell<std::collections::HashSet<usize>>,
    /// Where a function was given more than it takes, already said.
    extra_noted: std::cell::RefCell<std::collections::HashSet<(usize, usize)>>,
    /// Reading a text's `letter_*` property: `letter` and `letters` mean something.
    in_letters: std::cell::Cell<bool>,
    /// Reading a `cursor:`: the cursors' names are their codes.
    in_cursor: std::cell::Cell<bool>,
    /// `translations`: each language with its table, original → translated. And, per
    /// language, the texts found without a translation, to say so once at the end.
    translations: Vec<(String, HashMap<String, String>)>,
    untranslated: std::cell::RefCell<Vec<std::collections::BTreeSet<String>>>,
    props: HashMap<String, PropId>,
    facts: HashMap<String, FactId>,
    signals: HashMap<String, SignalId>,
    texts: HashMap<String, TextId>,
    images: HashMap<String, ImageId>,
    /// The figures: an SVG read as geometry, by layers.
    figures: HashMap<String, super::figure::Figure>,
    /// The scene's own shaders, by name: their number in `Scene::shaders`.
    shaders: HashMap<String, u16>,
    models: HashMap<String, usize>,
    measurements: HashMap<String, (PropId, PropId)>,
    gestures: HashMap<String, GestureId>,
    zones: HashMap<String, ZoneId>,
    lets: HashMap<String, Expr>,
    /// On which line each loose `let` was declared: so as not to let another one, further
    /// down, change its meaning without saying anything.
    let_lines: HashMap<String, usize>,
    /// The same, for the scene's own `let`s also when read once per monitor's
    /// copy: where each was seen last, to tell two files that both write one.
    scene_let_lines: std::cell::RefCell<HashMap<String, usize>>,
    /// The names of `let`s already said to be written in two files (once, not once per monitor).
    twice_noted: std::cell::RefCell<std::collections::HashSet<String>>,
    /// The scene's `let`s written outside any group, and on which line (inside
    /// a scope, its own `loose`).
    loose_lets: HashMap<String, usize>,
    /// The groups being read: how many scopes were open when each began.
    groups: Vec<usize>,
    colors: HashMap<String, Color>,
    springs: HashMap<String, Spring>,
    candidates: Vec<Candidate>,
    /// The group or copy whose children are sorted by `z:`, and the group
    /// with `z:` being read (one inside another is not sorted).
    zparents: Vec<u32>,
    next_zparent: u32,
    zblock: Option<u16>,
    /// The rules are left for the end: that way they can name shapes that are
    /// painted further down.
    rules: Vec<(&'a Node, Vec<Rc<Scope>>)>,
    errors: Vec<CompileError>,
    /// Each name that is declared, with its location: for the editor.
    declared: Vec<Symbol>,
    /// And every time something is named, for "where is it used" and for renaming it.
    /// As names are resolved without being able to write (`&self`), it goes in a cell.
    used: std::cell::RefCell<Vec<Symbol>>,
    /// The statement being read (`fact`, `prop`…), to know what class
    /// whatever is declared inside it belongs to.
    current_class: String,
    scopes: Vec<Rc<Scope>>,
    components: HashMap<String, Component<'a>>,
    copies: usize,
    /// The property each big `let` is computed in (see `let`), by what it
    /// computes: the same `let` read again in another monitor's copy is the
    /// same property, not one more. Only the first copy's are kept: those are
    /// the ones the render always moves (a copy whose monitor is not there is not).
    let_props: HashMap<String, PropId>,
    /// The properties of the loose `let`s that are each monitor's copy's own:
    /// the one the scene reads, which has one more per copy. A `let` that
    /// reads one of these is each copy's own too.
    copy_props: std::collections::HashSet<u16>,
    /// Reading a monitor's copy other than the first.
    later_copy: bool,
    /// How many groups with effects are open around what is being read.
    effects_depth: usize,
    /// Inside a `row` or a `column`, a child does not say where it goes: it goes to its slot.
    in_slot: bool,
    /// How much room the last thing painted took, for whoever is sharing out slots.
    last_size: Option<(Expr, Expr)>,
    /// The measure a layout imposes on the text it is going to paint.
    imposed_measure: Option<(PropId, PropId)>,
    pending_keyboard: Option<(&'a [Token], (usize, usize))>,
    /// Where each property was declared: the same line read twice is the same
    /// declaration (a `repeat` inside a surface per monitor); another line with
    /// the same name is a mistake.
    prop_sites: HashMap<String, (usize, usize)>,
    /// The transforms under which painting is happening: a zone inherits them.
    under: Vec<Transform>,
    /// Where each image was declared, and the image: read again from there, it is the same.
    image_sites: HashMap<String, ((usize, usize), ImageId)>,
    /// `let a[16]`: each array's length and the line it was declared on.
    arrays: HashMap<String, (usize, usize)>,
    /// The elements already defined (`a.3`), and what each first copy computes:
    /// a monitor's copy that computes something else has one of its own.
    elements: HashMap<String, String>,
    /// The `repeat`s at the scene's level whose definitions were read in pass 1,
    /// and how deep inside one of them the drawing pass is.
    hoisted: HashSet<*const Node>,
    in_hoisted: usize,
    /// Reading those definitions: for the scene (None) or for a monitor's copy.
    hoisting: Option<Option<usize>>,
    /// The scene's statements, as written.
    top: &'a [Entry],
    /// The arrays whose elements are arrays (`let dka[4][12]`).
    rows: HashSet<String>,
    /// The scope the folds of the `repeat` being hoisted are kept in, and
    /// each turn's, by `repeat`, copy and turn, for its drawing and rules.
    fold_scope: Option<usize>,
    turns: HashMap<(*const Node, Option<usize>, i64), Rc<Scope>>,
    /// What each fold's last link computes for the scene.
    folds: HashMap<String, String>,
}

/// A name the scene declares, with its location: what the editor needs to
/// complete, to go to where it was born and to show the outline of the file.
#[derive(Clone, Debug)]
pub struct Symbol {
    /// As it was written: `hit`, without the scope suffix.
    pub local: String,
    /// With which statement it was declared: `fact`, `prop`, `component`…
    pub class: String,
    pub line: usize,
    pub col: usize,
}

pub fn compile<'a>(tree: &'a [Entry], files: &'a [String], dirs: &'a [std::path::PathBuf], strict_files: &'a [usize], libraries: &'a [Library]) -> (Result<Scene, Vec<CompileError>>, Vec<Symbol>) {
    // Nothing left over from a compilation before this one (a reload runs on the same thread).
    LEFTOVER.with(|l| l.borrow_mut().clear());
    LEFTOVER_SAID.with(|s| s.borrow_mut().clear());
    let generated = typed_arena::Arena::new();
    let scene = match tree {
        [Entry::Node(n)] if matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "scene") => n,
        [Entry::Node(n)] if matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "library") => {
            return (Err(vec![CompileError::at(n.line, n.col, "this is a library: it is not opened, it is imported from a scene (`import \"…\"`)")]), Vec::new());
        }
        [Entry::Node(n)] if matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "part") => {
            return (Err(vec![CompileError::at(n.line, n.col, "this is a part of a scene: it is not opened, it is included from the scene (`include \"…\"`)")]), Vec::new());
        }
        _ => return (Err(vec![CompileError::at(1, 1, "a file is a scene: as many `import`s as you want, then `scene Name { … }`")]), Vec::new()),
    };
    let mut o = Compiler {
        e: Scene::default(),
        generated: &generated,
        translations: Vec::new(), untranslated: Default::default(),
        props: HashMap::new(), facts: HashMap::new(), signals: HashMap::new(), texts: HashMap::new(), images: HashMap::new(), figures: HashMap::new(), shaders: HashMap::new(), models: HashMap::new(),
        measurements: HashMap::new(), gestures: HashMap::new(), zones: HashMap::new(), lets: HashMap::new(), let_lines: HashMap::new(), scene_let_lines: Default::default(), twice_noted: Default::default(), loose_lets: HashMap::new(), groups: Vec::new(), colors: HashMap::new(),
        springs: vocab::SPRINGS.iter().map(|n| ((*n).to_owned(), match *n {
            "lively" => Spring::LIVELY,
            "calm" => Spring::CALM,
            "quick" => Spring::QUICK,
            "slow" => Spring::SLOW,
            "gentle" => Spring::GENTLE,
            "pose" => Spring::POSE,
            other => unreachable!("'{other}' is in the vocabulary, but it has no stiffness or damping"),
        })).collect(),
        under: Vec::new(), candidates: Vec::new(), zparents: vec![0], next_zparent: 1, zblock: None, rules: Vec::new(), errors: Vec::new(), declared: Vec::new(), used: Default::default(), current_class: String::new(),
        scrolls: Vec::new(), row_scrolls: Default::default(), pending_surfaces: Vec::new(), pending_levels: Vec::new(), pending_reserves: Vec::new(), pending_keyboards: Vec::new(), pending_rates: Vec::new(), hover_mentions: Default::default(), zone_springs: Vec::new(), pending_anchors: Vec::new(), pending_screens: Vec::new(), files, dirs, strict_files, libraries, boundary_of: HashMap::new(), permissions_of: HashMap::new(), pass: 0, next_origin: 0.0, values: HashMap::new(), ambiguous: Default::default(), instance_children: Vec::new(), from_library: Default::default(), unrequested: Default::default(), unwatched: Default::default(), hiding_noted: Default::default(), extra_noted: Default::default(), in_letters: Default::default(), in_cursor: Default::default(), scopes: Vec::new(), components: HashMap::new(), copies: 0, let_props: HashMap::new(), copy_props: Default::default(), later_copy: false, effects_depth: 0, in_slot: false, last_size: None, imposed_measure: None, pending_keyboard: None, prop_sites: HashMap::new(), arrays: HashMap::new(), elements: HashMap::new(), hoisted: HashSet::new(), in_hoisted: 0, hoisting: None, top: &[], rows: HashSet::new(), fold_scope: None, turns: HashMap::new(), folds: HashMap::new(), image_sites: HashMap::new(), type_index: Default::default(),
    };
    // Two facts that always exist: what the surface really measures. The
    // render sets them when the compositor configures it.
    // …and what a rule can read from the mouse while it fires.
    // `cursor.x`, `cursor.y`: the mouse wherever it is, even far from the scene.
    // `motion.reduced` (1: the system asks for less motion) and `color.scheme`
    // (0 no preference, 1 dark, 2 light): what the system says, or what is said
    // to the scene; the first, with `--reduced-motion`, settles every spring.
    for n in ["screen.width", "screen.height", "screen.index", "pointer.x", "pointer.y", "cursor.x", "cursor.y", "local.x", "local.y", "drag.dx", "drag.dy", "wheel", "lock.held", "drag.over", "motion.reduced", "color.scheme"] {
        let h = o.e.fact(n, 0.0);
        o.facts.insert(n.into(), h);
    }
    // A signal that always exists: `--demo` fires it, for scenes without a mouse.
    let demo = o.e.signal("demo");
    o.signals.insert("demo".into(), demo);
    let Some(body) = scene.body.as_ref() else {
        return (Err(vec![CompileError::at(scene.line, scene.col, "this scene is missing its `{ … }` block")]), Vec::new());
    };
    o.top = body;
    o.declare_measures_early(body);
    // `time`: the seconds since the scene started, for whoever wants something
    // that never stops —a shader's aurora, a `noise(time)` wobble—. Only if the
    // scene names it and has not declared a `time` of its own: a hand that goes
    // round keeps the frames coming, and a scene that does not ask for it must
    // be able to sleep. Like `spin`, it stops with reduced motion.
    if mentions(body, "time") && !declares(body, "time") {
        o.time_prop();
        // Named: it goes whether or not anything drawn reads it.
        o.e.shader_clock = None;
    }
    o.e.wants_cursor = mentions(body, "cursor") || mentions_part(body, "cursor");
    hover_mentions(body, &mut o.hover_mentions);
    // The translations first of all: the texts are read already knowing them,
    // wherever the block is —at the end, or in an imported library—.
    o.read_translations(body);
    // Four passes: declarations; names and layers; drawing; rules.
    for pass in 0..3 {
        o.pass = pass;
        // The pages' facts, wherever they are written: before anything else,
        // so that anything can read them and set them (`settings = look`) and
        // their pages' names are taken before other declarations use them.
        if pass == 0 {
            o.declare_pages_early(body);
        }
        // The surfaces are already known: if any is repeated per monitor, its named
        // stacks have one measure per copy (`desks#screen1.width`).
        if pass == 1 {
            let instances: Vec<usize> = o.e.surfaces.iter().filter(|s| matches!(s.screens, Screens::Number(_))).map(|s| s.instance).collect();
            for k in instances {
                o.scopes.push(Rc::new(o.screen_scope(k)));
                o.declare_measures_early(body);
                o.scopes.pop();
            }
        }
        // A `surface` is read twice: in 0 its properties, and in 2 what it draws.
        let this_pass: Vec<&Entry> = body.iter().filter(|e| pass_of(e) == pass || (pass == 0 && is_repeat(e)) || (pass == 2 && (is_surface(e) || hoists(e)))).collect();
        // Loose drawing belongs to the scene's surface. If that one is repeated per monitor
        // (`screens: each`), it is repeated with it: one copy per screen, with its own things.
        let copies: Vec<(f32, f32, usize)> = if pass == 2 {
            o.e.surfaces.iter().filter(|s| s.name.is_empty() && matches!(s.screens, Screens::Number(_))).map(|s| (s.origin.0, s.origin.1, s.instance)).collect()
        } else {
            Vec::new()
        };
        if copies.is_empty() {
            o.group(this_pass.into_iter());
            continue;
        }
        // The named surfaces are not the scene surface's: each one has its own
        // place in the plane and its own copies per monitor. Read inside each
        // copy of the scene's, what they draw was moved by that copy's place —out
        // of their own window— and skipped when that copy was closed: a named
        // surface next to a scene surface with `screens: each` drew nothing, or
        // only on the first monitor, whose place is zero.
        let (named, this_pass): (Vec<&Entry>, Vec<&Entry>) = this_pass.into_iter().partition(|e| is_surface(e) && matches!(e, Entry::Node(n) if n.head.len() > 1));
        o.group(named.into_iter());
        for (ox, oy, k) in copies {
            let mark = o.rules.len();
            let (i0, c0) = (o.e.instrs.len(), o.e.behaviors.len());
            let mut scope = o.screen_scope(k);
            // What this copy draws is moved to its place in the plane; the mouse a
            // rule reads is said the way the copy draws, over its own monitor.
            for (n, off) in [("pointer.x", ox), ("pointer.y", oy)] {
                if off != 0.0 {
                    scope.exprs.insert(n.to_owned(), Expr::Sub(Box::new(o.facts[n].e()), Box::new(Expr::K(off))));
                }
            }
            o.scopes.push(Rc::new(scope));
            // Each copy's groups with `z:` are sorted among themselves.
            o.zparents.push(o.next_zparent);
            o.next_zparent += 1;
            let t = Transform { translate: (ox.into(), oy.into()), ..Transform::at((0.0.into(), 0.0.into())) };
            o.e.paint(Instr::Transform(Some(t.clone())));
            o.under.push(t);
            o.later_copy = o.e.surfaces.get(k).is_some_and(|s| s.instance > 0);
            o.group(this_pass.clone().into_iter());
            o.later_copy = false;
            o.zparents.pop();
            o.under.pop();
            o.e.paint(Instr::Transform(None));
            o.close_scope(mark);
            // What belongs to this copy, so that the render can skip it entirely if
            // its surface is closed. The rules are read at the end, in the
            // same order in which they were noted: their span is that of `o.rules`.
            // The rules are compiled afterwards, all in a row and in this same order:
            // here it is noted how many NODES are its own, and below it is translated into compiled ones.
            o.e.spans.push(crate::scene::Span { surface: k, instrs: i0..o.e.instrs.len(), behaviors: c0..o.e.behaviors.len(), rules: mark..o.rules.len() });
        }
    }
    let mut arrays: Vec<(usize, String, usize)> = o.arrays.iter().map(|(name, (length, line))| (*line, name.clone(), *length)).collect();
    arrays.sort();
    for (line, name, length) in arrays.into_iter().filter(|(_, name, _)| !o.rows.contains(name)) {
        let missing: Vec<usize> = (0..length).filter(|k| !o.elements.contains_key(&format!("{name}.{k}"))).collect();
        if let Some(k) = missing.first() {
            let more = if missing.len() > 1 { format!(" (nor {} more)", missing.len() - 1) } else { String::new() };
            // A row of a table is said as the table: `at[0][1]`.
            let (name, i) = match name.rsplit_once('.').filter(|(row, _)| o.rows.contains(*row)) {
                Some((table, row)) => (table.to_owned(), format!("[{row}][")),
                None => (name.clone(), "[".to_owned()),
            };
            o.errors.push(CompileError::at(line, 1, format!("'{name}{i}{k}]' is never defined{more}: each element of an array is defined once, `{name}{} = ...` in a `repeat`", if i.len() > 1 { "[i][j]" } else { "[i]" })));
        }
    }
    o.materialize_zones();
    // From rule nodes to compiled rules: one can give several, or none.
    // For each node it is noted at which compiled one it starts, and with that the span of
    // each copy becomes a span of compiled ones.
    let mut starts_at: Vec<usize> = Vec::new();
    for (n, scopes) in std::mem::take(&mut o.rules) {
        starts_at.push(o.e.rules.len());
        o.scopes = scopes;
        let mut c = Cur::new(&n.head, n.line, n.col);
        let word = c.id("on or every").unwrap_or_default();
        if let Err(f) = o.rule(n, &word, &mut c) {
            o.push_error(f);
        }
    }
    starts_at.push(o.e.rules.len());
    for t in &mut o.e.spans {
        let (a, b) = (t.rules.start.min(starts_at.len() - 1), t.rules.end.min(starts_at.len() - 1));
        t.rules = starts_at[a]..starts_at[b];
    }
    o.e.twin_of = twins(&o.e.rules, &o.e.spans);
    if let Some((tokens, (l, col))) = o.pending_keyboard.take() {
        o.scopes.clear();
        let mut c = Cur::new(tokens, l, col);
        match o.expr(&mut c) {
            Ok(e) => o.e.keyboard_while = Some(e),
            Err(f) => o.errors.push(f),
        }
    }
    // Until the real one arrives, the one the file asks for (a scene that
    // declares none gets the default one here, not further down: reading it
    // without one was a panic).
    let (w, h) = {
        let s = o.e.surface_mut();
        (s.width as f32, s.height as f32)
    };
    o.e.facts[0].1 = if w > 0.0 { w } else { 1920.0 };
    o.e.facts[1].1 = h;
    for (which, tokens, (l, col)) in std::mem::take(&mut o.pending_surfaces) {
        o.scopes.clear();
        // With `screens: each`, the `open:` is read ONCE PER COPY, in its
        // scope: `open: blob.here > 0.01` has to be worth the `blob.here` of
        // each one, and every copy has to have its condition. Before, it was
        // read once, outside any scope, and only the first copy
        // received it: the others were always born open.
        let copies: Vec<(usize, usize)> = o.e.surfaces.iter().enumerate()
            .filter(|(k, s)| *k == which || (s.name == o.e.surfaces[which].name && matches!(s.screens, Screens::Number(_))))
            .map(|(k, s)| (k, s.instance)).collect();
        for (k, instance) in copies {
            let per_copy = matches!(o.e.surfaces[k].screens, Screens::Number(_));
            if per_copy {
                o.scopes.push(Rc::new(o.screen_scope(instance)));
            }
            let mut c = Cur::new(tokens, l, col);
            match o.expr(&mut c) {
                Ok(e) => o.e.surfaces[k].open = Some(e),
                Err(f) => o.errors.push(f),
            }
            if per_copy {
                o.scopes.pop();
            }
        }
    }
    for (which, tokens, (l, col)) in std::mem::take(&mut o.pending_keyboards) {
        o.scopes.clear();
        let mut c = Cur::new(tokens, l, col);
        match o.expr(&mut c) {
            Ok(e) => {
                let owner = o.e.surfaces[which].name.clone();
                for s in o.e.surfaces.iter_mut().filter(|s| s.name == owner) {
                    s.keyboard_when = Some(e.clone());
                }
            }
            Err(f) => o.errors.push(f),
        }
    }
    // A surface that does not say `keyboard:` asks for it as the first one does —its
    // mode and its condition—, and one that says it without a `while` follows the
    // scene's: so a scene that said it once, on its first surface, behaves as it always
    // did, and a `while` on another surface (a sheet asking for it while open) does not
    // become the condition of the ones that said nothing.
    if let Some((first, first_when)) = o.e.surfaces.first().map(|s| (s.keyboard, s.keyboard_when.clone())) {
        let scene_when = o.e.keyboard_while.is_some();
        for s in o.e.surfaces.iter_mut() {
            if !s.keyboard_said {
                s.keyboard = first;
                if s.keyboard_when.is_none() {
                    s.keyboard_when = first_when.clone();
                }
            }
            s.keyboard_while = s.keyboard_when.is_some() || scene_when;
        }
    }
    for (which, tokens, (l, col)) in std::mem::take(&mut o.pending_reserves) {
        o.scopes.clear();
        let mut c = Cur::new(tokens, l, col);
        match o.expr(&mut c) {
            Ok(e) => {
                let owner = o.e.surfaces[which].name.clone();
                for s in o.e.surfaces.iter_mut().filter(|s| s.name == owner) {
                    s.reserve_while = Some(e.clone());
                }
            }
            Err(f) => o.errors.push(f),
        }
    }
    for (which, step, low, tokens, (l, col)) in std::mem::take(&mut o.pending_rates) {
        o.scopes.clear();
        let mut c = Cur::new(tokens, l, col);
        match o.expr(&mut c) {
            Ok(e) => {
                let owner = o.e.surfaces[which].name.clone();
                for s in o.e.surfaces.iter_mut().filter(|s| s.name == owner) {
                    // By its step, so that a surface read twice —a copy per monitor— ends
                    // up with the steps it was written with, and not with them doubled.
                    if step < s.rate_while.len() {
                        s.rate_while[step] = (low, e.clone());
                    } else {
                        s.rate_while.push((low, e.clone()));
                    }
                }
            }
            Err(f) => o.errors.push(f),
        }
    }
    // A surface that does not say `rate:` keeps the first one's: so a scene that said
    // it once, on its first surface, goes at the pace it always went.
    if let Some((fps, steps)) = o.e.surfaces.first().map(|s| (s.max_fps, s.rate_while.clone())) {
        for s in o.e.surfaces.iter_mut().filter(|s| !s.rate_said) {
            s.max_fps = fps;
            s.rate_while = steps.clone();
        }
    }
    for (which, raised, tokens, (l, col)) in std::mem::take(&mut o.pending_levels) {
        o.scopes.clear();
        let mut c = Cur::new(tokens, l, col);
        match o.expr(&mut c) {
            Ok(e) => {
                let owner = o.e.surfaces[which].name.clone();
                for s in o.e.surfaces.iter_mut().filter(|s| s.name == owner) {
                    s.level_while = Some((raised, e.clone()));
                }
            }
            Err(f) => o.errors.push(f),
        }
    }
    // A lock surface without `open:` would be locked from the start: it is required.
    if let Some(s) = o.e.surfaces.iter().find(|s| s.lock_screen && s.open.is_none()) {
        o.errors.push(CompileError::at(1, 1, format!("the lock surface '{}' has to say when it locks: `open: locked`, with a fact. Without it, the session would be locked from the start", s.name)));
    }
    for (which, name, (l, col)) in std::mem::take(&mut o.pending_anchors) {
        let g = o.global(&name);
        let Some(h) = o.facts.get(&g).copied() else {
            let known: Vec<&String> = o.facts.keys().collect();
            let hint = closest_match(&name, known.into_iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
            o.push_error(CompileError::at(l, col, format!("'{name}' is not an anchor, and there is no fact called that either.{hint}")));
            continue;
        };
        let Some((_, FactType::Enum(values))) = o.e.types.iter().find(|(x, _)| *x == g) else {
            o.push_error(CompileError::at(l, col, format!("`anchor:` takes a word, or a fact whose values are anchors: `fact {name}: top_left | top_right = top_right`. '{name}' has no such type")));
            continue;
        };
        let anchors: Option<Vec<SurfaceAnchor>> = values.iter().map(|v| SurfaceAnchor::from_word(v)).collect();
        let Some(anchors) = anchors else {
            let bad = values.iter().find(|v| SurfaceAnchor::from_word(v).is_none()).unwrap();
            o.push_error(CompileError::at(l, col, format!("'{name}' decides an anchor, so every one of its values has to be one: '{bad}' is not. They are {}", join_or(vocab::SURFACE_ANCHORS))));
            continue;
        };
        // The value it is born with says where it starts attached.
        let initial = o.e.facts[h.0 as usize].1.round().max(0.0) as usize;
        let start_anchor = anchors.get(initial).copied().unwrap_or(anchors[0]);
        let owner_name = o.e.surfaces[which].name.clone();
        for surf in o.e.surfaces.iter_mut().filter(|s| s.name == owner_name) {
            surf.anchor = start_anchor;
            surf.anchor_from = Some((h, anchors.clone()));
        }
    }
    for (which, name, (l, col)) in std::mem::take(&mut o.pending_screens) {
        let g = o.global(&name);
        let Some(t) = o.texts.get(&g).copied() else {
            let known: Vec<&String> = o.texts.keys().collect();
            let hint = closest_match(&name, known.into_iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
            o.push_error(CompileError::at(l, col, format!("`screens:` takes `all`, `each`, monitors' names in quotes, or a text whose value is a monitor's name (`text on_screen = \"DP-1\"`, `screens: on_screen`): there is no text called '{name}'.{hint}")));
            continue;
        };
        // The value it is born with says where it starts; empty, on none.
        let first = o.e.texts[t.0 as usize].1.clone();
        let owner_name = o.e.surfaces[which].name.clone();
        for surf in o.e.surfaces.iter_mut().filter(|s| s.name == owner_name) {
            surf.screens = Screens::Named(if first.is_empty() { Vec::new() } else { vec![first.clone()] });
            surf.screens_from = Some(t);
        }
    }
    if o.e.surfaces.is_empty() {
        o.e.surfaces.push(Surface::default());
    }
    // A library with a `.luau` next to it is a plugin: its logic, with its permissions.
    for b in libraries {
        if let Some(logic) = &b.logic {
            o.e.plugins.push(Plugin { name: b.name.clone(), logic: logic.clone(), permissions: o.permissions_of.get(&b.file).cloned().unwrap_or_default() });
        } else if o.permissions_of.contains_key(&b.file) {
            o.errors.push(CompileError::at(b.file * super::PER_FILE + 1, 1, format!("library '{}' asks for permissions, but has no logic to use them: its `.luau` is missing next to it", b.name)));
        }
    }
    // What a component of a `strict` library read from the scene without asking for it.
    for (component, name, pos) in o.unrequested.take() {
        o.scopes.clear();
        o.push_error(CompileError::at(pos.0, pos.1, format!("'{component}' belongs to a `strict` library and reads '{name}', which is the scene\'s, without asking for it. Take it as a parameter, or declare it in the library")));
    }
    // What has no translation into some language: it is shown in the original there.
    // Said, not failed: a scene grows a text before someone translates it.
    if o.errors.is_empty() {
        for ((code, _), missing) in o.translations.iter().zip(o.untranslated.borrow().iter()) {
            if !missing.is_empty() {
                let list: Vec<String> = missing.iter().take(6).map(|t| format!("«{t}»")).collect();
                let more = if missing.len() > 6 { format!(" and {} more", missing.len() - 6) } else { String::new() };
                eprintln!("translations · {code}: {} text{} without a translation, shown as written: {}{more}", missing.len(), if missing.len() == 1 { "" } else { "s" }, list.join(", "));
            }
        }
    }
    // The names that were declared, one per name: the four passes and the copies
    // of a `repeat` declare the same one many times, and the first location is enough for the editor.
    let mut seen = std::collections::HashSet::new();
    let mut symbols: Vec<Symbol> = std::mem::take(&mut o.declared).into_iter().filter(|s| seen.insert(s.local.clone())).collect();
    // And every time something was named, once per location: the passes repeat.
    let mut seen_places = std::collections::HashSet::new();
    symbols.extend(o.used.take().into_iter().filter(|s| seen_places.insert((s.local.clone(), s.line, s.col))));
    crate::scene::settle_effects(&mut o.e.instrs);
    o.note_loose_clips(body);
    // A property and a fact are named with sixteen bits: one more than fit
    // would be taken for the first, and the scene would move what it should not.
    for (what, n) in [("properties", o.e.props.len()), ("facts", o.e.facts.len())] {
        if n > u16::MAX as usize + 1 {
            o.errors.push(CompileError::at(1, 1, format!("this scene has {n} {what}, and no more than 65536 fit: the ones it declares and the ones made for it (a spring per zone that is hovered, a slot per child of a layout, a big `let`). Fewer copies in its `repeat`s and `for`s, or a list that lays out only its window (`view:`, `content:`, `for … from`)")));
        }
    }
    (if o.errors.is_empty() { Ok(o.e) } else { Err(o.errors) }, symbols)
}

/// `surface … { … }`, which is read in two passes.
fn is_surface(e: &Entry) -> bool {
    matches!(e, Entry::Node(n) if matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "surface"))
}

/// In which pass each statement at the scene level is read.
/// The system's language, as the locale variables say it: `es_ES.UTF-8` is `es_es`.
fn system_language() -> String {
    for v in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(s) = std::env::var(v) {
            if !s.is_empty() && s != "C" && s != "POSIX" {
                return s.split(['.', '@']).next().unwrap_or("").to_lowercase();
            }
        }
    }
    "en".into()
}

/// Whether a word appears anywhere in the scene, as a name.
fn mentions(entries: &[Entry], word: &str) -> bool {
    let named = |t: &[crate::language::tokens::Token]| t.iter().any(|t| matches!(&t.kind, TokenKind::Id(w) if w == word || w.split('.').next() == Some(word)));
    entries.iter().any(|e| match e {
        Entry::Prop { value, .. } => named(value),
        Entry::Node(n) => named(&n.head) || n.body.as_ref().is_some_and(|b| mentions(b, word)),
    })
}

/// The zones named with `.hover` or `.pressed` anywhere in the scene, as they
/// are written: only those get their springs.
fn hover_mentions(entries: &[Entry], out: &mut std::collections::HashSet<String>) {
    let scan = |t: &[crate::language::tokens::Token], out: &mut std::collections::HashSet<String>| {
        for t in t {
            if let TokenKind::Id(w) = &t.kind {
                for end in [".hover", ".pressed"] {
                    if let Some(z) = w.strip_suffix(end) {
                        out.insert(z.to_owned());
                    }
                }
            }
        }
    };
    for e in entries {
        match e {
            Entry::Prop { value, .. } => scan(value, out),
            Entry::Node(n) => {
                scan(&n.head, out);
                if let Some(b) = &n.body {
                    hover_mentions(b, out);
                }
            }
        }
    }
}

/// Whether any name has that part in the middle: `nook.cursor.x`.
fn mentions_part(entries: &[Entry], part: &str) -> bool {
    let named = |t: &[crate::language::tokens::Token]| t.iter().any(|t| matches!(&t.kind, TokenKind::Id(w) if w.split('.').skip(1).any(|p| p == part)));
    entries.iter().any(|e| match e {
        Entry::Prop { value, .. } => named(value),
        Entry::Node(n) => named(&n.head) || n.body.as_ref().is_some_and(|b| mentions_part(b, part)),
    })
}

/// Whether the scene declares that name itself: `prop time`, `let time`…
fn declares(entries: &[Entry], word: &str) -> bool {
    entries.iter().any(|e| match e {
        Entry::Node(n) => {
            matches!((n.head.first().map(|t| &t.kind), n.head.get(1).map(|t| &t.kind)), (Some(TokenKind::Id(k)), Some(TokenKind::Id(w))) if w == word && ["prop", "pose", "let", "fact"].contains(&k.as_str()))
                || n.body.as_ref().is_some_and(|b| declares(b, word))
        }
        _ => false,
    })
}

/// What inside a layout is not a child that takes room: declarations and rules.
fn is_declaration(n: &Node) -> bool {
    matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if matches!(p.as_str(), "prop" | "pose" | "let" | "fact" | "on" | "every" | "follow" | "spin" | "wave" | "blink" | "look"))
}

/// `a[i] = ...`: an element of an array being defined.
fn is_definition(n: &Node) -> bool {
    matches!((n.head.first().map(|t| &t.kind), n.head.get(1).map(|t| &t.kind)), (Some(TokenKind::Id(_)), Some(TokenKind::Sym("["))))
}

/// `x = ...`, `x += ...`: a link of a fold. Not a statement that starts the
/// same way (`blur = file ...`).
fn is_fold(n: &Node) -> bool {
    let k = |i: usize| n.head.get(i).map(|t| &t.kind);
    match k(0) {
        Some(TokenKind::Id(w)) if !vocab::STATEMENTS.contains(&w.as_str()) => {
            matches!(k(1), Some(TokenKind::Sym("="))) || (matches!(k(1), Some(TokenKind::Sym("+" | "-" | "*"))) && matches!(k(2), Some(TokenKind::Sym("="))))
        }
        _ => false,
    }
}

/// The names a `repeat` folds that are not `let`s of its own: the scene's.
fn fold_targets(entries: &[Entry]) -> Vec<String> {
    fn walk(entries: &[Entry], folded: &mut Vec<String>, own: &mut HashSet<String>) {
        for e in entries {
            let Entry::Node(n) = e else { continue };
            match n.head.first().map(|t| &t.kind) {
                Some(TokenKind::Id(w)) if w == "let" => {
                    if let Some(TokenKind::Id(name)) = n.head.get(1).map(|t| &t.kind) {
                        own.insert(name.clone());
                    }
                }
                Some(TokenKind::Id(w)) if is_fold(n) && !folded.contains(w) => folded.push(w.clone()),
                _ => {}
            }
            if let Some(b) = &n.body {
                walk(b, folded, own);
            }
        }
    }
    let (mut folded, mut own) = (Vec::new(), HashSet::new());
    walk(entries, &mut folded, &mut own);
    folded.retain(|n| !own.contains(n));
    folded
}

/// What a `repeat` that defines elements reads with its definitions: its
/// `let`s, and the values it declares, which a definition may read.
fn hoisted_with_definitions(n: &Node) -> bool {
    declares_value(n) || matches!(n.head.first().map(|t| &t.kind), Some(TokenKind::Id(w)) if w == "let")
}

/// What declares a value of the scene (or holds such declarations): a
/// `repeat` at the scene's level reads these in pass 0.
fn declares_value(n: &Node) -> bool {
    match n.head.first().map(|t| &t.kind) {
        Some(TokenKind::Id(w)) => match w.as_str() {
            "repeat" | "prop" | "pose" | "fact" | "event" | "measure" => true,
            "text" => matches!(n.head.get(2).map(|t| &t.kind), Some(TokenKind::Sym("="))),
            _ => false,
        },
        _ => false,
    }
}

/// `repeat i in 0..n { ... }`.
fn is_repeat(e: &Entry) -> bool {
    matches!(e, Entry::Node(n) if matches!(n.head.first().map(|t| &t.kind), Some(TokenKind::Id(w)) if w == "repeat"))
}

/// A `repeat` that defines elements, in it or in a `repeat` inside it.
fn hoists(e: &Entry) -> bool {
    let Entry::Node(n) = e else { return false };
    matches!(n.head.first().map(|t| &t.kind), Some(TokenKind::Id(w)) if w == "repeat")
        && n.body.as_deref().unwrap_or(&[]).iter().any(|e| matches!(e, Entry::Node(s) if is_definition(s) || is_fold(s)) || hoists(e))
}

fn pass_of(e: &Entry) -> u8 {
    let Entry::Node(n) = e else { return 2 };
    if is_definition(n) || is_fold(n) || hoists(e) {
        return 1;
    }
    let is_assignment = matches!(n.head.get(2).map(|x| &x.kind), Some(TokenKind::Sym("=")));
    match n.head.first().map(|f| &f.kind) {
        Some(TokenKind::Id(p)) => match p.as_str() {
            "surface" | "permissions" | "model" | "service" | "spring" | "prop" | "pose" | "fact" | "event" | "measure" | "component" | "windows" => 0,
            // An image or a figure is read once: they belong to the scene, not to each copy.
            "text" | "image" | "figure" | "shader" if is_assignment => 0,
            "font" => 0,
            "let" | "layer" => 1,
            _ => 2,
        },
        _ => 2,
    }
}

impl<'a> Compiler<'a> {
    // ── names ───────────────────────────────────────────────────

    /// `item.$i.title`, with `i` being 3, is `item.3.title`.
    fn interpolate(&self, n: &str) -> String {
        if !n.contains('$') {
            return n.to_owned();
        }
        n.split('.')
            .map(|segment| match segment.strip_prefix('$') {
                Some(var) => match self.scopes.iter().rev().find_map(|e| e.exprs.get(var)) {
                    Some(Expr::K(v)) => format!("{}", *v as i64),
                    _ => segment.to_owned(),
                },
                None => segment.to_owned(),
            })
            .collect::<Vec<_>>()
            .join(".")
    }

    fn interpolate_in(&self, n: &str) -> String {
        self.interpolate(n)
    }

    /// What a name seen from in here is really called: what this copy
    /// of a component declared carries its suffix. It works for the whole name or
    /// for its beginning: `label.width` belongs to the measure `label`.
    /// A name marked with the screen copy (`drip#screen0`) that nobody
    /// declared that way: it belongs to the scene, shared by all the copies. It happens with
    /// loose `let`s and `figure`s, which are read once —not once per
    /// monitor— and a copy looks for them with its mark.
    fn strip_screen_suffix(&self, n: &str) -> Option<String> {
        let k = n.find("#screen")?;
        let rest = &n[k + 7..];
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        // The mark can go at the end (`drip#screen0`) or in the middle (`hat#screen0.brim`).
        Some(format!("{}{}", &n[..k], &rest[digits..]))
    }

    fn global(&self, n: &str) -> String {
        // Where this has been named: it is what the editor shows in "where is it used".
        if !self.unwatched.get() {
            let (line, col) = NAMED.with(|m| m.get());
            if line > 0 {
                self.used.borrow_mut().push(Symbol { local: n.to_owned(), class: "use".to_owned(), line, col });
            }
        }
        let n = self.interpolate(n);
        for e in self.scopes.iter().rev() {
            let mut until = n.len();
            loop {
                // The whole name always holds; its beginning, only if it belongs to something with
                // parts. Otherwise, a zone `hit` would swallow the text `hit.3`.
                if let Some(g) = e.alias.get(&n[..until]).filter(|_| until == n.len() || e.with_parts.contains(&n[..until])) {
                    return format!("{g}{}", &n[until..]);
                }
                match n[..until].rfind('.') {
                    Some(p) => until = p,
                    None => break,
                }
            }
        }
        // Inside a component of a library, `now` is the `Clock.now` of its boundary.
        // (Or at the library's own level: a gesture of its own that moves a pose of its own.)
        let from_library = self.scopes.iter().rev().find_map(|e| e.library).or_else(|| {
            let k = PEEKED.with(|m| m.get().0) / super::PER_FILE;
            (self.scopes.is_empty() && k > 0).then_some(k)
        });
        if let Some(own) = from_library.and_then(|k| self.boundary_of.get(&k)) {
            let mut until = n.len();
            loop {
                if let Some(g) = own.get(&n[..until]) {
                    return format!("{g}{}", &n[until..]);
                }
                match n[..until].rfind('.') {
                    Some(p) => until = p,
                    None => break,
                }
            }
        }
        self.watch(&n);
        n
    }

    /// Inside a component of a `strict` library, a name that is not its own, nor its
    /// library's, nor one of those that always exist, is something it reads from the scene without
    /// having asked for it. It is noted; it is reported at the end.
    fn watch(&self, name: &str) {
        if self.unwatched.get() {
            return;
        }
        let Some(e) = self.scopes.iter().rev().find(|e| e.instance.is_some()) else { return };
        if !e.strict || self.from_library.contains(name) || self.values.contains_key(name) {
            return;
        }
        // A parameter, or a `let` of the component: it is its own even though it has no suffix.
        if self.scopes.iter().any(|e| e.exprs.contains_key(name) || e.colors.contains_key(name) || e.strings.contains_key(name) || e.springs.contains_key(name)) {
            return;
        }
        let builtin = ["screen.", "pointer.", "local.", "drag."].iter().any(|p| name.starts_with(p)) || name == "wheel";
        // What the copy itself declared carries its suffix; what did not, is from outside.
        let own = self.scopes.iter().any(|e| !e.suffix.is_empty() && name.ends_with(&e.suffix));
        if !builtin && !own {
            let component = e.instance.as_ref().unwrap().0.clone();
            let mut v = self.unrequested.borrow_mut();
            if !v.iter().any(|(c, n, _)| *c == component && n == name) {
                v.push((component, name.to_owned(), PEEKED.with(|m| m.get())));
            }
        }
    }

    /// The name of a shape that may become a zone. A name made in a `repeat`
    /// (`drop.$k`) takes no suffix from the `repeat` —the `$k` already tells
    /// the turns apart, and so it can be named from outside it—, but in a
    /// screen copy it does take the copy's mark. The whole scene is written
    /// once per monitor, and two copies declaring the zone `drop.4` left one
    /// name for two zones —the map keeps the last one—, so every rule pointed at
    /// the last copy's, and on the other monitor the zone caught the pointer and
    /// nobody heard it. Only zones: a `fact n.$k` or a `prop glow.$k` are the
    /// scene's, one for all the copies, and so they stay.
    /// It is not an alias —a `prop glow.$k` may share the zone's name, and must
    /// keep meaning the prop—: whoever looks for a zone tries the marked name
    /// first (`zone_named`). A name of the `repeat`'s own (`hit`, which is
    /// `hit#k1` in its second turn) takes it too: every copy's second turn
    /// made a `hit#k1`, and its rules were heard on none of them.
    fn declare_zone(&mut self, local: &str) -> String {
        let g = self.declare(local);
        match self.screen_mark() {
            Some(mark) if !g.contains(&mark) => format!("{g}{mark}"),
            _ => g,
        }
    }

    /// What a name declared here carries after it: the mark of the copy it is
    /// in, and of every copy that one is inside of. A `repeat` in a `for` is
    /// `#k2#j0`: with only the inner mark, every turn of the outer one declared
    /// the same `hand#j0`, and a rule on it found the wrong one, or none.
    /// (Nothing, where the innermost scope is not a copy: as it was. And the
    /// mark of a copy per monitor goes on by its own way.)
    fn copy_suffix(&self) -> String {
        match self.scopes.last() {
            Some(e) if e.suffix.starts_with("#screen") => e.suffix.clone(),
            Some(e) if !e.suffix.is_empty() => self.scopes.iter().filter(|e| !e.suffix.is_empty() && !e.suffix.starts_with("#screen")).map(|e| e.suffix.as_str()).collect(),
            _ => String::new(),
        }
    }

    /// `hit.hover` and `hit.pressed` for a zone about to be declared here: the
    /// same name the zone will have (see `declare_zone`), and from here they
    /// are reached as written.
    fn zone_springs_for(&mut self, local: &str) {
        let interpolated = self.interpolate(local);
        let suffix = self.copy_suffix();
        let mut g = if !suffix.is_empty() && !local.contains('$') { format!("{interpolated}{suffix}") } else { interpolated.clone() };
        if let Some(mark) = self.screen_mark().filter(|m| !g.contains(m.as_str())) {
            g = format!("{g}{mark}");
        }
        let spring = Spring::at(0.14);
        let mut made = Vec::new();
        for part in ["hover", "pressed"] {
            let name = format!("{g}.{part}");
            let p = match self.props.get(&name) {
                Some(p) => *p,
                None => {
                    let p = self.e.prop_with(interned(&name), 0.0, spring);
                    self.props.insert(name.clone(), p);
                    p
                }
            };
            if g != interpolated {
                if let Some(e) = self.scopes.last_mut().map(Rc::make_mut) {
                    e.alias.insert(format!("{interpolated}.{part}"), name);
                }
            }
            made.push(p);
        }
        if !self.zone_springs.iter().any(|z| z.0 == g) {
            self.zone_springs.push((g, made[0], made[1]));
        }
    }

    /// The mark of the screen copy being read, if any: `#screen1`.
    fn screen_mark(&self) -> Option<String> {
        self.scopes.iter().rev().find(|e| e.suffix.starts_with("#screen")).map(|e| e.suffix.clone())
    }

    /// A zone by the name a rule gives it: this copy's own, if it has one.
    fn zone_named(&self, n: &str) -> Option<ZoneId> {
        self.screen_mark().and_then(|m| self.zones.get(&format!("{n}{m}"))).or_else(|| self.zones.get(n)).copied()
    }

    /// The name with which something is declared from in here.
    fn declare(&mut self, local: &str) -> String {
        let interpolated = self.interpolate(local);
        // The location of the name that has just been read, if it is on this same line.
        let (line, col) = match (PEEKED.with(std::cell::Cell::get), NAMED.with(std::cell::Cell::get)) {
            (m, n) if n.0 == m.0 => n,
            (m, _) => m,
        };
        if line > 0 {
            let class = if self.current_class.is_empty() { "name".to_owned() } else { self.current_class.clone() };
            self.declared.push(Symbol { local: interpolated.clone(), class, line, col });
        }
        // Inside a screen copy, a name the scene ALREADY has —the
        // text `query`, which its `input` declares again as a zone— is still
        // the scene's: if the copy aliased it to `query#screen0`,
        // everything that comes after it would look for a text that does not exist.
        let from_scene = self.scopes.last().is_some_and(|e| e.suffix.starts_with("#screen"))
            && (self.texts.contains_key(&interpolated) || self.facts.contains_key(&interpolated) || self.props.contains_key(&interpolated) || self.lets.contains_key(&interpolated));
        let suffix = self.copy_suffix();
        match self.scopes.last_mut().map(Rc::make_mut) {
            Some(e) if !e.suffix.is_empty() && !local.contains('$') && !from_scene => {
                let g = format!("{interpolated}{suffix}");
                e.alias.insert(interpolated, g.clone());
                g
            }
            _ => {
                // At the level of a library: its boundary lives under its name, so that two
                // plugins can each have their own `count` without stepping on each other, or on the scene.
                let file = PEEKED.with(|m| m.get().0) / super::PER_FILE;
                match self.libraries.iter().find(|b| b.file == file && file > 0) {
                    Some(b) => {
                        let g = format!("{}.{interpolated}", b.name);
                        self.boundary_of.entry(file).or_default().insert(interpolated, g.clone());
                        g
                    }
                    None => interpolated,
                }
            }
        }
    }

    /// One more error, up to eight. The same error in the same place is reported once:
    /// a badly written component fails in every copy, and it is the same typo.
    fn push_error(&mut self, mut f: CompileError) {
        // An error inside a component —which may be in another file— also says
        // where it was used from: often what is wrong is what was passed to it.
        if let Some((component, line)) = self.scopes.iter().rev().find_map(|e| e.instance.as_ref()) {
            if *line != f.line {
                f.message = format!("{} (inside '{component}', used at {})", f.message, super::location(self.files, *line));
            }
        }
        let repeated = self.errors.iter().any(|g| g.line == f.line && g.col == f.col && g.message == f.message);
        if !repeated && self.errors.len() < 8 {
            self.errors.push(f);
        }
    }

    fn unknown<T>(&self, c: &Cur, what: &str, name: &str, known: Vec<&String>) -> R<T> {
        let (l, col) = c.tokens.get(c.i.saturating_sub(1)).map_or(c.end, |x| (x.line, x.col));
        // Is it the field of a record? Then what matters are the fields of its model,
        // not that inside it is called `rows.3.lable`.
        let full_name = self.global(name);
        let row_field = full_name.rsplit_once('.').and_then(|(row, field)| {
            let (model, k) = row.rsplit_once('.')?;
            k.parse::<usize>().ok()?;
            Some((self.e.models.iter().find(|m| m.name == model)?, field))
        });
        if let Some((m, field)) = row_field {
            // It exists, but it is of another kind: a number where a text is needed, or the other way round.
            if let Some(c) = m.fields.iter().find(|c| c.name == field) {
                let (it_is, needed) = match c.kind {
                    FieldType::Text => ("text", "a number (number, bool or an enum)"),
                    FieldType::Image(..) => ("image", "a number (number, bool or an enum)"),
                    FieldType::Number => ("number", "a text"),
                    FieldType::Bool => ("bool", "a text"),
                    FieldType::Enum(_) => ("an enum", "a text"),
                    FieldType::List(_) => ("a list", "a plain field: walk it with `for`"),
                };
                return Err(CompileError::at(l, col, format!("field '{field}' of '{}' is {it_is}, and here {needed} is needed.", m.name)));
            }
            let mut fields: Vec<String> = m.fields.iter().map(|c| c.name.clone()).collect();
            fields.push("index".into());
            let hint = closest_match(field, fields.iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
            let of_type = match what { "no text" => " of type text", _ => "" };
            return Err(CompileError::at(l, col, format!("records of '{}' have no field{of_type} called '{field}'.{hint} Its fields are: {}.", m.name, fields.join(", "))));
        }
        let hint = closest_match(name, known.into_iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
        Err(CompileError::at(l, col, format!("there is {what} called '{name}'.{hint} A `let` has to come before whatever uses it; everything else can go anywhere.")))
    }

    fn prop(&self, c: &mut Cur) -> R<PropId> {
        let n = self.global(&c.id("a property name")?);
        match self.props.get(&n) {
            Some(p) => Ok(*p),
            None => self.unknown(c, "no property", &n, self.props.keys().collect()),
        }
    }
    fn fact(&self, c: &mut Cur) -> R<FactId> {
        let n = self.global(&c.id("a fact name")?);
        match self.facts.get(&n) {
            Some(h) => Ok(*h),
            None => self.unknown(c, "no fact", &n, self.facts.keys().collect()),
        }
    }
    fn signal(&self, c: &mut Cur) -> R<SignalId> {
        let n = self.global(&c.id("an event name")?);
        match self.signals.get(&n) {
            Some(s) => Ok(*s),
            None => self.unknown(c, "no event", &n, self.signals.keys().collect()),
        }
    }
    fn signals(&self, c: &mut Cur) -> R<Vec<SignalId>> {
        let mut v = vec![self.signal(c)?];
        while c.sym(",") {
            v.push(self.signal(c)?);
        }
        Ok(v)
    }
    fn zone(&self, c: &mut Cur) -> R<ZoneId> {
        let n = self.global(&c.id("the name of a shape or a zone")?);
        match self.zone_named(&n) {
            Some(z) => Ok(z),
            None => self.unknown(c, "no named shape or zone", &n, self.zones.keys().collect()),
        }
    }
    fn spring(&self, c: &mut Cur) -> R<Spring> {
        // `~620ms`: a spring given in time. It gets there in that while and does not bounce,
        // which is how the house's animation contracts are written.
        if let Some(TokenKind::Dur(_)) = c.peek() {
            let t = c.dur()?.as_secs_f32();
            // `~0ms`: at once, without travelling (a window let go where the mouse left it).
            if t == 0.0 {
                return Ok(Spring::INSTANT);
            }
            return Ok(Spring::at(t.max(0.016)));
        }
        let n = c.id("a spring name")?;
        if n == "spring" {
            c.expect_sym("(")?;
            let stiffness = c.num()?;
            c.expect_sym(",")?;
            let damping = c.num()?;
            c.expect_sym(")")?;
            return Ok(Spring { stiffness, damping });
        }
        if let Some(m) = self.scopes.iter().rev().find_map(|e| e.springs.get(&n)) {
            return Ok(*m);
        }
        if !vocab::SPRINGS.contains(&n.as_str()) {
            self.watch(&n);
        }
        match self.springs.get(&n) {
            Some(m) => Ok(*m),
            None => self.unknown(c, "no spring", &n, self.springs.keys().collect()),
        }
    }

    // ── expressions ─────────────────────────────────────────────

    /// A number read once, when loading: written plainly or worked out from
    /// numbers and `let`s of numbers (`40 * 2`, `1.5 * rung`), but not from
    /// anything that changes while the scene runs.
    fn constant(&self, c: &mut Cur, what: &str) -> R<f32> {
        let at = c.pos();
        let e = self.expr(c)?;
        c.expect_end()?;
        match e {
            Expr::K(v) => Ok(v),
            _ => Err(CompileError::at(at.0, at.1, format!("{what} is read once, when the scene loads: a number, or one worked out from numbers and `let`s of numbers, not from something that changes while it runs"))),
        }
    }

    fn expr(&self, c: &mut Cur) -> R<Expr> {
        let mut a = self.expr_and(c)?;
        while c.word("or") {
            a = a.or(self.expr_and(c)?);
        }
        Ok(a)
    }
    fn expr_and(&self, c: &mut Cur) -> R<Expr> {
        let mut a = self.expr_not(c)?;
        while c.word("and") {
            a = a.and(self.expr_not(c)?);
        }
        Ok(a)
    }
    fn expr_not(&self, c: &mut Cur) -> R<Expr> {
        if c.word("not") {
            return Ok(self.expr_not(c)?.not());
        }
        let from = c.i;
        let a = self.expr_sum(c)?;
        let enum_fact = self.bare_enum(c, from);
        for (s, f) in [(">=", 0), ("<=", 1), ("==", 4), ("!=", 5), (">", 2), ("<", 3)] {
            if c.sym(s) {
                // An enum is compared with ITS values: the name is looked up in its list, not in
                // everyone's (that way two enums can each have their own `normal`).
                let b = match (&enum_fact, c.peek(), c.tokens.get(c.i + 1).map(|x| &x.kind)) {
                    (Some((fact, names)), Some(TokenKind::Id(v)), next) if !matches!(next, Some(TokenKind::Sym("("))) && self.is_enum_value(v) => {
                        match names.iter().position(|n| n == v) {
                            Some(k) => {
                                c.i += 1;
                                Expr::K(k as f32)
                            }
                            None => return c.error(format!("'{v}' is not a value of '{fact}': it can be {}", join_or(&names.iter().map(String::as_str).collect::<Vec<_>>()))),
                        }
                    }
                    _ => self.expr_sum(c)?,
                };
                // Equal means "less than a thousandth apart": they are decimal numbers, and a spring never quite gets there.
                let differ = |a: Expr, b: Expr| (a - b).abs().gt(Expr::K(0.001));
                return Ok(match f {
                    0 => b.gt(a).not(),
                    1 => a.gt(b).not(),
                    2 => a.gt(b),
                    3 => b.gt(a),
                    4 => differ(a, b).not(),
                    _ => differ(a, b),
                });
            }
        }
        Ok(a)
    }
    fn expr_sum(&self, c: &mut Cur) -> R<Expr> {
        let from = c.i;
        let mut a = self.expr_prod(c)?;
        loop {
            let until = c.i;
            let op = if c.sym("+") { '+' } else if c.sym("-") { '-' } else { return Ok(a) };
            self.no_arithmetic(c, from, until)?;
            let other = c.i;
            let b = self.expr_prod(c)?;
            self.no_arithmetic(c, other, c.i)?;
            a = if op == '+' { a + b } else { a - b };
        }
    }
    fn expr_prod(&self, c: &mut Cur) -> R<Expr> {
        let from = c.i;
        let mut a = self.expr_atom(c)?;
        loop {
            let until = c.i;
            let op = if c.sym("*") { '*' } else if c.sym("/") { '/' } else { return Ok(a) };
            self.no_arithmetic(c, from, until)?;
            let other = c.i;
            let b = self.expr_atom(c)?;
            self.no_arithmetic(c, other, c.i)?;
            a = if op == '*' { a * b } else { a / b };
        }
    }

    /// If what was read from `from` is a bare enum fact —`mode`, `n.urgency`—: which one, and its values.
    fn bare_enum(&self, c: &Cur, from: usize) -> Option<(String, Vec<String>)> {
        self.enum_between(c, from, c.i)
    }

    /// The type of a fact, by its whole name (what `e.types.iter().find` gave, without searching).
    fn type_of(&self, name: &str) -> Option<&(String, FactType)> {
        let mut index = self.type_index.borrow_mut();
        let (seen, by_name) = &mut *index;
        for (k, (n, _)) in self.e.types.iter().enumerate().skip(*seen) {
            by_name.entry(n.clone()).or_insert(k);
        }
        *seen = self.e.types.len();
        by_name.get(name).map(|&k| &self.e.types[k])
    }

    fn enum_between(&self, c: &Cur, from: usize, until: usize) -> Option<(String, Vec<String>)> {
        let [Token { kind: TokenKind::Id(n), .. }] = c.tokens.get(from..until)? else { return None };
        let g = self.global_quiet(n);
        match self.type_of(&g) {
            Some((_, FactType::Enum(names))) => Some((n.clone(), names.clone())),
            _ => None,
        }
    }

    /// You do not do arithmetic with an enum: `mode + 1` means nothing. (With a yes or no, you do:
    /// `r.separator * 21` is how you write "21 if it is a separator".)
    fn no_arithmetic(&self, c: &Cur, from: usize, until: usize) -> R<()> {
        match self.enum_between(c, from, until) {
            Some((fact, names)) => {
                let f = &c.tokens[from];
                Err(CompileError::at(f.line, f.col, format!("'{fact}' is an enum ({}): you do not do arithmetic with it. You compare it: `{fact} == {}`", names.join(", "), names.last().cloned().unwrap_or_default())))
            }
            None => Ok(()),
        }
    }

    /// Whether that name is the value of some enum (and not something else in the scene).
    fn is_enum_value(&self, n: &str) -> bool {
        self.values.contains_key(n) || self.ambiguous.contains(n)
    }

    /// `global`, without it counting as having read anything (to look at what a name is).
    fn global_quiet(&self, n: &str) -> String {
        let before = self.unwatched.replace(true);
        let g = self.global(n);
        self.unwatched.set(before);
        g
    }
    fn expr_atom(&self, c: &mut Cur) -> R<Expr> {
        if c.sym("-") {
            return Ok(Expr::K(0.0) - self.expr_atom(c)?);
        }
        if c.sym("(") {
            let e = self.expr(c)?;
            c.expect_sym(")")?;
            return Ok(e);
        }
        match c.peek() {
            Some(TokenKind::Num(n) | TokenKind::Angle(n)) => {
                c.i += 1;
                Ok(Expr::K(*n))
            }
            Some(TokenKind::Dur(s)) => {
                c.i += 1;
                Ok(Expr::K(*s))
            }
            Some(TokenKind::Id(n)) => {
                c.mark_name();
                c.i += 1;
                if c.sym("(") {
                    return self.function(n, c);
                }
                match n.as_str() {
                    "true" => return Ok(Expr::K(1.0)),
                    "false" => return Ok(Expr::K(0.0)),
                    // In a `cursor:`, the cursors' names are theirs before the scene's.
                    _ if self.in_cursor.get() && vocab::CURSORS.contains(&n.as_str()) => {
                        return Ok(Expr::K(Cursor::from_name(n).unwrap_or_default().code() as f32));
                    }
                    "letter" if self.in_letters.get() => return Ok(Expr::Letter(0)),
                    "letters" if self.in_letters.get() => return Ok(Expr::Letter(1)),
                    "letter" | "letters" if !self.scopes.iter().any(|e| e.exprs.contains_key(n.as_str())) && !self.lets.contains_key(n.as_str()) && !self.props.contains_key(n.as_str()) && !self.facts.contains_key(n.as_str()) => {
                        return c.error(format!("there is nothing called '{n}' here: `letter` (which letter, from 0) and `letters` (how many) only exist inside a text's `letter_move`, `letter_opacity` and `letter_scale`"));
                    }
                    _ => {}
                }
                if c.sym("[") {
                    return self.indexed(n, c);
                }
                self.resolve(c, n)
            }
            _ => c.error("expected a number, a name or a parenthesis here"),
        }
    }

    /// `a[i]`, `win[i].screen`, `win.reserved[s].top`: the element at a place.
    /// A place known when loading is that element's name (`a.3`); one that
    /// changes is a `pick` over every element. Outside the elements, -1.
    fn indexed(&self, base: &str, c: &mut Cur) -> R<Expr> {
        let mut parts = Vec::new();
        loop {
            let i = self.expr(c)?;
            c.expect_sym("]")?;
            let rest = match c.peek() {
                Some(TokenKind::Id(r)) if r.starts_with('.') => {
                    c.i += 1;
                    r.clone()
                }
                _ => String::new(),
            };
            parts.push((i, rest));
            if !c.sym("[") {
                break;
            }
        }
        self.index_chain(c, base, &parts)
    }

    fn index_chain(&self, c: &Cur, base: &str, parts: &[(Expr, String)]) -> R<Expr> {
        let Some(((i, rest), more)) = parts.split_first() else {
            return self.resolve(c, base);
        };
        let element = |k: i64| format!("{base}.{k}{rest}");
        let length = self.arrays.get(&self.global_quiet(base)).map(|a| a.0);
        if let Expr::K(v) = i {
            let k = v.round() as i64;
            return match length {
                Some(n) if k < 0 || k >= n as i64 => Ok(Expr::K(-1.0)),
                None if k < 0 => Ok(Expr::K(-1.0)),
                // Past the last of a family that has elements: outside it too. One
                // with none is a name that does not exist, and says so.
                None if !self.present(&element(k), more) && self.present(&element(0), more) => Ok(Expr::K(-1.0)),
                _ => self.index_chain(c, &element(k), more),
            };
        }
        // Without a declared length, as many as there are: `win.0.screen`, `win.1.screen`, and so on.
        let n = length.unwrap_or_else(|| (0..512).take_while(|k| self.present(&element(*k), more)).count());
        if n == 0 {
            return c.error(format!("'{base}' has no elements to read: declare it with `let {base}[N]`, or read a name that has them (`win[i].screen`)"));
        }
        let mut options = vec![Expr::K(-1.0)];
        for k in 0..n {
            options.push(self.index_chain(c, &element(k as i64), more)?);
        }
        options.push(Expr::K(-1.0));
        Ok(Expr::Pick(Box::new(i.clone() + Expr::K(1.0)), options).folded_pub())
    }

    /// Whether an element exists, without saying anything if not: what
    /// follows it is looked for at its first place (`win.3.reserved.0.top`).
    fn present(&self, base: &str, parts: &[(Expr, String)]) -> bool {
        if let Some(((i, rest), more)) = parts.split_first() {
            let k = match i {
                Expr::K(v) => v.round() as i64,
                _ => 0,
            };
            return k >= 0 && self.present(&format!("{base}.{k}{rest}"), more);
        }
        if self.scopes.iter().any(|e| e.exprs.contains_key(base)) {
            return true;
        }
        let g = self.global_quiet(base);
        let has = |n: &str| self.lets.contains_key(n) || self.props.contains_key(n) || self.facts.contains_key(n);
        has(&g) || self.strip_screen_suffix(&g).is_some_and(|b| has(&b))
    }

    /// What a name is worth here: a parameter, a `let`, a prop, a fact or an enum's value.
    fn resolve(&self, c: &Cur, n: &str) -> R<Expr> {
        // First what is inside —parameters and `let`s of the component—, then what is outside.
        if let Some(e) = self.scopes.iter().rev().find_map(|e| e.exprs.get(n)) {
            return Ok(e.clone());
        }
        let n = &self.global(n);
        // Inside a screen copy, a `let` that was read once per
        // copy is stored with its mark: that is its own.
        let marked = self.scopes.iter().rev().find(|e| e.suffix.starts_with("#screen")).map(|e| format!("{n}{}", e.suffix)).filter(|m| self.lets.contains_key(m));
        let n = match &marked {
            Some(m) => m,
            None => n,
        };
        // Marked by the screen copy and not declared that way: it is the scene's.
        let base = self.strip_screen_suffix(n);
        let n = match &base {
            Some(base) if !self.lets.contains_key(n.as_str()) && !self.props.contains_key(n.as_str()) && !self.facts.contains_key(n.as_str()) && (self.lets.contains_key(base) || self.props.contains_key(base) || self.facts.contains_key(base)) => base,
            _ => n,
        };
        if let Some(e) = self.lets.get(n) {
            Ok(e.clone())
        } else if let Some(p) = self.props.get(n) {
            Ok(p.e())
        } else if let Some(h) = self.facts.get(n) {
            Ok(h.e())
        } else if let Some(v) = self.values.get(n) {
            // The value of an enum: `mode == critical`.
            Ok(Expr::K(*v))
        } else if let Some(k) = n.rsplit_once('.').and_then(|(fact, value)| match self.type_of(fact) {
            Some((_, FactType::Enum(names))) => names.iter().position(|x| x == value),
            _ => None,
        }) {
            // The long form, which cannot be confused with anything: `mode.critical`.
            Ok(Expr::K(k as f32))
        } else if self.ambiguous.contains(n.as_str()) {
            c.error(format!("'{n}' is a value of several enums, with different numbers: here there is no telling which. Compare it with its fact (`mode == {n}`) or write it in full (`mode.{n}`)"))
        } else if self.in_cursor.get() {
            // Most likely a cursor's name, mistyped: the list is what helps.
            let all: Vec<String> = vocab::CURSORS.iter().map(|s| s.to_string()).collect();
            let hint = closest_match(n, all.iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
            let t = &c.tokens[c.i - 1];
            Err(CompileError::at(t.line, t.col, format!("there is nothing called '{n}': in a `cursor:`, a cursor is {}.{hint}", join_or(vocab::CURSORS))))
        } else {
            let known: Vec<&String> = self.lets.keys().chain(self.props.keys()).chain(self.facts.keys()).chain(self.values.keys()).collect();
            self.unknown(c, "nothing", n, known)
        }
    }
    fn function(&self, name: &str, c: &mut Cur) -> R<Expr> {
        if name == "sort" {
            return c.error("`sort` makes an array, the slots in order: `let ord[win.max] = sort(i in win by win[i].place)`");
        }
        if !vocab::FUNCTIONS.contains(&name) {
            let all: Vec<String> = vocab::FUNCTIONS.iter().map(|s| s.to_string()).collect();
            let hint = closest_match(name, all.iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
            return c.error(format!("I don\'t know the function '{name}': there are {}.{hint}", vocab::FUNCTIONS.join(", ")));
        }
        let (l, col) = c.tokens.get(c.i.saturating_sub(2)).map_or(c.end, |x| (x.line, x.col));
        if name == "vel" {
            let p = self.prop(c)?;
            c.expect_sym(")")?;
            return Ok(p.vel());
        }
        let mut a = Vec::new();
        let first = c.i;
        if !c.sym(")") {
            loop {
                a.push(self.expr(c)?);
                if c.sym(")") {
                    break;
                }
                c.expect_sym(",")?;
            }
        }
        // `sin(30deg)`: `deg` turns the 30 into radians —which is what `rotate`
        // wants— and `sin` takes degrees. The sine of half a degree, silently.
        if ["sin", "cos", "tan"].contains(&name) {
            if let Some(t) = c.tokens[first..c.i].iter().find(|t| matches!(t.kind, TokenKind::Angle(_))) {
                return Err(CompileError::at(t.line, t.col, format!("`{name}` takes the angle in degrees as a plain number: `{name}(30)`, not `{name}(30deg)` —`deg` turns it into radians, which is what `rotate` wants—")));
            }
        }
        let constant = |e: &Expr| match e {
            Expr::K(v) => Ok(*v),
            _ => Err(CompileError::at(l, col, format!("in '{name}', the first two have to be numbers"))),
        };
        // More than it takes are not read: said, not refused, so a scene that
        // has them still loads. `min(a, b, c)` was the smaller of `a` and `b`.
        let takes = match name {
            "min" | "max" | "pow" | "atan2" | "mod" | "length" | "noise" => 2,
            "clamp" | "smooth" | "mix" | "if" => 3,
            "pick" | "rgb" => usize::MAX,
            _ => 1,
        };
        if a.len() > takes && self.extra_noted.borrow_mut().insert((l, col)) {
            let file = self.files.get(l / super::PER_FILE).map_or("", String::as_str);
            let extra = a.len() - takes;
            eprintln!("note   \u{b7} {file}:{}:{col}: `{name}` takes {takes} argument{}: the {extra} after {} are not read", l % super::PER_FILE, if takes == 1 { "" } else { "s" }, if takes == 1 { "it" } else { "them" });
        }
        let mut a = a.into_iter();
        let mut take = || a.next().ok_or_else(|| CompileError::at(l, col, format!("'{name}' is missing arguments")));
        Ok(match name {
            "min" => take()?.min(take()?),
            "max" => take()?.max(take()?),
            "abs" => take()?.abs(),
            "floor" => take()?.floor(),
            "sin" => take()?.sin(),
            "cos" => take()?.cos(),
            "ceil" => take()?.ceil(),
            "clamp" => {
                let (x, lo, hi) = (take()?, take()?, take()?);
                x.max(lo).min(hi)
            }
            "smooth" => {
                let (lo, hi, x) = (take()?, take()?, take()?);
                x.smoothstep(constant(&lo)?, constant(&hi)?)
            }
            "mix" => {
                let (x, y, t) = (take()?, take()?, take()?);
                x.mix(y, t)
            }
            // The condition is 1 or 0: choosing is mixing.
            "if" => {
                let (cond, x, y) = (take()?, take()?, take()?);
                // A cursor is one or the other, never a blend of their codes:
                // a spring halfway gives the nearer one.
                let cond = if self.in_cursor.get() { cond.gt(Expr::K(0.5)) } else { cond };
                y.mix(x, cond)
            }
            "sqrt" => take()?.un(Un::Sqrt),
            "fract" => take()?.un(Un::Fract),
            "sign" => take()?.un(Un::Sign),
            "round" => take()?.un(Un::Round),
            "exp" => take()?.un(Un::Exp),
            "log" => take()?.un(Un::Ln),
            "tan" => take()?.un(Un::Tan),
            "random" => take()?.un(Un::Random),
            "pow" => take()?.bin(Bin::Pow, take()?),
            "atan2" => take()?.bin(Bin::Atan2, take()?),
            "mod" => take()?.bin(Bin::Mod, take()?),
            "length" => take()?.bin(Bin::Length, take()?),
            // `pick(i, a, b, c)`: the one at place i. At least one to choose from.
            "pick" => {
                let i = take()?;
                let options: Vec<Expr> = a.collect();
                if options.is_empty() {
                    return Err(CompileError::at(l, col, "`pick` needs a place and something to choose: `pick(i, 10, 20, 30)`"));
                }
                Expr::Pick(Box::new(i), options).folded_pub()
            }
            // One argument or two: a wobble in time, or a field to move over.
            "noise" => {
                let x = take()?;
                match a.next() {
                    Some(y) => x.bin(Bin::Noise2, y),
                    None => x.un(Un::Noise),
                }
            }
            // A colour, not a number: it is read where a colour is wanted (see `color`).
            "rgb" => return Err(CompileError::at(l, col, "`rgb(r, g, b)` makes a colour, not a number: it goes where a colour does, `color: rgb(r, g, b)` or `let tint = rgb(r, g, b)`")),
            other => unreachable!("'{other}' is in the vocabulary, but `function` cannot compute it"),
        })
    }

    fn point(&self, c: &mut Cur) -> R<Point> {
        let x = self.expr(c)?;
        c.expect_sym(",")?;
        Ok((x, self.expr(c)?))
    }

    /// A colour: `#9ed6bd`, or `mix(#9ed6bd, #bdeed6, highlight)`.
    fn color(&self, c: &mut Cur) -> R<Color> {
        match c.peek() {
            Some(TokenKind::Color(k)) => {
                c.i += 1;
                Ok(color(k[0], k[1], k[2]))
            }
            Some(TokenKind::Id(n)) if self.scopes.iter().any(|e| e.colors.contains_key(n)) || self.colors.contains_key(n) => {
                c.i += 1;
                if !self.scopes.iter().any(|e| e.colors.contains_key(n)) {
                    self.watch(n);
                }
                Ok(self.scopes.iter().rev().find_map(|e| e.colors.get(n)).unwrap_or_else(|| &self.colors[n]).clone())
            }
            Some(TokenKind::Id(m)) if m == "mix" => {
                c.i += 1;
                c.expect_sym("(")?;
                let a = self.color(c)?;
                c.expect_sym(",")?;
                let b = self.color(c)?;
                c.expect_sym(",")?;
                let t = self.expr(c)?;
                c.expect_sym(")")?;
                let [a0, a1, a2] = a;
                let [b0, b1, b2] = b;
                Ok([a0.mix(b0, t.clone()), a1.mix(b1, t.clone()), a2.mix(b2, t)])
            }
            // `rgb(r, g, b)`: a colour from three numbers, 0 to 1 each, which can be anything that is
            // a number: a fact, a property, a sum. It is how the logic gives the scene a colour (a
            // palette taken from a wallpaper) without writing a file for the scene to reload.
            Some(TokenKind::Id(m)) if m == "rgb" && matches!(c.tokens.get(c.i + 1).map(|x| &x.kind), Some(TokenKind::Sym("("))) => {
                c.i += 1;
                c.expect_sym("(")?;
                let r = self.expr(c)?;
                c.expect_sym(",")?;
                let g = self.expr(c)?;
                c.expect_sym(",")?;
                let b = self.expr(c)?;
                c.expect_sym(")")?;
                Ok([r, g, b])
            }
            // `pick(i, #a, #b, #c)`: the one at place i, channel by channel.
            Some(TokenKind::Id(m)) if m == "pick" => {
                c.i += 1;
                c.expect_sym("(")?;
                let i = self.expr(c)?;
                let mut options = Vec::new();
                while c.sym(",") {
                    options.push(self.color(c)?);
                }
                c.expect_sym(")")?;
                if options.is_empty() {
                    return c.error("`pick` needs a place and colours to choose from: `pick(i, #a, #b)`");
                }
                let channel = |k: usize| Expr::Pick(Box::new(i.clone()), options.iter().map(|o: &Color| o[k].clone()).collect()).folded_pub();
                Ok([channel(0), channel(1), channel(2)])
            }
            // `if(cond, #a, #b)`: choosing a colour is mixing with 0 or 1, as with numbers.
            Some(TokenKind::Id(m)) if m == "if" => {
                c.i += 1;
                c.expect_sym("(")?;
                let cond = self.expr(c)?;
                c.expect_sym(",")?;
                let a = self.color(c)?;
                c.expect_sym(",")?;
                let b = self.color(c)?;
                c.expect_sym(")")?;
                let [a0, a1, a2] = a;
                let [b0, b1, b2] = b;
                Ok([b0.mix(a0, cond.clone()), b1.mix(a1, cond.clone()), b2.mix(a2, cond)])
            }
            _ => c.error("expected a colour here: #151616, the name of one, mix(#a, #b, how much), if(condition, #a, #b) or rgb(r, g, b)"),
        }
    }

    // ── the block of a node, seen as properties ─────────────────

    fn properties<'n>(&self, n: &'n Node, valid: &[&str]) -> R<HashMap<&'n str, Cur<'n>>> {
        let mut m = HashMap::new();
        for e in n.body.as_deref().unwrap_or(&[]) {
            if let Entry::Prop { name, value, line, col } = e {
                if !valid.contains(&name.as_str()) {
                    let v: Vec<String> = valid.iter().map(|s| s.to_string()).collect();
                    let hint = closest_match(name, v.iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
                    return Err(CompileError::at(*line, *col, format!("'{name}' does not exist here.{hint} Valid ones: {}", valid.join(", "))));
                }
                // Given twice, the second one swallowed the first without saying
                // anything: in marea-plm that left the audio service without permissions
                // —two `services:` lines, one per topic, and the one below won—
                // and a while was spent looking for it in the wrong place.
                let mut c = Cur::new(value, *line, *col);
                c.slot = true;
                if m.insert(name.as_str(), c).is_some() {
                    return Err(CompileError::at(*line, *col, format!("'{name}' is said twice here, and only the last one would count. Put it once, with everything it carries")));
                }
            }
        }
        Ok(m)
    }

    // ── shapes ──────────────────────────────────────────────────

    /// `ellipse orb { at: …; radius: … }` → the shape, its name, and whatever paint it carries.
    fn shape(&mut self, n: &Node, start: usize) -> R<ParsedShape> {
        let mut c = Cur::new(&n.head[start..], n.line, n.col);
        let class = c.id("a shape: ellipse, box, arc or line")?;
        let name = if c.at_end() { None } else { Some(c.id("a name for the shape")?) };
        c.expect_end()?;
        let common = vocab::properties("shape");
        let in_slot = std::mem::take(&mut self.in_slot);
        let own_props: &[&str] = match class.as_str() {
            "ellipse" | "box" | "arc" | "line" | "path" => vocab::properties(&class),
            other => return Err(CompileError::at(n.line, n.col, format!("I don\'t know the shape '{other}': there are ellipse, box, arc, line and path"))),
        };
        let valid: Vec<&str> = own_props.iter().chain(common.iter()).copied().collect();
        let mut p = self.properties(n, &valid)?;
        let missing = |what: &str| CompileError::at(n.line, n.col, format!("this '{class}' is missing '{what}'"));
        let glass_spec = self.glass_spec(&mut p, n)?;
        let mut one = |o: &Compiler, k: &str| -> R<Option<Expr>> {
            match p.get_mut(k) {
                Some(c) => {
                    let e = o.expr(c)?;
                    c.expect_end()?;
                    Ok(Some(e))
                }
                None => Ok(None),
            }
        };
        let (rotate, stroke, opacity, blend, active) = (one(self, "rotate")?, one(self, "stroke")?, one(self, "opacity")?, one(self, "blend")?, one(self, "active")?);
        // `show:` is "there or not there": it switches off entirely, and its zone with it. Pressing
        // what cannot be seen is worse than not being able to press it.
        let visible = one(self, "show")?;
        // `yields: true`: a zone under which a program's widget on the desktop (a
        // layer-shell surface on the bottom level) gets the press first, where
        // there is one; it still gets the presses on the bare desktop.
        let yields = one(self, "yields")?;
        let opacity = match (&visible, opacity) {
            (Some(v), Some(o)) => Some(o * v.clone().clamp(0.0, 1.0)),
            (Some(v), None) => Some(v.clone().clamp(0.0, 1.0)),
            (None, o) => o,
        };
        let active = match (&visible, active) {
            (Some(v), Some(a)) => Some(a * v.clone().clamp(0.0, 1.0)),
            (Some(v), None) => Some(v.clone().clamp(0.0, 1.0)),
            (None, a) => a,
        };
        let (radius, corner, span, width) = (one(self, "radius")?, one(self, "corner")?, one(self, "span")?, one(self, "width")?);
        let mut two = |o: &Compiler, k: &str| -> R<Option<Point>> {
            match p.get_mut(k) {
                Some(c) => {
                    let e = o.point(c)?;
                    c.expect_end()?;
                    Ok(Some(e))
                }
                None => Ok(None),
            }
        };
        let (at, from, size, scale, to) = (two(self, "at")?, two(self, "from")?, two(self, "size")?, two(self, "scale")?, two(self, "to")?);
        let color = match p.get_mut("color") {
            Some(c) => Some(self.color(c)?),
            None => None,
        };
        let (cursor, cursor_when) = match p.get_mut("cursor") {
            Some(c) => self.cursor(c)?,
            None => (Cursor::Normal, None),
        };
        // `carries: "file://{path}"`: dragged out of the scene, it goes to
        // another program as that (a file, a link, a text).
        let carries = match p.get_mut("carries") {
            Some(c) => match c.peek() {
                Some(TokenKind::Str(s)) => {
                    let s = s.clone();
                    let k = self.content_of_one(&s, &c.tokens[c.i])?;
                    c.i += 1;
                    c.expect_end()?;
                    Some(k)
                }
                _ => return c.error("`carries:` takes a text, with holes if it needs them: `carries: \"file://{path}\"`"),
            },
            None => None,
        };
        // `carry_icon: "{c.icon}"`: the path of a PNG that goes under the pointer while `carries:` is dragged out.
        let carry_icon = match p.get_mut("carry_icon") {
            Some(c) => match c.peek() {
                Some(TokenKind::Str(s)) => {
                    let s = s.clone();
                    let k = self.content_of_one(&s, &c.tokens[c.i])?;
                    c.i += 1;
                    c.expect_end()?;
                    Some(k)
                }
                _ => return c.error("`carry_icon:` takes a text with the path of a PNG, with holes if it needs them: `carry_icon: \"{file.icon}\"`"),
            },
            None => None,
        };
        let label = match p.get_mut("label") {
            Some(c) => Some(self.label(c)?),
            None => None,
        };
        let reach = match p.get_mut("agent") {
            Some(c) => reach(c)?,
            None => Reach::Any,
        };
        let mut told = Told::default();
        if let Some(c) = p.get_mut("role") {
            let r = c.one_of(vocab::ROLES, "what it is")?;
            c.expect_end()?;
            told.role = vocab::ROLES.iter().find(|x| **x == r).copied();
        }
        // `value: "{volume * 100} %"`, a text; or `value: volume`, a number (a fact with names, its name).
        if let Some(c) = p.get_mut("value") {
            told.value = Some(match c.peek() {
                Some(TokenKind::Str(s)) => {
                    let s = s.clone();
                    let at = c.i;
                    c.i += 1;
                    c.expect_end()?;
                    Said::Text(self.content_of(&s, &c.tokens[at])?)
                }
                _ => {
                    let e = self.expr(c)?;
                    c.expect_end()?;
                    Said::Number(e)
                }
            });
        }
        for (k, slot) in [("checked", &mut told.checked), ("selected", &mut told.selected)] {
            if let Some(c) = p.get_mut(k) {
                let e = self.expr(c)?;
                c.expect_end()?;
                *slot = Some(e);
            }
        }
        let told = (told.role.is_some() || told.value.is_some() || told.checked.is_some() || told.selected.is_some()).then(|| Box::new(told));
        // `handle: i`, or `handle: i, left`: a window's handle (see `Zone::handle`).
        let handle = match p.get_mut("handle") {
            Some(c) => {
                let window = self.expr(c)?;
                let edges = if c.sym(",") {
                    let names = ["top", "bottom", "left", "right", "top_left", "top_right", "bottom_left", "bottom_right"];
                    match c.one_of(&names, "the edge it stretches")?.as_str() {
                        "top" => 1,
                        "bottom" => 2,
                        "left" => 4,
                        "right" => 8,
                        "top_left" => 5,
                        "top_right" => 9,
                        "bottom_left" => 6,
                        _ => 10,
                    }
                } else {
                    0
                };
                c.expect_end()?;
                Some(Box::new(crate::scene::Handle { window, edges }))
            }
            None => None,
        };
        let mut extent: Option<(Expr, Expr)> = None;
        let mut shape = match class.as_str() {
            "ellipse" => {
                let radius = radius.ok_or_else(|| missing("radius"))?;
                let scale = scale.unwrap_or((1.0.into(), 1.0.into()));
                extent = Some((radius.clone() * 2.0 * scale.0.clone(), radius.clone() * 2.0 * scale.1.clone()));
                // In a slot, attached to its corner.
                let center = match at {
                    Some(a) => a,
                    None if in_slot => (radius.clone() * scale.0.clone(), radius.clone() * scale.1.clone()),
                    None => return Err(missing("at")),
                };
                Shape::Ellipse { center, radius, scale }
            }
            "box" => {
                let (w, h) = size.ok_or_else(|| missing("size"))?;
                extent = Some((w.clone(), h.clone()));
                let center = match (at, from) {
                    (None, None) if in_slot => (w.clone() * 0.5, h.clone() * 0.5),
                    (Some(a), None) => a,
                    (None, Some((x, y))) => (x + w.clone() * 0.5, y + h.clone() * 0.5),
                    _ => return Err(CompileError::at(n.line, n.col, "a box is placed with 'at' (its centre) or with 'from' (its corner), one of the two")),
                };
                Shape::Rect { center, half_size: (w * 0.5, h * 0.5), radius: corner.unwrap_or(Expr::K(0.0)) }
            }
            // `path { move 0, 0; line 20, 10; curve 40, 0 via 32, 10; close }`
            "path" => {
                let base = at.unwrap_or((Expr::K(0.0), Expr::K(0.0)));
                let offset_point = |o: &Compiler, c: &mut Cur| -> R<Point> {
                    let p = o.point(c)?;
                    Ok((base.0.clone() + p.0, base.1.clone() + p.1))
                };
                let (mut origin, mut steps, mut closed) = (None, Vec::new(), false);
                for e in n.body.as_deref().unwrap_or(&[]) {
                    let Entry::Node(x) = e else { continue };
                    let mut c = Cur::new(&x.head, x.line, x.col);
                    match c.one_of(vocab::PATH_COMMANDS, "a step of a path")?.as_str() {
                        "move" => {
                            if origin.is_some() || !steps.is_empty() {
                                return Err(CompileError::at(x.line, x.col, "a path starts at one place: `move` goes first, and only once. For several strokes, several `path`"));
                            }
                            origin = Some(offset_point(self, &mut c)?);
                        }
                        "line" => steps.push(PathStep::Line(offset_point(self, &mut c)?)),
                        "curve" => {
                            let a = offset_point(self, &mut c)?;
                            c.expect_word("via")?;
                            steps.push(PathStep::Curve { via: offset_point(self, &mut c)?, to: a });
                        }
                        _ => closed = true,
                    }
                    c.expect_end()?;
                }
                if steps.is_empty() {
                    return Err(CompileError::at(n.line, n.col, "a path goes somewhere: it needs at least one `line` or one `curve`"));
                }
                if steps.len() + 1 > crate::shapes::MAX_POINTS {
                    return Err(CompileError::at(n.line, n.col, format!("a path has at most {} steps", crate::shapes::MAX_POINTS - 1)));
                }
                extent = size;
                Shape::Path { origin: origin.unwrap_or(base), steps, closed }
            }
            "arc" => Shape::Arc { center: at.ok_or_else(|| missing("at"))?, radius: radius.ok_or_else(|| missing("radius"))?, opening: span.ok_or_else(|| missing("span"))? * 0.5, thickness: width.ok_or_else(|| missing("width"))? },
            _ => Shape::Segment { from: from.ok_or_else(|| missing("from"))?, to: to.ok_or_else(|| missing("to"))?, thickness: width.ok_or_else(|| missing("width"))? },
        };
        if let Some(a) = rotate {
            shape = shape.rotated(a);
        }
        if let Some(w) = stroke {
            shape = shape.stroke(w);
        }
        // A shape with a name can be a zone, with the transforms under
        // which it is painted. Whether it is or not is decided at the end: see `materialize_zones`.
        if let Some(name) = &name {
            let name = &self.declare_zone(name);
            self.candidates.push(Candidate { name: name.clone(), at: self.e.instrs.len(), zblock: self.zblock, shape: shape.clone(), active, visible: None, under: self.under.clone(), forced: carries.is_some() || handle.is_some(), cursor, cursor_when, carries, carry_icon, label, reach, within: None, told, handle, yields });
        }
        Ok(ParsedShape { shape, color, opacity, blend, size: extent, glass_spec })
    }

    // ── what gets painted ───────────────────────────────────────

    /// A group: its properties (transform, opacity) and its children in order.
    fn group(&mut self, entries: impl Iterator<Item = &'a Entry>) {
        let entries: Vec<&'a Entry> = entries.collect();
        // The zones of this group that are named with `.hover` or `.pressed`
        // get their springs before anything is read: a zone goes after what it
        // lights —the press belongs to the one declared last—, so its `.hover`
        // is used above it.
        for e in &entries {
            let Entry::Node(n) = e else { continue };
            self.zone_springs_prescan(n);
        }
        let mut clips = 0;
        for e in entries {
            let Entry::Node(n) = e else { continue };
            // An error does not stop the reading: it is noted and reading goes on, to report them all.
            let r = self.statement(n, &mut clips);
            let mut left = LEFTOVER.with(|l| std::mem::take(&mut *l.borrow_mut()));
            left.sort_unstable();
            match r {
                Err(f) => self.push_error(f),
                // Refused, as a plain number's slot refuses what follows it: it
                // was only noted, and the scene loaded drawn without it
                // (`padding: 6, 9` was a 6), so the same mistake had two
                // answers (issue #68).
                Ok(()) => {
                    for (l, c) in left {
                        if LEFTOVER_SAID.with(|s| s.borrow_mut().insert((l, c))) {
                            self.push_error(CompileError::at(l, c, "this is left over here: what follows the value would be dropped"));
                        }
                    }
                }
            }
        }
        // A clip holds until the end of its group.
        for _ in 0..clips {
            self.e.paint(Instr::Clip(None));
        }
    }

    fn statement(&mut self, n: &'a Node, clips: &mut usize) -> R<()> {
        if is_definition(n) {
            return self.element(n);
        }
        if is_fold(n) {
            return self.fold(n);
        }
        {
            let mut c = Cur::new(&n.head, n.line, n.col);
            let word = c.id("a declaration")?;
            if !vocab::STATEMENTS.contains(&word.as_str()) && !self.components.contains_key(&word) {
                let valid: Vec<String> = vocab::STATEMENTS.iter().map(|s| s.to_string()).chain(self.components.keys().cloned()).collect();
                let hint = closest_match(&word, valid.iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
                return Err(CompileError::at(n.line, n.col, format!("I don\'t know what '{word}' is.{hint}")));
            }
            self.current_class = word.clone();
            match word.as_str() {
                // Already read, before everything (see `read_translations`).
                "translations" => {}
                "surface" => {
                    // A surface with a name is another window: it starts clean. A loose
                    // `clip` of the scene reached the end of its group
                    // —that is, the end of the file— and also clipped what
                    // the other window drew, which lives far away on the same plane: nothing
                    // could be seen, and nothing said so. Open clips end here.
                    if n.body.as_deref().unwrap_or(&[]).iter().any(|e| matches!(e, Entry::Node(_))) {
                        for _ in 0..std::mem::take(clips) {
                            self.e.paint(Instr::Clip(None));
                        }
                    }
                    self.surface(n)?
                }
                "model" => self.model(n, &mut c)?,
                "service" => self.service(n, &mut c)?,
                "windows" => self.windows(n, &mut c)?,
                "permissions" => {
                    // permissions { run: "date", "notify-send";  services: "audio", "apps";  env: "PATH" }
                    let mut p = self.properties(n, vocab::properties("permissions"))?;
                    for (key, target) in [("run", 0), ("services", 1), ("env", 2)] {
                        let Some(c) = p.get_mut(key) else { continue };
                        let mut list = vec![c.string()?];
                        while c.sym(",") {
                            list.push(c.string()?);
                        }
                        c.expect_end()?;
                        let owner = match n.line / super::PER_FILE {
                            0 => &mut self.e.permissions,
                            k => self.permissions_of.entry(k).or_default(),
                        };
                        match target {
                            0 => owner.commands = list,
                            1 => owner.services = list,
                            _ => owner.env = list,
                        }
                    }
                }
                "spring" => {
                    let name = c.id("a name for the spring")?;
                    if n.line >= super::PER_FILE {
                        self.from_library.insert(name.clone());
                    }
                    c.expect_sym("=")?;
                    let stiffness = c.num()?;
                    c.expect_sym(",")?;
                    self.springs.insert(name, Spring { stiffness, damping: c.num()? });
                }
                "prop" | "pose" => {
                    let site = c.tokens.first().map_or((0, 0), |t| (t.line, t.col));
                    let name = self.declare(&c.id("a name for the property")?);
                    // Two declarations with one name are one property, moved by both:
                    // a `prop tide` next to the water's `pose tide` made the eyes
                    // swell with the tide, and nothing said why.
                    match self.prop_sites.get(&name) {
                        Some(&(line, col)) if (line, col) != site => {
                            return c.error(format!("'{name}' is already a property, declared on line {}: two with the same name would be the same one, and whatever moves one would move the other. Give this one another name", line % super::PER_FILE));
                        }
                        _ => {
                            self.prop_sites.insert(name.clone(), site);
                        }
                    }
                    c.expect_sym("=")?;
                    let v = c.num()?;
                    let spring = if c.sym("~") { self.spring(&mut c)? } else if word == "pose" { Spring::POSE } else { Spring::LIVELY };
                    c.expect_end()?;
                    let p = match self.props.get(&name) {
                        // Already declared: it is the same one. It happens when a `repeat` with declarations
                        // falls inside something that repeats, like a surface per monitor.
                        Some(existing) => *existing,
                        None => {
                            let p = self.e.prop_with(interned(&name), v, spring);
                            if word == "pose" {
                                self.e.pose.push(p);
                            }
                            p
                        }
                    };
                    self.props.insert(name, p);
                }
                "fact" => {
                    // fact open = false · fact count: number = 0 · fact mode: low | normal | critical = normal
                    let name = self.declare(&c.id("a name for the fact")?);
                    if name == "locale" && self.e.locale.is_some() {
                        return c.error("`locale` already exists: it comes with `translations`, and says which language the texts are shown in. Give this fact another name");
                    }
                    let declared = if c.sym(":") { Some(self.fact_type(&mut c)?) } else { None };
                    c.expect_sym("=")?;
                    let (v, kind) = match declared {
                        Some(Some(FactType::Enum(names))) => {
                            let which = c.one_of(&names.iter().map(String::as_str).collect::<Vec<_>>(), "a value of this fact")?;
                            (names.iter().position(|x| *x == which).unwrap() as f32, Some(FactType::Enum(names)))
                        }
                        Some(Some(FactType::Bool)) => (if c.word("true") { 1.0 } else if c.word("false") { 0.0 } else { return c.error("a `bool` fact is true or false") }, Some(FactType::Bool)),
                        Some(None) => (if c.sym("-") { -c.num()? } else { c.num()? }, None),
                        // Without a type, whatever it is worth says it: `true` and `false` make a yes or no; a number, a number.
                        None => if c.word("true") { (1.0, Some(FactType::Bool)) } else if c.word("false") { (0.0, Some(FactType::Bool)) } else { (if c.sym("-") { -c.num()? } else { c.num()? }, None) },
                    };
                    c.expect_end()?;
                    let h = match self.facts.get(&name) {
                        Some(existing) => *existing,
                        None => {
                            if let Some(t) = kind {
                                self.e.types.push((name.clone(), t));
                            }
                            self.e.fact(interned(&name), v)
                        }
                    };
                    self.facts.insert(name, h);
                }
                "event" => {
                    let name = self.declare(&c.id("a name for the event")?);
                    let outgoing = c.sym("->");
                    c.expect_end()?;
                    let s = match self.signals.get(&name) {
                        Some(existing) => *existing,
                        None if outgoing => self.e.outgoing_signal(interned(&name)),
                        None => self.e.signal(interned(&name)),
                    };
                    self.signals.insert(name, s);
                }
                "text" if matches!(c.tokens.get(2).map(|x| &x.kind), Some(TokenKind::Sym("="))) => {
                    let name = self.declare(&c.id("a name for the text")?);
                    c.expect_sym("=")?;
                    let value = c.string()?;
                    let versions = self.versions_of(&value);
                    let t = match self.texts.get(&name) {
                        Some(existing) => *existing,
                        None => {
                            // Born in the language the scene is shown in.
                            let start = self.e.locale.map_or(0, |l| self.e.facts[l.0 as usize].1 as usize);
                            let t = self.e.live_text(interned(&name), versions.as_ref().map_or(&value, |v| &v[start]));
                            if let Some(v) = versions {
                                self.e.text_versions.push((t, v));
                            }
                            t
                        }
                    };
                    self.texts.insert(name, t);
                }
                "image" if matches!(c.tokens.get(2).map(|x| &x.kind), Some(TokenKind::Sym("="))) => {
                    let name = self.declare(&c.id("a name for the image")?);
                    c.expect_sym("=")?;
                    let source = if c.word("icon") {
                        ImageSource::Icon(c.string()?)
                    } else if c.word("file") {
                        // Relative to the file that writes it, not to where it is launched from: that way a
                        // library carries its images with it.
                        let written = std::path::PathBuf::from(c.string()?);
                        let dir = self.dirs.get(n.line / super::PER_FILE);
                        ImageSource::File(match dir { Some(k) if written.is_relative() => k.join(written), _ => written })
                    } else if c.word("pixels") {
                        // The same, for pixel art: made bigger pixel by pixel.
                        let written = std::path::PathBuf::from(c.string()?);
                        let dir = self.dirs.get(n.line / super::PER_FILE);
                        ImageSource::Pixels(match dir { Some(k) if written.is_relative() => k.join(written), _ => written })
                    } else if c.word("from") {
                        // Whichever a live text says: that is how the logic chooses an image.
                        let t = self.global(&c.id("the name of a text")?);
                        match self.texts.get(&t) {
                            Some(t) => ImageSource::Live(*t),
                            None => return self.unknown(&c, "no text", &t, self.texts.keys().collect()),
                        }
                    } else {
                        return c.error("an image is `icon \"name\"`, `file \"path\"`, `pixels \"path\"` or `from some_text`");
                    };
                    c.expect_sym(",")?;
                    let w = c.num()?;
                    c.expect_sym(",")?;
                    let h = c.num()?;
                    // Read again from the same line (a `repeat` inside a surface per
                    // monitor), it is the same image, not one more to load.
                    let site = (n.line, n.col);
                    let i = match self.image_sites.get(&name) {
                        Some((at, i)) if *at == site => *i,
                        _ => self.e.image(source, w as u32, h as u32),
                    };
                    self.image_sites.insert(name.clone(), (site, i));
                    self.images.insert(name, i);
                }
                // `figure hat = file "wardrobe/hat.svg"`: its layers, as paths.
                "figure" if matches!(c.tokens.get(2).map(|x| &x.kind), Some(TokenKind::Sym("="))) => {
                    let name = self.declare(&c.id("a name for the figure")?);
                    c.expect_sym("=")?;
                    if !c.word("file") {
                        return c.error("a figure comes from a file: `figure hat = file \"hat.svg\"`");
                    }
                    // Relative to the file that writes it, like images: that way
                    // a library takes its pieces with it.
                    let written = std::path::PathBuf::from(c.string()?);
                    let dir = self.dirs.get(n.line / super::PER_FILE);
                    let path = match dir {
                        Some(k) if written.is_relative() => k.join(written),
                        _ => written,
                    };
                    let data = std::fs::read(&path).map_err(|e| CompileError::at(n.line, n.col, format!("that svg cannot be read: {e}")))?;
                    let (fig, warnings) = super::figure::read(&data).map_err(|m| CompileError::at(n.line, n.col, m))?;
                    for a in warnings {
                        eprintln!("figure · {}: {a}", path.display());
                    }
                    self.e.attachments.push(path);
                    self.figures.insert(name, fig);
                }
                // `font file "fonts/pixel.ttf"`: a font the scene brings with it,
                // used by its family's name as if it were installed. A game or a
                // theme carries its lettering, and nothing is put in the system.
                "font" => {
                    if !c.word("file") {
                        return c.error("a font comes from a file: `font file \"fonts/pixel.ttf\"`, and is then used by its family's name");
                    }
                    let written = std::path::PathBuf::from(c.string()?);
                    let dir = self.dirs.get(n.line / super::PER_FILE);
                    let path = match dir {
                        Some(k) if written.is_relative() => k.join(written),
                        _ => written,
                    };
                    let mut head = [0u8; 4];
                    std::io::Read::read_exact(&mut std::fs::File::open(&path).map_err(|e| CompileError::at(n.line, n.col, format!("that font cannot be read: {e}")))?, &mut head)
                        .map_err(|e| CompileError::at(n.line, n.col, format!("that font cannot be read: {e}")))?;
                    if !matches!(&head, [0, 1, 0, 0] | b"OTTO" | b"true" | b"ttcf") {
                        return Err(CompileError::at(n.line, n.col, "that is not a font: a TrueType or OpenType file (.ttf, .otf, .ttc)".to_string()));
                    }
                    if !self.e.fonts.contains(&path) {
                        self.e.fonts.push(path);
                    }
                }
                // `shader aurora = file "aurora.wgsl"`: a function of its own in
                // WGSL that paints a box. Read and checked now, like an svg.
                "shader" if matches!(c.tokens.get(2).map(|x| &x.kind), Some(TokenKind::Sym("="))) => {
                    let name = self.declare(&c.id("a name for the shader")?);
                    c.expect_sym("=")?;
                    if !c.word("file") {
                        return c.error("a shader comes from a file: `shader aurora = file \"aurora.wgsl\"`");
                    }
                    let shown = c.string()?;
                    let written = std::path::PathBuf::from(&shown);
                    let dir = self.dirs.get(n.line / super::PER_FILE);
                    let path = match dir {
                        Some(k) if written.is_relative() => k.join(written),
                        _ => written,
                    };
                    if self.e.shaders.len() >= 64 {
                        return c.error("more than 64 shaders in one scene: one that takes parameters goes further than 64 files");
                    }
                    let shader = crate::shaders::load(&path, &shown, self.e.shaders.len()).map_err(|m| CompileError::at(n.line, n.col, m))?;
                    self.e.attachments.push(path);
                    self.shaders.insert(name, self.e.shaders.len() as u16);
                    self.e.shaders.push(shader);
                }
                // `blur = file "blur.wgsl" { passes: down 0.5 repeat 4, up 2 repeat 3; values: …; reach: 34 }`:
                // the frosting behind every glass of the scene, as passes of its own (`blur.rs`).
                "blur" if matches!(c.tokens.get(1).map(|x| &x.kind), Some(TokenKind::Sym("="))) => {
                    c.expect_sym("=")?;
                    if !c.word("file") {
                        return c.error("a blur comes from a file: `blur = file \"blur.wgsl\" { passes: down 0.5 repeat 4, up 2 repeat 3 }`");
                    }
                    let shown = c.string()?;
                    c.expect_end()?;
                    let written = std::path::PathBuf::from(&shown);
                    let path = match self.dirs.get(n.line / super::PER_FILE) {
                        Some(k) if written.is_relative() => k.join(written),
                        _ => written,
                    };
                    let mut p = self.properties(n, vocab::properties("blur"))?;
                    // Each pass: its function, its size next to the one before,
                    // `repeat n` and `keep k`, separated by commas.
                    let mut passes = Vec::new();
                    let Some(pc) = p.get_mut("passes") else {
                        return Err(CompileError::at(n.line, n.col, "a blur says its passes: `passes: down 0.5 repeat 4, up 2 repeat 3`".to_string()));
                    };
                    loop {
                        let function = pc.id("the function of a pass")?;
                        let scale = pc.num()?;
                        let repeat = if pc.word("repeat") { pc.num()?.max(1.0) as u32 } else { 1 };
                        let keep = if pc.word("keep") {
                            let k = pc.num()?;
                            if !(0.0..=3.0).contains(&k) {
                                return pc.error("a pass keeps its picture in one of four slots, 0 to 3");
                            }
                            Some(k as u8)
                        } else {
                            None
                        };
                        passes.push(crate::blur::Pass { function, scale, repeat, keep });
                        if !pc.sym(",") {
                            break;
                        }
                    }
                    pc.expect_end()?;
                    // Up to eight numbers, `s.a` and `s.b`, worked out every frame.
                    let mut values = Vec::new();
                    if let Some(vc) = p.get_mut("values") {
                        loop {
                            values.push(self.expr(vc)?);
                            if !vc.sym(",") {
                                break;
                            }
                        }
                        if values.len() > 8 {
                            return vc.error("a blur takes up to eight numbers: `s.a` and `s.b`, four each");
                        }
                    }
                    let reach = match p.get_mut("reach") {
                        Some(rc) => rc.num()?,
                        // Enough for a blur of a few passes; one that reaches further says so.
                        None => 32.0,
                    };
                    let plan = crate::blur::load(&path, &shown, &passes, reach).map_err(|m| CompileError::at(n.line, n.col, m))?;
                    // A scene copied per monitor reads it once per copy: the same one again is fine.
                    if self.e.blur.as_ref().is_some_and(|b| *b != plan) {
                        return Err(CompileError::at(n.line, n.col, "a scene has one blur: this one would replace the one above".to_string()));
                    }
                    self.e.attachments.push(path);
                    self.e.blur = Some(plan);
                    self.e.blur_values = values;
                }
                // `lens = image "~/.cache/wallpaper.png"`: the glass bends this picture (the wallpaper, any size)
                // instead of a capture of what is behind, which costs a screencopy and a readback each time something moves.
                "lens" if matches!(c.tokens.get(1).map(|x| &x.kind), Some(TokenKind::Sym("="))) => {
                    c.expect_sym("=")?;
                    if !c.word("image") {
                        return c.error("a lens bends a picture: `lens = image \"wallpaper.png\"`");
                    }
                    let shown = c.string()?;
                    // `while expr`: the picture only while it holds; otherwise the glass captures what is behind, as without a lens.
                    let when = if c.word("while") { Some(self.expr(&mut c)?) } else { None };
                    c.expect_end()?;
                    let written = match shown.strip_prefix("~/") {
                        Some(rest) => match std::env::var_os("HOME") {
                            Some(h) => std::path::PathBuf::from(h).join(rest),
                            None => return Err(CompileError::at(n.line, n.col, "`~` needs HOME, which is not set".to_string())),
                        },
                        None => std::path::PathBuf::from(&shown),
                    };
                    let path = match self.dirs.get(n.line / super::PER_FILE) {
                        Some(k) if written.is_relative() => k.join(written),
                        _ => written,
                    };
                    // A scene copied per monitor reads it once per copy: the same one again is fine.
                    if self.e.lens_image.as_ref().is_some_and(|p| *p != path) {
                        return Err(CompileError::at(n.line, n.col, "a scene has one lens picture: this one would replace the one above".to_string()));
                    }
                    self.e.lens_image = Some(path);
                    self.e.lens_image_while = when;
                }
                // `lens every 250ms`: at most one capture of what is behind the glass that often.
                // `lens hold while expr`: none at all while it holds, one as soon as it stops.
                // A scene copied per monitor reads them once per copy: the last one stays.
                "lens" if matches!(c.tokens.get(1).map(|x| &x.kind), Some(TokenKind::Id(w)) if w == "every" || w == "hold") => {
                    if c.word("every") {
                        let at = c.i;
                        let every = self.expr(&mut c)?;
                        c.expect_end()?;
                        if let Expr::K(s) = &every {
                            let (least, most) = crate::lens::INTERVAL_RANGE;
                            if !(least..=most).contains(s) {
                                c.i = at;
                                return c.error(format!("a lens captures every {}ms to {}s: write the time with its unit, like `lens every 250ms`; `lens hold while …` stops the captures", (least * 1000.0).round(), most));
                            }
                        }
                        self.e.lens_every = Some(every);
                    } else {
                        c.expect_word("hold")?;
                        if !c.word("while") {
                            return c.error("a lens holds its captures while something holds: `lens hold while opening`");
                        }
                        let when = self.expr(&mut c)?;
                        c.expect_end()?;
                        self.e.lens_hold_while = Some(when);
                    }
                }
                "measure" => {
                    let local = c.id("a name for the measure")?;
                    let name = self.declare(&local);
                    let part = self.interpolate_in(&local);
                    if let Some(e) = self.scopes.last_mut().map(Rc::make_mut) {
                        e.with_parts.insert(part);
                    }
                    // Brought forward already (a component's `size:` reads it): the same one.
                    let (w, h) = match self.measurements.get(&name) {
                        Some(&known) => known,
                        None => self.e.measured(interned(&name)),
                    };
                    self.props.insert(format!("{name}.width"), w);
                    self.props.insert(format!("{name}.height"), h);
                    self.measurements.insert(name, (w, h));
                }
                "let" => {
                    let name = c.id("a name")?;
                    if c.sym("[") {
                        return self.array(n, &name, &mut c);
                    }
                    if n.line >= super::PER_FILE {
                        self.from_library.insert(name.clone());
                    }
                    // Two `let`s with the same name in the same file: the second one
                    // changed what the first one means in everything below, without
                    // a word. In marea-plm, a `let out` for "something is open"
                    // stepped on the `let out = place` 2,700 lines further up, and she
                    // dropped 46 px every time something opened. Reading the same line
                    // again —once per screen copy— does not count.
                    if self.scopes.is_empty() {
                        if let Some(&before) = self.let_lines.get(&name) {
                            if before != n.line && before / super::PER_FILE == n.line / super::PER_FILE {
                                return Err(CompileError::at(n.line, n.col, format!("there is already a `let {name}`, on line {}: a second one would change what the first means in everything below it. Give this one another name", before % super::PER_FILE)));
                            }
                        }
                        self.let_lines.insert(name.clone(), n.line);
                    }
                    // A `let` with the name of a fact or a prop of the scene hid it,
                    // without a word, from everything that read it: Bahía's
                    // Settings measured its pane with `let pw`, and the dialog that
                    // asks before restarting —`fact pw`— took 496 for "open".
                    // (In a component or a `repeat` the `let` is theirs.) Said, not
                    // refused: a scene that has one still loads.
                    // And one in another file of the scene —two parts— did the
                    // same: it changed what the first one means in everything
                    // after it. Copied per monitor it was worse, each copy took
                    // one or the other: Bahía's ⇧⌘5 bar (`let sbw = 556`) drew its
                    // glass 214 wide, Settings' sidebar, and placed its buttons
                    // for its own width. Said, not refused, like the next one:
                    // scenes that have one still load.
                    if self.scopes.iter().all(|e| e.suffix.starts_with("#screen")) {
                        let before = self.scene_let_lines.borrow_mut().insert(name.clone(), n.line);
                        if let Some(before) = before.filter(|b| b / super::PER_FILE != n.line / super::PER_FILE) {
                            if self.twice_noted.borrow_mut().insert(name.clone()) {
                                let file = |line: usize| self.files.get(line / super::PER_FILE).map_or("", String::as_str);
                                eprintln!("note   · {}:{}: there is already a `let {name}` in {}:{}: this one changes what `{name}` means in everything after it, and in a scene copied per monitor each copy may take one or the other. Give one of them another name", file(n.line), n.line % super::PER_FILE, file(before), before % super::PER_FILE);
                            }
                        }
                    }
                    if self.scopes.iter().all(|e| e.suffix.starts_with("#screen")) && self.hiding_noted.borrow_mut().insert(n.line) {
                        let what = if self.facts.contains_key(&name) { Some("fact") } else if self.props.contains_key(&name) { Some("prop") } else { None };
                        if let Some(what) = what {
                            let file = self.files.get(n.line / super::PER_FILE).map_or("", String::as_str);
                            eprintln!("note   · {file}:{}: there is already a `{what} {name}`: this `let` hides it from everything that reads `{name}` after it. Give one of them another name", n.line % super::PER_FILE);
                        }
                    }
                    // A group does not keep its `let`s: one written in it is seen
                    // by everything after it, outside too. One with the name of a
                    // `let` written outside any group took that one's place below:
                    // Marea's focus page wrote `let cx` for its dial, and her body's
                    // zone, further down, took the dial's centre for hers —a
                    // hundred pixels from where she was drawn, and nobody could
                    // click her—. (Inside a `repeat`, a `for` or a component the
                    // `let` is theirs, and steps on nothing.)
                    let in_group = self.groups.iter().any(|&d| d == self.scopes.len());
                    // In its own scope; and in a monitor's copy of the scene, which
                    // lasts as long as the scene, the scene's too. In a component
                    // or a `repeat` it goes with them, and outside it steps on nothing.
                    let loose = match self.scopes.last() {
                        Some(e) if e.suffix.starts_with("#screen") => e.loose.get(&name).or_else(|| self.loose_lets.get(&name)),
                        Some(e) => e.loose.get(&name),
                        None => self.loose_lets.get(&name),
                    }
                    .copied();
                    match loose {
                        Some(before) if in_group && before != n.line => {
                            let at = if before / super::PER_FILE == n.line / super::PER_FILE {
                                format!("line {}", before % super::PER_FILE)
                            } else {
                                let file = self.files.get(before / super::PER_FILE).map_or("", |f| f.rsplit('/').next().unwrap_or(f));
                                format!("{file}, line {}", before % super::PER_FILE)
                            };
                            return Err(CompileError::at(n.line, n.col, format!("there is already a `let {name}` outside any group ({at}), and a group does not keep its `let`s: this one would change what `{name}` means in everything after it, outside the group too. Give this one another name")));
                        }
                        _ if !in_group => match self.scopes.last_mut().map(Rc::make_mut) {
                            Some(e) => {
                                e.loose.insert(name.clone(), n.line);
                            }
                            None => {
                                self.loose_lets.insert(name.clone(), n.line);
                            }
                        },
                        _ => {}
                    }
                    c.expect_sym("=")?;
                    // `let mint = #9ed6bd`: a colour with a name.
                    // `mix` works for colours and for arithmetic, so it looks
                    // INWARD until the first thing that is not another `mix`: that is the one
                    // that says what this is about. Before, only one level was looked at, and
                    // `mix(mix(#a, #b, x), #c, y)` —four categories, which is what
                    // a notification tray asks for— was read as arithmetic.
                    // `pick(i, #a, #b)` and `if(cond, #a, #b)` too: there it is their first choice
                    // that says it, past the place or the condition (which can be any sum).
                    let is_color = {
                        let mut k = c.i;
                        // Inside a `pick` or an `if`, a colour's name is the choice only if it is the
                        // whole of it: `if(t == 0, skin == lens, …)` chooses numbers, `skin` or not.
                        let mut in_choice = false;
                        loop {
                            let call = |name: &str| matches!(c.tokens.get(k).map(|x| &x.kind), Some(TokenKind::Id(m)) if m == name)
                                && matches!(c.tokens.get(k + 1).map(|x| &x.kind), Some(TokenKind::Sym("(")));
                            if call("mix") {
                                k += 2;
                            } else if call("pick") || call("if") {
                                // Past its first argument: to the comma at its own depth.
                                let mut depth = 0i32;
                                let mut j = k + 2;
                                while let Some(t) = c.tokens.get(j) {
                                    match &t.kind {
                                        TokenKind::Sym("(") | TokenKind::Sym("[") => depth += 1,
                                        TokenKind::Sym(")") | TokenKind::Sym("]") if depth == 0 => break,
                                        TokenKind::Sym(")") | TokenKind::Sym("]") => depth -= 1,
                                        TokenKind::Sym(",") if depth == 0 => break,
                                        _ => {}
                                    }
                                    j += 1;
                                }
                                if !matches!(c.tokens.get(j).map(|x| &x.kind), Some(TokenKind::Sym(","))) {
                                    break;
                                }
                                k = j + 1;
                                in_choice = true;
                            } else {
                                break;
                            }
                        }
                        match c.tokens.get(k).map(|x| &x.kind) {
                            Some(TokenKind::Color(_)) => true,
                            Some(TokenKind::Id(n)) if n == "rgb" && matches!(c.tokens.get(k + 1).map(|x| &x.kind), Some(TokenKind::Sym("("))) => true,
                            Some(TokenKind::Id(n)) => {
                                (self.colors.contains_key(n) || self.scopes.iter().any(|e| e.colors.contains_key(n)))
                                    && (!in_choice || matches!(c.tokens.get(k + 1).map(|x| &x.kind), Some(TokenKind::Sym(",")) | Some(TokenKind::Sym(")"))))
                            }
                            _ => false,
                        }
                    };
                    if is_color {
                        let k = self.color(&mut c)?;
                        c.expect_end()?;
                        // A colour with a name is substituted where it is named too, and one
                        // that chooses among many —a theme's, a category's— is a tree per
                        // channel in every shape painted with it. Big, each channel is
                        // computed once per frame, like any other `let`. (Not one that is
                        // each monitor's copy's own: that one stays as it is written.)
                        let own_per_copy = self.scopes.is_empty() && k.iter().any(|e| self.is_per_copy(e));
                        let k = if own_per_copy { k } else { k.map(|e| self.shared(&name, e)) };
                        match self.scopes.last_mut().map(Rc::make_mut) {
                            Some(e) => e.colors.insert(name, k),
                            None => self.colors.insert(name, k),
                        };
                        return Ok(());
                    }
                    let e = self.expr(&mut c)?;
                    c.expect_end()?;
                    // A `let` is **substituted** where it is named, so a big
                    // one used three times is its tree three times. Written as a
                    // chain —each link naming the previous one twice, which
                    // is what comes out of writing `x + (k - x) * t`— the tree
                    // DOUBLES per link: marea-plm's face reached 2¹⁵ nodes
                    // per property, 41 ms per frame and 1 GB of memory, without
                    // anything saying so. From a handful of nodes upwards it is
                    // computed **once per frame** in a property, and what
                    // gets substituted is that: one node.
                    // And never one that reads something PER screen COPY —`screen.index`,
                    // `screen.width`…—: it is worth something different in each copy, and a
                    // property computed once would be the first one's for
                    // all of them. It stays substituted, which is what makes it each one's own.
                    // Only a scene that is copied per monitor (or has several surfaces) needs it: with one surface, `screen.*`
                    // is worth the same everywhere, and a let that reads it can be computed once like any other.
                    let has_copies = self.e.surfaces.iter().any(|s| matches!(s.screens, Screens::Number(_))) || self.e.surfaces.len() > 1;
                    let per_copy = has_copies && self.scopes.is_empty() && self.is_per_copy(&e);
                    // The same one in every monitor's copy —it reads nothing of
                    // its copy— is one property: a property per copy was the
                    // same work once per monitor, and a rule naming it read
                    // differently in each copy, so it acted once per copy (a
                    // `toggle` in it undid itself).
                    // One that is each copy's own is a property too, **one per
                    // copy** (below): left substituted, a chain of them —a scale
                    // from `screen.height`, a size from that scale, a place from
                    // that size— doubled per link like any other, and a bar
                    // written that way kept trees of 190,000 nodes in single
                    // instructions and gigabytes of them in all (issue 46).
                    let e = if e.node_count() > 8 {
                        let p = self.computed_once(&name, e);
                        if per_copy {
                            self.copy_props.insert(p.0);
                        }
                        p.e()
                    } else {
                        e
                    };
                    match self.scopes.last_mut().map(Rc::make_mut) {
                        Some(env) => env.exprs.insert(name.clone(), e.clone()),
                        None => self.lets.insert(name.clone(), e.clone()),
                    };
                    // A loose `let` that reads something PER screen COPY is worth something different
                    // in each copy, and it has been read once with the names of the
                    // first. It is read again once per copy, in its scope, and
                    // stored with its mark (`crosses#screen1`): the copy finds it through
                    // its alias, and the loose scene keeps seeing the first one's.
                    if per_copy && self.scopes.is_empty() {
                        let copies: Vec<usize> = self.e.surfaces.iter().filter(|s| s.name.is_empty() && matches!(s.screens, Screens::Number(_))).map(|s| s.instance).collect();
                        for k in copies {
                            self.scopes.push(Rc::new(self.screen_scope(k)));
                            let mut c2 = Cur::new(&n.head[3..], n.line, n.col);
                            let own = self.expr(&mut c2);
                            self.scopes.pop();
                            if let Ok(own) = own {
                                let own = if own.node_count() > 8 { self.computed_once(&name, own).e() } else { own };
                                self.lets.insert(format!("{name}#screen{k}"), own);
                            }
                        }
                    }
                }
                "zone" => {
                    // A shape that is not painted: it is only sensitive.
                    self.shape(n, 1)?;
                    if let Some(c) = self.candidates.last_mut() {
                        c.forced = true;
                    }
                }
                "body" => self.body(n)?,
                "ellipse" | "box" | "arc" | "line" | "path" => {
                    let f = self.shape(n, 0)?;
                    self.last_size = f.size;
                    self.e.paint(Instr::Solid { shape: f.shape, color: f.color.unwrap_or_else(|| color(1.0, 1.0, 1.0)), alpha: f.opacity.unwrap_or(Expr::K(1.0)), glass_spec: f.glass_spec });
                }
                "text" => self.text(n)?,
                "image" => self.image(n)?,
                "figure" => self.figure(n)?,
                "shader" => self.shader(n)?,
                "particles" => self.particles(n)?,
                "input" => self.input_field(n)?,
                "window" => self.window(n)?,
                "terminal" => self.terminal(n)?,
                "clip" => {
                    let margin = if c.word("inset") { c.num()? } else { 0.0 };
                    let from = c.i;
                    let f = self.shape(n, from)?;
                    self.e.paint(Instr::Clip(Some((f.shape, margin))));
                    *clips += 1;
                }
                "group" => self.group_with_properties(n, n.body.as_deref().unwrap_or(&[]))?,
                "popup" => self.popup(n, &mut c)?,
                "children" => self.outer_children(n)?,
                // Inside a stack, with several things, it acts as a group; on its own it is nothing.
                "between" if self.in_slot => self.group_with_properties(n, n.body.as_deref().unwrap_or(&[]))?,
                "between" => return Err(CompileError::at(n.line, n.col, "`between` only works inside a `row` or a `column`: it is what goes between every two children")),
                "component" => self.declare_component(n, &mut c)?,
                "repeat" => self.repeat(n, &mut c)?,
                "for" => self.for_loop(n, &mut c)?,
                "row" | "column" => self.stack(n, &mut c, word == "row")?,
                "grid" => self.grid(n, &mut c)?,
                "pages" => self.pages(n)?,
                "space" => {
                    let v = self.expr(&mut c)?;
                    self.last_size = Some((v.clone(), v));
                }
                instance if self.components.contains_key(instance) => self.instantiate(n, &mut c)?,
                "layer" => self.layer(n, &mut c)?,
                "on" | "every" => self.rules.push((n, self.scopes.clone())),
                "blink" | "wave" | "spin" | "follow" | "look" => self.behavior(&word, &mut c)?,
                "gesture" | "posture" => self.gesture(n, &word, &mut c)?,
                // The gate above only lets through what is in the vocabulary: if we get
                // here, a word was noted and nobody handles it.
                other => unreachable!("'{other}' is in the vocabulary, but `statement` does not know what to do with it"),
            }
        }
        Ok(())
    }

    /// `n` brings the properties; `body`, the children: those of the group itself, or those
    /// of a component when `n` is one of its copies.
    fn group_with_properties(&mut self, n: &Node, body: &'a [Entry]) -> R<()> {
        let mut p = self.properties(n, vocab::properties("group"))?;
        self.in_slot = false;
        // `z:` draws it over the groups with `z:` beside it that have less, and
        // its zones catch the mouse over theirs.
        let z = match p.get_mut("z") {
            Some(c) => {
                if self.zblock.is_some() {
                    return Err(CompileError::at(n.line, n.col, "a group with `z:` inside another one with `z:`: only the outer ones are sorted. Give it to one of the two"));
                }
                Some(self.expr(c)?)
            }
            None => None,
        };
        let z_start = self.e.instrs.len();
        let parent = *self.zparents.last().unwrap_or(&0);
        if z.is_some() {
            self.zblock = Some(self.e.zblocks.len() as u16);
            self.e.zblocks.push(crate::scene::ZBlock { range: z_start..z_start, z: Expr::K(0.0), parent });
        }
        self.zparents.push(self.next_zparent);
        self.next_zparent += 1;
        self.groups.push(self.scopes.len());
        let result = self.group_body(n, body, p);
        self.groups.pop();
        self.zparents.pop();
        if let Some(z) = z {
            let k = self.zblock.take().unwrap() as usize;
            self.e.zblocks[k] = crate::scene::ZBlock { range: z_start..self.e.instrs.len(), z, parent };
        }
        result
    }

    fn group_body(&mut self, n: &Node, body: &'a [Entry], mut p: HashMap<&str, Cur>) -> R<()> {
        let size = match p.get_mut("size") {
            Some(c) => Some(self.point(c)?),
            None => None,
        };
        let mut t = Transform::at((0.0.into(), 0.0.into()));
        let mut transforms = false;
        if let Some(c) = p.get_mut("pivot") {
            t.pivot = self.point(c)?;
        }
        if let Some(c) = p.get_mut("rotate") {
            t.rotate = self.expr(c)?;
            transforms = true;
        }
        if let Some(c) = p.get_mut("scale") {
            let x = self.expr(c)?;
            t.scale = if c.sym(",") { (x, self.expr(c)?) } else { (x.clone(), x) };
            transforms = true;
        }
        if let Some(c) = p.get_mut("move") {
            t.translate = self.point(c)?;
            transforms = true;
        }
        let opacity = match p.get_mut("opacity") {
            Some(c) => Some(self.expr(c)?),
            None => None,
        };
        // `show:` multiplies the opacity: what is not there is not seen.
        let hide = match p.get_mut("show") {
            Some(c) => Some(self.expr(c)?.clamp(0.0, 1.0)),
            None => None,
        };
        let opacity = match (&hide, opacity) {
            (Some(v), o) => Some(o.map_or(v.clone(), |o| o * v.clone())),
            (None, o) => o,
        };
        let effects = self.group_effects(n, &mut p, &opacity)?;
        // With no effects, whether it is seen goes first and how it moves
        // after: a hidden group is jumped over whole when drawing, its
        // transform included (a window manager's empty slots, each with its
        // `pivot` and its `scale`). With effects, the transform first: the
        // effect's mask moves with the group.
        let opacity_first = effects.is_none() && opacity.is_some();
        if opacity_first {
            if let Some(o) = &opacity {
                self.e.paint(Instr::Opacity(Some(o.clone())));
            }
        }
        if transforms {
            self.e.paint(Instr::Transform(Some(t.clone())));
            self.under.push(t);
        }
        if let Some(fx) = &effects {
            if self.effects_depth > 0 {
                return Err(CompileError::at(n.line, n.col, "a group with effects inside another group with effects: only the outer one would get them. Put the effects on one of the two, or side by side"));
            }
            self.e.paint(Instr::Effect(Box::new(fx.clone())));
        }
        self.effects_depth += effects.is_some() as usize;
        let from = self.candidates.len();
        self.group(body.iter());
        self.effects_depth -= effects.is_some() as usize;
        // What is not seen does not stop anyone's click, whether it went away with `show:` or
        // with an opacity that reaches zero. Inside a stack that already happened with
        // `show:`; a `group` not doing it was a trap for the faces: the
        // list of another page kept catching the mouse on top of what was
        // visible, invisible and in front. And with opacity it was worse, because it is
        // how almost everything that goes out and comes in with a spring is hidden: a
        // closed search box, with its rows at zero, LAUNCHED applications when pressing
        // on the panel above it. The guard `active: x > 0.9` written by
        // hand in each zone gets forgotten; the rule does not.
        let _ = &hide;
        if let Some(shown) = &opacity {
            let is_present = shown.clone().gt(0.01);
            for c in &mut self.candidates[from..] {
                c.visible = Some(match c.visible.take() { Some(v) => v * is_present.clone(), None => is_present.clone() });
            }
        }
        if size.is_some() {
            self.last_size = size;
        }
        if effects.is_some() {
            self.e.paint(Instr::Opacity(None));
        }
        if transforms {
            self.under.pop();
            self.e.paint(Instr::Transform(None));
        }
        if opacity_first {
            self.e.paint(Instr::Opacity(None));
        }
        Ok(())
    }

    /// `blur`, `glow`, `saturation`, `brightness`, `contrast`, `hue`, `mask` and
    /// `mode` of a group: what is done to what it holds when blending it.
    fn group_effects(&mut self, n: &Node, p: &mut HashMap<&str, Cur>, opacity: &Option<Expr>) -> R<Option<Effects>> {
        let words = ["blur", "glow", "saturation", "brightness", "contrast", "hue", "mask", "mode", "shader"];
        if !words.iter().any(|w| p.contains_key(*w)) {
            return Ok(None);
        }
        let one = |o: &Self, name: &str, p: &mut HashMap<&str, Cur>| -> R<Option<Expr>> {
            match p.get_mut(name) {
                Some(c) => Ok(Some(o.expr(c)?)),
                None => Ok(None),
            }
        };
        let blur = one(self, "blur", p)?;
        let saturation = one(self, "saturation", p)?;
        let brightness = one(self, "brightness", p)?;
        let contrast = one(self, "contrast", p)?;
        let hue = one(self, "hue", p)?;
        // `glow: 16, 80%` or `glow: 16, 80%, mint`.
        let glow = match p.get_mut("glow") {
            Some(c) => {
                let r = self.expr(c)?;
                c.expect_sym(",")?;
                let k = self.expr(c)?;
                let col = if c.sym(",") { Some(self.color(c)?) } else { None };
                Some((r, k, col))
            }
            None => None,
        };
        // `mask: x1, y1 to x2, y2` · `mask: radial x, y radius r` · `… radius r1 to r2`.
        let mask = match p.get_mut("mask") {
            Some(c) => {
                if c.word("radial") {
                    let at = self.point(c)?;
                    c.expect_word("radius")?;
                    let r = self.expr(c)?;
                    Some(if c.word("to") { Mask::Radial(at, r, self.expr(c)?) } else { Mask::Radial(at, Expr::K(0.0), r) })
                } else {
                    let from = self.point(c)?;
                    c.expect_word("to")?;
                    Some(Mask::Linear(from, self.point(c)?))
                }
            }
            None => None,
        };
        let mode = match p.get_mut("mode") {
            Some(c) => {
                let word = c.one_of(vocab::GROUP_MODES, "how a group blends")?;
                vocab::GROUP_MODES.iter().position(|m| *m == word).unwrap_or(0) as u8
            }
            None => 0,
        };
        // `shader: rain, 0.8, open`: one of the scene's shaders, and up to eight numbers.
        let shader = match p.get_mut("shader") {
            Some(c) => {
                let name = self.global(&c.id("the name of a shader")?);
                let Some(k) = self.shaders.get(&name).copied() else {
                    return self.unknown(c, "no shader", &name, self.shaders.keys().collect());
                };
                let mut values = Vec::new();
                while c.sym(",") {
                    values.push(self.expr(c)?);
                }
                if values.len() > 8 {
                    return c.error("a shader takes up to eight numbers: `s.a` and `s.b`, four each");
                }
                // `shader: wave, a, b while bending`: only while that holds —a
                // shader that, its numbers at rest, would return what the group
                // holds as it is still costs the group a pass of its own—.
                let on = if c.word("while") { Some(self.expr(c)?) } else { None };
                let u = self.e.shaders[k as usize].clone();
                if u.behind {
                    return c.error("a group's shader reads what the group holds, with `inside(s, at)`: `behind` is for a `shader { … }` of its own");
                }
                let time = u.animated.then(|| self.time_prop().e());
                Some(GroupShader { shader: k, values, time, on })
            }
            None => None,
        };
        let _ = n;
        Ok(Some(Effects { alpha: opacity.clone().unwrap_or(Expr::K(1.0)), blur, glow, saturation, brightness, contrast, hue, mask, mode, shader }))
    }

    /// How much room a named stack takes is known when it finishes drawing, but it
    /// may be wanted earlier (the panel that wraps its list, the `size:` of a
    /// component): its measures are declared right away, as properties, and the stack
    /// fills them in at the end. Inside a copy, with that copy's own name.
    fn declare_measures_early(&mut self, entries: &'a [Entry]) {
        for e in entries {
            let Entry::Node(n) = e else { continue };
            let word_at = |k: usize| match n.head.get(k).map(|f| &f.kind) { Some(TokenKind::Id(p)) => Some(p.as_str()), _ => None };
            match word_at(0) {
                Some("component" | "repeat" | "for") => continue,
                // `measure m` too: a component's `size:` is read before its
                // body, and a row that is as tall as its text needs it there.
                Some("measure") => if let Some(local) = word_at(1) {
                    let name = self.declare(local);
                    let part = self.interpolate_in(local);
                    if let Some(e) = self.scopes.last_mut().map(Rc::make_mut) {
                        e.with_parts.insert(part);
                    }
                    if !self.measurements.contains_key(&name) {
                        let (w, h) = self.e.measured(interned(&name));
                        self.props.insert(format!("{name}.width"), w);
                        self.props.insert(format!("{name}.height"), h);
                        self.measurements.insert(name, (w, h));
                    }
                },
                Some("row" | "column") => if let Some(local) = word_at(1) {
                    // It is brought forward so it can be read before getting to it, but the location
                    // noted is its own: it is where the editor has to take you.
                    PEEKED.with(|m| m.set((n.line, n.col)));
                    self.current_class = word_at(0).unwrap_or("row").to_owned();
                    let name = self.declare(local);
                    if let Some(e) = self.scopes.last_mut().map(Rc::make_mut) {
                        e.with_parts.insert(local.to_owned());
                    }
                    for part in ["width", "height", "count", "content"] {
                        let full_name = format!("{name}.{part}");
                        if !self.props.contains_key(&full_name) {
                            let p = self.e.prop_with(interned(&full_name), 0.0, Spring::LIVELY);
                            self.props.insert(full_name, p);
                        }
                    }
                },
                _ => {}
            }
            self.declare_measures_early(n.body.as_deref().unwrap_or(&[]));
        }
    }

    /// What the copy in progress brings for the slot this `children` names, and the scope
    /// of whoever wrote it. A slot is placed once.
    fn take_slot(&mut self, n: &Node) -> R<(Vec<&'a Entry>, Vec<Rc<Scope>>)> {
        let which = match n.head.get(1).map(|f| &f.kind) {
            Some(TokenKind::Id(p)) => p.clone(),
            None => String::new(),
            Some(_) => return Err(CompileError::at(n.line, n.col, "after `children` only the slot name can go: `children header`")),
        };
        let Some(instance) = self.instance_children.last_mut() else {
            return Err(CompileError::at(n.line, n.col, "`children` only works inside a component: it is where whatever each copy brings goes"));
        };
        let h = instance.slots.iter_mut().find(|h| h.0 == which).expect("the slots were noted when the component was declared");
        h.2 = true;
        Ok((h.1.clone(), instance.outer.clone()))
    }

    /// `children { move: 12, 40 }`, inside a component: here goes whatever each copy
    /// brings in its block. It is a group, and what is inside is read with the names of
    /// whoever wrote it: a component does not see —or step on— what is put into it.
    fn outer_children(&mut self, n: &'a Node) -> R<()> {
        let (children, outside) = self.take_slot(n)?;
        let mut p = self.properties(n, vocab::properties("children"))?;
        let translate = match p.get_mut("move") {
            Some(c) => Some(self.point(c)?),
            None => None,
        };
        if let Some(m) = &translate {
            let t = Transform { translate: m.clone(), ..Transform::at((0.0.into(), 0.0.into())) };
            self.e.paint(Instr::Transform(Some(t.clone())));
            self.under.push(t);
        }
        // While they are read, the component is not there: its parameters hide nothing from outside.
        let inside = std::mem::replace(&mut self.scopes, outside);
        let pending = std::mem::take(&mut self.instance_children);
        self.group(children.into_iter());
        self.instance_children = pending;
        self.scopes = inside;
        if translate.is_some() {
            self.under.pop();
            self.e.paint(Instr::Transform(None));
        }
        Ok(())
    }

    /// `popup menu { at: x, y; size: w, h; open: menu_open; … }`: a child
    /// surface. What is inside is drawn with (0, 0) at its corner, as if it were another
    /// scene; in reality it is a piece of this one, placed far away.
    fn popup(&mut self, n: &'a Node, c: &mut Cur) -> R<()> {
        let local = c.id("a name for the popup")?;
        let name = self.declare(&local);
        if !self.scopes.is_empty() || !self.under.is_empty() {
            return Err(CompileError::at(n.line, n.col, "a `popup` goes at the scene level, not inside a group or a component"));
        }
        let mut p = self.properties(n, vocab::properties("popup"))?;
        let missing = |q: &str| CompileError::at(n.line, n.col, format!("this `popup` is missing '{q}'"));
        let at = self.point(p.get_mut("at").ok_or_else(|| missing("at"))?)?;
        let size = self.point(p.get_mut("size").ok_or_else(|| missing("size"))?)?;
        let open = {
            let c = p.get_mut("open").ok_or_else(|| missing("open"))?;
            let h = self.global(&c.id("the fact that opens it")?);
            match self.facts.get(&h) {
                Some(h) => *h,
                None => return self.unknown(c, "no fact", &h, self.facts.keys().collect()),
            }
        };
        // Each one in its place, far from the surface and from the others.
        self.next_origin += 10000.0;
        let origin = (0.0, self.next_origin);
        let t = Transform::at((origin.0.into(), origin.1.into()));
        let t = Transform { translate: (origin.0.into(), origin.1.into()), ..t };
        self.e.paint(Instr::Transform(Some(t.clone())));
        self.under.push(t);
        self.group(n.body.as_deref().unwrap_or(&[]).iter());
        self.under.pop();
        self.e.paint(Instr::Transform(None));
        self.e.popups.push(Popup { name: interned(&name), open, at, size, origin });
        Ok(())
    }

    /// `body { color: …; shadow: …; ellipse {…}; box {… blend: …} }`
    fn body(&mut self, n: &Node) -> R<()> {
        let mut p = self.properties(n, vocab::properties("body"))?;
        let in_slot = std::mem::take(&mut self.in_slot);
        let mut size = None;
        let shadow = match p.get_mut("shadow") {
            Some(c) => {
                let dx = self.expr(c)?;
                c.expect_sym(",")?;
                let dy = self.expr(c)?;
                c.expect_sym(",")?;
                let blur = self.expr(c)?;
                c.expect_sym(",")?;
                let alpha = self.expr(c)?;
                //  And which colour, if it is given: `shadow: 0, 0, 18, 55%, mint`.
                let color = if c.sym(",") { Some(self.color(c)?) } else { None };
                Some(Shadow { offset: (dx, dy), blur, alpha, color })
            }
            None => None,
        };
        self.e.paint(Instr::Group { shadow });
        let mut shapes = 0;
        for e in n.body.as_deref().unwrap_or(&[]) {
            if let Entry::Node(h) = e {
                self.in_slot = in_slot && shapes == 0;
                // A figure brings its layers, which can be several shapes: they
                // blend with the rest like any other.
                if matches!(h.head.first().map(|f| &f.kind), Some(TokenKind::Id(x)) if x == "figure") {
                    shapes += self.figure_in_body(h)?;
                    continue;
                }
                let f = self.shape(h, 0)?;
                if shapes == 0 {
                    size = f.size;
                }
                self.e.paint(Instr::Shape { shape: f.shape, fusion: f.blend.unwrap_or(Expr::K(0.0)) });
                shapes += 1;
            }
        }
        if shapes == 0 {
            return Err(CompileError::at(n.line, n.col, "a 'body' with no shapes paints nothing"));
        }
        let paint = if let Some(c) = p.get_mut("gradient") {
            self.gradient(n, c)?
        } else if let Some(c) = p.get_mut("color") {
            Paint::Color(self.color(c)?)
        } else {
            return Err(CompileError::at(n.line, n.col, "this 'body' is missing 'color' or 'gradient'"));
        };
        let light = match p.get_mut("light") {
            Some(c) => {
                let amount = c.num()?;
                c.expect_sym(",")?;
                let from_y = self.expr(c)?;
                c.expect_sym(",")?;
                Some(Light { amount, from_y, height: c.num()? })
            }
            None => None,
        };
        let border = match p.get_mut("border") {
            Some(c) => {
                let w = self.expr(c)?;
                c.expect_sym(",")?;
                Some((w, self.color(c)?))
            }
            None => None,
        };
        // An expression, like the opacity: `rim: 10% + 12% * hover` glows as
        // it is hovered (it used to be read as its first number, 10%).
        let rim = match p.get_mut("rim") {
            Some(c) => {
                let e = self.expr(c)?;
                c.expect_end()?;
                e
            }
            None => Expr::K(0.0),
        };
        let glass_spec = self.glass_spec(&mut p, n)?;
        let alpha = match p.get_mut("opacity") {
            Some(c) => self.expr(c)?,
            None => Expr::K(1.0),
        };
        // `show:` multiplies the opacity: what is not there is not seen.
        let alpha = match p.get_mut("show") {
            Some(c) => alpha * self.expr(c)?.clamp(0.0, 1.0),
            None => alpha,
        };
        self.e.paint(Instr::Fill { paint, alpha, rim, light, border, glass_spec });
        self.last_size = size;
        Ok(())
    }

    /// `glass` and what goes with it: `lens`, `shine`, `refraction`,
    /// `dispersion`, `dome`, `ripple`, `frost`, `smoke`, `milk`, `tint`, `vivid`, `mirror` and
    /// `highlight`. Without glass there is nothing for
    /// any of them to do.
    fn glass_spec(&self, p: &mut HashMap<&str, Cur>, n: &Node) -> R<Option<Glass>> {
        let mut one = |k: &str| -> R<Option<Expr>> {
            match p.get_mut(k) {
                Some(c) => {
                    let e = self.expr(c)?;
                    c.expect_end()?;
                    Ok(Some(e))
                }
                None => Ok(None),
            }
        };
        let (glass, lens, refraction, dispersion, dome, ripple) = (one("glass")?, one("lens")?, one("refraction")?, one("dispersion")?, one("dome")?, one("ripple")?);
        let (smoke, milk) = (one("smoke")?, one("milk")?);
        let (tint, vivid, mirror, highlight) = (one("tint")?, one("vivid")?, one("mirror")?, one("highlight")?);
        // `frost: auto` is frosted only where what is behind is busy; else how much.
        let frost = match p.get_mut("frost") {
            Some(c) => {
                let e = if c.word("auto") { Expr::K(-1.0) } else { self.expr(c)?.clamp(0.0, 1.0) };
                c.expect_end()?;
                Some(e)
            }
            None => None,
        };
        // `shine: pointer` is the light in your hand; otherwise, a point.
        let shine = match p.get_mut("shine") {
            Some(c) => {
                let at = if c.word("pointer") {
                    if !c.at_end() {
                        return c.error("`shine: pointer` goes alone; for a point near it, `shine: pointer.x + 40, pointer.y`");
                    }
                    (self.facts["pointer.x"].e(), self.facts["pointer.y"].e())
                } else {
                    self.point(c)?
                };
                c.expect_end()?;
                Some(at)
            }
            None => None,
        };
        let Some(g) = glass else {
            let loose = [("lens", lens.is_some()), ("shine", shine.is_some()), ("refraction", refraction.is_some()), ("dispersion", dispersion.is_some()), ("dome", dome.is_some()), ("ripple", ripple.is_some()), ("frost", frost.is_some()), ("smoke", smoke.is_some()), ("milk", milk.is_some()), ("tint", tint.is_some()), ("vivid", vivid.is_some()), ("mirror", mirror.is_some()), ("highlight", highlight.is_some())];
            return match loose.iter().find(|w| w.1) {
                Some((w, _)) => Err(CompileError::at(n.line, n.col, format!("`{w}` is something glass does, and here there is no `glass`: `glass: 100%; {w}: …`"))),
                None => Ok(None),
            };
        };
        Ok(Some(Glass {
            amount: g.clamp(0.0, 1.0),
            lens: lens.unwrap_or(Expr::K(1.0)),
            shine,
            refraction: refraction.map_or(Expr::K(1.0), |e| e.max(0.0)),
            dispersion: dispersion.map_or(Expr::K(1.0), |e| e.max(0.0)),
            dome: dome.map_or(Expr::K(0.0), |e| e.clamp(-1.0, 1.0)),
            ripple: ripple.map_or(Expr::K(1.0), |e| e.max(0.0)),
            frost: frost.unwrap_or(Expr::K(1.0)),
            smoke: smoke.map_or(Expr::K(0.0), |e| e.clamp(0.0, 1.0)),
            milk: milk.map_or(Expr::K(0.0), |e| e.clamp(0.0, 1.0)),
            tint: tint.map_or(Expr::K(0.22), |e| e.clamp(0.0, 1.0)),
            vivid: vivid.map_or(Expr::K(1.18), |e| e.max(0.0)),
            mirror: mirror.map_or(Expr::K(1.0), |e| e.max(0.0)),
            highlight: highlight.map_or(Expr::K(1.0), |e| e.max(0.0)),
        }))
    }

    // ── texts with holes ────────────────────────────────────────

    /// `"Dismiss"` is a literal text; `"{r.title} · {volume * 100, 1} %"`, a template.
    fn content_of(&self, s: &str, token: &Token) -> R<Content> {
        let original = self.content_of_one(s, token)?;
        let (Some(locale), true) = (self.e.locale, self.translatable(s)) else { return Ok(original) };
        let mut versions = vec![original.clone()];
        let mut any = false;
        for (k, (_, table)) in self.translations.iter().enumerate() {
            match table.get(s) {
                Some(t) => {
                    versions.push(self.content_of_one(t, token)?);
                    any = true;
                }
                None => {
                    self.untranslated.borrow_mut()[k].insert(s.to_owned());
                    versions.push(original.clone());
                }
            }
        }
        Ok(if any { Content::Translated { locale, versions } } else { original })
    }

    /// Whether a text is worth translating: one with no letters (`·`, `{n} %`) is the
    /// same in every language, and not worth warning about.
    fn translatable(&self, s: &str) -> bool {
        let mut in_hole = 0;
        s.chars().any(|ch| {
            match ch {
                '{' => in_hole += 1,
                '}' => in_hole -= 1,
                _ => {}
            }
            in_hole == 0 && ch.is_alphabetic()
        })
    }

    /// Each language's version of a literal with no placeholders —a declared text, a
    /// component's string—, the original first; `None` if nobody translates it.
    fn versions_of(&self, s: &str) -> Option<Vec<String>> {
        self.e.locale?;
        if !self.translatable(s) {
            return None;
        }
        let mut any = false;
        let mut versions = vec![s.to_owned()];
        for (k, (_, table)) in self.translations.iter().enumerate() {
            match table.get(s) {
                Some(t) => {
                    versions.push(t.clone());
                    any = true;
                }
                None => {
                    self.untranslated.borrow_mut()[k].insert(s.to_owned());
                    versions.push(s.to_owned());
                }
            }
        }
        any.then_some(versions)
    }

    /// `translations es { "Control center" = "Centro de control" }`, read before
    /// everything else. With at least one, the fact `locale` exists: `en` —the
    /// language the scene is written in—, then each translation in order. It starts
    /// as the system's language (`LC_ALL`, `LC_MESSAGES`, `LANG`), and the scene or the
    /// logic can change it while it runs: every text changes with it.
    fn read_translations(&mut self, entries: &'a [Entry]) {
        for e in entries {
            let Entry::Node(n) = e else { continue };
            if !matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(w)) if w == "translations") {
                continue;
            }
            if let Err(f) = self.translations_block(n) {
                self.push_error(f);
            }
        }
        if self.translations.is_empty() {
            return;
        }
        *self.untranslated.borrow_mut() = vec![Default::default(); self.translations.len()];
        let mut names = vec!["en".to_owned()];
        names.extend(self.translations.iter().map(|(code, _)| code.clone()));
        let system = system_language();
        let start = names.iter().position(|n| *n == system).or_else(|| names.iter().position(|n| system.split('_').next() == Some(n.as_str()))).unwrap_or(0);
        for (k, n) in names.iter().enumerate() {
            match self.values.get(n) {
                Some(v) if *v != k as f32 => {
                    self.values.remove(n);
                    self.ambiguous.insert(n.clone());
                }
                _ if self.ambiguous.contains(n) => {}
                _ => {
                    self.values.insert(n.clone(), k as f32);
                }
            }
        }
        self.e.types.push(("locale".into(), FactType::Enum(names)));
        let h = self.e.fact("locale", start as f32);
        self.facts.insert("locale".into(), h);
        self.e.locale = Some(h);
        self.e.translations = self.translations.clone();
    }

    fn translations_block(&mut self, n: &Node) -> R<()> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        let code = c.id("the code of a language: `translations es { … }`")?;
        c.expect_end()?;
        if code.len() < 2 || !code.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_') {
            return Err(CompileError::at(n.line, n.col, format!("'{code}' does not look like a language: it is its code in lower case, `es`, `pt_br`")));
        }
        if code == "en" {
            return Err(CompileError::at(n.line, n.col, "`en` is the language the scene is written in: translations go into another one"));
        }
        let k = match self.translations.iter().position(|(c, _)| *c == code) {
            Some(k) => k,
            None => {
                self.translations.push((code.clone(), HashMap::new()));
                self.translations.len() - 1
            }
        };
        for entry in n.body.as_deref().unwrap_or(&[]) {
            let (line, col, pair) = match entry {
                Entry::Node(m) => (m.line, m.col, match m.head.as_slice() {
                    [Token { kind: TokenKind::Str(a), .. }, Token { kind: TokenKind::Sym("="), .. }, Token { kind: TokenKind::Str(b), .. }] => Some((a.clone(), b.clone())),
                    _ => None,
                }),
                Entry::Prop { line, col, .. } => (*line, *col, None),
            };
            let Some((a, b)) = pair else {
                return Err(CompileError::at(line, col, "a translation is the original and what it becomes: `\"Control center\" = \"Centro de control\"`"));
            };
            if self.translations[k].1.insert(a.clone(), b).is_some() {
                return Err(CompileError::at(line, col, format!("«{a}» is translated into `{code}` twice, and only the last one would count")));
            }
        }
        Ok(())
    }

    /// `label: "Save"`, `label: "Delete {n.title}"`, or a live text or a
    /// component's text: what a zone is, said in words. Translated like any text.
    fn label(&self, c: &mut Cur) -> R<Content> {
        let k = match c.peek() {
            // `label: pick(k, "Brightness", "Volume")`: one per copy, as a `text` does.
            Some(TokenKind::Id(n)) if (n == "pick" || n == "if") && matches!(c.tokens.get(c.i + 1).map(|x| &x.kind), Some(TokenKind::Sym("("))) => self.text_pick(c)?,
            Some(TokenKind::Id(_)) if matches!(c.tokens.get(c.i + 1).map(|x| &x.kind), Some(TokenKind::Sym("["))) => self.text_content_at(c)?,
            Some(TokenKind::Str(s)) => {
                let s = s.clone();
                let at = c.i;
                c.i += 1;
                self.content_of(&s, &c.tokens[at])?
            }
            Some(TokenKind::Id(name)) => {
                let name = name.clone();
                c.i += 1;
                if let Some(k) = self.scopes.iter().rev().find_map(|e| e.contents.get(&name)) {
                    k.clone()
                } else if let Some(t) = self.scopes.iter().rev().find_map(|e| e.strings.get(&name)).cloned() {
                    match (self.e.locale, self.versions_of(&t)) {
                        (Some(locale), Some(v)) => Content::Translated { locale, versions: v.into_iter().map(Content::Literal).collect() },
                        _ => Content::Literal(t),
                    }
                } else if let Some(t) = self.texts.get(&self.global(&name)) {
                    Content::Live(*t)
                } else {
                    return self.unknown(c, "no text", &name, self.texts.keys().collect());
                }
            }
            _ => return c.error("`label:` is a text: `label: \"Save\"`, with holes if it needs them, a live text, or `pick(k, \"One\", \"Other\")`"),
        };
        c.expect_end()?;
        Ok(k)
    }

    fn content_of_one(&self, s: &str, token: &Token) -> R<Content> {
        if !s.contains('{') && !s.contains('}') {
            return Ok(Content::Literal(s.to_owned()));
        }
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        let pieces = self.pieces(&chars, &mut i, false, token)?;
        Ok(match pieces.as_slice() {
            [] => Content::Literal(String::new()),
            [Piece::Literal(t)] => Content::Literal(t.clone()),
            _ => Content::Template(pieces),
        })
    }

    /// Until the end, or until the `}` that closes an optional span.
    fn pieces(&self, s: &[char], i: &mut usize, inside: bool, token: &Token) -> R<Vec<Piece>> {
        // Where to point if something fails: the quote, plus how far it has gone (exact if there are no line breaks).
        let at_offset = |i: usize| (token.line, token.col + 1 + i);
        let mut outside = Vec::new();
        let mut literal = String::new();
        while *i < s.len() {
            match (s[*i], s.get(*i + 1)) {
                // Two in a row are a real one.
                ('{', Some('{')) | ('}', Some('}')) => {
                    literal.push(s[*i]);
                    *i += 2;
                }
                ('}', _) if inside => {
                    *i += 1;
                    if !literal.is_empty() { outside.push(Piece::Literal(literal)) }
                    return Ok(outside);
                }
                ('}', _) => {
                    let (l, c) = at_offset(*i);
                    return Err(CompileError::at(l, c, "this `}` closes nothing. If it is a real brace, write it twice: `}}`"));
                }
                ('{', Some('?')) => {
                    if !literal.is_empty() { outside.push(Piece::Literal(std::mem::take(&mut literal))) }
                    let open_at = *i;
                    *i += 2;
                    let optional = self.pieces(s, i, true, token)?;
                    if s.get(*i - 1) != Some(&'}') || *i > s.len() {
                        let (l, c) = at_offset(open_at);
                        return Err(CompileError::at(l, c, "this `{? …}` part is missing its `}`"));
                    }
                    outside.push(Piece::Optional(optional));
                }
                ('{', _) => {
                    if !literal.is_empty() { outside.push(Piece::Literal(std::mem::take(&mut literal))) }
                    let open_at = *i;
                    let Some(close_at) = s[open_at..].iter().position(|c| *c == '}').map(|k| open_at + k) else {
                        let (l, c) = at_offset(open_at);
                        return Err(CompileError::at(l, c, "this hole is missing its `}`. If it is a real brace, write it twice: `{{`"));
                    };
                    let source: String = s[open_at + 1..close_at].iter().collect();
                    outside.push(self.placeholder(&source, at_offset(open_at + 1))?);
                    *i = close_at + 1;
                }
                (c, _) => {
                    literal.push(c);
                    *i += 1;
                }
            }
        }
        if inside {
            // The text ended without closing the span: whoever opened it will say so.
            *i = s.len() + 1;
        }
        if !literal.is_empty() { outside.push(Piece::Literal(literal)) }
        Ok(outside)
    }

    /// What is inside a hole: a live text (`r.title`, `upper(r.app)`) or an
    /// expression with its decimals (`volume * 100, 1`).
    fn placeholder(&self, source: &str, (line, col): (usize, usize)) -> R<Piece> {
        let mut tokens = super::tokens::tokenize(source).map_err(|f| CompileError::at(line, col + f.col.saturating_sub(1), f.message))?;
        // The end of line the tokenizer puts here means nothing.
        tokens.retain(|f| !matches!(f.kind, TokenKind::Line));
        for f in &mut tokens {
            (f.line, f.col) = (line, col + f.col.saturating_sub(1));
        }
        let mut c = Cur::new(&tokens, line, col);
        if tokens.is_empty() {
            return c.error("an empty hole: inside goes the name of a text or an expression");
        }
        let text_of = |o: &Self, n: &str| o.texts.get(&o.global(n)).copied();
        // upper(name) · lower(name)
        if let (Some(TokenKind::Id(f)), Some(TokenKind::Sym("("))) = (c.peek(), tokens.get(1).map(|x| &x.kind)) {
            let case = vocab::TEXT_FUNCTIONS.contains(&f.as_str()).then(|| match f.as_str() {
                "upper" => LetterCase::Upper,
                "lower" => LetterCase::Lower,
                other => unreachable!("'{other}' is in the vocabulary, but a hole cannot apply it"),
            });
            if let Some(case) = case {
                c.i += 2;
                let name = c.id("the name of a text")?;
                if matches!(c.peek(), Some(TokenKind::Sym("["))) {
                    let at = c.pos();
                    let p = self.text_at(&name, &mut c, case)?.ok_or_else(|| CompileError::at(at.0, at.1, format!("'{name}[...]' reads a text by its place, and there are none")))?;
                    c.expect_sym(")")?;
                    c.expect_end()?;
                    return Ok(p);
                }
                // First what is inside: a text parameter beats a text of the scene with the same name.
                if let Some(e) = self.scopes.iter().rev().find(|e| e.strings.contains_key(&name)) {
                    if e.contents.contains_key(&name) {
                        return c.error(format!("'{name}' arrived with holes, and `{f}` only knows whole texts: put the `{f}` inside those holes, where it was written"));
                    }
                    c.expect_sym(")")?;
                    c.expect_end()?;
                    let literal = &e.strings[&name];
                    return Ok(if literal.is_empty() { Piece::Empty } else { Piece::Literal(if case == LetterCase::Upper { literal.to_uppercase() } else { literal.to_lowercase() }) });
                }
                let Some(t) = text_of(self, &name) else {
                    let g = self.global(&name);
                    if self.facts.contains_key(&g) || self.props.contains_key(&g) {
                        return c.error(format!("'{name}' is a number, and `{f}` is for texts"));
                    }
                    return self.unknown(&c, "no text", &name, self.texts.keys().collect());
                };
                c.expect_sym(")")?;
                c.expect_end()?;
                return Ok(Piece::Live(t, case));
            }
        }
        // `{win[i].title}`: a live text by its place (a number by its place stays a number).
        if let (Some(TokenKind::Id(n)), Some(TokenKind::Sym("["))) = (c.peek(), tokens.get(1).map(|x| &x.kind)) {
            let n = n.clone();
            let mut probe = Cur::new(&tokens, line, col);
            probe.i = 1;
            if let Ok(Some(p)) = self.text_at(&n, &mut probe, LetterCase::AsIs) {
                if probe.at_end() {
                    return Ok(p);
                }
            }
        }
        if let (Some(TokenKind::Id(n)), 1) = (c.peek(), tokens.len()) {
            // A text parameter of the component where this string is: what was passed to it.
            if let Some(e) = self.scopes.iter().rev().find(|e| e.strings.contains_key(n)) {
                return Ok(match e.contents.get(n) {
                    Some(Content::Template(pieces)) => Piece::Span(pieces.clone()),
                    _ if e.strings[n].is_empty() => Piece::Empty,
                    _ => Piece::Literal(e.strings[n].clone()),
                });
            }
            if let Some(t) = text_of(self, n) {
                return Ok(Piece::Live(t, LetterCase::AsIs));
            }
            // An enum fact is shown by its name, not by its number.
            let g = self.global(n);
            if let (Some(h), Some((_, FactType::Enum(names)))) = (self.facts.get(&g), self.type_of(&g)) {
                return Ok(Piece::Name(h.e(), names.clone()));
            }
        }
        let e = self.expr(&mut c)?;
        // `{secs, time}`: some seconds, as a clock writes them.
        if c.sym(",") {
            if c.word("time") {
                c.expect_end()?;
                return Ok(Piece::TimeSpan(e));
            }
            let decimals = c.num()? as u8;
            c.expect_end()?;
            return Ok(Piece::Number(e, decimals));
        }
        c.expect_end()?;
        Ok(Piece::Number(e, 0))
    }

    /// `win[i].title` where a text goes: the live text at that place, as a
    /// number is read by its place (`expr_atom`). `None` if those are no texts.
    fn text_at(&self, base: &str, c: &mut Cur, case: LetterCase) -> R<Option<Piece>> {
        c.expect_sym("[")?;
        let i = self.expr(c)?;
        c.expect_sym("]")?;
        let rest = match c.peek() {
            Some(TokenKind::Id(r)) if r.starts_with('.') => {
                c.i += 1;
                r.clone()
            }
            _ => String::new(),
        };
        let text = |k: usize| self.texts.get(&self.global_quiet(&format!("{base}.{k}{rest}"))).copied();
        if let Expr::K(k) = i {
            return Ok((k >= 0.0).then(|| text(k.round() as usize)).flatten().map(|t| Piece::Live(t, case)));
        }
        let all: Vec<TextId> = (0..4096).map_while(text).collect();
        Ok((!all.is_empty()).then(|| Piece::Pick(i, all, case)))
    }

    /// The same where a whole text goes: `text win[i].title { ... }`, `label: win[i].title`.
    fn text_content_at(&self, c: &mut Cur) -> R<Content> {
        let base = c.id("a text")?;
        let at = c.pos();
        match self.text_at(&base, c, LetterCase::AsIs)? {
            Some(Piece::Live(t, _)) => Ok(Content::Live(t)),
            Some(p) => Ok(Content::Template(vec![p])),
            None => Err(CompileError::at(at.0, at.1, format!("'{base}[...]' reads a text by its place, and there are none: no '{base}.0' that is a text"))),
        }
    }

    /// `pick(i, "a", b, c)` where a text goes: the one at place `i`. Each
    /// option is a quoted text (translated like any other), a live text of the
    /// scene, or a component's text parameter. And `if(cond, "yes", "no")`, the
    /// same choice said the way it is thought: `pick(cond, "no", "yes")`.
    fn text_pick(&self, c: &mut Cur) -> R<Content> {
        let is_if = matches!(c.peek(), Some(TokenKind::Id(n)) if n == "if");
        c.i += 2;
        let i = self.expr(c)?;
        let mut options = Vec::new();
        while c.sym(",") {
            let option = match c.peek() {
                Some(TokenKind::Str(s)) => {
                    let s = s.clone();
                    let at = c.i;
                    c.i += 1;
                    self.content_of(&s, &c.tokens[at])?
                }
                Some(TokenKind::Id(name)) => {
                    let name = name.clone();
                    c.i += 1;
                    if let Some(k) = self.scopes.iter().rev().find_map(|e| e.contents.get(&name)) {
                        k.clone()
                    } else if let Some(t) = self.scopes.iter().rev().find_map(|e| e.strings.get(&name)).cloned() {
                        match (self.e.locale, self.versions_of(&t)) {
                            (Some(locale), Some(v)) => Content::Translated { locale, versions: v.into_iter().map(Content::Literal).collect() },
                            _ => Content::Literal(t),
                        }
                    } else if let Some(t) = self.texts.get(&self.global(&name)) {
                        Content::Live(*t)
                    } else {
                        return c.error(format!("in a text's `pick`, '{name}' is not a text: each option is a quoted text, a live text or a component's text"));
                    }
                }
                _ => return c.error("in a text's `pick`, each option is a quoted text, a live text or a component's text"),
            };
            options.push(option);
        }
        c.expect_sym(")")?;
        if is_if {
            if options.len() != 2 {
                return c.error("a text's `if` takes a condition and two texts: `if(cond, \"yes\", \"no\")`");
            }
            options.swap(0, 1);
            return Ok(Content::Pick(i, options));
        }
        if options.is_empty() {
            return c.error("`pick` needs a place and texts to choose from: `pick(i, \"a\", \"b\")`");
        }
        Ok(Content::Pick(i, options))
    }

    /// `text notice.title { at: …; size: 20 }` or `text "Dismiss" { … }`
    fn text(&mut self, n: &Node) -> R<()> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        let content = match c.peek() {
            Some(TokenKind::Str(s)) => self.content_of(s, &c.tokens[c.i])?,
            // `text number(volume * 100, 0, " %")`: a number that comes out of an expression.
            Some(TokenKind::Id(n)) if n == "number" && matches!(c.tokens.get(c.i + 1).map(|x| &x.kind), Some(TokenKind::Sym("("))) => {
                c.i += 2;
                let e = self.expr(&mut c)?;
                let decimals = if c.sym(",") { c.num()? as u8 } else { 0 };
                let trailing = if c.sym(",") { c.string()? } else { String::new() };
                c.expect_sym(")")?;
                Content::Number(e, decimals, trailing)
            }
            // `text pick(skin, "Liquid", "Light liquid", "Classic")`: the one at
            // that place; each is a text like any other, and is translated.
            Some(TokenKind::Id(n)) if (n == "pick" || n == "if") && matches!(c.tokens.get(c.i + 1).map(|x| &x.kind), Some(TokenKind::Sym("("))) => self.text_pick(&mut c)?,
            // `text win[i].title`: the live text at that place.
            Some(TokenKind::Id(_)) if matches!(c.tokens.get(c.i + 1).map(|x| &x.kind), Some(TokenKind::Sym("["))) => self.text_content_at(&mut c)?,
            // A component parameter that is worth a quoted text.
            Some(TokenKind::Id(name)) if self.scopes.iter().any(|e| e.contents.contains_key(name)) => {
                self.scopes.iter().rev().find_map(|e| e.contents.get(name)).unwrap().clone()
            }
            Some(TokenKind::Id(name)) if self.scopes.iter().any(|e| e.strings.contains_key(name)) => {
                let s = self.scopes.iter().rev().find_map(|e| e.strings.get(name)).unwrap().clone();
                match (self.e.locale, self.versions_of(&s)) {
                    (Some(locale), Some(v)) => Content::Translated { locale, versions: v.into_iter().map(Content::Literal).collect() },
                    _ => Content::Literal(s),
                }
            }
            Some(TokenKind::Id(name)) => match self.texts.get(&{ c.mark_name(); self.global(name) }) {
                Some(t) => Content::Live(*t),
                None => {
                    c.i += 1;
                    return self.unknown(&c, "no text", name, self.texts.keys().collect());
                }
            },
            _ => return c.error("a text is `text \"literal\" { … }` or `text name { … }`"),
        };
        let mut p = self.properties(n, vocab::properties("text"))?;
        let in_slot = std::mem::take(&mut self.in_slot);
        let at = match p.get_mut("at") {
            Some(c) => self.point(c)?,
            None if in_slot => (0.0.into(), 0.0.into()),
            None => return Err(CompileError::at(n.line, n.col, "this text is missing 'at'")),
        };
        let mut style = Style::new(14.0, color(1.0, 1.0, 1.0));
        // Its size is an expression like any other: worked out when loading if it
        // does not change (`21 * u`), and at every frame if it does.
        if let Some(c) = p.get_mut("size") {
            match self.expr(c)? {
                Expr::K(v) => style.px = v,
                e => style.size = Some(e),
            }
            c.expect_end()?;
        }
        if let Some(c) = p.get_mut("weight") {
            match self.expr(c)? {
                Expr::K(v) => style.weight = v as u16,
                e => style.weight_of = Some(e),
            }
            c.expect_end()?;
        }
        if let Some(c) = p.get_mut("line_height") {
            style.line_height = self.constant(c, "a text's `line_height:`")?;
        }
        // How many lines: a number, or —like its size— an expression worked out
        // at every frame (`lines: if(open, 12, 2)`); 0 is as many as it has.
        if let Some(c) = p.get_mut("lines") {
            match self.expr(c)? {
                Expr::K(v) if v >= 1.0 => style.max_lines = Some(v as usize),
                Expr::K(_) => style.max_lines = None,
                e => style.lines_of = Some(e),
            }
            c.expect_end()?;
        }
        // `tight: true`: with a width, only as wide as its longest line.
        if let Some(c) = p.get_mut("tight") {
            let v = c.id("true or false")?;
            c.expect_end()?;
            style.tight = match v.as_str() {
                "true" => true,
                "false" => false,
                _ => return Err(CompileError::at(n.line, n.col, "`tight` is true or false")),
            };
        }
        if let Some(c) = p.get_mut("markdown") {
            let v = c.id("true or false")?;
            c.expect_end()?;
            style.markdown = match v.as_str() {
                "true" => true,
                "false" => false,
                _ => return c.error("`markdown` is true or false"),
            };
        }
        if let Some(c) = p.get_mut("family") {
            style.family = Some(interned(&c.string()?));
        }
        if let Some(c) = p.get_mut("color") {
            style.color = self.color(c)?;
        }
        if let Some(c) = p.get_mut("align") {
            style.align = match c.one_of(vocab::TEXT_ALIGNS, "the alignment of a text")?.as_str() {
                "left" => TextAlign::Left,
                "center" => TextAlign::Center,
                "right" => TextAlign::Right,
                _ => unreachable!(),
            };
        }
        let mut anchor = (0.0, 0.0);
        if let Some(c) = p.get_mut("anchor") {
            // `anchor: center` · `anchor: right center` · `anchor: left top`
            let mut axes = [None, None];
            while !c.at_end() {
                let word = c.id("left, center, right, top or bottom")?;
                match word.as_str() {
                    "left" => axes[0] = Some(0.0),
                    "right" => axes[0] = Some(1.0),
                    "top" => axes[1] = Some(0.0),
                    "bottom" => axes[1] = Some(1.0),
                    "center" => {
                        let k = if axes[0].is_none() { 0 } else { 1 };
                        axes[k] = Some(0.5);
                    }
                    _ => return c.error("an anchor is left, center or right, and top, center or bottom"),
                }
            }
            anchor = (axes[0].unwrap_or(0.5), axes[1].unwrap_or(if axes[0] == Some(0.5) { 0.5 } else { 0.0 }));
        }
        let width = match p.get_mut("width") {
            Some(c) => Some(self.expr(c)?),
            None => None,
        };
        let alpha = match p.get_mut("opacity") {
            Some(c) => self.expr(c)?,
            None => Expr::K(1.0),
        };
        // `show:` multiplies the opacity: what is not there is not seen.
        let alpha = match p.get_mut("show") {
            Some(c) => alpha * self.expr(c)?.clamp(0.0, 1.0),
            None => alpha,
        };
        let measure = match p.get_mut("measure") {
            Some(c) => {
                let name = self.global(&c.id("the name of a measure")?);
                match self.measurements.get(&name) {
                    Some(m) => Some(*m),
                    None => return self.unknown(c, "no measure", &name, self.measurements.keys().collect()),
                }
            }
            // Inside a stack, whoever shares out the room needs to know how much it takes.
            None => self.imposed_measure.take(),
        };
        if let Some((w, h)) = measure {
            // (Tight, its slot is what it measures —its longest line—, not its width.)
            self.last_size = Some((if style.tight { w.e() } else { width.clone().unwrap_or(w.e()) }, h.e()));
        }
        // Its effects, if it has any: they go right before it.
        let gradient = match p.get_mut("gradient") {
            Some(c) => Some(self.gradient(n, c)?),
            None => None,
        };
        let outline = match p.get_mut("outline") {
            Some(c) => {
                let w = self.expr(c)?;
                c.expect_sym(",")?;
                Some((w, self.color(c)?))
            }
            None => None,
        };
        let shadow = match p.get_mut("shadow") {
            Some(c) => {
                let dx = self.expr(c)?;
                c.expect_sym(",")?;
                let dy = self.expr(c)?;
                c.expect_sym(",")?;
                let blur = self.expr(c)?;
                c.expect_sym(",")?;
                let alpha = self.expr(c)?;
                let color = if c.sym(",") { Some(self.color(c)?) } else { None };
                Some(Shadow { offset: (dx, dy), blur, alpha, color })
            }
            None => None,
        };
        // Each letter on its own: `letter` is which one (from 0), `letters` how many.
        self.in_letters.set(true);
        let letters = (|| -> R<_> {
            let letter_move = match p.get_mut("letter_move") {
                Some(c) => Some(self.point(c)?),
                None => None,
            };
            let letter_opacity = match p.get_mut("letter_opacity") {
                Some(c) => Some(self.expr(c)?),
                None => None,
            };
            let letter_scale = match p.get_mut("letter_scale") {
                Some(c) => Some(self.expr(c)?),
                None => None,
            };
            Ok((letter_move, letter_opacity, letter_scale))
        })();
        self.in_letters.set(false);
        let (letter_move, letter_opacity, letter_scale) = letters?;
        if gradient.is_some() || outline.is_some() || shadow.is_some() || letter_move.is_some() || letter_opacity.is_some() || letter_scale.is_some() {
            self.e.paint(Instr::TextFx(Box::new(TextFx { gradient, outline, shadow, letter_move, letter_opacity, letter_scale })));
        }
        // `selectable: true`: the mouse selects it and Ctrl+C copies it.
        let selectable = match p.get_mut("selectable") {
            Some(c) => {
                let v = c.id("true or false")?;
                c.expect_end()?;
                match v.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return c.error("`selectable` is true or false"),
                }
            }
            None => false,
        };
        let selection = match p.get_mut("selection") {
            Some(c) => self.color(c)?,
            None => color(0.25, 0.42, 0.62),
        };
        if selectable {
            self.e.paint(Instr::Selectable(selection));
        }
        self.e.paint(Instr::Text { content, at, anchor, width, style, alpha, measure });
        Ok(())
    }

    /// `input query { at: x, y; width: 300; size: 16; placeholder: "Search…" }`: a
    /// field to type in, which edits the live text of that name.
    fn input_field(&mut self, n: &Node) -> R<()> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        let local = c.id("the name of the text it edits")?;
        let name = self.global(&local);
        let Some(text) = self.texts.get(&name).copied() else {
            return self.unknown(&c, "no text", &name, self.texts.keys().collect());
        };
        let mut p = self.properties(n, vocab::properties("input"))?;
        let in_slot = std::mem::take(&mut self.in_slot);
        let at = match p.get_mut("at") {
            Some(c) => self.point(c)?,
            None if in_slot => (0.0.into(), 0.0.into()),
            None => return Err(CompileError::at(n.line, n.col, "this input is missing 'at'")),
        };
        let width = match p.get_mut("width") {
            Some(c) => self.expr(c)?,
            None => return Err(CompileError::at(n.line, n.col, "this input is missing 'width'")),
        };
        let mut style = Style::new(15.0, color(1.0, 1.0, 1.0));
        // (An input's size sets the height of its zone, which is laid out once.)
        if let Some(c) = p.get_mut("size") {
            style.px = self.constant(c, "an input's `size:`")?;
        }
        if let Some(c) = p.get_mut("weight") {
            style.weight = self.constant(c, "an input's `weight:`")? as u16;
        }
        if let Some(c) = p.get_mut("family") {
            style.family = Some(interned(&c.string()?));
        }
        if let Some(c) = p.get_mut("color") {
            style.color = self.color(c)?;
        }
        // What it says while empty: translated like any other text.
        let placeholder = match p.get_mut("placeholder") {
            Some(c) => {
                let s = c.string()?;
                match (self.e.locale, self.versions_of(&s)) {
                    (Some(locale), Some(v)) => Content::Translated { locale, versions: v.into_iter().map(Content::Literal).collect() },
                    _ => Content::Literal(s),
                }
            }
            None => Content::Literal(String::new()),
        };
        // `secret: true`: a password. It is typed the same and painted as dots.
        let secret = match p.get_mut("secret") {
            Some(c) => {
                let v = c.id("true or false")?;
                c.expect_end()?;
                match v.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return c.error("`secret` is true or false"),
                }
            }
            None => false,
        };
        // `passive: true`: a field that is only there to have the keyboard lent to the surface it is on
        // (`focus` it), and takes nothing of what is typed: every key goes on to the rules and the logic,
        // the arrows included. The loan goes to the copy the field is on and to no other.
        let passive = match p.get_mut("passive") {
            Some(c) => {
                let v = c.id("true or false")?;
                c.expect_end()?;
                match v.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return c.error("`passive` is true or false"),
                }
            }
            None => false,
        };
        if secret {
            let mut s = crate::scene::SECRETS.lock().unwrap();
            let own = interned(&name);
            if !s.contains(&own) {
                s.push(own);
            }
        }
        let selection = match p.get_mut("selection") {
            Some(c) => self.color(c)?,
            None => color(0.25, 0.42, 0.62),
        };
        let label = match p.get_mut("label") {
            Some(c) => Some(self.label(c)?),
            None => None,
        };
        let reach = match p.get_mut("agent") {
            Some(c) => reach(c)?,
            None => Reach::Any,
        };
        let alpha = match p.get_mut("opacity") {
            Some(c) => self.expr(c)?,
            None => Expr::K(1.0),
        };
        // `show:` multiplies the opacity: what is not there is not seen. And its zone goes with it:
        // a hidden field took the clicks (and the focus) meant for what was under it.
        let shown = match p.get_mut("show") {
            Some(c) => Some(self.expr(c)?.clamp(0.0, 1.0)),
            None => None,
        };
        let alpha = match &shown {
            Some(v) => alpha * v.clone(),
            None => alpha,
        };
        let height = style.px * style.line_height;
        // Its zone: pressing it focuses it, and over it the cursor is the typing one.
        // It is named like the text: `on drop query`, `on enter query`.
        let zone = self.declare_zone(&local);
        self.candidates.push(Candidate {
            name: zone.clone(),
            at: self.e.instrs.len(),
            zblock: self.zblock,
            shape: Shape::Rect { center: (at.0.clone() + width.clone() * 0.5, at.1.clone() + height * 0.5), half_size: (width.clone() * 0.5, (height * 0.5 + 3.0).into()), radius: 0.0.into() },
            active: None, visible: shown, under: self.under.clone(), forced: true, cursor: Cursor::Text, cursor_when: None, carries: None, carry_icon: None, label, reach, within: None, told: None, handle: None, yields: None,
        });
        self.last_size = Some((width.clone(), height.into()));
        self.e.paint(Instr::Field { text, zone: interned(&zone), at, width, style, alpha, placeholder, selection, secret, passive });
        Ok(())
    }

    /// A figure placed where it belongs: its transform and its strokes.
    ///
    /// `figure hat { at: x, y; size: 44, 34 }` is the whole piece and
    /// `figure hat.brim { … }` one of its layers, **in its place inside the
    /// piece**: two layers drawn separately still fit together, which is what
    /// lets one rotate and the other not. And `pivot:` is the point of the piece
    /// that rests on `at` and around which it rotates: the brim of a hat rotates
    /// around where it rests, not around its centre.
    fn placed_figure(&mut self, n: &Node) -> R<(Transform, Vec<(Shape, Color, Expr)>, Expr)> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        let written = c.id("the name of a figure")?;
        let name = self.global(&written);
        // Figures belong to the scene: a screen copy looks for them with its mark.
        let name = match self.strip_screen_suffix(&name) {
            Some(base) if !self.figures.contains_key(&name) => base,
            _ => name,
        };
        let (piece, layer) = if self.figures.contains_key(&name) {
            (name.clone(), None)
        } else if let Some((f, leaf)) = name.rsplit_once('.') {
            (f.to_owned(), Some(leaf.to_owned()))
        } else {
            return self.unknown(&c, "no figure", &name, self.figures.keys().collect());
        };
        let Some(fig) = self.figures.get(&piece) else {
            return self.unknown(&c, "no figure", &piece, self.figures.keys().collect());
        };
        let size = fig.size;
        let own = |t: &super::figure::Stroke| (t.shape.clone(), t.color, t.alpha, t.thickness);
        let strokes: Vec<(Shape, [f32; 3], f32, f32)> = match &layer {
            None => fig.layers.iter().flat_map(|c| c.strokes.iter()).map(own).collect(),
            Some(which) => match fig.layer(which) {
                Some(c) => c.strokes.iter().map(own).collect(),
                None => {
                    let available = fig.names().join(", ");
                    return Err(CompileError::at(n.line, n.col, format!("'{piece}' has no layer called '{which}': it has {available}")));
                }
            },
        };
        let mut p = self.properties(n, vocab::properties("figure"))?;
        let in_slot = std::mem::take(&mut self.in_slot);
        let at = match p.get_mut("at") {
            Some(c) => self.point(c)?,
            None if in_slot => (0.0.into(), 0.0.into()),
            None => (0.0.into(), 0.0.into()),
        };
        // `size:` paints it at that size; `scale:`, by that factor; with neither,
        // one SVG unit is one pixel.
        let has_scale = p.contains_key("scale");
        let scale = match (p.get_mut("size"), has_scale) {
            (Some(c), _) => {
                let (w, h) = self.point(c)?;
                self.last_size = Some((w.clone(), h.clone()));
                (w / Expr::K(size.0), h / Expr::K(size.1))
            }
            (None, true) => {
                let c = p.get_mut("scale").unwrap();
                let sx = self.expr(c)?;
                let sy = if c.sym(",") { self.expr(c)? } else { sx.clone() };
                self.last_size = Some((Expr::K(size.0) * sx.clone(), Expr::K(size.1) * sy.clone()));
                (sx, sy)
            }
            _ => {
                self.last_size = Some((Expr::K(size.0), Expr::K(size.1)));
                (Expr::K(1.0), Expr::K(1.0))
            }
        };
        // The pivot is given in SVG units and from its centre. And it is only the
        // point around which it **rotates**: `at` still puts the centre of the piece
        // where it is told, so two layers with different pivots still
        // fit together. With the pivot also used for translation, the brim wandered off on its own to
        // somewhere else and the piece came apart at the seams.
        let pivot = match p.get_mut("pivot") {
            Some(c) => self.point(c)?,
            None => (Expr::K(0.0), Expr::K(0.0)),
        };
        let rotate = match p.get_mut("rotate") {
            Some(c) => self.expr(c)?,
            None => Expr::K(0.0),
        };
        let alpha = match p.get_mut("opacity") {
            Some(c) => self.expr(c)?,
            None => Expr::K(1.0),
        };
        let alpha = match p.get_mut("show") {
            Some(c) => alpha * self.expr(c)?.clamp(0.0, 1.0),
            None => alpha,
        };
        // `color:` overrides the SVG's: the same piece in another tone.
        let tint = match p.get_mut("color") {
            Some(c) => Some(self.color(c)?),
            None => None,
        };
        let stroke_width = match p.get_mut("stroke") {
            Some(c) => Some(self.expr(c)?),
            None => None,
        };
        let blend = match p.get_mut("blend") {
            Some(c) => self.expr(c)?,
            None => Expr::K(0.0),
        };
        let placed_items = strokes
            .into_iter()
            .map(|(shape, col, a, thickness)| {
                let shape = match (&stroke_width, thickness) {
                    (Some(g), _) => shape.stroke(g.clone()),
                    (None, g) if g > 0.0 => shape.stroke(Expr::K(g)),
                    _ => shape,
                };
                let color = tint.clone().unwrap_or_else(|| crate::scene::color(col[0], col[1], col[2]));
                (shape, color, alpha.clone() * Expr::K(a))
            })
            .collect();
        // And the arithmetic that makes rotating around one side not move the piece: the
        // scaling is applied around the centre, and the rotation around the
        // pivot. It comes from solving the affine, and with pivot 0 or scale 1 it is `at`.
        let translate = (
            at.0 + pivot.0.clone() * (scale.0.clone() - Expr::K(1.0)),
            at.1 + pivot.1.clone() * (scale.1.clone() - Expr::K(1.0)),
        );
        Ok((Transform { pivot, rotate, scale, translate }, placed_items, blend))
    }

    /// Loose: each stroke, a solid shape.
    fn figure(&mut self, n: &Node) -> R<()> {
        let (affine, strokes, _) = self.placed_figure(n)?;
        self.e.paint(Instr::Transform(Some(affine)));
        for (shape, color, alpha) in strokes {
            self.e.paint(Instr::Solid { shape, color, alpha, glass_spec: None });
        }
        self.e.paint(Instr::Transform(None));
        Ok(())
    }

    /// Inside a body: its strokes blend with the rest, which is what
    /// makes an accessory part of her and not something stuck on top.
    fn figure_in_body(&mut self, n: &Node) -> R<usize> {
        let (affine, strokes, blend) = self.placed_figure(n)?;
        let how_many = strokes.len();
        self.e.paint(Instr::Transform(Some(affine)));
        for (shape, _, _) in strokes {
            self.e.paint(Instr::Shape { shape, fusion: blend.clone() });
        }
        self.e.paint(Instr::Transform(None));
        Ok(how_many)
    }

    fn image(&mut self, n: &Node) -> R<()> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        let name = self.global(&c.id("the name of an image")?);
        let Some(image) = self.images.get(&name).copied() else {
            return self.unknown(&c, "no image", &name, self.images.keys().collect());
        };
        let mut p = self.properties(n, vocab::properties("image"))?;
        let in_slot = std::mem::take(&mut self.in_slot);
        let missing = |q: &str| CompileError::at(n.line, n.col, format!("this image is missing '{q}'"));
        let (x, y) = match p.get_mut("at") {
            Some(c) => self.point(c)?,
            None if in_slot => (0.0.into(), 0.0.into()),
            None => return Err(missing("at")),
        };
        let (w, h) = self.point(p.get_mut("size").ok_or_else(|| missing("size"))?)?;
        self.last_size = Some((w.clone(), h.clone()));
        let alpha = match p.get_mut("opacity") {
            Some(c) => self.expr(c)?,
            None => Expr::K(1.0),
        };
        // `show:` multiplies the opacity: what is not there is not seen.
        let alpha = match p.get_mut("show") {
            Some(c) => alpha * self.expr(c)?.clamp(0.0, 1.0),
            None => alpha,
        };
        let tint = match p.get_mut("tint") {
            Some(c) => Some(self.color(c)?),
            None => None,
        };
        // `cell: which, columns, rows`: one picture of a sheet of them.
        let cell = match p.get_mut("cell") {
            Some(c) => {
                let which = self.expr(c)?;
                c.expect_sym(",")?;
                let columns = self.expr(c)?;
                let rows = if c.sym(",") { self.expr(c)? } else { Expr::K(1.0) };
                Some((which, columns, rows))
            }
            None => None,
        };
        let fit = match p.get_mut("fit") {
            Some(c) => {
                let word = c.one_of(vocab::IMAGE_FITS, "how an image fills its box")?;
                vocab::IMAGE_FITS.iter().position(|m| *m == word).unwrap_or(0) as u8
            }
            None => 0,
        };
        self.e.paint(Instr::Image { image, target: (x, y, w, h), alpha, tint, cell, fit });
        Ok(())
    }

    /// `gradient: 0, 0 to 0, 44, mint, sand 40%, coal` · `gradient: radial x, y radius r, …`:
    /// for a `body` and for a `text`.
    fn gradient(&self, n: &Node, c: &mut Cur) -> R<Paint> {
        // `gradient: radial 20, 20 radius 30 { … }`: from a centre outwards.
        let radial = c.word("radial");
        let from = self.point(c)?;
        let a = if radial {
            c.expect_word("radius")?;
            (self.expr(c)?, Expr::K(0.0))
        } else {
            // `0, 0 to 0, 44` reads the same as `0, 0, 0, 44`.
            if !c.word("to") {
                c.expect_sym(",")?;
            }
            self.point(c)?
        };
        // The colours, separated by commas. Each one can say where it falls
        // (`sand 40%`); those that do not say are spread evenly. Two plain
        // colours is the usual gradient.
        let mut stops: Vec<(Expr, Color)> = Vec::new();
        // Which ones did not say where. Marking them with a -1 will not do: `mint -10%` is already
        // a constant -0.1 when read, and it would be confused with not saying anything.
        let mut unplaced: Vec<bool> = Vec::new();
        while c.sym(",") {
            let col = self.color(c)?;
            let given = !c.at_end() && !matches!(c.peek(), Some(TokenKind::Sym(",")));
            stops.push((if given { self.expr(c)? } else { Expr::K(0.0) }, col));
            unplaced.push(!given);
        }
        c.expect_end()?;
        if stops.len() < 2 {
            return Err(CompileError::at(n.line, n.col, "a gradient needs at least two colours: `gradient: 0, 0 to 0, 44, mint, sand 40%, coal`"));
        }
        if stops.len() > 8 {
            return Err(CompileError::at(n.line, n.col, "a gradient holds at most 8 colours"));
        }
        let last = stops.len() - 1;
        for (k, stop) in stops.iter_mut().enumerate() {
            if unplaced[k] {
                stop.0 = Expr::K(k as f32 / last as f32);
            }
        }
        Ok(Paint::Gradient { radial, from, to: a, stops })
    }

    /// `shader aurora { at: 360, 60; size: 400, 120; corner: 20; values: open, glow; colors: mint, deep }`
    fn shader(&mut self, n: &Node) -> R<()> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        let name = self.global(&c.id("the name of a shader")?);
        let Some(k) = self.shaders.get(&name).copied() else {
            return self.unknown(&c, "no shader", &name, self.shaders.keys().collect());
        };
        let mut p = self.properties(n, vocab::properties("shader"))?;
        let in_slot = std::mem::take(&mut self.in_slot);
        let missing = |q: &str| CompileError::at(n.line, n.col, format!("this shader is missing '{q}'"));
        let (x, y) = match p.get_mut("at") {
            Some(c) => self.point(c)?,
            None if in_slot => (0.0.into(), 0.0.into()),
            None => return Err(missing("at")),
        };
        let (w, h) = self.point(p.get_mut("size").ok_or_else(|| missing("size"))?)?;
        self.last_size = Some((w.clone(), h.clone()));
        let corner = match p.get_mut("corner") {
            Some(c) => self.expr(c)?,
            None => Expr::K(0.0),
        };
        let alpha = match p.get_mut("opacity") {
            Some(c) => self.expr(c)?,
            None => Expr::K(1.0),
        };
        let alpha = match p.get_mut("show") {
            Some(c) => alpha * self.expr(c)?.clamp(0.0, 1.0),
            None => alpha,
        };
        // Up to eight numbers and two colours: what the scene tells it.
        let mut values = Vec::new();
        if let Some(c) = p.get_mut("values") {
            loop {
                values.push(self.expr(c)?);
                if !c.sym(",") {
                    break;
                }
            }
            if values.len() > 8 {
                return c.error("a shader takes up to eight numbers: `s.a` and `s.b`, four each");
            }
        }
        let mut colors = Vec::new();
        if let Some(c) = p.get_mut("colors") {
            loop {
                colors.push(self.color(c)?);
                if !c.sym(",") {
                    break;
                }
            }
            if colors.len() > 2 {
                return c.error("a shader takes up to two colours: `s.color` and `s.color2`");
            }
        }
        let u = self.e.shaders[k as usize].clone();
        // The time only if it reads it: a shader that does not move must let the
        // surface rest. It is the scene's `time`, created now if nobody named it.
        let time = u.animated.then(|| self.time_prop().e());
        // Over the box it is drawn in, which is this surface's own: a shader in
        // a surface of its own, or in a monitor's copy, reads it as the rules do.
        let here = |n: &str| self.scopes.iter().rev().find_map(|e| e.exprs.get(n)).cloned().unwrap_or_else(|| self.facts[n].e());
        let pointer = u.pointer.then(|| (here("pointer.x"), here("pointer.y")));
        self.e.paint(Instr::Shader { shader: k, target: (x, y, w, h), corner, alpha, values, colors, time, pointer, behind: u.behind });
        Ok(())
    }

    /// `particles sparks { at: 200, 100; count: 300; life: 0.6s .. 1.4s; speed: 40 .. 160; … }`
    fn particles(&mut self, n: &Node) -> R<()> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        // A name, if it likes: it says what they are, nothing refers to it yet.
        if matches!(c.peek(), Some(TokenKind::Id(_))) {
            let _ = c.id("a name")?;
        }
        c.expect_end()?;
        let mut p = self.properties(n, vocab::properties("particles"))?;
        let missing = |q: &str| CompileError::at(n.line, n.col, format!("these particles are missing '{q}'"));
        let at = self.point(p.get_mut("at").ok_or_else(|| missing("at"))?)?;
        // `a .. b`, or one number that is both.
        let range = |o: &Self, c: &mut Cur| -> R<(Expr, Expr)> {
            let a = o.expr(c)?;
            Ok(if c.sym("..") { (a, o.expr(c)?) } else { (a.clone(), a) })
        };
        let pair = |o: &Self, c: &mut Cur| -> R<(Expr, Expr)> {
            let a = o.expr(c)?;
            Ok(if c.sym(",") { (a, o.expr(c)?) } else { (a.clone(), a) })
        };
        let area = match p.get_mut("area") {
            Some(c) => self.point(c)?,
            None => (Expr::K(0.0), Expr::K(0.0)),
        };
        let count = match p.get_mut("count") {
            Some(c) => match self.expr(c)? {
                Expr::K(v) if (1.0..=4096.0).contains(&v) => v as u32,
                Expr::K(_) => return c.error("between 1 and 4096 particles per emitter"),
                _ => return c.error("how many particles there are is a number, not something that changes: it is how many the card reserves"),
            },
            None => 100,
        };
        let life = match p.get_mut("life") {
            Some(c) => range(self, c)?,
            None => (Expr::K(1.0), Expr::K(1.0)),
        };
        let speed = match p.get_mut("speed") {
            Some(c) => range(self, c)?,
            None => (Expr::K(60.0), Expr::K(60.0)),
        };
        let direction = match p.get_mut("direction") {
            Some(c) => self.expr(c)?,
            None => Expr::K(-std::f32::consts::FRAC_PI_2),
        };
        let spread = match p.get_mut("spread") {
            Some(c) => self.expr(c)?,
            None => Expr::K(std::f32::consts::TAU),
        };
        let gravity = match p.get_mut("gravity") {
            Some(c) => self.point(c)?,
            None => (Expr::K(0.0), Expr::K(0.0)),
        };
        let drag = match p.get_mut("drag") {
            Some(c) => self.expr(c)?,
            None => Expr::K(0.0),
        };
        let size = match p.get_mut("size") {
            Some(c) => pair(self, c)?,
            None => (Expr::K(4.0), Expr::K(4.0)),
        };
        let opacity = match p.get_mut("opacity") {
            Some(c) => pair(self, c)?,
            None => (Expr::K(1.0), Expr::K(0.0)),
        };
        let colors = match p.get_mut("colors") {
            Some(c) => {
                let a = self.color(c)?;
                let b = if c.sym(",") { self.color(c)? } else { a.clone() };
                (a, b)
            }
            None => (color(1.0, 1.0, 1.0), color(1.0, 1.0, 1.0)),
        };
        let shape = match p.get_mut("shape") {
            Some(c) => match c.one_of(vocab::PARTICLE_SHAPES, "the shape of a particle")?.as_str() {
                "square" => ParticleShape::Square,
                "spark" => ParticleShape::Spark,
                _ => ParticleShape::Dot,
            },
            None => ParticleShape::Dot,
        };
        let burst = match p.get_mut("burst") {
            Some(c) => Some(self.signal(c)?),
            None => None,
        };
        let emit = match p.get_mut("emit") {
            Some(c) if burst.is_some() => return c.error("particles are either born all the time (`emit:`) or all at once (`burst:`), not both"),
            Some(c) => self.expr(c)?,
            None => Expr::K(if burst.is_some() { 0.0 } else { 1.0 }),
        };
        let alpha = match p.get_mut("show") {
            Some(c) => self.expr(c)?.clamp(0.0, 1.0),
            None => Expr::K(1.0),
        };
        self.e.paint(Instr::Particles(Box::new(Particles { at, area, count, life, speed, direction, spread, gravity, drag, size, colors, opacity, shape, emit, burst, alpha })));
        Ok(())
    }

    /// A `clip` loose in the scene clips everything after it, up to the next
    /// named `surface` —not to the end of what it seems to belong to—. When that
    /// is meant (Marea's card), fine; when it is not, whatever was written below
    /// it came out clipped to a shape that may be closed, that is, not at all,
    /// and nothing said so. Now something does, once, when the scene is read.
    fn note_loose_clips(&self, body: &[Entry]) {
        let head = |e: &Entry| match e {
            Entry::Node(n) => n.head.first().and_then(|t| if let TokenKind::Id(w) = &t.kind { Some(w.clone()) } else { None }),
            _ => None,
        };
        for (k, e) in body.iter().enumerate() {
            let Entry::Node(n) = e else { continue };
            if head(e).as_deref() != Some("clip") {
                continue;
            }
            let mut reached = 0;
            let mut until = None;
            for later in &body[k + 1..] {
                match (head(later).as_deref(), later) {
                    (Some("surface"), Entry::Node(m)) if m.head.len() > 1 => {
                        until = Some(m);
                        break;
                    }
                    (Some(w), _) if vocab::STATEMENTS.contains(&w) && !matches!(w, "clip" | "let" | "prop" | "fact" | "event" | "text" | "on" | "follow" | "spin" | "wave" | "blink" | "look" | "gesture" | "posture" | "layer" | "spring" | "pose" | "measure" | "model" | "service" | "permissions" | "every" | "image" | "figure" | "component" | "translations") => reached += 1,
                    // A copy of a component, or a text being painted, also draws.
                    (Some(_), Entry::Node(m)) if m.body.is_some() => reached += 1,
                    _ => {}
                }
            }
            if reached == 0 {
                continue;
            }
            let file = self.files.get(n.line / super::PER_FILE).map_or("", String::as_str);
            let line = n.line % super::PER_FILE;
            let limit = until.map_or("the end of the scene".to_owned(), |m| format!("`surface` on line {}", m.line % super::PER_FILE));
            eprintln!("note   · {file}:{line}: this `clip` is loose in the scene: it also clips the {reached} drawings after it, up to {limit}. To clip only some, put it inside a `group` with them");
        }
    }

    /// `time`: the seconds since the scene started. Created the first time
    /// something needs it, with the hand that keeps it going.
    fn time_prop(&mut self) -> PropId {
        if let Some(p) = self.props.get("time") {
            return *p;
        }
        let p = self.e.prop("time", 0.0);
        self.props.insert("time".into(), p);
        self.e.shader_clock = Some(p);
        self.e.behaviors.push(Behavior::Advance { prop: p, per_second: Expr::K(1.0) });
        p
    }

    /// `surface { … }` is the scene's window; `surface bar { …; …drawing… }`, one of
    /// several, each one with its own things inside. They all share properties, facts and rules:
    /// inside they are different pieces of the same plane, like the popups.
    fn surface(&mut self, n: &'a Node) -> R<()> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        let name = match c.peek() {
            Some(TokenKind::Id(_)) => self.declare(&c.id("a name for the surface")?),
            _ => String::new(),
        };
        c.expect_end()?;
        let draws = n.body.as_deref().unwrap_or(&[]).iter().any(|e| matches!(e, Entry::Node(_)));
        if name.is_empty() && draws {
            return Err(CompileError::at(n.line, n.col, "a surface that carries what it draws needs a name: `surface bar { … }`. Without a name it is the scene\'s own, and it draws whatever is loose"));
        }
        // Pass 2: what it draws, in its piece of the plane.
        if self.pass == 2 {
            if !draws {
                return Ok(());
            }
            let copies: Vec<(f32, f32, usize, bool)> = self.e.surfaces.iter().filter(|s| s.name == name).map(|s| (s.origin.0, s.origin.1, s.instance, matches!(s.screens, Screens::Number(_)))).collect();
            // A lock screen covers all of its monitor, whatever its `size:`
            // says: inside it, what the monitor measures is what it measures.
            let lock = self.e.surfaces.iter().any(|s| s.name == name && s.lock_screen);
            for (ox, oy, instance, per_screen) in copies {
                let mark = self.rules.len();
                // With `screens: each`, each copy has its own: its properties, its zones
                // and its rules. `$screen` is its number, and `screen.name` that of its monitor.
                let mut env = if per_screen {
                    let mut env = self.screen_scope(instance);
                    // (What this copy measures, see `screens: each` below.)
                    if instance > 0 && !name.is_empty() && !lock {
                        for part in ["width", "height"] {
                            env.alias.insert(format!("{name}.{part}"), format!("{name}.screen{instance}.{part}"));
                        }
                    }
                    Some(env)
                } else if lock {
                    let mut env = Scope::default();
                    for part in ["width", "height"] {
                        env.alias.insert(format!("screen.{part}"), format!("{name}.{part}"));
                    }
                    Some(env)
                } else {
                    None
                };
                // Its piece of the plane is out of the way, and the mouse
                // arrives in the plane's coordinates: a rule reads it the way
                // this surface draws, from its own corner, as the scene's
                // surface's copies do. Read raw, a press at the top of a
                // surface said `pointer.y` 90 000.
                if ox != 0.0 || oy != 0.0 {
                    let env = env.get_or_insert_with(Scope::default);
                    for (n, off) in [("pointer.x", ox), ("pointer.y", oy)] {
                        if off != 0.0 {
                            env.exprs.insert(n.to_owned(), Expr::Sub(Box::new(self.facts[n].e()), Box::new(Expr::K(off))));
                        }
                    }
                }
                let scoped = env.is_some();
                if let Some(env) = env {
                    self.scopes.push(Rc::new(env));
                }
                let t = Transform { translate: (ox.into(), oy.into()), ..Transform::at((0.0.into(), 0.0.into())) };
                self.e.paint(Instr::Transform(Some(t.clone())));
                self.under.push(t);
                self.group(n.body.as_deref().unwrap_or(&[]).iter());
                self.under.pop();
                self.e.paint(Instr::Transform(None));
                if per_screen {
                    self.close_scope(mark);
                } else if scoped {
                    self.scopes.pop();
                }
            }
            return Ok(());
        }
        if self.e.surfaces.iter().any(|s| s.name == name) {
            return Err(CompileError::at(n.line, n.col, match name.as_str() {
                "" => "the scene already has its surface: the others carry a name (`surface panel { … }`)".to_owned(),
                _ => format!("there is already a surface called '{name}'"),
            }));
        }
        let mut p = self.properties(n, vocab::properties("surface"))?;
        // `screens: each [max 4]`: one surface per monitor, each one with its state.
        let mut how_many = None;
        if let Some(c) = p.get_mut("screens") {
            if c.word("each") {
                let limit = if c.word("max") { c.num()? as usize } else { 4 };
                if !(1..=8).contains(&limit) {
                    return Err(CompileError::at(n.line, n.col, "`each` takes between 1 and 8 monitors: each one unfolds when loading"));
                }
                how_many = Some(limit);
            }
            // It is read again further down, on the surface by then.
            c.i = 0;
        }
        // The main one is the first, whether it has a name or not.
        let fresh = Surface { name: name.clone(), ..Default::default() };
        if name.is_empty() {
            self.e.surfaces.insert(0, fresh);
        } else {
            self.next_origin += 10000.0;
            // What it really measures, once the compositor says: `panel.width`,
            // `panel.height`. With `full`, it is the only way to know it.
            let size_props = match self.props.get(&format!("{name}.width")) {
                Some(w) => Some((*w, self.props[&format!("{name}.height")])),
                None => {
                    let (w, h) = self.e.measured(interned(&name));
                    self.props.insert(format!("{name}.width"), w);
                    self.props.insert(format!("{name}.height"), h);
                    Some((w, h))
                }
            };
            // And where the mouse is, in this surface's own coordinates:
            // `cursor.x` is the scene surface's, and a surface of its own draws
            // from its own corner.
            let cursor_props = match self.props.get(&format!("{name}.cursor.x")) {
                Some(x) => Some((*x, self.props[&format!("{name}.cursor.y")])),
                None => {
                    let (x, y) = self.e.surface_cursor(interned(&name));
                    self.props.insert(format!("{name}.cursor.x"), x);
                    self.props.insert(format!("{name}.cursor.y"), y);
                    Some((x, y))
                }
            };
            self.e.surfaces.push(Surface { origin: (0.0, self.next_origin), size_props, cursor_props, ..fresh });
        }
        let which = self.e.surfaces.iter().position(|s| s.name == name).unwrap();
        let mut pending_open = None;
        let mut pending_anchor = None;
        if let Some(c) = p.get_mut("open") {
            // A whole expression, not just a fact: `open: tuck > 0.01` lets a
            // surface stay there while what it carries inside finishes leaving.
            pending_open = Some((&c.tokens[c.i..], c.pos()));
            c.i = c.tokens.len();
        }
        if let Some((fact, pos)) = pending_open {
            // Like the keyboard's `while`: it can name a fact declared further down.
            self.pending_surfaces.push((which, fact, pos));
        }
        let s = &mut self.e.surfaces[which];
        if let Some(c) = p.get_mut("size") {
            // `size: full, 36`: the whole width of the monitor; `full, full`, all of it.
            s.width = if c.word("full") { 0 } else { c.num()? as u32 };
            c.expect_sym(",")?;
            s.height = if c.word("full") { 0 } else { c.num()? as u32 };
        }
        if let Some(c) = p.get_mut("anchor") {
            let pos = c.pos();
            let word = c.id("the anchor of a surface")?;
            match SurfaceAnchor::from_word(&word) {
                Some(a) => s.anchor = a,
                // If it is not one of the nine words, it has to be a typed fact
                // whose values are anchors. It is looked at the end: it may be
                // declared further down, like the keyboard condition.
                None => pending_anchor = Some((word, pos)),
            }
            c.expect_end()?;
        }
        if let Some(c) = p.get_mut("margin") {
            for k in 0..4 {
                s.margin[k] = c.num()? as i32;
                if k < 3 && !c.sym(",") {
                    break;
                }
            }
        }
        if let Some(c) = p.get_mut("level") {
            let level = |c: &mut Cur| -> R<Level> {
                Ok(match c.one_of(vocab::LEVELS, "a level")?.as_str() {
                    "background" => Level::Background,
                    "bottom" => Level::Below,
                    "top" => Level::Above,
                    "overlay" => Level::Overlay,
                    _ => unreachable!(),
                })
            };
            s.level = level(c)?;
            // `level: top, overlay while open`: another one while that holds.
            if c.sym(",") {
                let raised = level(c)?;
                c.expect_word("while")?;
                self.pending_levels.push((which, raised, &c.tokens[c.i..], c.pos()));
                c.i = c.tokens.len();
            }
        }
        // `kind: window`: a normal window, which the compositor decorates and places. What
        // belongs to a panel —anchor, level, reserve, monitor— does not apply to it.
        if let Some(c) = p.get_mut("kind") {
            match c.one_of(vocab::SURFACE_KINDS, "what kind of surface this is")?.as_str() {
                // (A window has its frame's close button: the prototype's emergency exit —a right click closes
                // a scene that does not use the right button— is not for it. An app closed at a right click on
                // anything, with nothing to say why.)
                "window" => {
                    s.window = Some(String::new());
                    s.right_click_quits = false;
                }
                // `kind: lock`: the lock screen. It takes the whole monitor
                // —how big it is, the compositor says— and goes on all of them.
                // Its keyboard is its own, whatever the first surface says: taken
                // from a first surface with none, a lock screen never asked for
                // the keyboard and its password could not be typed (pleamar-wm#38).
                "lock" => {
                    s.lock_screen = true;
                    s.right_click_quits = false;
                    s.screens = Screens::All;
                    s.keyboard = Keyboard::Always;
                    s.keyboard_said = true;
                    s.keyboard_when = None;
                }
                _ => {}
            }
        }
        if let Some(c) = p.get_mut("title") {
            let title = c.string()?;
            c.expect_end()?;
            match &mut s.window {
                Some(t) => *t = title,
                None => return Err(CompileError::at(n.line, n.col, "only a window has a title: add `kind: window`")),
            }
        }
        if let Some(c) = p.get_mut("app_id") {
            let id = c.string()?;
            c.expect_end()?;
            if s.window.is_none() {
                return Err(CompileError::at(n.line, n.col, "only a window has an app_id: add `kind: window`"));
            }
            if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch)) {
                return Err(CompileError::at(n.line, n.col, "an app_id is letters, digits, `.`, `_` and `-`, as a `.desktop` file is named: `app_id: \"org.me.notes\"`"));
            }
            s.app_id = Some(id);
        }
        if s.window.is_some() && s.width == 0 {
            return Err(CompileError::at(n.line, n.col, "a window says how wide it is: `full` is for a panel, which is as wide as its monitor"));
        }
        if let Some(c) = p.get_mut("keyboard") {
            s.keyboard = match c.one_of(vocab::KEYBOARD_MODES, "how the keyboard is asked for")?.as_str() {
                "none" => Keyboard::Never,
                "on_demand" => Keyboard::OnDemand,
                "exclusive" => Keyboard::Always,
                _ => unreachable!(),
            };
            s.keyboard_said = true;
            // `keyboard: exclusive while open`: only while that is true, for this
            // surface; and, as it always was, the scene's condition for the
            // surfaces that do not write one of their own (the last one written).
            // The condition is read at the end: it can name a fact declared further down.
            if c.word("while") {
                s.keyboard_while = true;
                self.pending_keyboard = Some((&c.tokens[c.i..], c.pos()));
                self.pending_keyboards.push((which, &c.tokens[c.i..], c.pos()));
                c.i = c.tokens.len();
            } else {
                c.expect_end()?;
            }
        }
        if let Some(c) = p.get_mut("reserve") {
            s.exclusive_zone = c.num()? as i32;
            // `reserve: 72 while taking_room`: only while that holds.
            if c.word("while") {
                self.pending_reserves.push((which, &c.tokens[c.i..], c.pos()));
                c.i = c.tokens.len();
            } else {
                c.expect_end()?;
            }
        }
        // `rate: 60`: at most, that many frames per second, on any monitor.
        // `captures: hidden`: on the monitor, not in what is captured of it.
        if let Some(c) = p.get_mut("captures") {
            s.hidden_from_captures = c.one_of(vocab::CAPTURES, "what it does in captures")? == "hidden";
        }
        // `agent: hidden`: what it holds is not told to an agent.
        if let Some(c) = p.get_mut("agent") {
            match reach(c)? {
                Reach::Hidden => s.agent_hidden = true,
                Reach::Person => return c.error("a surface is `agent: hidden` or nothing: `agent: no` goes on the zones a person's hand keeps"),
                Reach::Any => {}
            }
        }
        if let Some(c) = p.get_mut("rate") {
            let r = c.num()?;
            if !(1.0..=480.0).contains(&r) {
                return Err(CompileError::at(n.line, n.col, "`rate` is how many frames a second at most, from 1 to 480; without it, the monitor's"));
            }
            s.max_fps = r as u32;
            s.rate_said = true;
            // `rate: 60, 20 while asleep, 5 while away`: fewer still while that holds, and
            // then fewer again, each one lower than the one before it. The first whose
            // condition holds is the one that rules, so they go from the busiest down.
            let mut prev = r;
            let mut step = 0;
            while c.sym(",") {
                let low = c.num()?;
                if !(1.0..=prev).contains(&low) {
                    let which_one = if step == 0 { "the second `rate` is the lower one".to_string() } else { format!("`rate` number {} is lower still", step + 2) };
                    return Err(CompileError::at(n.line, n.col, format!("{which_one}, for while something holds: from 1 to {prev}")));
                }
                c.expect_word("while")?;
                // The condition reaches to the next comma that is not inside brackets, so
                // that `while mix(a, b) > 0.5` is one condition and not two.
                let from = c.i;
                let at = c.pos();
                let to = condition_end(c.tokens, from);
                self.pending_rates.push((which, step, low as u32, &c.tokens[from..to], at));
                c.i = to;
                prev = low;
                step += 1;
            }
            c.expect_end()?;
        }
        if let Some(c) = p.get_mut("screens") {
            s.screens = if c.word("all") {
                Screens::All
            } else if c.word("each") {
                if c.word("max") {
                    let _ = c.num()?;
                }
                Screens::Number(0)
            } else if matches!(c.peek(), Some(TokenKind::Str(_))) {
                let mut v = vec![c.string()?];
                while c.sym(",") {
                    v.push(c.string()?);
                }
                Screens::Named(v)
            } else {
                // A text fact whose value is the monitor's name: looked for at
                // the end, as it may be declared further down.
                let pos = c.pos();
                let word = c.id("the monitors of a surface: `all`, `each`, names in quotes, or a text")?;
                c.expect_end()?;
                self.pending_screens.push((which, word, pos));
                Screens::Named(Vec::new())
            };
        }
        if let Some((name, pos)) = pending_anchor {
            self.pending_anchors.push((which, name, pos));
        }
        // One copy per monitor: the same surface, each one in its piece of the plane.
        if let Some(limit) = how_many {
            let base = self.e.surfaces[which].clone();
            for k in 1..limit {
                self.next_origin += 10000.0;
                let mut copy = Surface { instance: k, origin: (0.0, self.next_origin), screens: Screens::Number(k), ..base.clone() };
                // Each copy measures its own monitor: `panel.width` inside it is
                // its own. Shared, the last copy to be measured said it for all —
                // a phone's monitor of 442 came and went, and the bar on a 1920
                // one stayed laid out for 442—. (The lock's copies do share it:
                // the largest, see the render.)
                if !base.name.is_empty() && !base.lock_screen {
                    let (w, h) = self.e.measured(interned(&format!("{}.screen{k}", base.name)));
                    self.props.insert(format!("{}.screen{k}.width", base.name), w);
                    self.props.insert(format!("{}.screen{k}.height", base.name), h);
                    copy.size_props = Some((w, h));
                }
                // The main one's copies go together at the start: the first one is still the main one.
                if base.name.is_empty() {
                    self.e.surfaces.insert(k, copy);
                } else {
                    self.e.surfaces.push(copy);
                }
            }
            // What the render reports about each monitor, and how many there are.
            // Once: a second `screens: each` surface —the lock, a corner— speaks
            // of the same monitors, and a second `screen.0.name` would be the one
            // the scene names while the render fills the first, so it stayed empty.
            for k in 0..limit {
                if !self.texts.contains_key(&format!("screen.{k}.name")) {
                    let t = self.e.live_text(interned(&format!("screen.{k}.name")), "");
                    self.texts.insert(format!("screen.{k}.name"), t);
                }
                for part in ["width", "height"] {
                    let full_name = format!("screen.{k}.{part}");
                    if !self.facts.contains_key(&full_name) {
                        let h = self.e.fact(interned(&full_name), 0.0);
                        self.facts.insert(full_name, h);
                    }
                }
                // And its number, as a fact: that way a loose `let` —which is read
                // once, not once per copy— can say `if(screen.index == 0, …)`
                // and be worth something different in each one, which is what a
                // little ball crossing from one monitor to the other needs without wrapping three thousand
                // lines in a group.
                let full_name = format!("screen.{k}.index");
                if !self.facts.contains_key(&full_name) {
                    let h = self.e.fact(interned(&full_name), k as f32);
                    self.facts.insert(full_name, h);
                }
            }
            if !self.facts.contains_key("screens.count") {
                let h = self.e.fact("screens.count", 0.0);
                self.facts.insert("screens.count".into(), h);
            }
        }
        Ok(())
    }

    /// The property a big `let` is computed in, once per frame: the one that
    /// already computes the same thing, or a new one.
    fn computed_once(&mut self, name: &str, e: Expr) -> PropId {
        let key = format!("{e:?}");
        if let Some(p) = self.let_props.get(&key) {
            return *p;
        }
        self.copies += 1;
        let p = self.e.prop_with(interned(&format!("·{name}{}", self.copies)), 0.0, Spring::LIVELY);
        self.e.behaviors.push(Behavior::Bind { prop: p, to: e });
        if !self.later_copy {
            self.let_props.insert(key, p);
        }
        p
    }

    /// What several children of a layout read —how far along the ones before
    /// reach, the breadth of the fattest, how many are there—: big, it is
    /// computed once per frame in a property, and each child reads that. Written
    /// out in each of them, and in each of their zones, a list of *n* rows was
    /// *n²* nodes.
    fn shared(&mut self, what: &str, e: Expr) -> Expr {
        if e.node_count() > 8 { self.computed_once(what, e).e() } else { e }
    }

    /// Whether a loose `let` is worth something different in each monitor's
    /// copy: it reads `screen.*`, or another `let` that does and is computed
    /// in a property per copy.
    fn is_per_copy(&self, e: &Expr) -> bool {
        if e.reads(|p| self.e.facts.get(p.0 as usize).is_some_and(|(n, _)| n.starts_with("screen."))) {
            return true;
        }
        if self.copy_props.is_empty() {
            return false;
        }
        let (mut props, mut facts) = (Vec::new(), Vec::new());
        e.inputs(&mut props, &mut facts);
        props.iter().any(|p| self.copy_props.contains(p))
    }

    /// Inside a `screens: each` surface: what holds for that copy.
    fn screen_scope(&self, k: usize) -> Scope {
        let mut env = Scope { suffix: format!("#screen{k}"), ..Default::default() };
        // `$screen` in a name is its number, like `$i` in a `repeat`.
        env.exprs.insert("screen".to_owned(), Expr::K(k as f32));
        // And `screen.name`, `screen.width`, `screen.height`, `screen.index` are those of ITS monitor.
        for part in ["name", "width", "height", "index"] {
            env.alias.insert(format!("screen.{part}"), format!("screen.{k}.{part}"));
        }
        env.with_parts.insert("screen".to_owned());
        env
    }

    // ── layers ──────────────────────────────────────────────────

    /// `layer card ~calm { open while open { orb.x: 140 ~lively after 70ms } rest { … } }`
    fn layer(&mut self, n: &Node, c: &mut Cur) -> R<()> {
        let name = self.declare(&c.id("a name for the layer")?);
        let spring = if c.sym("~") { self.spring(c)? } else { Spring::QUICK };
        c.expect_end()?;
        let mut claims = Vec::new();
        let mut names = Vec::new();
        for e in n.body.as_deref().unwrap_or(&[]) {
            let Entry::Node(r) = e else {
                return Err(CompileError::at(n.line, n.col, "a layer holds only claims: `name while …`, `name for 700ms after …`, `name { … }`"));
            };
            let mut c = Cur::new(&r.head, r.line, r.col);
            let who = c.id("a name for the claim")?;
            let when = if c.word("while") {
                When::While(self.expr(&mut c)?)
            } else if c.word("for") {
                let duration = c.dur()?;
                c.expect_word("after")?;
                When::After { signals: self.signals(&mut c)?, duration }
            } else if c.word("from") {
                let from = self.signals(&mut c)?;
                c.expect_word("until")?;
                When::FromUntil { from, until: self.signals(&mut c)? }
            } else {
                When::Always
            };
            c.expect_end()?;
            let mut sets = Vec::new();
            for s in r.body.as_deref().unwrap_or(&[]) {
                match s {
                    Entry::Prop { name, value, line, col } => sets.push(self.transition(name, value, *line, *col)?),
                    Entry::Node(x) => return Err(CompileError::at(x.line, x.col, "here go properties and where they travel to: `orb.x: 140 ~lively after 70ms`")),
                }
            }
            names.push(who.clone());
            claims.push(Claim { name: interned(&who), when, sets });
        }
        if claims.is_empty() {
            return Err(CompileError::at(n.line, n.col, "a layer with no claims decides nothing"));
        }
        let layer = self.e.layer(interned(&name), spring, claims);
        // `card.open` is 1 while it wins, and comes and goes with the layer's spring.
        for (k, who) in names.iter().enumerate() {
            self.props.insert(format!("{name}.{who}"), layer.presence(k));
        }
        Ok(())
    }

    /// `orb.x: 140 ~lively after 70ms`
    fn transition(&self, name: &str, value: &[Token], line: usize, col: usize) -> R<Transition> {
        PEEKED.with(|m| m.set((line, col)));
        let name = &self.global(name);
        let Some(prop) = self.props.get(name).copied() else {
            let hint = closest_match(name, self.props.keys()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
            return Err(CompileError::at(line, col, format!("there is no property called '{name}'.{hint}")));
        };
        let mut c = Cur::new(value, line, col);
        let a = self.expr(&mut c)?;
        // Without `~`, the property's. Writing `prop padlock = 0 ~150ms` and then having
        // `padlock: 1` go at 250 was contradicting the declaration: the spring
        // belongs to the property, and the rule only changes it if it says so.
        let spring = if c.sym("~") { self.spring(&mut c)? } else { self.e.spring_of(prop) };
        let delay = if c.word("after") { c.dur()? } else { Duration::ZERO };
        c.expect_end()?;
        Ok(Transition { prop, to: a, spring, delay })
    }

    // ── components, repetitions and stacks ─────────────────────

    /// `component Chip(label, tone) { size: …; … }`
    fn declare_component(&mut self, n: &'a Node, c: &mut Cur) -> R<()> {
        let name = c.id("a name for the component")?;
        let strict = self.strict_files.contains(&(n.line / super::PER_FILE));
        // (r: record, chosen: event, tone: color = mint, height = 30)
        let mut params: Vec<Param> = Vec::new();
        if c.sym("(") && !c.sym(")") {
            loop {
                let name = c.id("a parameter name")?;
                if params.iter().any(|p| p.name == name) {
                    c.i -= 1;
                    return c.error(format!("'{name}' is already a parameter of this component"));
                }
                let kind = if c.sym(":") { Some(c.one_of(vocab::PARAMETER_TYPES, "the type of a parameter")?) } else { None };
                let fallback = if c.sym("=") {
                    // Up to the comma or the parenthesis that closes it, not counting the ones inside.
                    let (from, mut depth) = (c.i, 0);
                    while let Some(f) = c.peek() {
                        match f {
                            TokenKind::Sym("(") => depth += 1,
                            TokenKind::Sym(")") if depth == 0 => break,
                            TokenKind::Sym(")") => depth -= 1,
                            TokenKind::Sym(",") if depth == 0 => break,
                            _ => {}
                        }
                        c.i += 1;
                    }
                    if c.i == from {
                        return c.error("the default value is missing here");
                    }
                    Some(c.tokens[from..c.i].to_vec())
                } else {
                    // After one with a default value, all of them: otherwise there would be no knowing which one was skipped.
                    if params.last().is_some_and(|p| p.fallback.is_some()) {
                        c.i -= 1;
                        return c.error(format!("'{name}' comes after a parameter with a default, so it needs one too"));
                    }
                    None
                };
                params.push(Param { name, kind, fallback });
                if c.sym(")") {
                    break;
                }
                c.expect_sym(",")?;
            }
        }
        c.expect_end()?;
        if let Some(existing) = self.components.get(&name) {
            // With `screens: each` the drawing is read once per monitor, and a
            // component written inside it comes through here again: it is the
            // same one, on the same line, and not two. Two real ones are on different lines.
            if existing.node.line == n.line && existing.node.col == n.col {
                return Ok(());
            }
            return Err(CompileError::at(n.line, n.col, format!("there is already a component '{name}', at {}. Two with the same name cannot live together: change one", super::location(self.files, existing.node.line))));
        }
        if n.body.is_none() {
            return Err(CompileError::at(n.line, n.col, "this component is missing its `{ … }` block"));
        }
        // Which slots it has: it needs to be known when using it, to share out what the copy brings.
        fn slots_of(entries: &[Entry], out: &mut Vec<String>) {
            for e in entries {
                let Entry::Node(x) = e else { continue };
                let word_at = |k: usize| match x.head.get(k).map(|f| &f.kind) { Some(TokenKind::Id(p)) => Some(p.clone()), _ => None };
                match word_at(0).as_deref() {
                    Some("children") => out.push(word_at(1).unwrap_or_default()),
                    Some("component") => {}
                    _ => slots_of(x.body.as_deref().unwrap_or(&[]), out),
                }
            }
        }
        let mut slots = Vec::new();
        slots_of(n.body.as_deref().unwrap_or(&[]), &mut slots);
        if let Some(word) = slots.iter().find(|h| vocab::STATEMENTS.contains(&h.as_str())) {
            return Err(CompileError::at(n.line, n.col, format!("a slot cannot be called '{word}', which is a word of the language: in the copy, `{word} {{ … }}` already means something else")));
        }
        if let Some(repeated) = slots.iter().enumerate().find(|(k, h)| slots[..*k].contains(h)).map(|(_, h)| h.clone()) {
            let which = if repeated.is_empty() { "a single unnamed `children`".to_owned() } else { format!("a single slot called '{repeated}'") };
            return Err(CompileError::at(n.line, n.col, format!("a component has {which}: give the other one a name (`children footer`)")));
        }
        self.components.insert(name, Component { strict, slots, params, node: n });
        Ok(())
    }

    /// Reads an argument and leaves it in the copy's scope. With a type, it is read as
    /// what it is and an error says what was expected; without one, it is guessed from its shape.
    fn argument(&self, component: &str, p: &Param, c: &mut Cur, env: &mut Scope) -> R<()> {
        let name = &p.name;
        let expected = |c: &Cur, what: &str| c.error::<()>(format!("'{name}', of '{component}', is {what}, and this is not")).unwrap_err();
        match p.kind.as_deref() {
            Some("number" | "bool") => {
                env.exprs.insert(name.clone(), self.expr(c)?);
            }
            Some("spring") => {
                env.springs.insert(name.clone(), self.spring(c)?);
            }
            Some("gesture") => {
                let Some(TokenKind::Id(x)) = c.peek() else { return Err(expected(c, "a gesture")) };
                if !self.gestures.contains_key(x) {
                    c.i += 1;
                    return self.unknown(c, "no gesture", x, self.gestures.keys().collect());
                }
                env.alias.insert(name.clone(), x.clone());
                c.i += 1;
            }
            Some("color") => {
                env.colors.insert(name.clone(), self.color(c).map_err(|_| expected(c, "a colour"))?);
            }
            Some("text") => match c.peek() {
                Some(TokenKind::Str(t)) => {
                    if let template @ Content::Template(_) = self.content_of(t, &c.tokens[c.i])? {
                        env.contents.insert(name.clone(), template);
                    }
                    env.strings.insert(name.clone(), t.clone());
                    c.i += 1;
                }
                Some(TokenKind::Id(x)) if self.texts.contains_key(&self.global(x)) => {
                    env.alias.insert(name.clone(), self.global(x));
                    c.i += 1;
                }
                // `Tile("Language", pick(mode, "System", "English"), …)`: a text chosen by place.
                Some(TokenKind::Id(x)) if (x == "pick" || x == "if") && matches!(c.tokens.get(c.i + 1).map(|t| &t.kind), Some(TokenKind::Sym("("))) => {
                    let k = self.text_pick(c)?;
                    env.contents.insert(name.clone(), k);
                }
                Some(TokenKind::Id(x)) if self.scopes.iter().any(|e| e.strings.contains_key(x)) => {
                    let from_outside = self.scopes.iter().rev().find(|e| e.strings.contains_key(x)).unwrap();
                    env.strings.insert(name.clone(), from_outside.strings[x].clone());
                    if let Some(k) = from_outside.contents.get(x) {
                        env.contents.insert(name.clone(), k.clone());
                    }
                    c.i += 1;
                }
                _ => return Err(expected(c, "a text: quoted, the name of a live text, or pick(…)")),
            },
            Some("record") => {
                let Some(TokenKind::Id(x)) = c.peek() else { return Err(expected(c, "a record of a model")) };
                let Some((row, index)) = self.row(x) else { return Err(expected(c, "a record of a model (the one from a `for`, or `rows.0`)")) };
                env.alias.insert(name.clone(), row);
                env.with_parts.insert(name.clone());
                env.exprs.insert(format!("{name}.index"), Expr::K(index as f32));
                c.i += 1;
            }
            Some("event") => {
                // Inside, `emit chosen` and `on chosen` talk about the signal that was passed.
                let Some(TokenKind::Id(x)) = c.peek() else { return Err(expected(c, "an event")) };
                let g = self.global(x);
                if !self.signals.contains_key(&g) {
                    c.i += 1;
                    return self.unknown(c, "no event", &g, self.signals.keys().collect());
                }
                env.alias.insert(name.clone(), g);
                c.i += 1;
            }
            Some("image") => {
                let Some(TokenKind::Id(x)) = c.peek() else { return Err(expected(c, "an image")) };
                let g = self.global(x);
                if !self.images.contains_key(&g) {
                    c.i += 1;
                    return self.unknown(c, "no image", &g, self.images.keys().collect());
                }
                env.alias.insert(name.clone(), g);
                c.i += 1;
            }
            Some(other) => unreachable!("'{other}' is in the vocabulary, but `argument` cannot read it"),
            None => self.untyped_argument(name, c, env)?,
        }
        Ok(())
    }

    /// No declared type: whatever it looks like. It is how components were written before having them.
    fn untyped_argument(&self, p: &String, c: &mut Cur, env: &mut Scope) -> R<()> {
        let next = c.tokens.get(c.i + 1).map(|x| &x.kind);
        let alone = matches!(next, Some(TokenKind::Sym(",")) | Some(TokenKind::Sym(")")) | None);
        match c.peek() {
            Some(TokenKind::Str(t)) => {
                // The holes talk about the names from here, not those from inside the component.
                if let template @ Content::Template(_) = self.content_of(t, &c.tokens[c.i])? {
                    env.contents.insert(p.clone(), template);
                }
                env.strings.insert(p.clone(), t.clone());
                c.i += 1;
            }
            Some(TokenKind::Color(_)) => {
                env.colors.insert(p.clone(), self.color(c)?);
            }
            Some(TokenKind::Id(x)) if x == "mix" && matches!(c.tokens.get(c.i + 2).map(|y| &y.kind), Some(TokenKind::Color(_))) => {
                env.colors.insert(p.clone(), self.color(c)?);
            }
            Some(TokenKind::Id(x)) if x == "rgb" && matches!(c.tokens.get(c.i + 1).map(|y| &y.kind), Some(TokenKind::Sym("("))) => {
                env.colors.insert(p.clone(), self.color(c)?);
            }
            Some(TokenKind::Id(x)) if alone && (self.colors.contains_key(x) || self.scopes.iter().any(|e| e.colors.contains_key(x))) => {
                env.colors.insert(p.clone(), self.color(c)?);
            }
            // A record of a model —the one from a `for`, or `rows.3`—: inside, `p.label` is its field.
            Some(TokenKind::Id(x)) if alone && self.row(x).is_some() => {
                let (row, index) = self.row(x).unwrap();
                env.alias.insert(p.clone(), row);
                env.with_parts.insert(p.clone());
                env.exprs.insert(format!("{p}.index"), Expr::K(index as f32));
                c.i += 1;
            }
            // The name of a text, an image or a gesture: the parameter is another name for it.
            Some(TokenKind::Id(x)) if alone && { let g = self.global(x); self.texts.contains_key(&g) || self.images.contains_key(&g) || self.gestures.contains_key(&g) || self.signals.contains_key(&g) } => {
                env.alias.insert(p.clone(), self.global(x));
                c.i += 1;
            }
            Some(TokenKind::Id(x)) if alone && self.scopes.iter().any(|e| e.strings.contains_key(x)) => {
                env.strings.insert(p.clone(), self.scopes.iter().rev().find_map(|e| e.strings.get(x)).unwrap().clone());
                c.i += 1;
            }
            _ => {
                env.exprs.insert(p.clone(), self.expr(c)?);
            }
        }
        Ok(())
    }

    /// The rules noted while a scope was being read keep that
    /// whole scope: that way they can name a shape that was declared later.
    fn close_scope(&mut self, from: usize) {
        let now = self.scopes.clone();
        // The rules of THIS scope: the same chain of marks, not just the same
        // depth. A component given as another's child is read with the scope of
        // whoever wrote it, so it is as deep as the component it goes into; by
        // depth alone, closing `Card` took the rules of the `Switch` inside it
        // as its own, and `on press touch` looked for `touch` in `Card`.
        let marks = |v: &[Rc<Scope>]| v.iter().map(|e| e.suffix.clone()).collect::<Vec<_>>();
        let ours = marks(&now);
        for r in &mut self.rules[from..] {
            if r.1.len() == now.len() && marks(&r.1) == ours {
                r.1 = now.clone();
            }
        }
        self.scopes.pop();
    }

    /// `Chip("Hello", mint) { move: 10, 20 }`: a copy, with its parameters and its
    /// own names inside. From outside it is a group.
    fn instantiate(&mut self, n: &'a Node, c: &mut Cur) -> R<()> {
        let name = c.tokens[0].kind.clone();
        let TokenKind::Id(name) = name else { unreachable!() };
        let (params, body) = {
            let k = &self.components[&name];
            (k.params.clone(), k.node.body.as_deref().unwrap_or(&[]))
        };
        let mut env = Scope::default();
        let mut given_flags = vec![false; params.len()];
        let signature = || params.iter().map(|p| match (&p.kind, &p.fallback) {
            (Some(t), None) => format!("{}: {t}", p.name),
            (Some(t), Some(_)) => format!("{}: {t} = …", p.name),
            (None, None) => p.name.clone(),
            (None, Some(_)) => format!("{} = …", p.name),
        }).collect::<Vec<_>>().join(", ");
        if c.sym("(") && !c.sym(")") {
            let mut by_name = false;
            let mut k = 0;
            loop {
                // `tone: mint`: from the first named one on, all of them named.
                let named = matches!((c.peek(), c.tokens.get(c.i + 1).map(|x| &x.kind)), (Some(TokenKind::Id(_)), Some(TokenKind::Sym(":"))));
                let which = if named {
                    by_name = true;
                    let n = c.id("a parameter name")?;
                    c.expect_sym(":")?;
                    match params.iter().position(|p| p.name == n) {
                        Some(k) => k,
                        None => {
                            c.i -= 2;
                            let all: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                            let hint = closest_match(&n, all.iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
                            return c.error(format!("'{name}' has no parameter called '{n}'.{hint} It is {name}({})", signature()));
                        }
                    }
                } else if by_name {
                    return c.error("after one named argument, they all carry their name");
                } else {
                    k += 1;
                    k - 1
                };
                if which >= params.len() {
                    return c.error(format!("'{name}' has too many arguments: it is {name}({})", signature()));
                }
                if std::mem::replace(&mut given_flags[which], true) {
                    return c.error(format!("'{}' has already been given", params[which].name));
                }
                self.argument(&name, &params[which], c, &mut env)?;
                if c.sym(")") {
                    break;
                }
                c.expect_sym(",")?;
            }
        }
        // What has not been given: its default value, read here, or an error that says what is missing.
        for (p, given) in params.iter().zip(&given_flags) {
            if *given {
                continue;
            }
            let Some(tokens) = &p.fallback else {
                let what = p.kind.as_ref().map_or(String::new(), |t| format!(" ({})", type_name(t)));
                return Err(CompileError::at(n.line, n.col, format!("'{name}' is missing '{}'{what}: it is {name}({})", p.name, signature())));
            };
            let mut d = Cur::new(tokens, n.line, n.col);
            self.argument(&name, p, &mut d, &mut env)?;
            d.expect_end()?;
        }
        c.expect_end()?;
        self.copies += 1;
        env.suffix = format!("#{name}{}", self.copies);
        env.instance = Some((name.clone(), n.line));
        env.strict = self.components[&name].strict;
        env.library = Some(self.components[&name].node.line / super::PER_FILE).filter(|k| *k > 0);
        let in_slot = std::mem::take(&mut self.in_slot);
        let from = self.rules.len();
        // What the copy brings inside its block goes where the component says `children`,
        // and it is read with the names from out here.
        let slot_names = self.components[&name].slots.clone();
        let mut slots: Vec<(String, Vec<&'a Entry>, bool)> = slot_names.iter().map(|h| (h.clone(), Vec::new(), false)).collect();
        let mut unplaced = None;
        for e in n.body.as_deref().unwrap_or(&[]) {
            let Entry::Node(x) = e else { continue };
            // `header { … }`: a block with the name of a slot is what goes in that slot.
            let block = match (x.head.as_slice(), &x.body) {
                ([Token { kind: TokenKind::Id(p), .. }], Some(_)) if slot_names.contains(p) || (!vocab::STATEMENTS.contains(&p.as_str()) && !self.components.contains_key(p)) => Some(p.clone()),
                _ => None,
            };
            match block {
                Some(p) => match slots.iter_mut().find(|h| h.0 == p) {
                    Some(h) => h.1.extend(x.body.as_deref().unwrap_or(&[]).iter()),
                    None => {
                        let named: Vec<String> = slot_names.iter().filter(|h| !h.is_empty()).cloned().collect();
                        let hint = closest_match(&p, named.iter()).map_or(String::new(), |q| format!(" Did you mean '{q}'?"));
                        let has_text = if named.is_empty() { "has no named slots".to_owned() } else { format!("has {}", named.join(", ")) };
                        return Err(CompileError::at(x.line, x.col, format!("'{name}' has no slot called '{p}': it {has_text}.{hint}")));
                    }
                },
                None => match slots.iter_mut().find(|h| h.0.is_empty()) {
                    Some(h) => h.1.push(e),
                    None => unplaced = unplaced.or(Some((x.line, x.col))),
                },
            }
        }
        if let Some((l, col)) = unplaced {
            return Err(CompileError::at(l, col, format!("'{name}' has nowhere to put what goes inside it: its component is missing a `children`")));
        }
        self.instance_children.push(InstanceChildren { outer: self.scopes.clone(), slots });
        self.scopes.push(Rc::new(env));
        self.declare_measures_early(body);
        // How much room it takes is said by the component itself: `size: 300, 44`. It is read after
        // the body, still inside its scope, so that it sees the body's own `let`s. If it fails, the
        // scope is closed all the same: otherwise, what comes after would be read as if it were in here.
        let mut r = self.group_with_properties(n, body);
        let mut size = None;
        for e in body {
            if let Entry::Prop { name, value, line, col } = e {
                if name == "size" && r.is_ok() {
                    let mut c = Cur::new(value, *line, *col);
                    match self.point(&mut c) {
                        Ok(t) => size = Some(t),
                        Err(f) => r = Err(f),
                    }
                }
            }
        }
        self.close_scope(from);
        self.instance_children.pop();
        let _ = in_slot;
        if size.is_some() {
            self.last_size = size;
        }
        r
    }

    /// `repeat i in 0..6 { … }`: the block, once for each value. It is unrolled
    /// when loading; there are no running loops.
    /// One that defines elements, at the scene's level, is read twice: in
    /// pass 1 only its `let`s and definitions, so that what comes after it can
    /// read them; in pass 2 the rest, with the definitions already made.
    fn repeat(&mut self, n: &'a Node, c: &mut Cur) -> R<()> {
        let mut head = self.repeat_head(c);
        // In pass 0, one at the scene's level declares its values with the
        // other declarations: anything can read them, wherever it is written.
        if self.pass == 0 {
            // Its bounds may be `let`s, which are only read in pass 1: the
            // scene's `let`s of numbers are read for them here, and let go.
            if head.is_err() {
                let added = self.constant_lets();
                head = self.repeat_head(&mut Cur::new(&n.head[1..], n.line, n.col));
                for name in added {
                    self.lets.remove(&name);
                }
            }
            let Ok((var, from, until)) = head else { return Ok(()) };
            for v in from..until {
                self.open_iteration(&var, v);
                for e in n.body.as_deref().unwrap_or(&[]) {
                    if matches!(e, Entry::Node(s) if declares_value(s)) {
                        let Entry::Node(s) = e else { unreachable!() };
                        let _ = self.statement(s, &mut 0);
                    }
                }
                self.scopes.pop();
            }
            return Ok(());
        }
        let (var, from, until) = head?;
        if self.hoisting.is_some() {
            self.hoist_turns(n, &var, from, until, false);
            return Ok(());
        }
        let key = n as *const Node;
        if self.pass == 1 && self.scopes.is_empty() {
            self.hoisted.insert(key);
            // Each copy's folds are settled once all have been read: the
            // scene's last link is not where a copy's chain starts.
            let mut settled = Vec::new();
            self.each_copy(|o| {
                // What each of the scene's `let`s it folds is worth as it begins.
                let mut start = Scope { folds: true, ..Default::default() };
                for name in fold_targets(n.body.as_deref().unwrap_or(&[])) {
                    let own = o.hoisting.flatten().map(|s| format!("{name}#screen{s}")).filter(|m| o.lets.contains_key(m));
                    if let Some(e) = own.as_ref().or(Some(&name)).and_then(|m| o.lets.get(m)) {
                        start.exprs.insert(name, e.clone());
                    }
                }
                o.scopes.push(Rc::new(start));
                o.fold_scope = Some(o.scopes.len() - 1);
                o.hoist_turns(n, &var, from, until, true);
                o.fold_scope = None;
                settled.push((o.hoisting.flatten(), o.scopes.pop().unwrap()));
            });
            for (copy, folds) in settled {
                self.settle_folds(copy, &folds);
            }
            return Ok(());
        }
        let hoisted = self.hoisted.contains(&key) as usize;
        self.in_hoisted += hoisted;
        let copy = self.screen_mark().and_then(|m| m["#screen".len()..].parse::<usize>().ok());
        for v in from..until {
            let mark = self.rules.len();
            // A turn reads each fold as it was at the end of that turn.
            let folds = self.turns.get(&(key, copy, v)).cloned();
            let had_folds = folds.is_some();
            if let Some(f) = folds {
                self.scopes.push(f);
            }
            self.open_iteration(&var, v);
            self.group(n.body.as_deref().unwrap_or(&[]).iter());
            self.close_scope(mark);
            if had_folds {
                self.scopes.pop();
            }
        }
        self.in_hoisted -= hoisted;
        Ok(())
    }

    /// The scene's `let`s that are a number, or worked out from numbers and
    /// the ones before them, put in for now: what it put, to take out again.
    fn constant_lets(&mut self) -> Vec<String> {
        let mut added = Vec::new();
        for e in self.top {
            let Entry::Node(n) = e else { continue };
            let (Some(TokenKind::Id(w)), Some(TokenKind::Id(name)), Some(TokenKind::Sym("="))) = (n.head.first().map(|t| &t.kind), n.head.get(1).map(|t| &t.kind), n.head.get(2).map(|t| &t.kind)) else { continue };
            if w != "let" || self.lets.contains_key(name) {
                continue;
            }
            let before = self.unwatched.replace(true);
            let mut c = Cur::new(&n.head[3..], n.line, n.col);
            let value = self.expr(&mut c).ok().filter(|_| c.at_end());
            self.unwatched.set(before);
            if let Some(Expr::K(v)) = value {
                self.lets.insert(name.clone(), Expr::K(v));
                added.push(name.clone());
            }
        }
        added
    }

    /// What defines elements is read for the scene, and again for each
    /// monitor's copy: one that reads `screen.*` is each copy's own.
    fn each_copy(&mut self, mut read: impl FnMut(&mut Self)) {
        let copies: Vec<usize> = self.e.surfaces.iter().filter(|s| s.name.is_empty() && matches!(s.screens, Screens::Number(_))).map(|s| s.instance).collect();
        for copy in std::iter::once(None).chain(copies.into_iter().map(Some)) {
            if let Some(k) = copy {
                self.scopes.push(Rc::new(self.screen_scope(k)));
            }
            self.hoisting = Some(copy);
            read(self);
            self.hoisting = None;
            if copy.is_some() {
                self.scopes.pop();
            }
        }
    }

    fn hoist_turns(&mut self, n: &'a Node, var: &str, from: i64, until: i64, top: bool) {
        for v in from..until {
            // Each turn's drawing and rules start from the folds as they were
            // when it began, and follow its links as they come.
            if let Some(k) = self.fold_scope.filter(|_| top) {
                self.turns.insert((n as *const Node, self.hoisting.flatten(), v), self.scopes[k].clone());
            }
            self.open_iteration(var, v);
            for e in n.body.as_deref().unwrap_or(&[]) {
                let Entry::Node(s) = e else { continue };
                let r = if is_definition(s) { Some(self.element(s)) } else if is_fold(s) { Some(self.fold(s)) } else { None };
                if let Some(Err(f)) = r {
                    self.push_error(f);
                } else if r.is_some() {
                } else if hoisted_with_definitions(s) {
                    // One that reads what only the drawing declares fails here,
                    // and is read again in pass 2: a definition that needs it says so.
                    // The declarations are read again too, and are the same ones.
                    let _ = self.statement(s, &mut 0);
                }
            }
            self.scopes.pop();
        }
    }

    /// `x = ...`, `x += ...` in a `repeat` at the scene's level: each turn is
    /// one more link of a chain, and after the `repeat` `x` is its last link.
    /// Nothing is ever changed: each link is a `let` of its own.
    fn fold(&mut self, n: &Node) -> R<()> {
        let mut c = Cur::new(&n.head, n.line, n.col);
        let name = c.id("a name")?;
        let op = ["+", "-", "*"].into_iter().find(|o| c.sym(o));
        c.expect_sym("=")?;
        // A `let` of the turn or the block it is in (not the scene's): each
        // link takes its place there, and what comes after reads that one.
        let local = self.scopes.iter().rposition(|e| e.exprs.contains_key(&name)).filter(|k| !self.scopes[*k].folds);
        let k = match local {
            Some(k) => k,
            None => {
                if self.in_hoisted > 0 {
                    match self.scopes.iter().rposition(|e| e.folds && e.exprs.contains_key(&name)) {
                        Some(k) => k,
                        None => return Ok(()),
                    }
                } else {
                let Some(k) = self.fold_scope.filter(|_| self.hoisting.is_some()) else {
                    return Err(CompileError::at(n.line, n.col, format!("`{name} = ...` gives a `let` a new value: one of the turn or the block it is in, or one of the scene's in a `repeat` at the scene's level. There is no `let {name}` before it here")));
                };
                let g = self.global_quiet(&name);
                if !self.lets.contains_key(&g) || self.arrays.contains_key(&g) {
                    return Err(CompileError::at(n.line, n.col, format!("'{name}' is not a `let` of the scene: declare it before the `repeat`, `let {name} = 0`")));
                }
                k
                }
            }
        };
        let e = self.expr(&mut c)?;
        c.expect_end()?;
        let e = match op {
            Some(o) => {
                let now = self.resolve(&c, &name)?;
                match o {
                    "+" => now + e,
                    "-" => now - e,
                    _ => now * e,
                }
            }
            None => e,
        };
        let link = if e.node_count() > 8 { self.computed_once(&name, e).e() } else { e };
        Rc::make_mut(&mut self.scopes[k]).exprs.insert(name, link);
        Ok(())
    }

    /// After the `repeat`, each fold is its last link: the scene's, and a
    /// copy's own where its chain is another.
    fn settle_folds(&mut self, copy: Option<usize>, folds: &Scope) {
        for (name, e) in &folds.exprs {
            let raw = format!("{e:?}");
            match copy {
                None => {
                    self.folds.insert(name.clone(), raw);
                    self.lets.insert(name.clone(), e.clone());
                }
                Some(s) if self.folds.get(name).is_some_and(|base| *base != raw) => {
                    if let Some(Expr::P(p)) = self.lets.get(name) {
                        self.copy_props.insert(p.0);
                    }
                    self.lets.insert(format!("{name}#screen{s}"), e.clone());
                }
                // The same as the scene's: a value of its own left from before the
                // `repeat` (a start that read its monitor) would hide it.
                Some(s) => {
                    self.lets.remove(&format!("{name}#screen{s}"));
                }
            }
        }
    }

    /// `let a[16]`: an array, whose elements are each defined once (`a[i] = ...`).
    fn array(&mut self, n: &Node, name: &str, c: &mut Cur) -> R<()> {
        let at = c.pos();
        let length = match self.expr(c)? {
            Expr::K(v) if v >= 1.0 && v <= 4096.0 => v as usize,
            _ => return Err(CompileError::at(at.0, at.1, "an array's length is read when loading: a number from 1 to 4096, a `let` of one, or `win.max`")),
        };
        c.expect_sym("]")?;
        // `let dka[4][12]`: each of its elements is an array of its own.
        let columns = if c.sym("[") {
            let at = c.pos();
            let m = match self.expr(c)? {
                Expr::K(v) if v >= 1.0 && v <= 4096.0 => v as usize,
                _ => return Err(CompileError::at(at.0, at.1, "an array's length is read when loading: a number from 1 to 4096, a `let` of one, or `win.max`")),
            };
            c.expect_sym("]")?;
            Some(m)
        } else {
            None
        };
        let sorted = columns.is_none() && c.sym("=");
        if sorted {
            c.expect_word("sort")?;
            c.expect_sym("(")?;
        } else {
            c.expect_end()?;
        }
        if !self.scopes.is_empty() || self.pass != 1 {
            return Err(CompileError::at(n.line, n.col, "an array is declared at the scene's level: not inside a `repeat`, a component or a surface"));
        }
        let g = self.declare(name);
        if let Some(&(_, line)) = self.arrays.get(&g) {
            if line != n.line {
                return Err(CompileError::at(n.line, n.col, format!("there is already an array '{name}', on line {}", line % super::PER_FILE)));
            }
            return Ok(());
        }
        self.arrays.insert(g.clone(), (length, n.line));
        if let Some(m) = columns {
            self.rows.insert(g.clone());
            for k in 0..length {
                self.arrays.insert(format!("{g}.{k}"), (m, n.line));
            }
        }
        if sorted {
            return self.sort(n, &g, length, c);
        }
        Ok(())
    }

    /// `let ord[win.max] = sort(i in win by win[i].row, win[i].col)`: the
    /// slots in order of their keys, compared one after another, ties by
    /// slot. `ord[p]` is the slot at place `p`, and `ord.pos[i]` the place of
    /// slot `i`. Worked out while the scene runs, once for every reader.
    fn sort(&mut self, n: &Node, g: &str, length: usize, c: &mut Cur) -> R<()> {
        let var = c.id("a name for the slot")?;
        c.expect_word("in")?;
        let at = c.pos();
        let range = matches!(c.tokens.get(c.i + 1).map(|t| &t.kind), Some(TokenKind::Sym("..")));
        let count = match c.peek() {
            Some(TokenKind::Id(_)) if !range => {
                let what = c.id("what to sort")?;
                let w = self.global_quiet(&what);
                let max = match self.lets.get(&format!("{w}.max")) {
                    Some(Expr::K(v)) => Some(*v as usize),
                    _ => None,
                };
                self.arrays.get(&w).map(|a| a.0).or(max).ok_or_else(|| CompileError::at(at.0, at.1, format!("'{what}' has no slots to sort: `win`, an array, or `0..16`")))?
            }
            _ => {
                let from = self.expr(c)?;
                c.expect_sym("..")?;
                match (from, self.expr(c)?) {
                    (Expr::K(a), Expr::K(b)) if a == 0.0 && b >= 1.0 => b as usize,
                    _ => return Err(CompileError::at(at.0, at.1, "the slots a `sort` orders are known when loading, from 0: `0..16`")),
                }
            }
        };
        if count != length {
            return Err(CompileError::at(at.0, at.1, format!("this sorts {count} slots, and '{g}' has {length} places: they are the same number")));
        }
        c.expect_word("by")?;
        if !matches!(c.tokens.last().map(|t| &t.kind), Some(TokenKind::Sym(")"))) || c.i + 1 >= c.tokens.len() {
            return c.error("a `sort` orders by at least one key, and ends its parenthesis: `sort(i in win by win[i].place)`");
        }
        let written = &c.tokens[c.i..c.tokens.len() - 1];
        let mut keys = Vec::with_capacity(length);
        for i in 0..length {
            self.open_iteration(&var, i as i64);
            let mut k = Cur::new(written, n.line, n.col);
            let mut these = Vec::new();
            let read = loop {
                match self.expr(&mut k) {
                    Ok(e) => these.push(e),
                    Err(f) => break Err(f),
                }
                if !k.sym(",") {
                    break k.expect_end();
                }
            };
            self.scopes.pop();
            read?;
            keys.push(these);
        }
        c.i = c.tokens.len();
        let made = |o: &mut Self, name: String| {
            let p = o.e.prop_with(interned(&format!("\u{b7}{name}")), 0.0, Spring::LIVELY);
            o.lets.insert(name.clone(), p.e());
            o.elements.insert(name, String::new());
            p
        };
        let ord: Vec<PropId> = (0..length).map(|p| made(self, format!("{g}.{p}"))).collect();
        let pos: Vec<PropId> = (0..length).map(|i| made(self, format!("{g}.pos.{i}"))).collect();
        self.arrays.insert(format!("{g}.pos"), (length, n.line));
        self.e.behaviors.push(Behavior::Sort { keys, ord, pos });
        Ok(())
    }

    /// `a[i] = ...`: one element of an array. Read in pass 1, with the `let`s,
    /// as a `let` of the scene called `a.3`: anything after it can read it.
    fn element(&mut self, n: &'a Node) -> R<()> {
        if self.in_hoisted > 0 {
            return Ok(());
        }
        if self.hoisting.is_none() {
            if self.pass != 1 || !self.scopes.is_empty() {
                return Err(CompileError::at(n.line, n.col, "an element is defined at the scene's level, or in a `repeat` there: not inside a group, a component or a surface"));
            }
            let mut result = Ok(());
            self.each_copy(|o| {
                if let Err(f) = o.element(n) {
                    result = Err(f);
                }
            });
            return result;
        }
        let copy = self.hoisting.flatten();
        let mut c = Cur::new(&n.head, n.line, n.col);
        let name = c.id("an array")?;
        let mut g = self.global_quiet(&name);
        let mut places = Vec::new();
        if !self.arrays.contains_key(&g) {
            return Err(CompileError::at(n.line, n.col, format!("there is no array called '{name}': declare it first, `let {name}[N]`")));
        }
        while c.sym("[") {
            let at = c.pos();
            let k = match self.expr(&mut c)? {
                Expr::K(v) => v.round() as i64,
                _ => return Err(CompileError::at(at.0, at.1, "which element is defined is known when loading: a number, or a `repeat`'s counter")),
            };
            c.expect_sym("]")?;
            let Some(&(length, _)) = self.arrays.get(&g) else {
                return Err(CompileError::at(at.0, at.1, format!("'{name}' has {} places: this is one more", places.len())));
            };
            if k < 0 || k >= length as i64 {
                return Err(CompileError::at(at.0, at.1, format!("'{name}' has {length} elements there, from 0 to {}: {k} is not one of them", length - 1)));
            }
            places.push(k.to_string());
            g = format!("{g}.{k}");
        }
        if self.rows.contains(&g) || self.arrays.contains_key(&g) {
            return Err(CompileError::at(n.line, n.col, format!("'{name}' has two places: `{name}[i][j] = ...`")));
        }
        c.expect_sym("=")?;
        let k = places.join("][");
        let e = self.expr(&mut c)?;
        c.expect_end()?;
        let element = g;
        let raw = format!("{e:?}");
        match copy {
            None => {
                if self.elements.insert(element.clone(), raw).is_some() {
                    return Err(CompileError::at(n.line, n.col, format!("'{name}[{k}]' is already defined: each element is defined once")));
                }
                let e = if e.node_count() > 8 { self.computed_once(&element, e).e() } else { e };
                self.lets.insert(element, e);
            }
            // Only where this copy computes something else than the scene's.
            Some(s) if self.elements.get(&element).is_some_and(|base| *base != raw) => {
                if let Some(Expr::P(p)) = self.lets.get(&element) {
                    self.copy_props.insert(p.0);
                }
                let own = if e.node_count() > 8 { self.computed_once(&element, e).e() } else { e };
                self.lets.insert(format!("{element}#screen{s}"), own);
            }
            Some(_) => {}
        }
        Ok(())
    }

    fn repeat_head(&self, c: &mut Cur) -> R<(String, i64, i64)> {
        let var = c.id("a name for the counter")?;
        c.expect_word("in")?;
        let constant = |o: &Self, c: &mut Cur| -> R<i64> {
            match o.expr(c)? {
                Expr::K(v) => Ok(v as i64),
                _ => c.error("the bounds of a `repeat` have to be numbers: it unfolds when loading"),
            }
        };
        let from = constant(self, c)?;
        c.expect_sym("..")?;
        let until = constant(self, c)?;
        c.expect_end()?;
        if until - from > 512 {
            return c.error("more than 512 turns in a `repeat` means something is wrong");
        }
        Ok((var, from, until))
    }

    fn open_iteration(&mut self, var: &str, v: i64) {
        let mut env = Scope { suffix: format!("#{var}{v}"), ..Default::default() };
        env.exprs.insert(var.to_owned(), Expr::K(v as f32));
        self.scopes.push(Rc::new(env));
    }

    /// `row bar ~calm { at: x, y; gap: 8; padding: 6; align: center; fill: #222; corner: 12; …children… }`
    ///
    /// Lays out its children in a row or in a column. There is no layout engine: the
    /// place of each child is an expression —what the previous ones take up—, so
    /// if one grows the others shift, and with a spring they shift animated.
    fn stack(&mut self, n: &'a Node, c: &mut Cur, is_row: bool) -> R<()> {
        let name = match c.peek() {
            Some(TokenKind::Id(_)) => Some(c.id("a name")?),
            _ => None,
        };
        let spring = if c.sym("~") { Some(self.spring(c)?) } else { None };
        c.expect_end()?;
        let mut p = self.properties(n, vocab::properties("layout"))?;
        let (stack_cursor, stack_cursor_when) = match p.get_mut("cursor") {
            Some(c) => self.cursor(c)?,
            None => (Cursor::Normal, None),
        };
        self.in_slot = false;
        let origin = match p.get_mut("at") {
            Some(c) => self.point(c)?,
            None => (0.0.into(), 0.0.into()),
        };
        let mut one = |o: &Self, k: &str, default_value: f32| -> R<Expr> {
            match p.get_mut(k) {
                Some(c) => o.expr(c),
                None => Ok(Expr::K(default_value)),
            }
        };
        let (gap, padding, corner) = (one(self, "gap", 0.0)?, one(self, "padding", 0.0)?, one(self, "corner", 0.0)?);
        let zone_corner = corner.clone();
        let align_factor = match p.get_mut("align") {
            Some(c) => match c.one_of(vocab::STACK_ALIGNS, "the alignment of a layout")?.as_str() {
                "start" => 0.0,
                "center" => 0.5,
                "end" => 1.0,
                _ => unreachable!(),
            },
            None => 0.0,
        };
        // `fill: ink 9%`: the colour, and if it is given, how much of it shows. Over glass an
        // opaque background covers what the glass shows; a translucent one lets it through.
        let (fill, fill_alpha) = match p.get_mut("fill") {
            Some(c) => {
                let col = self.color(c)?;
                let alpha = if c.at_end() { Expr::K(1.0) } else { self.expr(c)?.clamp(0.0, 1.0) };
                c.expect_end()?;
                (Some(col), alpha)
            }
            None => (None, Expr::K(1.0)),
        };
        // And its background can be glass, like a loose shape.
        let fill_glass = self.glass_spec(&mut p, n)?;
        // `size: 164, 66`: what the stack measures. It is needed so that a child
        // can ask for "whatever is left over": without saying how much is being shared, there is no remainder.
        let given_size = match p.get_mut("size") {
            Some(c) => Some(self.point(c)?),
            None => None,
        };
        // `view: 300, 200`: what is seen. What is inside can be longer, and it scrolls.
        let view = match p.get_mut("view") {
            Some(c) => Some(self.point(c)?),
            None => None,
        };
        let step = match p.get_mut("step") {
            Some(c) => self.expr(c)?,
            None => Expr::K(60.0),
        };
        // `stick: end`: it starts at its end and stays there while what is inside
        // grows —a chat, a log—, unless it was moved away from it; back at the end, it sticks again.
        let stick_end = match p.get_mut("stick") {
            Some(c) => {
                let w = c.id("end")?;
                c.expect_end()?;
                if w != "end" {
                    return Err(CompileError::at(n.line, n.col, "`stick` says where a list with `view:` stays: `stick: end`"));
                }
                if view.is_none() {
                    return Err(CompileError::at(n.line, n.col, "`stick: end` is for a layout with `view:`: one that scrolls"));
                }
                true
            }
            None => false,
        };
        // `wrap: 5`: five per line and on to the next. A grid, with the cell the
        // size of the biggest child; what is not there leaves no gap.
        let wrap = match p.get_mut("wrap") {
            Some(c) => {
                let how_many = c.num()? as usize;
                c.expect_end()?;
                if !(1..=64).contains(&how_many) {
                    return Err(CompileError::at(n.line, n.col, "`wrap` goes from 1 to 64: how many fit in a line before jumping to the next"));
                }
                Some(how_many)
            }
            None => None,
        };
        // `content: rows.total * 30`: what there would be if it were all there. For a list
        // that unfolds no more than its window, it is the real length.
        let given_content = match p.get_mut("content") {
            Some(c) => Some(self.expr(c)?),
            None => None,
        };
        let opacity = match p.get_mut("opacity") {
            Some(c) => Some(self.expr(c)?),
            None => None,
        };
        // `show:` multiplies the opacity: what is not there is not seen.
        let hide = match p.get_mut("show") {
            Some(c) => Some(self.expr(c)?.clamp(0.0, 1.0)),
            None => None,
        };
        let opacity = match (&hide, opacity) {
            (Some(v), o) => Some(o.map_or(v.clone(), |o| o * v.clone())),
            (None, o) => o,
        };

        // Which part of the stack falls on `at`: `anchor: right` attaches it by the right
        // whatever it measures, which is what whatever goes at the end of a bar wants.
        let mut anchor = (0.0f32, 0.0f32);
        let mut x_given = false;
        if let Some(c) = p.get_mut("anchor") {
            while !c.at_end() {
                match c.id("left, center, right, top or bottom")?.as_str() {
                    "left" => (anchor.0, x_given) = (0.0, true),
                    "right" => (anchor.0, x_given) = (1.0, true),
                    "top" => anchor.1 = 0.0,
                    "bottom" => anchor.1 = 1.0,
                    // As in a text: `center` is the axis still left to say.
                    // Before, it was always the horizontal one, so `right center`
                    // —which in a text is "by the right, at half height"—
                    // here read as "right… no, centred" and said nothing: the
                    // stack fell on top of whatever it had beside it.
                    "center" if x_given => anchor.1 = 0.5,
                    "center" => (anchor.0, x_given) = (0.5, true),
                    "middle" => anchor.1 = 0.5,
                    _ => return c.error("an anchor is left, center or right, and top, middle or bottom"),
                }
            }
        }
        // The whole stack lives under a transform that takes it to its origin. If
        // it has an anchor, the origin depends on what it measures, and that is known at the end.
        let base = Transform::at((0.0.into(), 0.0.into())).translate(origin.0.clone(), origin.1.clone());
        let (instr_base, base_level, base_candidates) = (self.e.instrs.len(), self.under.len(), self.candidates.len());
        self.e.paint(Instr::Transform(Some(base.clone())));
        self.under.push(base);
        if let Some(o) = &opacity {
            self.e.paint(Instr::Opacity(Some(o.clone())));
        }
        // The background is painted before the children, but its size is known afterwards:
        // the place is left and filled in at the end.
        let background_slot = fill.as_ref().map(|_| {
            let k = self.e.instrs.len();
            self.e.paint(Instr::Clip(None));
            k
        });

        // The children, with the `repeat`s already unrolled.
        let mut children: Vec<(&'a Node, Vec<Rc<Scope>>)> = Vec::new();
        self.unroll(n.body.as_deref().unwrap_or(&[]).iter().collect(), &mut Vec::new(), &mut children)?;

        struct Placed {
            instr: usize,
            candidates: std::ops::Range<usize>,
            size: (Expr, Expr),
            visible: Expr,
            /// It is what goes between two children, not a child.
            separator: bool,
            /// `grow: 2`: how much of what is left over it asks for. 0 is the normal: its size.
            grow: f32,
            /// `place: end`: where it goes across its layout, if not where the layout's `align:` puts every child.
            place: Option<Expr>,
            /// Its instructions, to be able to tell it its width when it is known.
            own_instrs: std::ops::Range<usize>,
        }
        // What is seen is a window onto what there is: it is clipped, and what is inside goes shifted.
        let scroller = view.as_ref().map(|(vw, vh)| {
            self.copies += 1;
            let prop = self.e.prop_with(interned(&format!("·scroll{}", self.copies)), 0.0, spring.unwrap_or(Spring::QUICK));
            let bounds = Shape::Rect { center: (vw.clone() * 0.5, vh.clone() * 0.5), half_size: (vw.clone() * 0.5, vh.clone() * 0.5), radius: corner.clone() };
            self.e.paint(Instr::Clip(Some((bounds, 0.0))));
            let scroll = Expr::K(0.0) - prop.e();
            let translate = if is_row { (scroll, Expr::K(0.0)) } else { (Expr::K(0.0), scroll) };
            let t = Transform { translate, ..Transform::at((0.0.into(), 0.0.into())) };
            self.e.paint(Instr::Transform(Some(t.clone())));
            self.under.push(t);
            prop
        });
        let level = self.under.len();
        let mut placed_items: Vec<Placed> = Vec::new();
        // `between { … }`: what goes between every two children that are there.
        let mut separator: Option<(&'a Node, Option<String>)> = None;
        for e in n.body.as_deref().unwrap_or(&[]) {
            let Entry::Node(x) = e else { continue };
            if !matches!(x.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "between") {
                continue;
            }
            if separator.is_some() {
                return Err(CompileError::at(x.line, x.col, "a layout has a single `between`"));
            }
            // `between i { … }`: inside, `i` is between which ones it is: 1 after the first child, 2 after the second…
            let counter = match x.head.as_slice() {
                [_] => None,
                [_, Token { kind: TokenKind::Id(v), .. }] => Some(v.clone()),
                _ => return Err(CompileError::at(x.line, x.col, "after `between` only a name for its position can go: `between i { … }`")),
            };
            let body = x.body.as_deref().unwrap_or(&[]);
            let inside: Vec<&'a Node> = body.iter().filter_map(|e| if let Entry::Node(y) = e { Some(y) } else { None }).collect();
            let has_size = body.iter().any(|e| matches!(e, Entry::Prop { name, .. } if name == "size"));
            separator = Some(match (inside.as_slice(), has_size) {
                // A single thing, which says itself how much room it takes.
                ([one], false) => (*one, counter),
                // Several (or one with its room around it): the `between` acts as a group, and says how much room it takes.
                ([_, ..], true) => (x, counter),
                ([], _) => return Err(CompileError::at(x.line, x.col, "an empty `between` separates nothing: `between { box { size: 200, 1; color: ink } }`")),
                _ => return Err(CompileError::at(x.line, x.col, "a `between` with several things has to say how much room it takes: `between { size: 200, 9; … }`")),
            });
        }
        // The children come out of a queue: after each one (except the first) its separator slips in,
        // which is seen if that child is there and some of the earlier ones too.
        let mut queue: std::collections::VecDeque<(&'a Node, Vec<Rc<Scope>>, Option<Expr>)> = children.into_iter().map(|(h, e)| (h, e, None)).collect();
        // How many of the children read so far are there, and how many were read.
        let mut present_so_far: Option<Expr> = None;
        let mut children_read = 0usize;
        while let Some((child, scopes, forced)) = queue.pop_front() {
            let mark = self.rules.len();
            // A child that comes from outside the component is read with the scope of whoever wrote it.
            let from_outside = scopes.first().and_then(|e| e.outer_scope.clone());
            let (extra, inside, pending) = match from_outside {
                Some(outside) => {
                    // After the mark come the iterations of `repeat` or `for` that wrap it, if there are any.
                    let inside = std::mem::replace(&mut self.scopes, outside);
                    let extra = scopes.len() - 1;
                    self.scopes.extend(scopes.into_iter().skip(1));
                    (extra, Some(inside), std::mem::take(&mut self.instance_children))
                }
                None => {
                    let extra = scopes.len();
                    self.scopes.extend(scopes);
                    (extra, None, Vec::new())
                }
            };
            // `show:` decides whether the child is there: it takes room and is seen, or neither.
            // And `grow:`, how much of what is left over along the axis it asks for.
            let mut visible = Expr::K(1.0);
            let mut grow = 0.0f32;
            let mut place: Option<Expr> = None;
            for e in child.body.as_deref().unwrap_or(&[]) {
                if let Entry::Prop { name, value, line, col } = e {
                    let mut c = Cur::new(value, *line, *col);
                    match name.as_str() {
                        "show" => visible = self.expr(&mut c)?,
                        // `place: end`: this child, at the end across its layout —its own
                        // `align:` for it alone—; or a number from 0 (start) to 1 (end) that
                        // may change (`place: if(m.mine, 1, 0)`: a chat's two sides).
                        "place" => {
                            place = Some(match c.peek() {
                                Some(TokenKind::Id(w)) if matches!(w.as_str(), "start" | "center" | "end") && c.i + 1 == c.tokens.len() => {
                                    let k = match w.as_str() {
                                        "start" => 0.0,
                                        "center" => 0.5,
                                        _ => 1.0,
                                    };
                                    c.i += 1;
                                    Expr::K(k)
                                }
                                _ => self.expr(&mut c)?.clamp(0.0, 1.0),
                            });
                            c.expect_end()?;
                        }
                        "grow" => {
                            grow = c.num()?;
                            c.expect_end()?;
                            if grow < 0.0 {
                                return Err(CompileError::at(*line, *col, "`grow` is how much of what is left it asks for: 0 or more"));
                            }
                            if given_size.is_none() {
                                return Err(CompileError::at(*line, *col, "`grow:` needs the layout to say how big it is, or there is no `left over` to share: `row x { size: 164, 66; … }`"));
                            }
                        }
                        _ => {}
                    }
                }
            }
            for e in &self.scopes[self.scopes.len() - extra..] {
                if let Some(v) = &e.visible {
                    visible = if matches!(visible, Expr::K(k) if k == 1.0) { v.clone() } else { visible * v.clone() };
                }
            }
            if let Some(f) = &forced {
                visible = f.clone();
            }
            // What decides whether it is there, without springs: it is what switches off its zones.
            // Constant does not mean "always": `show: k < 3` inside a
            // `repeat` is known when reading it, and for k = 5 it is a zero.
            let always = matches!(visible, Expr::K(k) if k == 1.0);
            let is_present = (!always).then(|| visible.clone());
            if let (Some(m), false) = (spring, always) {
                // With a spring, appearing and disappearing is a journey too.
                self.copies += 1;
                let v = self.e.prop_with(interned(&format!("·visible{}", self.copies)), 0.0, m);
                self.e.behaviors.push(Behavior::Follow { prop: v, to: visible });
                visible = v.e().clamp(0.0, 1.0);
            }
            let with_opacity = !always;
            if with_opacity {
                self.e.paint(Instr::Opacity(Some(visible.clone())));
            }
            let instr = self.e.instrs.len();
            let child_slot = Transform::at((0.0.into(), 0.0.into()));
            self.e.paint(Instr::Transform(Some(child_slot.clone())));
            self.under.push(child_slot);
            let from = self.candidates.len();
            if matches!(child.head.first().map(|f| &f.kind), Some(TokenKind::Id(t)) if t == "text") {
                self.copies += 1;
                self.imposed_measure = Some(self.e.measured(interned(&format!("·measure{}", self.copies))));
            }
            self.in_slot = true;
            self.last_size = None;
            let mut no_clips = 0;
            let r = self.statement(child, &mut no_clips);
            self.in_slot = false;
            self.imposed_measure = None;
            self.under.pop();
            self.e.paint(Instr::Transform(None));
            if with_opacity {
                self.e.paint(Instr::Opacity(None));
            }
            for _ in 0..extra {
                self.close_scope(mark);
            }
            if let Some(inside) = inside {
                self.scopes = inside;
                self.instance_children = pending;
            }
            r?;
            let Some(size) = self.last_size.take() else {
                return Err(CompileError::at(child.line, child.col, "I don\'t know how much room this takes inside a layout: put it in a `group` with `size: width, height`"));
            };
            if let Some(is_present) = &is_present {
                for c in &mut self.candidates[from..] {
                    c.visible = Some(match c.visible.take() { Some(v) => v * is_present.clone(), None => is_present.clone() });
                }
            }
            let placed = Placed {
                instr,
                candidates: from..self.candidates.len(),
                size,
                visible,
                separator: forced.is_some(),
                grow,
                place,
                own_instrs: instr + 1..self.e.instrs.len(),
            };
            match forced {
                // A separator is painted after its child, but its place is just before.
                Some(_) => placed_items.insert(placed_items.len() - 1, placed),
                None => {
                    let presence = is_present.unwrap_or(Expr::K(1.0));
                    if let (Some((sep, counter)), Some(before)) = (&separator, present_so_far.clone()) {
                        self.copies += 1;
                        let mut env = Scope { suffix: format!("#between{}", self.copies), ..Default::default() };
                        if let Some(v) = counter {
                            env.exprs.insert(v.clone(), Expr::K(children_read as f32));
                        }
                        let sep = *sep;
                        queue.push_front((sep, vec![Rc::new(env)], Some(presence.clone() * before.min(Expr::K(1.0)))));
                    }
                    present_so_far = Some(match present_so_far.take() {
                        Some(before) => self.shared("count", before + presence),
                        None => presence,
                    });
                    children_read += 1;
                    placed_items.push(placed);
                }
            }
        }
        // How many children are there right now: `list.count`.
        let how_many = present_so_far.unwrap_or(Expr::K(0.0));

        // How much room each one takes along the axis, and the most any of them takes across it.
        let length_of = |p: &Placed| if is_row { p.size.0.clone() } else { p.size.1.clone() };
        let breadth_of = |p: &Placed| if is_row { p.size.1.clone() } else { p.size.0.clone() };
        // `grow:`: what is left over along the axis, shared among those who ask for it. It is
        // known here, when they have all been measured, so the child is told
        // afterwards: its texts that did not give a width get this one. That
        // is what makes whatever does not fit get cut instead of slipping under
        // its neighbour, which is the bug this comes to remove.
        let total_weight: f32 = placed_items.iter().map(|p| p.grow).sum();
        if total_weight > 0.0 {
            let (tw, th) = given_size.clone().expect("`grow` already demanded `size`");
            let along = if is_row { tw } else { th };
            let fixed_total = placed_items.iter().filter(|p| p.grow == 0.0).fold(Expr::K(0.0), |a, p| a + length_of(p) * p.visible.clone());
            let gaps = gap.clone() * (how_many.clone() - Expr::K(1.0)).max(Expr::K(0.0));
            let left_over = self.shared("left", (along - padding.clone() * 2.0 - gaps - fixed_total).max(Expr::K(0.0)));
            for p in placed_items.iter_mut().filter(|p| p.grow > 0.0) {
                let part = left_over.clone() * (p.grow / total_weight);
                for k in p.own_instrs.clone() {
                    if let Instr::Text { width, .. } = &mut self.e.instrs[k] {
                        if width.is_none() {
                            *width = Some(part.clone());
                        }
                    }
                }
                if is_row {
                    p.size.0 = part;
                } else {
                    p.size.1 = part;
                }
            }
        }
        let mut max_across = placed_items.iter().fold(Expr::K(0.0), |m, p| m.max(breadth_of(p) * p.visible.clone()));
        // With `size:` given, the breadth of the stack is what was given and not what
        // the fattest child takes: that way `align: center` centres inside the box
        // asked for. Without this, a card 66 tall with 32 of content
        // left it all at the top and the gap at the bottom.
        if let Some((tw, th)) = &given_size {
            let given = if is_row { th.clone() } else { tw.clone() };
            max_across = max_across.max((given - padding.clone() * 2.0).max(Expr::K(0.0)));
        }
        // With `view:` the same: across, what is seen —a chat's bubbles placed at its
        // end go to the end of the window, not of the widest bubble—.
        if given_size.is_none()
            && let Some((vw, vh)) = &view
        {
            let seen = if is_row { vh.clone() } else { vw.clone() };
            max_across = max_across.max((seen - padding.clone() * 2.0).max(Expr::K(0.0)));
        }
        let max_across = self.shared("across", max_across);
        // With `wrap`, the cell measures what the biggest child does, and each one goes to its own.
        let cell_along = match wrap {
            Some(_) => self.shared("cell", placed_items.iter().fold(Expr::K(0.0), |m, p| m.max(length_of(p))) + gap.clone()),
            None => Expr::K(0.0),
        };
        let cell_across = max_across.clone() + gap.clone();
        let mut running = padding.clone();
        // How many of the earlier ones are there: what is not seen takes no cell.
        let mut placed_so_far = Expr::K(0.0);
        for (k, p) in placed_items.iter().enumerate() {
            // A separator does not open another gap: it goes in the middle of the one already between its neighbours.
            let mut along = if p.separator { running.clone() - gap.clone() * 0.5 } else { running.clone() };
            let mut grid_across = None;
            if let Some(how_many) = wrap {
                let per_line = Expr::K(how_many as f32);
                let grid_row = (placed_so_far.clone() / per_line.clone()).floor();
                let column = placed_so_far.clone() - grid_row.clone() * per_line;
                along = padding.clone() + column * cell_along.clone();
                grid_across = Some(padding.clone() + grid_row * cell_across.clone());
                placed_so_far = self.shared("placed", placed_so_far + p.visible.clone());
            }
            if let Some(m) = spring {
                // The slot is a target: the child moves towards it with the stack's spring.
                self.copies += 1;
                let prop = self.e.prop_with(interned(&format!("·slot{}", self.copies)), 0.0, m);
                self.e.behaviors.push(Behavior::Follow { prop, to: along });
                along = prop.e();
                if let Some(a) = grid_across.take() {
                    self.copies += 1;
                    let other = self.e.prop_with(interned(&format!("·jump{}", self.copies)), 0.0, m);
                    self.e.behaviors.push(Behavior::Follow { prop: other, to: a });
                    grid_across = Some(other.e());
                }
            }
            // At the start of the breadth nothing is left to share out: it is the padding.
            let across = grid_across.unwrap_or_else(|| match &p.place {
                Some(f) => padding.clone() + (max_across.clone() - breadth_of(p)) * f.clone(),
                None if align_factor == 0.0 => padding.clone(),
                None => padding.clone() + (max_across.clone() - breadth_of(p)) * align_factor,
            });
            let translate = if is_row { (along, across) } else { (across, along) };
            if let Instr::Transform(Some(t)) = &mut self.e.instrs[p.instr] {
                t.translate = translate.clone();
            }
            for c in &mut self.candidates[p.candidates.clone()] {
                c.under[level].translate = translate.clone();
            }
            let last = k + 1 == placed_items.len();
            running = self.shared("along", running + (length_of(p) + if last || p.separator { Expr::K(0.0) } else { gap.clone() }) * p.visible.clone());
        }
        let (total_along, total_across) = match wrap {
            // A grid measures what its lines do: the last one carries no gap after it.
            Some(per_line_count) => {
                let per_line = Expr::K(per_line_count as f32);
                let in_line = how_many.clone().min(per_line.clone());
                let lines = (how_many.clone() / per_line).ceil();
                (
                    (in_line * cell_along - gap.clone()).max(Expr::K(0.0)) + padding.clone() * 2.0,
                    (lines * cell_across - gap.clone()).max(Expr::K(0.0)) + padding.clone() * 2.0,
                )
            }
            None => (running + padding.clone(), max_across + padding.clone() * 2.0),
        };
        let mut content = if is_row { (total_along, total_across) } else { (total_across, total_along) };
        // What the scene says rules: the stack only holds the window, but the
        // scrolling is over the whole list.
        if let Some(e) = &given_content {
            if is_row { content.0 = e.clone() } else { content.1 = e.clone() }
        }
        // With `view:`, towards the outside it takes what is seen, not what it carries inside.
        // What it measures: what was given if it was given, what is seen if there is a window, and otherwise,
        // what its children take. Given, the background and the zone are that size
        // even if the children do not reach it: it is what was asked for, not what came out.
        let size = given_size.clone().or_else(|| view.clone()).unwrap_or_else(|| content.clone());

        if let (Some(k), Some(color)) = (background_slot, fill) {
            self.e.instrs[k] = Instr::Solid {
                shape: Shape::Rect { center: (size.0.clone() * 0.5, size.1.clone() * 0.5), half_size: (size.0.clone() * 0.5, size.1.clone() * 0.5), radius: corner },
                color,
                alpha: fill_alpha,
                glass_spec: fill_glass,
            };
        }
        if anchor != (0.0, 0.0) {
            let translate = (origin.0 - size.0.clone() * anchor.0, origin.1 - size.1.clone() * anchor.1);
            if let Instr::Transform(Some(t)) = &mut self.e.instrs[instr_base] {
                t.translate = translate.clone();
            }
            for c in &mut self.candidates[base_candidates..] {
                c.under[base_level].translate = translate.clone();
            }
        }
        if scroller.is_some() {
            self.under.pop();
            self.e.paint(Instr::Transform(None));
            self.e.paint(Instr::Clip(None));
        }
        if opacity.is_some() {
            self.e.paint(Instr::Opacity(None));
        }
        self.under.pop();
        self.e.paint(Instr::Transform(None));
        // With a name, its size can be used further down (`bar.width`), and its whole
        // box is a zone if some rule names it. It goes under those of its
        // children: the wheel over the stack does not take the click from what is inside.
        if let Some(local) = name {
            // The name of a stack is declared when its children have already been read, so
            // we have to say again where it was: otherwise, the editor takes you to the last one.
            PEEKED.with(|m| m.set((n.line, n.col)));
            self.current_class = if is_row { "row".to_owned() } else { "column".to_owned() };
            let name = self.declare(&local);
            if let Some(e) = self.scopes.last_mut().map(Rc::make_mut) {
                e.with_parts.insert(local.clone());
            }
            let mut under = self.under.clone();
            if let Instr::Transform(Some(t)) = &self.e.instrs[instr_base] {
                under.push(t.clone());
            }
            let bounds = Shape::Rect { center: (size.0.clone() * 0.5, size.1.clone() * 0.5), half_size: (size.0.clone() * 0.5, size.1.clone() * 0.5), radius: zone_corner };
            self.candidates.insert(base_candidates, Candidate { name: name.clone(), at: instr_base, zblock: self.zblock, shape: bounds, active: None, visible: None, under, forced: scroller.is_some(), cursor: stack_cursor, cursor_when: stack_cursor_when, carries: None, carry_icon: None, label: None, reach: Reach::Any, within: None, told: None, handle: None, yields: None });
            // A hidden stack does not catch the mouse: neither its children nor IT, which with
            // `view:` has a zone of its own —the one for the wheel and dragging— the
            // size of its window. Hidden and in front, that zone
            // kept the clicks of everything underneath it: in marea-plm,
            // four of the five cards of the control centre.
            let _ = &hide;
            if let Some(shown) = &opacity {
                let is_present = shown.clone().gt(0.01);
                for c in &mut self.candidates[base_candidates..] {
                    c.visible = Some(match c.visible.take() { Some(v) => v * is_present.clone(), None => is_present.clone() });
                }
            }
            if scroller.is_some() {
                for c in &mut self.candidates[base_candidates + 1..] {
                    c.within.get_or_insert_with(|| name.clone());
                }
            }
            if let Some(prop) = &scroller {
                // The wheel over it scrolls it, without going past what there is. The rule is created at the
                // end, when it is already known which named shapes are real zones.
                let visible = if is_row { size.0.clone() } else { size.1.clone() };
                let until = (if is_row { content.0.clone() } else { content.1.clone() } - visible).max(Expr::K(0.0));
                // And where it was when it was grabbed, to be able to drag it.
                let grab = self.e.fact(interned(&format!("{name}.grab")), 0.0);
                if is_row {
                    self.row_scrolls.insert(name.clone());
                }
                // `stick: end`: `list.pinned`, whether it is at its end (1 when it starts).
                let pinned = stick_end.then(|| {
                    let f = self.e.fact(interned(&format!("{name}.pinned")), 1.0);
                    self.facts.insert(format!("{name}.pinned"), f);
                    f
                });
                self.scrolls.push((name.clone(), *prop, until, step.clone(), spring.unwrap_or(Spring::QUICK), grab, pinned));
            }
            let target = match self.scopes.last_mut().map(Rc::make_mut) {
                Some(e) => &mut e.exprs,
                None => &mut self.lets,
            };
            target.insert(format!("{name}.width"), size.0.clone());
            target.insert(format!("{name}.height"), size.1.clone());
            target.insert(format!("{name}.count"), how_many.clone());
            // With `view:`: how much there really is, and where it is at.
            let content_length = if is_row { content.0.clone() } else { content.1.clone() };
            target.insert(format!("{name}.content"), content_length);
            if let Some(prop) = &scroller {
                target.insert(format!("{name}.scroll"), prop.e());
                // And as a real property: a rule can take it wherever it wants
                // (`list.scroll: 0 ~calm`), not only the wheel.
                self.props.insert(format!("{name}.scroll"), *prop);
            }
            // Whoever read it before this point read the property declared early: here it is filled in.
            let content_length = if is_row { content.0.clone() } else { content.1.clone() };
            for (part, a) in [("width", &size.0), ("height", &size.1), ("count", &how_many), ("content", &content_length)] {
                if let Some(prop) = self.props.get(&format!("{name}.{part}")).copied() {
                    self.e.behaviors.push(Behavior::Bind { prop, to: a.clone() });
                }
            }
        }
        self.last_size = Some(size);
        Ok(())
    }

    /// The children of a stack, with each `repeat` unrolled into its iterations.
    fn unroll(&mut self, entries: Vec<&'a Entry>, scope: &mut Vec<Rc<Scope>>, children: &mut Vec<(&'a Node, Vec<Rc<Scope>>)>) -> R<()> {
        for e in entries {
            let Entry::Node(n) = e else { continue };
            if matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "repeat") {
                let mut c = Cur::new(&n.head[1..], n.line, n.col);
                self.scopes.extend(scope.iter().cloned());
                let head = self.repeat_head(&mut c);
                self.scopes.truncate(self.scopes.len() - scope.len());
                let (var, from, until) = head?;
                for v in from..until {
                    let mut env = Scope { suffix: format!("#{var}{v}"), ..Default::default() };
                    env.exprs.insert(var.clone(), Expr::K(v as f32));
                    scope.push(Rc::new(env));
                    self.unroll(n.body.as_deref().unwrap_or(&[]).iter().collect(), scope, children)?;
                    scope.pop();
                }
            } else if matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "between") {
                // It is not a child: it is what goes between them. The stack places it.
            } else if matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "children") {
                let (from_outside, outside) = self.take_slot(n)?;
                // Each one takes its place in the stack, and is read with the names of whoever
                // wrote it. A `repeat` or a `for` from outside unrolls like the ones inside.
                let mut outer_scope = vec![Rc::new(Scope { outer_scope: Some(outside.clone()), ..Default::default() })];
                let inside = std::mem::replace(&mut self.scopes, outside);
                let r = self.unroll(from_outside, &mut outer_scope, children);
                self.scopes = inside;
                r?;
            } else if matches!(n.head.first().map(|f| &f.kind), Some(TokenKind::Id(p)) if p == "for") {
                let mut c = Cur::new(&n.head[1..], n.line, n.col);
                // With the outer iterations in view: `for c in m.children` needs to know who `m` is.
                self.scopes.extend(scope.iter().cloned());
                let head = self.for_head(&mut c);
                self.scopes.truncate(self.scopes.len() - scope.len());
                let (var, model, capacity, from) = head?;
                for k in 0..capacity {
                    scope.push(Rc::new(self.for_iteration(&var, &model, k, &from)));
                    self.unroll(n.body.as_deref().unwrap_or(&[]).iter().collect(), scope, children)?;
                    scope.pop();
                }
            } else if is_declaration(n) {
                // A `prop`, a `let`, a rule… inside a layout is not a child that
                // takes room: it is what it is, read where it is written, with
                // the names of the `repeat`s around it.
                let depth = scope.len();
                self.scopes.extend(scope.iter().cloned());
                let mut no_clips = 0;
                let r = self.statement(n, &mut no_clips);
                self.scopes.truncate(self.scopes.len() - depth);
                r?;
            } else {
                children.push((n, scope.clone()));
            }
        }
        Ok(())
    }

/// The pages written anywhere in these entries: their name, their pages'
    /// names and titles, and where each `pages` node is.
    fn pages_of(n: &Node) -> R<(String, Vec<(String, String, &Node)>, Option<(Vec<Token>, usize, usize)>)> {
        let Some(TokenKind::Id(name)) = n.head.get(1).map(|t| &t.kind) else {
            return Err(CompileError::at(n.line, n.col, "`pages` needs a name, which is the fact that says which one is shown: `pages settings { … }`"));
        };
        let mut pages = Vec::new();
        let mut header = None;
        for e in n.body.as_deref().unwrap_or(&[]) {
            match e {
                Entry::Prop { name: p, value, line, col } if p == "header" => header = Some((value.clone(), *line, *col)),
                Entry::Prop { name: p, line, col, .. } => return Err(CompileError::at(*line, *col, format!("`pages` only takes `header: x, y`, and '{p}' is not that"))),
                Entry::Node(x) => {
                    let (Some(TokenKind::Id(w)), Some(TokenKind::Id(id)), Some(TokenKind::Str(title))) = (x.head.first().map(|t| &t.kind), x.head.get(1).map(|t| &t.kind), x.head.get(2).map(|t| &t.kind)) else {
                        return Err(CompileError::at(x.line, x.col, "inside `pages` go its pages: `page look \"Her look\" { … }`"));
                    };
                    if w != "page" || x.head.len() != 3 {
                        return Err(CompileError::at(x.line, x.col, "inside `pages` go its pages: `page look \"Her look\" { … }`"));
                    }
                    pages.push((id.clone(), title.clone(), x));
                }
            }
        }
        if pages.is_empty() {
            return Err(CompileError::at(n.line, n.col, "`pages` without a single `page` shows nothing"));
        }
        if pages.len() == 1 {
            return Err(CompileError::at(n.line, n.col, "`pages` with a single page has nowhere to go: that is a `group`"));
        }
        Ok((name.clone(), pages, header))
    }

    fn declare_pages_early(&mut self, entries: &'a [Entry]) {
        for e in entries {
            let Entry::Node(n) = e else { continue };
            if matches!(n.head.first().map(|t| &t.kind), Some(TokenKind::Id(w)) if w == "pages") {
                match Self::pages_of(n) {
                    Ok((name, pages, _)) => {
                        let ids: Vec<&str> = pages.iter().map(|p| p.0.as_str()).collect();
                        let source = format!("fact {name}: {} = {}", ids.join(" | "), ids[0]);
                        if let Err(f) = self.expand(&source, n.line, n.col).and_then(|nodes| {
                            let mut no_clips = 0;
                            nodes.iter().try_for_each(|x| self.statement(x, &mut no_clips))
                        }) {
                            self.push_error(f);
                        }
                    }
                    Err(f) => self.push_error(f),
                }
            }
            if let Some(b) = &n.body {
                self.declare_pages_early(b);
            }
        }
    }

    /// Source written by the compiler itself, read as if it were the scene's:
    /// its tokens carry the place of whatever it stands for, so an error in it
    /// points there. The compile owns it until all deferred passes finish.
    fn expand(&self, source: &str, line: usize, col: usize) -> R<Vec<&'a Node>> {
        let mut tokens = crate::language::tokens::tokenize(source)?;
        for t in &mut tokens {
            t.line = line;
            t.col = col;
        }
        let entries = crate::language::tree::parse(&tokens)?;
        Ok(entries.into_iter().filter_map(|e| if let Entry::Node(x) = e {
            Some(&*self.generated.alloc(x))
        } else { None }).collect())
    }

    /// `pages settings { header: 20, 45; page menu "Settings" { … } page look "Her look" { … } }`:
    /// one page at a time. `settings` is a fact with the pages' names, which
    /// anything sets (`settings = look`); each page slides in as it becomes
    /// the one —the first from the left, the others from the right— and its
    /// zones are only there while it is seen. With `header:` comes, at that
    /// point, the page's title and, on any but the first, a ← that goes back
    /// to it; Esc goes back too.
    fn pages(&mut self, n: &'a Node) -> R<()> {
        let (name, pages, header) = Self::pages_of(n)?;
        let first = pages[0].0.clone();
        for (k, (id, _, page)) in pages.iter().enumerate() {
            let shown = format!("{name}.{id}.shown");
            let slide = if k == 0 { -24 } else { 24 };
            let source = format!(
                "prop {shown} = {initial} ~260ms\nfollow {shown} = if({name} == {name}.{id}, 1, 0)\ngroup {{ opacity: {shown}; move: (1 - {shown}) * {slide}, 0 }}",
                initial = if k == 0 { 1 } else { 0 }
            );
            let nodes = self.expand(&source, page.line, page.col)?;
            let mut no_clips = 0;
            self.statement(nodes[0], &mut no_clips)?;
            self.statement(nodes[1], &mut no_clips)?;
            // The page's own content goes inside its group, as it was written.
            let mut group = nodes[2].clone();
            group.body.get_or_insert_with(Vec::new).extend(page.body.clone().unwrap_or_default());
            let group = self.generated.alloc(group);
            self.statement(group, &mut no_clips)?;
        }
        if let Some((value, line, col)) = header {
            // `header: x, y`: two expressions, which become `settings.hx` and
            // `settings.hy` with their own tokens —and their own place, for errors—.
            let mut depth = 0i32;
            let comma = value.iter().position(|t| match t.kind {
                TokenKind::Sym("(") => { depth += 1; false }
                TokenKind::Sym(")") => { depth -= 1; false }
                TokenKind::Sym(",") => depth == 0,
                _ => false,
            }).ok_or_else(|| CompileError::at(line, col, "`header:` is where the title goes: `header: x, y`"))?;
            let (hx, hy) = (format!("{name}.hx"), format!("{name}.hy"));
            for (let_name, part) in [(&hx, &value[..comma]), (&hy, &value[comma + 1..])] {
                let mut head = vec![
                    Token { kind: TokenKind::Id("let".into()), line, col },
                    Token { kind: TokenKind::Id(let_name.clone()), line, col },
                    Token { kind: TokenKind::Sym("="), line, col },
                ];
                head.extend(part.iter().cloned());
                let node = self.generated.alloc(Node { head, body: None, line, col });
                let mut no_clips = 0;
                self.statement(node, &mut no_clips)?;
            }
            let titles: Vec<String> = pages.iter().map(|p| format!("{:?}", p.1)).collect();
            let back = format!("{name}.back");
            self.hover_mentions.insert(back.clone());
            let first_shown = format!("{name}.{first}.shown");
            let source = format!(
                "group {{ opacity: 1 - {first_shown}\n  path {{ at: {hx}, {hy} - 5; color: #f5f7f5; stroke: 1.8; opacity: 70% + {back}.hover * 30%; move 5, 0; line 0, 5; line 5, 10 }}\n}}\n\
                 text pick({name}, {titles}) {{ at: {hx} + 20 * (1 - {first_shown}), {hy}; anchor: left center; size: 18; weight: 500; color: #f5f7f5 }}\n\
                 zone box {back} {{ at: {hx} + 3, {hy}; size: 30, 30; corner: 15; cursor: pointer; active: {name} != {name}.{first} }}\n\
                 on press {back} {{ {name} = {first} }}\n\
                 on key Escape while {name} != {name}.{first} {{ {name} = {first} }}",
                titles = titles.join(", ")
            );
            let nodes = self.expand(&source, line, col)?;
            for x in &nodes {
                self.zone_springs_prescan(x);
            }
            let mut no_clips = 0;
            for x in nodes {
                self.statement(x, &mut no_clips)?;
            }
        }
        Ok(())
    }

    /// A zone about to be declared outside `group` —an expansion—: its springs.
    fn zone_springs_prescan(&mut self, n: &Node) {
        // `zone box hit { … }`, and `window win.$i { … }`, which is a zone too.
        let at = match n.head.first().map(|t| &t.kind) {
            Some(TokenKind::Id(w)) if w == "zone" => 2,
            Some(TokenKind::Id(w)) if w == "window" => 1,
            _ => return,
        };
        if let Some(TokenKind::Id(local)) = n.head.get(at).map(|t| &t.kind) {
            if self.hover_mentions.contains(local.as_str()) {
                let local = local.clone();
                self.zone_springs_for(&local);
            }
        }
    }

    /// `grid { at: x, y; columns: 2; gap: 12; width: 456; row: 106 }`: its
    /// children in cells, left to right and then down. A child takes one cell,
    /// or several with `span:`; inside it, `cell.w` and `cell.h` are the size of
    /// its cell, to draw in without writing a coordinate from outside. It does
    /// not have to say its size; if it says it, and the grid has no `row:`,
    /// each row is as tall as its tallest child. Its zones go where it goes.
    fn grid(&mut self, n: &'a Node, c: &mut Cur) -> R<()> {
        c.expect_end()?;
        let mut p = self.properties(n, vocab::properties("grid"))?;
        let origin = match p.get_mut("at") {
            Some(c) => self.point(c)?,
            None => (0.0.into(), 0.0.into()),
        };
        let columns = match p.get_mut("columns") {
            Some(c) => {
                let k = c.num()? as usize;
                c.expect_end()?;
                if !(1..=24).contains(&k) {
                    return Err(CompileError::at(n.line, n.col, "`columns` goes from 1 to 24"));
                }
                k
            }
            None => return Err(CompileError::at(n.line, n.col, "a grid says how many columns it has: `columns: 2`")),
        };
        let width = match p.get_mut("width") {
            Some(c) => self.expr(c)?,
            None => return Err(CompileError::at(n.line, n.col, "a grid says how wide it is, to share it among its columns: `width: 456`")),
        };
        let gap = match p.get_mut("gap") {
            Some(c) => self.expr(c)?,
            None => Expr::K(0.0),
        };
        let row_given = match p.get_mut("row") {
            Some(c) => Some(self.expr(c)?),
            None => None,
        };
        let opacity = match p.get_mut("opacity") {
            Some(o) => Some(self.expr(o)?),
            None => None,
        };
        let opacity = match p.get_mut("show") {
            Some(c) => {
                let v = self.expr(c)?.clamp(0.0, 1.0);
                Some(opacity.map_or(v.clone(), |o| o * v))
            }
            None => opacity,
        };
        let base = Transform::at((0.0.into(), 0.0.into())).translate(origin.0.clone(), origin.1.clone());
        self.e.paint(Instr::Transform(Some(base.clone())));
        self.under.push(base);
        if let Some(o) = &opacity {
            self.e.paint(Instr::Opacity(Some(o.clone())));
        }
        let mut children: Vec<(&'a Node, Vec<Rc<Scope>>)> = Vec::new();
        self.unroll(n.body.as_deref().unwrap_or(&[]).iter().collect(), &mut Vec::new(), &mut children)?;
        let cell_w = (width.clone() - gap.clone() * (columns as f32 - 1.0)) / Expr::K(columns as f32);
        let level = self.under.len();
        struct Cell {
            instr: usize,
            candidates: std::ops::Range<usize>,
            row: usize,
            height: Expr,
        }
        let mut cells: Vec<Cell> = Vec::new();
        let (mut col, mut row) = (0usize, 0usize);
        for (child, scopes) in children {
            let mark = self.rules.len();
            let extra = scopes.len() + 1;
            self.scopes.extend(scopes);
            // How many columns it takes: a number known when reading the scene,
            // which inside a `repeat` may depend on it (`span: if(t == 4, 2, 1)`).
            let mut span = 1usize;
            for e in child.body.as_deref().unwrap_or(&[]) {
                if let Entry::Prop { name, value, line, col: pc } = e {
                    if name == "span" {
                        let mut c = Cur::new(value, *line, *pc);
                        span = match self.expr(&mut c)? {
                            Expr::K(k) => (k.round().max(1.0) as usize).min(columns),
                            _ => return Err(CompileError::at(*line, *pc, "`span` is a number known when reading the scene: how many columns the child takes")),
                        };
                    }
                }
            }
            if col + span > columns {
                row += 1;
                col = 0;
            }
            let w = cell_w.clone() * span as f32 + gap.clone() * (span as f32 - 1.0);
            let x = (cell_w.clone() + gap.clone()) * col as f32;
            // A turn of a `for` only exists if the list reaches that far: as in
            // a row or a column, a cell past the end is neither seen nor pressed.
            // (Its place stays: a grid's cells do not move up to fill it.)
            let mut present = Expr::K(1.0);
            for e in &self.scopes[self.scopes.len() - (extra - 1)..] {
                if let Some(v) = &e.visible {
                    present = if matches!(present, Expr::K(k) if k == 1.0) { v.clone() } else { present * v.clone() };
                }
            }
            let present = (!matches!(present, Expr::K(k) if k == 1.0)).then_some(present);
            let mut here = Scope::default();
            here.exprs.insert("cell.w".into(), w.clone());
            here.exprs.insert("cell.h".into(), row_given.clone().unwrap_or(Expr::K(0.0)));
            self.scopes.push(Rc::new(here));
            if let Some(v) = &present {
                self.e.paint(Instr::Opacity(Some(v.clone())));
            }
            let instr = self.e.instrs.len();
            let slot = Transform::at((0.0.into(), 0.0.into())).translate(x, Expr::K(0.0));
            self.e.paint(Instr::Transform(Some(slot.clone())));
            self.under.push(slot);
            let from = self.candidates.len();
            self.in_slot = true;
            self.last_size = None;
            let mut no_clips = 0;
            let r = self.statement(child, &mut no_clips);
            self.in_slot = false;
            self.under.pop();
            self.e.paint(Instr::Transform(None));
            if let Some(v) = &present {
                self.e.paint(Instr::Opacity(None));
                for k in &mut self.candidates[from..] {
                    k.visible = Some(match k.visible.take() { Some(w) => w * v.clone(), None => v.clone() });
                }
            }
            for _ in 0..extra {
                self.close_scope(mark);
            }
            r?;
            let height = match (self.last_size.take(), &row_given) {
                (Some((_, h)), None) => h,
                (_, Some(h)) => h.clone(),
                (None, None) => return Err(CompileError::at(child.line, child.col, "in a grid without `row:`, each child says how tall it is: `group { size: cell.w, 90; … }`, or the grid gives the height of its rows")),
            };
            cells.push(Cell { instr, candidates: from..self.candidates.len(), row, height });
            col += span;
        }
        // Each row as tall as its tallest child (or as `row:` says), and each
        // child at the top of its own.
        let rows = cells.last().map_or(0, |c| c.row + 1);
        let mut tops: Vec<Expr> = Vec::with_capacity(rows);
        let mut running = Expr::K(0.0);
        for r in 0..rows {
            tops.push(running.clone());
            let tallest = cells.iter().filter(|c| c.row == r).fold(Expr::K(0.0), |m, c| m.max(c.height.clone()));
            running = self.shared("top", running + tallest + gap.clone());
        }
        for c in &cells {
            let y = tops[c.row].clone();
            if let Instr::Transform(Some(t)) = &mut self.e.instrs[c.instr] {
                t.translate.1 = y.clone();
            }
            for k in &mut self.candidates[c.candidates.clone()] {
                k.under[level].translate.1 = y.clone();
            }
        }
        if opacity.is_some() {
            self.e.paint(Instr::Opacity(None));
        }
        self.under.pop();
        self.e.paint(Instr::Transform(None));
        let total_h = if rows > 0 { running - gap } else { Expr::K(0.0) };
        self.last_size = Some((width, total_h));
        Ok(())
    }

    // ── models ──────────────────────────────────────────────────

    /// `model rows max 14 { label: text;  enabled: bool = true;  depth: number }`
    /// The type of a fact: `number`, `bool`, or an enum (`low | normal | critical`).
    /// `None` is a plain number.
    fn fact_type(&mut self, c: &mut Cur) -> R<Option<FactType>> {
        if let Some(names) = self.enumeration(c)? {
            return Ok(Some(FactType::Enum(names)));
        }
        let or_enum_hint = |mut f: CompileError| { f.message.push_str(" Or an enum: `low | normal | critical`."); f };
        Ok(match c.one_of(vocab::FACT_TYPES, "the type of a fact").map_err(or_enum_hint)?.as_str() {
            "number" => None,
            "bool" => Some(FactType::Bool),
            _ => unreachable!(),
        })
    }

    /// `low | normal | critical`, if that is what comes. Its names come to be worth their position
    /// in any expression: `mode == critical`.
    fn enumeration(&mut self, c: &mut Cur) -> R<Option<Vec<String>>> {
        if !matches!((c.peek(), c.tokens.get(c.i + 1).map(|x| &x.kind)), (Some(TokenKind::Id(_)), Some(TokenKind::Sym("|")))) {
            return Ok(None);
        }
        let mut names = vec![c.id("a value")?];
        while c.sym("|") {
            let n = c.id("another value of the enum")?;
            if names.contains(&n) {
                c.i -= 1;
                return c.error(format!("'{n}' is there twice"));
            }
            names.push(n);
        }
        for (k, n) in names.iter().enumerate() {
            // The same name in two enums is fine as long as it means the same number.
            // The same name in two enums is fine. If it is also a different number, on its own it no longer says
            // anything: it will have to be compared with its fact (`speed == normal`) or written in full.
            match self.values.get(n) {
                Some(v) if *v != k as f32 => {
                    self.values.remove(n);
                    self.ambiguous.insert(n.clone());
                }
                _ if self.ambiguous.contains(n) => {}
                _ => { self.values.insert(n.clone(), k as f32); }
            }
            if self.facts.contains_key(n) || self.props.contains_key(n) || self.lets.contains_key(n) {
                return c.error(format!("'{n}' is already something else in this scene, and as an enum value it would hide it"));
            }
        }
        Ok(Some(names))
    }

    /// `model rows max 14 { label: text;  enabled: bool = true;  list items max 8 { … } }`
    /// `service clock as now { time: text; hour: number }`
    ///
    /// Whatever the system reports fills in `now.time` and `now.hour` by itself, without a line of
    /// logic. What each service brings is in the vocabulary: asking it for what it does not have is
    /// an error when loading, like everything else.
    fn service(&mut self, n: &'a Node, c: &mut Cur) -> R<()> {
        let service_names: Vec<&str> = vocab::SERVICES.iter().map(|(x, _)| *x).collect();
        let name = c.one_of(&service_names, "a service")?;
        // Without `as`, its fields go after its own name: `audio.volume`.
        let alias = if c.word("as") { c.id("a name to put before its fields")? } else { name.clone() };
        c.expect_end()?;
        let own = vocab::SERVICES.iter().find(|(x, _)| *x == name).map_or(&[][..], |(_, k)| *k);
        let alias = self.declare(&alias);
        if self.e.services.iter().any(|s| s.alias == alias) {
            return Err(CompileError::at(n.line, n.col, format!("'{alias}' is already the name of another service: give this one another with `as`")));
        }
        // `networks: nets`: a list the service reports, into a model of the scene.
        // Those entries are taken out; the rest are fields like any other.
        let mut models: Vec<(String, String)> = Vec::new();
        let mut plain = n.clone();
        if let Some(body) = plain.body.as_mut() {
            let mut kept = Vec::new();
            for e in body.drain(..) {
                if let Entry::Prop { name: field, value, line, col } = &e {
                    if vocab::LIST_FIELDS.contains(&field.as_str()) {
                        let mut v = Cur::new(value, *line, *col);
                        let model = v.id("the model its records go into")?;
                        v.expect_end()?;
                        if !own.contains(&field.as_str()) {
                            return Err(CompileError::at(*line, *col, format!("'{name}' does not report '{field}': it reports {}", join_or(own))));
                        }
                        if !self.models.contains_key(&model) {
                            return Err(CompileError::at(*line, *col, format!("'{field}' is a list: it goes into a model, and there is no model '{model}' declared before this service (`model {model} max 12 {{ … }}`)")));
                        }
                        models.push((field.clone(), model));
                        continue;
                    }
                }
                kept.push(e);
            }
            *body = kept;
        }
        let plain = self.generated.alloc(plain);
        // Only lists (`service tray { list: icons }`): no fields of its own.
        let fields = if plain.body.as_ref().is_some_and(|b| b.is_empty()) && !models.is_empty() { Vec::new() } else { self.fields_of(plain, 1)? };
        for k in &fields {
            if !own.contains(&k.name.as_str()) {
                let hint = closest_match(&k.name, own.iter().map(|s| s.to_string()).collect::<Vec<_>>().iter()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
                return Err(CompileError::at(n.line, n.col, format!("'{name}' does not report '{}': it reports {}.{hint}", k.name, join_or(own))));
            }
        }
        // `now.time`, not `now.0.time`: there is one of a service, not a list.
        for field in &fields {
            let full_name = format!("{alias}.{}", field.name);
            match &field.fallback {
                FieldValue::Text(t) => {
                    let id = self.e.live_text(interned(&full_name), t);
                    if let FieldType::Image(w, h) = field.kind {
                        let image = self.e.image(ImageSource::Live(id), w, h);
                        self.images.insert(full_name.clone(), image);
                    }
                    self.texts.insert(full_name, id);
                }
                FieldValue::Number(v) => {
                    let id = self.e.fact(interned(&full_name), *v);
                    match &field.kind {
                        FieldType::Bool => self.e.types.push((full_name.clone(), FactType::Bool)),
                        FieldType::Enum(x) => self.e.types.push((full_name.clone(), FactType::Enum(x.clone()))),
                        _ => {}
                    }
                    self.facts.insert(full_name, id);
                }
            }
        }
        self.e.services.push(crate::scene::Service { name, alias, fields, models });
        Ok(())
    }

    /// `windows win max 6`: a compositor inside the scene, and what the scene
    /// knows of each of its windows. They are facts and texts like any other:
    /// the layout is written with them, and the render keeps them up to date.
    fn windows(&mut self, n: &'a Node, c: &mut Cur) -> R<()> {
        let name = self.declare(&c.id("a name for the windows")?);
        let max = if c.word("max") { c.num()? as usize } else { 8 };
        // `dock programs`: one dock for the session, of the programs running
        // wherever their windows are (see `Nest::programs`).
        let programs = if c.word("dock") {
            c.expect_word("programs")?;
            true
        } else {
            false
        };
        // `placement manual`: the scene says where a new one goes (see `Nest::manual`).
        let manual = if c.word("placement") {
            c.expect_word("manual")?;
            true
        } else {
            false
        };
        // `workspaces kept`: they stay while empty (see `Nest::kept`).
        let kept = if c.word("workspaces") {
            c.expect_word("kept")?;
            true
        } else {
            false
        };
        c.expect_end()?;
        if self.e.nest.is_some() {
            return Err(CompileError::at(n.line, n.col, "a scene holds one set of windows: this is the second `windows`"));
        }
        if !(1..=32).contains(&max) {
            return Err(CompileError::at(n.line, n.col, "`max` is between 1 and 32 windows"));
        }
        let fact = |o: &mut Self, full: String, initial: f32, boolean: bool| {
            let id = o.e.fact(interned(&full), initial);
            if boolean {
                o.e.types.push((full.clone(), FactType::Bool));
            }
            o.facts.insert(full, id);
        };
        for k in 0..max {
            fact(self, format!("{name}.{k}.open"), 0.0, true);
            fact(self, format!("{name}.{k}.focused"), 0.0, true);
            fact(self, format!("{name}.{k}.width"), 0.0, false);
            fact(self, format!("{name}.{k}.height"), 0.0, false);
            fact(self, format!("{name}.{k}.place"), -1.0, false);
            fact(self, format!("{name}.{k}.screen"), 0.0, false);
            fact(self, format!("{name}.{k}.fullscreen"), 0.0, true);
            fact(self, format!("{name}.{k}.dialog"), 0.0, true);
            fact(self, format!("{name}.{k}.minimized"), 0.0, true);
            fact(self, format!("{name}.{k}.floating"), 0.0, true);
            fact(self, format!("{name}.{k}.pending"), 0.0, true);
            // It lets the compositor draw its frame; and what it asks, from its
            // own title bar or edges, while the button is down.
            fact(self, format!("{name}.{k}.framed"), 0.0, true);
            // It asks for attention, until it gets the keyboard.
            fact(self, format!("{name}.{k}.urgent"), 0.0, true);
            fact(self, format!("{name}.{k}.held"), 0.0, false);
            fact(self, format!("{name}.{k}.edges"), 0.0, false);
            // Workspaces: which one it is on (they start at 1), how many share
            // its monitor and workspace (what its layout is shared out
            // among), and its turn among the windows shown.
            fact(self, format!("{name}.{k}.workspace"), 1.0, false);
            // And the pool itself, by an identity that does not change when
            // the ones before it dry up and it moves up the stack.
            fact(self, format!("{name}.{k}.pool"), 0.0, false);
            fact(self, format!("{name}.{k}.aside"), 0.0, false);
            fact(self, format!("{name}.{k}.among"), 0.0, false);
            fact(self, format!("{name}.{k}.rank"), -1.0, false);
            // Its item in its monitor's dock, and its turn among the ones put
            // away there (−1: none).
            fact(self, format!("{name}.{k}.dockat"), -1.0, false);
            fact(self, format!("{name}.{k}.minat"), -1.0, false);
            fact(self, format!("{name}.{k}.dockturn"), -1.0, false);
            for field in ["title", "app", "icon"] {
                let full = format!("{name}.{k}.{field}");
                let id = self.e.live_text(interned(&full), "");
                self.texts.insert(full, id);
            }
        }
        // Which window is at each place of the layout: `win.order.0` leads.
        for k in 0..max {
            fact(self, format!("{name}.order.{k}"), -1.0, false);
        }
        fact(self, format!("{name}.count"), 0.0, false);
        // How many are on each monitor: `win.on.$screen` in a copy per monitor.
        for s in 0..4 {
            fact(self, format!("{name}.on.{s}"), 0.0, false);
            // What other programs' bars keep on that monitor: the windows go around it.
            for edge in ["top", "right", "bottom", "left"] {
                fact(self, format!("{name}.reserved.{s}.{edge}"), 0.0, false);
            }
            // The workspace each monitor shows, by its turn and by its identity;
            // how many it has (the one shown, even empty, among them) and how
            // many hold windows.
            fact(self, format!("{name}.shown.{s}"), 1.0, false);
            fact(self, format!("{name}.pool.{s}"), (s + 1) as f32, false);
            fact(self, format!("{name}.pools.{s}"), 1.0, false);
            fact(self, format!("{name}.used.{s}"), 0.0, false);
            // Its dock: the programs pinned, then the others with windows on
            // the workspace it shows; each one's icon, how many windows it has
            // there, whether one has the keyboard, how many are put away.
            fact(self, format!("{name}.docks.{s}"), 0.0, false);
            fact(self, format!("{name}.mins.{s}"), 0.0, false);
            for k in 0..DOCK_ITEMS {
                let full = format!("{name}.dock.{s}.{k}.icon");
                let id = self.e.live_text(interned(&full), "");
                self.texts.insert(full, id);
                let full = format!("{name}.dock.{s}.{k}.name");
                let id = self.e.live_text(interned(&full), "");
                self.texts.insert(full, id);
                for field in ["windows", "focused", "pinned", "away", "badge"] {
                    fact(self, format!("{name}.dock.{s}.{k}.{field}"), 0.0, false);
                }
            }
        }
        fact(self, format!("{name}.focus"), -1.0, false);
        // What is unread, by program, as whoever keeps the notifications says
        // (`discord:3 org.telegram.desktop:1`): the dock shows it on each item.
        let full = format!("{name}.badges");
        let id = self.e.live_text(interned(&full), "");
        self.texts.insert(full, id);
        // What the compositor asks the scene to choose: 1 a monitor, 2 a window, 3 either.
        fact(self, format!("{name}.picking"), 0.0, false);
        let full = format!("{name}.socket");
        let id = self.e.live_text(interned(&full), "");
        self.texts.insert(full, id);
        // The right button is the windows' as well: it no longer closes the scene.
        self.e.surface_mut().right_click_quits = false;
        // How many slots there are, as a number known when loading: `let tw[win.max]`.
        self.lets.insert(format!("{name}.max"), Expr::K(max as f32));
        self.e.nest = Some(crate::scene::Nest { name, max, programs, kept, manual });
        Ok(())
    }

    /// `window win.$i { at: x, y; size: w, h }`: that slot's window, drawn in
    /// that box and answering the mouse there. It is also a zone with its name.
    /// `terminal term { at: x, y; size: w, h; … }`: a shell and its screen (`crate::terminal`).
    fn terminal(&mut self, n: &Node) -> R<()> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        let local = c.id("the terminal's name: `terminal term`")?;
        c.expect_end()?;
        let name = self.global(&local);
        let mut p = self.properties(n, vocab::properties("terminal"))?;
        let in_slot = std::mem::take(&mut self.in_slot);
        let missing = |q: &str| CompileError::at(n.line, n.col, format!("this terminal is missing '{q}'"));
        let (x, y) = match p.get_mut("at") {
            Some(c) => self.point(c)?,
            None if in_slot => (0.0.into(), 0.0.into()),
            None => return Err(missing("at")),
        };
        let (w, h) = self.point(p.get_mut("size").ok_or_else(|| missing("size"))?)?;
        self.last_size = Some((w.clone(), h.clone()));
        let family = match p.get_mut("family") {
            Some(c) => Some(interned(&c.string()?)),
            None => Some("monospace"),
        };
        let px = match p.get_mut("font_size") {
            Some(c) => self.expr(c)?,
            None => Expr::K(13.0),
        };
        let line_height = match p.get_mut("line_height") {
            Some(c) => self.constant(c, "a terminal's `line_height:`")?,
            None => 1.25,
        };
        // Ink, paper and the sixteen ANSI colours, in that order.
        let mut colors = Vec::new();
        if let Some(c) = p.get_mut("colors") {
            loop {
                colors.push(self.color(c)?);
                if !c.sym(",") {
                    break;
                }
            }
            c.expect_end()?;
            if colors.len() > 18 {
                return c.error("`colors:` are ink, paper and the sixteen ANSI ones: 18 at most");
            }
        }
        let cursor = match p.get_mut("cursor") {
            Some(c) => Some(self.color(c)?),
            None => None,
        };
        let selection = match p.get_mut("selection") {
            Some(c) => Some(self.color(c)?),
            None => None,
        };
        let mut command = Vec::new();
        if let Some(c) = p.get_mut("command") {
            loop {
                command.push(c.string()?);
                if !c.sym(",") {
                    break;
                }
            }
            c.expect_end()?;
        }
        let folder = match p.get_mut("folder") {
            Some(c) => Some(match c.peek() {
                Some(TokenKind::Str(_)) => Content::Literal(c.string()?),
                _ => {
                    let t = self.global(&c.id("a text, or a folder between quotes")?);
                    match self.texts.get(&t) {
                        Some(k) => Content::Live(*k),
                        None => return self.unknown(c, "no text", &t, self.texts.keys().collect()),
                    }
                }
            }),
            None => None,
        };
        // What its shell's environment has besides the program's: a text of
        // `KEY=VALUE` lines, read when it starts.
        let env = match p.get_mut("env") {
            Some(c) => {
                let t = self.global(&c.id("a text of KEY=VALUE lines")?);
                match self.texts.get(&t) {
                    Some(k) => Some(Content::Live(*k)),
                    None => return self.unknown(c, "no text", &t, self.texts.keys().collect()),
                }
            }
            None => None,
        };
        let run = match p.get_mut("run") {
            Some(c) => self.expr(c)?,
            None => Expr::K(1.0),
        };
        let alpha = match p.get_mut("opacity") {
            Some(c) => self.expr(c)?,
            None => Expr::K(1.0),
        };
        let shown = match p.get_mut("show") {
            Some(c) => Some(self.expr(c)?.clamp(0.0, 1.0)),
            None => None,
        };
        let alpha = match &shown {
            Some(v) => alpha * v.clone(),
            None => alpha,
        };
        let label = match p.get_mut("label") {
            Some(c) => Some(self.label(c)?),
            None => None,
        };
        let reach = match p.get_mut("agent") {
            Some(c) => reach(c)?,
            None => Reach::Any,
        };
        // Its zone: pressing it gives it the keys; over it the cursor is the typing one.
        let zone = self.declare_zone(&local);
        self.candidates.push(Candidate {
            name: zone.clone(),
            at: self.e.instrs.len(),
            zblock: self.zblock,
            shape: Shape::Rect { center: (x.clone() + w.clone() * 0.5, y.clone() + h.clone() * 0.5), half_size: (w.clone() * 0.5, h.clone() * 0.5), radius: 0.0.into() },
            active: None, visible: Some(alpha.clone()), under: self.under.clone(), forced: true, cursor: Cursor::Text, cursor_when: None, carries: None, carry_icon: None, label, reach, within: None, told: None, handle: None, yields: None,
        });
        self.e.paint(Instr::Terminal(Box::new(TerminalSpec { name: interned(&name), zone: interned(&zone), target: (x, y, w, h), alpha, family, px, line_height, colors, cursor, selection, run, command, folder, env })));
        Ok(())
    }

    fn window(&mut self, n: &Node) -> R<()> {
        let mut c = Cur::new(&n.head[1..], n.line, n.col);
        let local = c.id("which window: `win.$i`")?;
        c.expect_end()?;
        let name = self.global(&local);
        let Some(nest) = self.e.nest.clone() else {
            return Err(CompileError::at(n.line, n.col, "there are no windows here: declare them first, `windows win max 6`"));
        };
        let slot = name.strip_prefix(&format!("{}.", nest.name)).and_then(|k| k.parse::<usize>().ok());
        let Some(slot) = slot.filter(|k| *k < nest.max) else {
            return Err(CompileError::at(n.line, n.col, format!("'{name}' is not one of the windows: they are {0}.0 to {0}.{1}", nest.name, nest.max - 1)));
        };
        let mut p = self.properties(n, vocab::properties("window"))?;
        let in_slot = std::mem::take(&mut self.in_slot);
        let missing = |q: &str| CompileError::at(n.line, n.col, format!("this window is missing '{q}'"));
        let (x, y) = match p.get_mut("at") {
            Some(c) => self.point(c)?,
            None if in_slot => (0.0.into(), 0.0.into()),
            None => return Err(missing("at")),
        };
        let (w, h) = self.point(p.get_mut("size").ok_or_else(|| missing("size"))?)?;
        let ask = match p.get_mut("ask") {
            Some(c) => self.point(c)?,
            None => (w.clone(), h.clone()),
        };
        self.last_size = Some((w.clone(), h.clone()));
        let alpha = match p.get_mut("opacity") {
            Some(c) => self.expr(c)?,
            None => Expr::K(1.0),
        };
        let alpha = match p.get_mut("show") {
            Some(c) => alpha * self.expr(c)?.clamp(0.0, 1.0),
            None => alpha,
        };
        // Its zone: where it is drawn, and only while it can be seen. Not for a
        // picture of it (`ask: -1, -1`, a preview): the mouse over the picture
        // is not over the window.
        let picture = matches!((&ask.0, &ask.1), (Expr::K(a), Expr::K(b)) if *a < 0.0 && *b < 0.0);
        if picture {
            self.e.paint(Instr::Window { slot, target: (x, y, w, h), alpha, ask });
            return Ok(());
        }
        let zone = self.declare_zone(&local);
        self.candidates.push(Candidate {
            name: zone,
            at: self.e.instrs.len(),
            zblock: self.zblock,
            shape: Shape::Rect { center: (x.clone() + w.clone() * 0.5, y.clone() + h.clone() * 0.5), half_size: (w.clone() * 0.5, h.clone() * 0.5), radius: 0.0.into() },
            active: None,
            visible: Some(alpha.clone()),
            under: self.under.clone(),
            forced: true,
            cursor: Cursor::Normal,
            cursor_when: None,
            carries: None,
            carry_icon: None,
            label: None,
            reach: Reach::Any,
            within: None,
            told: None,
            handle: None,
            yields: None,
        });
        self.e.paint(Instr::Window { slot, target: (x, y, w, h), alpha, ask });
        Ok(())
    }

    fn model(&mut self, n: &'a Node, c: &mut Cur) -> R<()> {
        let name = self.declare(&c.id("a name for the model")?);
        let capacity = if c.word("max") { c.num()? as usize } else { 16 };
        c.expect_end()?;
        let fields = self.fields_of(n, capacity)?;
        let model = Model { name: name.clone(), capacity, fields };
        let mut how_many = 0;
        self.declare_rows(&name, &model, &mut how_many).map_err(|m| CompileError::at(n.line, n.col, m))?;
        self.e.models.push(model);
        Ok(())
    }

    /// The fields of a model, or of a list inside a model.
    fn fields_of(&mut self, n: &'a Node, capacity: usize) -> R<Vec<Field>> {
        if !(1..=1024).contains(&capacity) {
            return Err(CompileError::at(n.line, n.col, "a list holds between 1 and 1024 records: each one unfolds when loading"));
        }
        let mut fields: Vec<Field> = Vec::new();
        let mut recursive: Option<(String, usize, usize)> = None;
        for e in n.body.as_deref().unwrap_or(&[]) {
            let field = match e {
                // `list items max 8 { label: text }`: records inside the record.
                Entry::Node(x) => {
                    let mut c = Cur::new(&x.head, x.line, x.col);
                    c.one_of(vocab::MODEL_TYPES, "what a model holds besides fields")?;
                    let name = c.id("a name for the list")?;
                    let capacity = if c.word("max") { c.num()? as usize } else { 8 };
                    // `list children max 6 depth 3`, without a block: records like the outer one, one inside
                    // another down to that depth. A tree: the menu of an application, with its submenus.
                    if c.word("depth") {
                        let depth = c.num()? as usize;
                        c.expect_end()?;
                        if x.body.is_some() {
                            return Err(CompileError::at(x.line, x.col, "a list with `depth` carries no block: its records are like the outer one"));
                        }
                        if !(1..=6).contains(&depth) || !(1..=256).contains(&capacity) {
                            return Err(CompileError::at(x.line, x.col, "`depth` goes from 1 to 6, and `max` from 1 to 256: every level multiplies the records that unfold"));
                        }
                        if recursive.is_some() {
                            return Err(CompileError::at(x.line, x.col, "a record has a single list with `depth`"));
                        }
                        recursive = Some((name, capacity, depth));
                        continue;
                    }
                    c.expect_end()?;
                    let inside = self.fields_of(x, capacity)?;
                    Field { name: name.clone(), kind: FieldType::List(Box::new(Model { name, capacity, fields: inside })), fallback: FieldValue::Number(0.0) }
                }
                Entry::Prop { name, value, line, col } => {
                    let mut c = Cur::new(value, *line, *col);
                    let (kind, fallback) = if let Some(names) = self.enumeration(&mut c)? {
                        let v = if c.sym("=") {
                            let which = c.one_of(&names.iter().map(String::as_str).collect::<Vec<_>>(), "a value of this field")?;
                            names.iter().position(|x| *x == which).unwrap() as f32
                        } else { 0.0 };
                        (FieldType::Enum(names), FieldValue::Number(v))
                    } else {
                        let or_enum_hint = |mut f: CompileError| { f.message.push_str(" Or an enum: `low | normal | critical`."); f };
                        match c.one_of(vocab::TYPES, "the type of a field").map_err(or_enum_hint)?.as_str() {
                            "text" => (FieldType::Text, FieldValue::Text(if c.sym("=") { c.string()? } else { String::new() })),
                            "number" => (FieldType::Number, FieldValue::Number(if c.sym("=") { if c.sym("-") { -c.num()? } else { c.num()? } } else { 0.0 })),
                            "bool" => (FieldType::Bool, FieldValue::Number(if !c.sym("=") || c.word("false") { 0.0 } else if c.word("true") { 1.0 } else { return c.error("a bool is true or false") })),
                            // `icon: image 24, 24`: the name of an icon or a path, and the image it says.
                            "image" => {
                                let w = c.num()?;
                                c.expect_sym(",")?;
                                (FieldType::Image(w as u32, c.num()? as u32), FieldValue::Text(if c.sym("=") { c.string()? } else { String::new() }))
                            }
                            _ => unreachable!(),
                        }
                    };
                    c.expect_end()?;
                    Field { name: name.clone(), kind, fallback }
                }
            };
            let (l, col) = match e { Entry::Node(x) => (x.line, x.col), Entry::Prop { line, col, .. } => (*line, *col) };
            if ["index", "count", "total"].contains(&field.name.as_str()) {
                return Err(CompileError::at(l, col, format!("`{}` already exists: the language provides it", field.name)));
            }
            if fields.iter().any(|k| k.name == field.name) {
                return Err(CompileError::at(l, col, format!("field '{}' is there twice", field.name)));
            }
            fields.push(field);
        }
        if fields.is_empty() {
            return Err(CompileError::at(n.line, n.col, "this list is missing its fields: `label: text`"));
        }
        // The list that contains itself unrolls from the inside out: the last level
        // no longer has children; each of the ones above, a list of the ones below.
        if let Some((name, capacity, depth)) = recursive {
            if fields.iter().any(|k| k.name == name) {
                return Err(CompileError::at(n.line, n.col, format!("field '{name}' is there twice")));
            }
            let mut level = fields.clone();
            for _ in 0..depth {
                let mut above = fields.clone();
                above.push(Field { name: name.clone(), kind: FieldType::List(Box::new(Model { name: name.clone(), capacity, fields: level })), fallback: FieldValue::Number(0.0) });
                level = above;
            }
            fields = level;
        }
        Ok(fields)
    }

    /// Inside, each field of each record is a text or a fact with a name:
    /// `rows.3.label`, and if there are lists inside, `rows.3.items.0.label`.
    fn declare_rows(&mut self, prefix: &str, m: &Model, how_many: &mut usize) -> Result<(), String> {
        for k in 0..m.capacity {
            *how_many += 1;
            if *how_many > 4096 {
                return Err("this model unfolds more than 4096 records across its lists: lower some `max`".into());
            }
            for field in &m.fields {
                let full_name = format!("{prefix}.{k}.{}", field.name);
                if let FieldType::List(inside) = &field.kind {
                    self.declare_rows(&full_name, inside, how_many)?;
                    continue;
                }
                match &field.fallback {
                    FieldValue::Text(t) => {
                        let id = self.e.live_text(interned(&full_name), t);
                        if let FieldType::Image(w, h) = field.kind {
                            let image = self.e.image(ImageSource::Live(id), w, h);
                            self.images.insert(full_name.clone(), image);
                        }
                        self.texts.insert(full_name, id);
                    }
                    FieldValue::Number(v) => {
                        let id = self.e.fact(interned(&full_name), *v);
                        match &field.kind {
                            FieldType::Bool => self.e.types.push((full_name.clone(), FactType::Bool)),
                            FieldType::Enum(n) => self.e.types.push((full_name.clone(), FactType::Enum(n.clone()))),
                            _ => {}
                        }
                        self.facts.insert(full_name, id);
                    }
                }
            }
        }
        for part in ["count", "total"] {
            let full_name = format!("{prefix}.{part}");
            let id = self.e.fact(interned(&full_name), 0.0);
            self.facts.insert(full_name, id);
        }
        self.models.insert(prefix.to_owned(), m.capacity);
        Ok(())
    }

    /// If that name is a record of a model: what it is really called, and which one it is.
    fn row(&self, n: &str) -> Option<(String, usize)> {
        let g = self.global(n);
        let (model, k) = g.rsplit_once('.')?;
        let k: usize = k.parse().ok()?;
        (k < *self.models.get(model)?).then_some((g, k))
    }

    /// `for r in rows`: the name of the record, the model and how many fit.
    fn for_head(&mut self, c: &mut Cur) -> R<(String, String, usize, Expr)> {
        let var = c.id("a name for the record")?;
        c.expect_word("in")?;
        let model = self.global(&c.id("the name of a model")?);
        // `for r in rows from first`: record 0 of what is unrolled is the `first` of the
        // real list, so `r.index` counts from there. It is what lets a
        // list of five thousand fit in twelve copies.
        let from = if c.word("from") { self.expr(c)? } else { Expr::K(0.0) };
        c.expect_end()?;
        match self.models.get(&model) {
            Some(capacity) => Ok((var, model, *capacity, from)),
            None => self.unknown(c, "no model", &model, self.models.keys().collect()),
        }
    }

    /// Inside iteration `k`, `r.label` is `rows.k.label`, `r.index` is `k`, and
    /// everything drawn only exists if the list reaches that far.
    fn for_iteration(&self, var: &str, model: &str, k: usize, from: &Expr) -> Scope {
        let mut env = Scope { suffix: format!("#{var}{k}"), ..Default::default() };
        env.alias.insert(var.to_owned(), format!("{model}.{k}"));
        env.with_parts.insert(var.to_owned());
        env.exprs.insert(format!("{var}.index"), from.clone() + Expr::K(k as f32));
        env.visible = Some(self.facts[&format!("{model}.count")].e().gt(Expr::K(k as f32 + 0.5)));
        env
    }

    /// A loose `for`, outside a stack: each iteration is painted where it says, if it exists.
    fn for_loop(&mut self, n: &'a Node, c: &mut Cur) -> R<()> {
        let (var, model, capacity, from) = self.for_head(c)?;
        for k in 0..capacity {
            let mark = self.rules.len();
            let env = self.for_iteration(&var, &model, k, &from);
            let is_present = env.visible.clone().unwrap();
            self.scopes.push(Rc::new(env));
            let from = self.candidates.len();
            self.e.paint(Instr::Opacity(Some(is_present.clone())));
            self.group(n.body.as_deref().unwrap_or(&[]).iter());
            self.e.paint(Instr::Opacity(None));
            for c in &mut self.candidates[from..] {
                c.visible = Some(match c.visible.take() { Some(v) => v * is_present.clone(), None => is_present.clone() });
            }
            self.close_scope(mark);
        }
        Ok(())
    }

    // ── zones ───────────────────────────────────────────────────

    /// Of the named shapes, the zones are those some rule names, those
    /// declared with `zone` and those that carry `active`. The others had a name
    /// only to read better, and have no reason to stop the click. They are created in the
    /// order in which they were written: the one further down in the file ends up on top.
    fn materialize_zones(&mut self) {
        // Each rule names from its scope: inside a copy of a component,
        // `hit` is that copy's zone and not another's.
        let mut named = std::collections::HashSet::new();
        let rules = std::mem::take(&mut self.rules);
        // (This looks at every word of each rule, `on` and `press` too: it is not reading anything.)
        self.unwatched.set(true);
        for (n, scopes) in &rules {
            self.scopes = scopes.clone();
            let feedback = n.body.as_deref().unwrap_or(&[]).iter().filter_map(|e| match e {
                Entry::Node(n) if matches!(n.head.first().map(|t| &t.kind), Some(TokenKind::Id(s)) if s == "ripple") => n.head.get(1),
                _ => None,
            });
            for f in n.head.iter().chain(feedback) {
                if let TokenKind::Id(s) = &f.kind {
                    let g = self.global(s);
                    if let Some(m) = self.screen_mark() {
                        named.insert(format!("{g}{m}"));
                    }
                    named.insert(g);
                }
            }
        }
        self.unwatched.set(false);
        self.scopes.clear();
        self.rules = rules;
        for k in std::mem::take(&mut self.candidates) {
            if k.forced || k.active.is_some() || named.contains(&k.name) {
                // A zone of something that is not there does not stop anyone's click.
                let active = match (k.active, k.visible) {
                    (Some(a), Some(v)) => a * v,
                    (a, v) => a.or(v).unwrap_or(Expr::K(1.0)),
                };
                let z = self.e.zone_under(interned(&k.name), k.shape, active, k.under);
                self.e.zones[z.0 as usize].cursor = k.cursor;
                self.e.zones[z.0 as usize].cursor_when = k.cursor_when;
                self.e.zones[z.0 as usize].at = k.at;
                self.e.zones[z.0 as usize].zblock = k.zblock;
                self.e.zones[z.0 as usize].carries = k.carries;
                self.e.zones[z.0 as usize].carry_icon = k.carry_icon;
                self.e.zones[z.0 as usize].label = k.label;
                self.e.zones[z.0 as usize].reach = k.reach;
                self.e.zones[z.0 as usize].within = k.within.and_then(|w| self.zones.get(&w).copied());
                self.e.zones[z.0 as usize].told = k.told;
                self.e.zones[z.0 as usize].handle = k.handle;
                self.e.zones[z.0 as usize].yields = k.yields;
                self.zones.insert(k.name, z);
            }
        }
        // The zones' own springs, now that the zones exist.
        for (name, hover, pressed) in std::mem::take(&mut self.zone_springs) {
            if let Some(z) = self.zones.get(&name) {
                self.e.zone_springs.push((*z, hover, pressed));
            }
        }
        // And the rules of the stacks that scroll, now that their zones exist.
        let wheel = self.facts["wheel"].e();
        let (dx, dy) = (self.facts["drag.dx"].e(), self.facts["drag.dy"].e());
        for (name, prop, until, step, spring, grab, pinned) in std::mem::take(&mut self.scrolls) {
            let Some(zone) = self.zones.get(&name).copied() else { continue };
            self.e.zones[zone.0 as usize].scrolls = Some(prop);
            // `stick: end`: whether it is at its end (two pixels' leeway), said by the
            // wheel and the drag that move it; and while it is, it follows the end.
            let at_end = |to: &Expr| to.clone().gt(until.clone() - Expr::K(2.0));
            let a = (prop.e() - wheel.clone() * step).max(Expr::K(0.0)).min(until.clone());
            let mut on_wheel = vec![Effect::Animate(Transition { prop, to: a.clone(), spring, delay: Duration::ZERO })];
            if let Some(f) = pinned {
                on_wheel.push(Effect::Fact(f, at_end(&a)));
            }
            self.e.rule(Trigger::Wheel(zone), on_wheel);
            // Dragging it: on press, where it was is noted, and while it moves it goes from there.
            // The stack's zone is under those of its children, and the drag reaches it all the same.
            let amount = if self.row_scrolls.contains(&name) { dx.clone() } else { dy.clone() };
            self.e.rule(Trigger::Press(zone), vec![Effect::Fact(grab, prop.e())]);
            let a = (grab.e() - amount).max(Expr::K(0.0)).min(until.clone());
            let mut on_drag = vec![Effect::Animate(Transition { prop, to: a.clone(), spring: Spring::QUICK, delay: Duration::ZERO })];
            if let Some(f) = pinned {
                on_drag.push(Effect::Fact(f, at_end(&a)));
            }
            self.e.rule(Trigger::Drag(zone), on_drag);
            if let Some(f) = pinned {
                // What is inside grows (or the window shrinks): there it goes, if it was at the end.
                self.e.rules.push(Rule { when: Trigger::Change(until.clone()), guard: Some(f.e().gt(Expr::K(0.5))), effects: vec![Effect::Animate(Transition { prop, to: until.clone(), spring, delay: Duration::ZERO })] });
            }
        }
    }

    // ── rules ───────────────────────────────────────────────────

    fn rule(&mut self, n: &Node, word: &str, c: &mut Cur) -> R<()> {
        let mut payload_name: Option<String> = None;
        let r = self.rule_inner(n, word, c, &mut payload_name);
        if payload_name.is_some() {
            self.scopes.pop();
        }
        r
    }

    /// `x[i] = v` in a rule: the fact `x.3` when the place is known when
    /// loading, otherwise whichever of `x.0`, `x.1`... the place says when it fires.
    fn indexed_write(&self, base: &str, c: &mut Cur) -> R<Effect> {
        c.expect_sym("[")?;
        let i = self.expr(c)?;
        c.expect_sym("]")?;
        let rest = match c.peek() {
            Some(TokenKind::Id(r)) if r.starts_with('.') => {
                c.i += 1;
                r.clone()
            }
            _ => String::new(),
        };
        c.expect_sym("=")?;
        let v = self.expr(c)?;
        let fact = |k: usize| self.facts.get(&self.global_quiet(&format!("{base}.{k}{rest}"))).copied();
        if let Expr::K(k) = i {
            return match (k >= 0.0).then(|| fact(k.round() as usize)).flatten() {
                Some(h) => Ok(Effect::Fact(h, v)),
                None => c.error(format!("there is no fact '{base}.{}{rest}' to write", k.round())),
            };
        }
        let all: Vec<FactId> = (0..4096).map_while(fact).collect();
        if all.is_empty() {
            return c.error(format!("'{base}[...]{rest}' writes a fact by its place: there is no '{base}.0{rest}'. Declare them, `repeat k in 0..16 {{ fact {base}.$k{rest} = 0 }}`"));
        }
        Ok(Effect::FactAt(all, i, v))
    }

    /// A rule's effects, in order. A `repeat` among them is unfolded in
    /// place: `repeat k in 0..16 { tre.w.$k = 0 }` is sixteen effects of the
    /// same rule, which each see what the ones before them set.
    fn effects(&mut self, entries: &[Entry], effects: &mut Vec<Effect>) -> R<()> {
        for e in entries {
            if let Entry::Node(x) = e
                && matches!(x.head.first().map(|t| &t.kind), Some(TokenKind::Id(w)) if w == "repeat")
            {
                let mut c = Cur::new(&x.head[1..], x.line, x.col);
                let (var, from, until) = self.repeat_head(&mut c)?;
                for v in from..until {
                    self.open_iteration(&var, v);
                    let r = self.effects(x.body.as_deref().unwrap_or(&[]), effects);
                    self.scopes.pop();
                    r?;
                }
                continue;
            }
            match e {
                Entry::Prop { name, value, line, col } => effects.push(Effect::Animate(self.transition(name, value, *line, *col)?)),
                Entry::Node(x) => {
                    let mut c = Cur::new(&x.head, x.line, x.col);
                    let p = c.id("an effect")?;
                    effects.push(match if vocab::EFFECTS.contains(&p.as_str()) { p.as_str() } else { "" } {
                        "ripple" => Effect::Ripple(self.zone(&mut c)?),
                        "toggle" => Effect::Toggle(self.fact(&mut c)?),
                        "blur" => Effect::FocusField(None),
                        // `quit`: the scene ends, as closing its window would (a terminal whose last shell ended).
                        "quit" => Effect::Quit,
                        "focus" if self.names_window(&c) => Effect::Window(WindowAction::Focus, self.which_window(&mut c)?),
                        // `close workspace 2 on 0`: that workspace gone, its windows to the one beside.
                        "close" if c.word("workspace") => {
                            if self.e.nest.is_none() {
                                return c.error("`close workspace` closes one of the scene's windows' workspaces: declare them first, `windows win max 16`");
                            }
                            let n = self.expr(&mut c)?;
                            let on = if c.word("on") { Some(self.expr(&mut c)?) } else { None };
                            Effect::WorkspaceClose(n, on)
                        }
                        "close" => Effect::Window(WindowAction::Close, self.which_window(&mut c)?),
                        "promote" => Effect::Window(WindowAction::Promote, self.which_window(&mut c)?),
                        "fullscreen" => Effect::Window(WindowAction::Fullscreen, self.which_window(&mut c)?),
                        "minimize" => Effect::Window(WindowAction::Minimize, self.which_window(&mut c)?),
                        "restore" => Effect::Window(WindowAction::Restore, self.which_window(&mut c)?),
                        "float" => Effect::Window(WindowAction::Float, self.which_window(&mut c)?),
                        "tile" => Effect::Window(WindowAction::Tile, self.which_window(&mut c)?),
                        // `send win(win.focus) to 1`: to that monitor's copy of the scene;
                        // `send win(win.focus) to workspace 2`: to that workspace.
                        "send" => {
                            // `send workspace 2 on 0 to 1`: that monitor's whole
                            // workspace, with its windows, to the other monitor.
                            if c.word("workspace") {
                                if self.e.nest.is_none() {
                                    return c.error("`send workspace` moves one of the scene's windows' workspaces: declare them first, `windows win max 16`");
                                }
                                let n = self.expr(&mut c)?;
                                let on = if c.word("on") { Some(self.expr(&mut c)?) } else { None };
                                c.expect_word("to")?;
                                effects.push(Effect::WorkspaceTo(n, on, self.expr(&mut c)?));
                                c.expect_end()?;
                                continue;
                            }
                            let which = self.which_window(&mut c)?;
                            // `send win(k) aside`: on no workspace (`win.$i.aside`).
                            if c.word("aside") {
                                if self.e.nest.is_none() {
                                    return c.error("`send win(…) aside` puts one of the scene's windows on no workspace: declare them first, `windows win max 16`");
                                }
                                effects.push(Effect::WindowAside(which));
                                c.expect_end()?;
                                continue;
                            }
                            c.expect_word("to")?;
                            if c.word("workspace") {
                                // `… to workspace 2 on 1`: that workspace of another monitor.
                                let n = self.expr(&mut c)?;
                                let on = if c.word("on") { Some(self.expr(&mut c)?) } else { None };
                                Effect::WindowToWorkspace(which, n, on)
                            } else {
                                Effect::WindowTo(which, self.expr(&mut c)?)
                            }
                        }
                        // `workspace 3`: shown on the monitor the pointer is on; `workspace 3 on 1`, on that one.
                        "workspace" => {
                            if self.e.nest.is_none() {
                                return c.error("`workspace` shows one of the scene's windows' workspaces: declare them first, `windows win max 16`");
                            }
                            let n = self.expr(&mut c)?;
                            let on = if c.word("on") { Some(self.expr(&mut c)?) } else { None };
                            Effect::Workspace(n, on)
                        }
                        // `pick win(x)`, `pick screen 1`, `pick none`: what the compositor
                        // asked to be chosen (`win.picking`).
                        "pick" => {
                            if self.e.nest.is_none() {
                                return c.error("`pick` answers what the scene's compositor asks: declare its windows first, `windows win max 16`");
                            }
                            if c.word("none") {
                                Effect::Pick(None)
                            } else if c.word("screen") {
                                Effect::Pick(Some((false, self.expr(&mut c)?)))
                            } else {
                                Effect::Pick(Some((true, self.which_window(&mut c)?)))
                            }
                        }
                        // `swap win(a) with win(b)`: they change places.
                        "swap" => {
                            let a = self.which_window(&mut c)?;
                            c.expect_word("with")?;
                            Effect::WindowSwap(a, self.which_window(&mut c)?)
                        }
                        // `accept win(i)`: it takes its place (see `placement manual`).
                        "accept" => {
                            if !self.e.nest.as_ref().is_some_and(|n| n.manual) {
                                return c.error("`accept` takes a window the scene placed itself: declare its windows with `placement manual`, `windows win max 16 placement manual`");
                            }
                            Effect::Accept(self.which_window(&mut c)?)
                        }
                        // `dock 2 on 0`: that item of that monitor's dock.
                        "dock" => {
                            if self.e.nest.is_none() {
                                return c.error("`dock` opens an item of the scene's windows' dock: declare them first, `windows win max 16`");
                            }
                            let k = self.expr(&mut c)?;
                            let on = if c.word("on") { Some(self.expr(&mut c)?) } else { None };
                            // `… with drop`, `… pin`, `… unpin`, `… close`, `… new`, `… hide`.
                            let what = if c.word("with") {
                                c.expect_word("drop")?;
                                DockAction::OpenDrop
                            } else if c.word("pin") {
                                DockAction::Pin
                            } else if c.word("unpin") {
                                DockAction::Unpin
                            } else if c.word("close") {
                                DockAction::Close
                            } else if c.word("new") {
                                DockAction::New
                            } else if c.word("hide") {
                                DockAction::Hide
                            } else {
                                DockAction::Go
                            };
                            Effect::Dock(k, on, what)
                        }
                        "launch" => {
                            if self.e.nest.is_none() {
                                return c.error("`launch` opens a program among the scene's windows: declare them first, `windows win max 6`");
                            }
                            // A command with holes, like any text: filled in when the rule fires.
                            let at = c.i;
                            let s = c.string()?;
                            Effect::Launch(self.content_of_one(&s, &c.tokens[at])?)
                        }
                        "focus" => {
                            let n = self.global(&c.id("the name of the input")?);
                            match self.texts.get(&n) {
                                Some(t) => Effect::FocusField(Some(*t)),
                                // (Not a text: a terminal's name, `focus term`, declared before or after.)
                                None => Effect::FocusTerminal(interned(&n)),
                            }
                        }
                        // `emit opened` or, with a payload, `emit opened(i)`.
                        "emit" => {
                            let s = self.signal(&mut c)?;
                            let payload = if c.sym("(") {
                                let e = self.expr(&mut c)?;
                                c.expect_sym(")")?;
                                Some(e)
                            } else {
                                None
                            };
                            Effect::Signal(s, payload)
                        }
                        "impulse" => Effect::Impulse(self.prop(&mut c)?, self.expr(&mut c)?),
                        "play" => {
                            let g = self.global(&c.id("the name of a gesture")?);
                            match self.gestures.get(&g) {
                                Some(id) => Effect::Gesture(*id),
                                None => return self.unknown(&c, "no gesture", &g, self.gestures.keys().collect()),
                            }
                        }
                        other if vocab::EFFECTS.contains(&other) => unreachable!("'{other}' is in the vocabulary, but `rule` cannot carry it out"),
                        // `label = ""` or `label = draft`: a live text, from words or another.
                        "" if self.texts.contains_key(&self.global(&p)) && matches!(c.peek(), Some(TokenKind::Sym(s)) if *s == "=") => {
                            let t = self.texts[&self.global(&p)];
                            c.expect_sym("=")?;
                            let value = match c.peek() {
                                Some(TokenKind::Str(_)) => crate::scene::TextValue::Words(c.string()?),
                                _ => {
                                    let other = self.global(&c.id("words in quotes or another text")?);
                                    match self.texts.get(&other) {
                                        Some(o) => crate::scene::TextValue::Of(*o),
                                        None => return self.unknown(&c, "no text", &other, self.texts.keys().collect()),
                                    }
                                }
                            };
                            Effect::Text(t, value)
                        }
                        // `tre.x[tre.nw] = tre.ax`: one of a numbered family of facts.
                        "" if matches!(c.peek(), Some(TokenKind::Sym("["))) => self.indexed_write(&p, &mut c)?,
                        _ => {
                            // `open = true`
                            c.i -= 1;
                            let from = c.i;
                            let h = self.fact(&mut c)?;
                            let enum_fact = self.bare_enum(&c, from);
                            c.expect_sym("=")?;
                            // `mode = critical`: the value, from that fact's list.
                            let value = match (&enum_fact, c.peek()) {
                                (Some((fact, names)), Some(TokenKind::Id(v))) if c.tokens.len() == c.i + 1 && self.is_enum_value(v) => match names.iter().position(|n| n == v) {
                                    Some(k) => {
                                        c.i += 1;
                                        Expr::K(k as f32)
                                    }
                                    None => return c.error(format!("'{v}' is not a value of '{fact}': it can be {}", join_or(&names.iter().map(String::as_str).collect::<Vec<_>>()))),
                                },
                                // An expression, which is evaluated when firing: `level = clamp(local.x / 64, 0, 1)`.
                                _ => self.expr(&mut c)?,
                            };
                            Effect::Fact(h, value)
                        }
                    });
                    c.expect_end()?;
                }
            }
        }
        Ok(())
    }

    fn rule_inner(&mut self, n: &Node, word: &str, c: &mut Cur, payload_name: &mut Option<String>) -> R<()> {
        let while_cond = |o: &Compiler, c: &mut Cur| -> R<Expr> { if c.word("while") { o.expr(c) } else { Ok(Expr::K(1.0)) } };
        let when = if word == "every" {
            let a = c.dur()?.as_secs_f32();
            let b = if c.sym("..") { c.dur()?.as_secs_f32() } else { a };
            Trigger::Every { between: (a, b), during: while_cond(self, c)? }
        } else {
            let what = c.id("what has to happen: press, release, scroll, drag, hold, key, submit, focus, blur, drop, enter, leave, hover, away, idle, or an event")?;
            // Only what the vocabulary says is a trigger; anything else is the name of a signal.
            match if vocab::TRIGGERS.contains(&what.as_str()) { what.as_str() } else { "" } {
                // `on press orb`, or with another button: `on press right orb`.
                "press" => {
                    if c.button("right") {
                        self.e.surface_mut().right_click_quits = false;
                        Trigger::PressWith(self.zone(c)?, 1)
                    } else if c.button("middle") {
                        Trigger::PressWith(self.zone(c)?, 2)
                    } else if c.button("back") {
                        // The thumb buttons: the pointer already hands BTN_SIDE and BTN_EXTRA
                        // over as 3 and 4 (see the match in platform/wayland.rs), there was
                        // just no way to ask for them.
                        Trigger::PressWith(self.zone(c)?, 3)
                    } else if c.button("forward") {
                        Trigger::PressWith(self.zone(c)?, 4)
                    } else {
                        Trigger::Press(self.zone(c)?)
                    }
                }
                "release" => Trigger::Release(self.zone(c)?),
                "scroll" => Trigger::Wheel(self.zone(c)?),
                "drag" => Trigger::Drag(self.zone(c)?),
                "hold" => {
                    let zone = self.zone(c)?;
                    c.expect_word("for")?;
                    Trigger::Hold { zone, duration: c.dur()? }
                }
                // `on key Escape`, `on key Ctrl+k`: the name can carry a `+`.
                "key" => {
                    let mut name = c.id("the name of a key: Escape, Return, a, Ctrl+k…")?;
                    while c.sym("+") {
                        name = format!("{name}+{}", c.id("the key")?);
                    }
                    Trigger::Key(key_combo(&name))
                }
                "submit" => {
                    let n = self.global(&c.id("the name of the input")?);
                    match self.texts.get(&n) {
                        Some(t) => Trigger::Submit(*t),
                        None => return self.unknown(c, "no text", &n, self.texts.keys().collect()),
                    }
                }
                "focus" => Trigger::FocusGained,
                "blur" => Trigger::FocusLost,
                // `on change floor(list.scroll / 34) { … }`: when that expression changes.
                "change" => Trigger::Change(self.expr(c)?),
                // `on still audio.volume for 1.1s { … }`: when it has stayed the same for that while.
                "still" => {
                    let what = self.expr(c)?;
                    c.expect_word("for")?;
                    Trigger::Still { value: what, duration: c.dur()? }
                }
                "drop" => Trigger::Receive(self.zone(c)?),
                "carry" => Trigger::Carry(self.zone(c)?),
                "enter" => Trigger::Enter(self.zone(c)?),
                "leave" => Trigger::Leave(self.zone(c)?),
                // `away button, panel for 320ms`: several zones count as one —over any of
                // them is over—, so a button and the panel it opens close together.
                "hover" | "away" => {
                    let zone = self.zone(c)?;
                    let mut also = Vec::new();
                    while c.sym(",") {
                        also.push(self.zone(c)?);
                    }
                    c.expect_word("for")?;
                    let duration = c.dur()?;
                    if what == "hover" { Trigger::Above { zone, also, duration } } else { Trigger::Away { zone, also, duration } }
                }
                "idle" => {
                    c.expect_word("for")?;
                    let duration = c.dur()?;
                    Trigger::Idle { duration, during: while_cond(self, c)? }
                }
                other if vocab::TRIGGERS.contains(&other) => unreachable!("'{other}' is in the vocabulary, but `rule` does not handle it"),
                _ => {
                    c.i -= 1;
                    let s = self.signal(c)?;
                    // `on chosen(v) { … }`: inside, `v` is the value it arrived with.
                    if c.sym("(") {
                        let v = c.id("a name for the event's value: `on chosen(v) { … }`")?;
                        c.expect_sym(")")?;
                        *payload_name = Some(v);
                    }
                    Trigger::On(s)
                }
            }
        };
        // The event's value, by the name it was given, for the guard and the effects.
        if let Some(v) = payload_name.as_ref() {
            let mut here = Scope::default();
            here.exprs.insert(v.clone(), Expr::Payload);
            self.scopes.push(Rc::new(here));
        }
        // `while` works in any rule: it is checked at the moment of firing.
        // (`idle` and `every` have already taken it: in them it also decides whether the time counts.)
        let guard = if c.word("while") { Some(self.expr(c)?) } else { None };
        c.expect_end()?;
        let mut effects = Vec::new();
        self.effects(n.body.as_deref().unwrap_or(&[]), &mut effects)?;
        self.e.rule(when, effects);
        if let Some(r) = self.e.rules.last_mut() {
            r.guard = guard;
        }
        Ok(())
    }

    /// Whether what follows names one of the scene's windows: `win.3`, `win.$i`
    /// or `win(win.focus)`.
    fn names_window(&self, c: &Cur) -> bool {
        let Some(nest) = &self.e.nest else { return false };
        match c.peek() {
            Some(TokenKind::Id(w)) => w == &nest.name || w.starts_with(&format!("{}.", nest.name)),
            _ => false,
        }
    }

    /// Which window: `win.3` and `win.$i` are that slot; `win(expr)`, the one
    /// the expression says when the rule fires.
    fn which_window(&self, c: &mut Cur) -> R<Expr> {
        let Some(nest) = self.e.nest.clone() else {
            return c.error("there are no windows here: declare them first, `windows win max 6`");
        };
        let local = c.id("which window: `win.$i`, or `win(win.focus)`")?;
        if local == nest.name {
            c.expect_sym("(")?;
            let e = self.expr(c)?;
            c.expect_sym(")")?;
            return Ok(e);
        }
        let name = self.global(&local);
        match name.strip_prefix(&format!("{}.", nest.name)).and_then(|k| k.parse::<usize>().ok()).filter(|k| *k < nest.max) {
            Some(k) => Ok(Expr::K(k as f32)),
            None => c.error(format!("'{name}' is not one of the windows: they are {0}.0 to {0}.{1}, or {0}(an expression)", nest.name, nest.max - 1)),
        }
    }

    // ── what the render carries on its own ──────────────────────

    fn behavior(&mut self, word: &str, c: &mut Cur) -> R<()> {
        let comp = match word {
            // blink eyelid every 2.4s..6s for 170ms · blink lid every 5.2s for 120ms
            "blink" => {
                let prop = self.prop(c)?;
                c.expect_word("every")?;
                let a = c.dur()?.as_secs_f32();
                // Without `..`, always the same while. The period is counted from start to
                // start: "every 5.2 s" is every 5.2 s, not 5.2 s after closing.
                let b = if c.sym("..") { c.dur()?.as_secs_f32() } else { a };
                c.expect_word("for")?;
                Behavior::Blink { prop, every: (a, b), duration: c.dur()?.as_secs_f32() }
            }
            // wave breath = asleep * 1.3 at 1.7
            "wave" => {
                let prop = self.prop(c)?;
                c.expect_sym("=")?;
                let amplitude = self.expr(c)?;
                c.expect_word("at")?;
                Behavior::Wave { prop, frequency: c.num()?, amplitude }
            }
            // spin angle by 0.9
            "spin" => {
                let prop = self.prop(c)?;
                c.expect_word("by")?;
                Behavior::Advance { prop, per_second: self.expr(c)? }
            }
            // follow chip.w = label.width + 32
            "follow" => {
                let prop = self.prop(c)?;
                c.expect_sym("=")?;
                Behavior::Follow { prop, to: self.expr(c)? }
            }
            // look gaze.x, gaze.y at orb.x, orb.y reach 5, 3.2 within 140 rest 3.2, 0.6
            _ => {
                let x = self.prop(c)?;
                c.expect_sym(",")?;
                let y = self.prop(c)?;
                c.expect_word("at")?;
                let center = self.point(c)?;
                c.expect_word("reach")?;
                let rx = c.num()?;
                c.expect_sym(",")?;
                let ry = c.num()?;
                c.expect_word("within")?;
                let distance = c.num()?;
                let rest = if c.word("rest") { self.point(c)? } else { (0.0.into(), 0.0.into()) };
                Behavior::Gaze { x, y, center, reach: (rx, ry), distance, rest }
            }
        };
        c.expect_end()?;
        self.e.behaviors.push(comp);
        Ok(())
    }

    // ── gestures ────────────────────────────────────────────────

    /// `gesture nod reflex { 130ms out_quad { look.y: 4; eyes: 10 } … }`
    fn gesture(&mut self, n: &Node, word: &str, c: &mut Cur) -> R<()> {
        let name = self.declare(&c.id("a name for the gesture")?);
        let (class, while_cond) = if word == "posture" {
            c.expect_word("while")?;
            (Class::Posture, Some(self.expr(c)?))
        } else {
            let k = match c.one_of(vocab::CLASSES, "the class of a gesture")?.as_str() {
                "ambient" => Class::Ambient,
                "reflex" => Class::Reflex,
                "asked" => Class::Asked,
                "state" => Class::State,
                _ => unreachable!(),
            };
            (k, None)
        };
        c.expect_end()?;
        let mut keyframes = Vec::new();
        for e in n.body.as_deref().unwrap_or(&[]) {
            let Entry::Node(f) = e else {
                return Err(CompileError::at(n.line, n.col, "a gesture is made of frames: `130ms out_quad { eyes: 10 }`"));
            };
            let mut c = Cur::new(&f.head, f.line, f.col);
            let ms = (c.dur()?.as_secs_f32() * 1000.0) as u32;
            let mut frame = keyframe(ms, Curve::InOutSine);
            while !c.at_end() {
                let frame_words: Vec<&str> = vocab::CURVES.iter().chain(vocab::KEYFRAME_OPTIONS).copied().collect();
                let p = c.one_of(&frame_words, "what a frame carries")?;
                match p.as_str() {
                    "hold" => frame.hold = (c.dur()?.as_secs_f32() * 1000.0) as u32,
                    "emit" => frame.emit = Some(self.signal(&mut c)?),
                    "linear" => frame.curve = Curve::Linear,
                    "in_quad" => frame.curve = Curve::InQuad,
                    "out_quad" => frame.curve = Curve::OutQuad,
                    "in_cubic" => frame.curve = Curve::InCubic,
                    "out_cubic" => frame.curve = Curve::OutCubic,
                    "in_out_sine" => frame.curve = Curve::InOutSine,
                    "out_back" => frame.curve = Curve::OutBack,
                    // `bezier(0.2, 0.9, 0.3, 1.2)`: CSS's control points. The x of
                    // both have to stay between 0 and 1 —it is time, and time does
                    // not go back—; the y can overshoot, which is what a bounce is.
                    "bezier" => {
                        c.expect_sym("(")?;
                        let mut k = [0f32; 4];
                        for (i, v) in k.iter_mut().enumerate() {
                            if i > 0 {
                                c.expect_sym(",")?;
                            }
                            *v = c.num()?;
                        }
                        c.expect_sym(")")?;
                        if !(0.0..=1.0).contains(&k[0]) || !(0.0..=1.0).contains(&k[2]) {
                            return c.error("in `bezier(x1, y1, x2, y2)` the x of both points go from 0 to 1: they are time, and time does not go back");
                        }
                        frame.curve = Curve::Bezier(k[0], k[1], k[2], k[3]);
                    }
                    _ => unreachable!(),
                }
            }
            for v in f.body.as_deref().unwrap_or(&[]) {
                let Entry::Prop { name, value, line, col } = v else { continue };
                let name = &self.global(name);
                let Some(prop) = self.props.get(name).copied() else {
                    let hint = closest_match(name, self.props.keys()).map_or(String::new(), |p| format!(" Did you mean '{p}'?"));
                    return Err(CompileError::at(*line, *col, format!("there is no property called '{name}'.{hint}")));
                };
                if !self.e.pose.contains(&prop) {
                    return Err(CompileError::at(*line, *col, format!("'{name}' is not part of the pose: a gesture only leads by the hand what was declared with `pose`")));
                }
                let mut c = Cur::new(value, *line, *col);
                frame.values.push((prop, self.expr(&mut c)?));
                c.expect_end()?;
            }
            keyframes.push(frame);
        }
        if keyframes.is_empty() {
            return Err(CompileError::at(n.line, n.col, "a gesture with no frames does nothing"));
        }
        let g = self.e.gesture(interned(&name), class, keyframes);
        if let Some(m) = while_cond {
            self.e.posture(g, m);
        }
        self.gestures.insert(name, g);
        Ok(())
    }
}

/// "a, b or c"
fn join_or(list: &[&str]) -> String {
    match list {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [before @ .., last] => format!("{} or {last}", before.join(", ")),
    }
}

fn type_name(t: &str) -> &'static str {
    match t {
        "number" => "a number",
        "color" => "a colour",
        "text" => "a text",
        "record" => "a record of a model",
        "event" => "an event",
        "image" => "an image",
        "bool" => "a yes or no",
        "spring" => "a spring",
        "gesture" => "a gesture",
        _ => "something",
    }
}

/// `agent: yes | no | hidden`.
fn reach(c: &mut Cur) -> R<Reach> {
    Ok(match c.one_of(vocab::AGENT, "what an agent may do with it")?.as_str() {
        "no" => Reach::Person,
        "hidden" => Reach::Hidden,
        _ => Reach::Any,
    })
}

impl Compiler<'_> {
    /// `cursor: grab`, or one that changes: `cursor: if(held, grabbing, grab)`,
    /// where each cursor's name is its code. A name alone is read as before, so
    /// a mistyped one still gets the list of cursors.
    fn cursor(&self, c: &mut Cur) -> R<(Cursor, Option<Expr>)> {
        if matches!(c.peek(), Some(TokenKind::Id(_))) && c.i + 1 == c.tokens.len() {
            let name = c.one_of(vocab::CURSORS, "a cursor")?;
            return Ok((Cursor::from_name(&name).unwrap_or_default(), None));
        }
        self.in_cursor.set(true);
        let e = self.expr(c);
        self.in_cursor.set(false);
        let e = e?;
        c.expect_end()?;
        Ok(match e {
            Expr::K(v) => (Cursor::from_code(v.round().clamp(0.0, Cursor::LAST_CODE as f32) as u8), None),
            e => (Cursor::Normal, Some(e)),
        })
    }
}

struct ParsedShape {
    /// How much room it takes, if known: what a `row` needs to share out.
    size: Option<(Expr, Expr)>,
    shape: Shape,
    color: Option<Color>,
    opacity: Option<Expr>,
    blend: Option<Expr>,
    /// Loose, a shape can also be glass. Inside a `body`, the body says so.
    glass_spec: Option<Glass>,
}

/// For each rule, the first one of the copies that came out exactly like it
/// (see `Scene::twin_of`). The copies were compiled from the same nodes in the
/// same order, so the n-th rule of one copy is the n-th of the first; if they
/// read the same —same trigger, same guard, same effects, all with the same
/// ids— nothing in them belongs to a copy.
fn twins(rules: &[crate::scene::Rule], spans: &[crate::scene::Span]) -> Vec<usize> {
    let mut twin: Vec<usize> = (0..rules.len()).collect();
    let Some(first) = spans.first() else { return Vec::new() };
    let read: Vec<String> = first.rules.clone().map(|k| format!("{:?}", rules[k])).collect();
    for t in &spans[1..] {
        if t.rules.len() != first.rules.len() {
            continue;
        }
        for (i, k) in t.rules.clone().enumerate() {
            if format!("{:?}", rules[k]) == read[i] {
                twin[k] = first.rules.start + i;
            }
        }
    }
    twin
}

/// A key as the render names it: the modifiers always in the same order
/// —`Ctrl+Alt+Super+Shift+`— whatever order the scene wrote them in.
fn key_combo(written: &str) -> String {
    let parts: Vec<&str> = written.split('+').collect();
    let (key, mods) = parts.split_last().expect("split always gives one");
    let order = ["Ctrl", "Alt", "Super", "Shift"];
    if mods.iter().any(|m| !order.contains(m)) {
        return written.to_owned();
    }
    let mut out: String = order.iter().filter(|m| mods.contains(m)).map(|m| format!("{m}+")).collect();
    out.push_str(key);
    out
}
