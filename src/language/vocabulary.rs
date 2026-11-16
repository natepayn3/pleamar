//! The words of the language, in one single place. **These lists are the ones the
//! compiler consults to accept or reject**, and the same ones `pleamar --grammar`
//! prints: what is not here is not valid; and what is here and nobody handles
//! blows up in the tests. That way the written reference cannot fall out of
//! date without `./run-tests.sh` saying so.

/// What a statement can start with (besides the name of a component).
pub const STATEMENTS: &[&str] = &[
    "surface", "permissions", "model", "service", "spring", "prop", "pose", "fact", "event", "text", "image", "figure", "font", "shader", "blur", "lens", "particles", "measure", "let", "zone",
    "body", "ellipse", "box", "arc", "line", "path", "input", "clip", "group", "popup",
    "component", "children", "repeat", "for", "row", "column", "grid", "pages", "space", "between",
    "layer", "on", "every", "blink", "wave", "spin", "follow", "look", "gesture", "posture",
    "translations", "windows", "window", "terminal",
];

/// What a library can declare.
pub const LIBRARY_STATEMENTS: &[&str] = &["let", "spring", "component", "permissions", "fact", "text", "model", "service", "event", "image", "figure", "font", "shader", "prop", "pose", "gesture", "posture", "layer", "translations"];

/// Which properties each element accepts. `shape` are the ones common to all shapes.
pub const PROPERTIES: &[(&str, &[&str])] = &[
    ("surface", &["size", "anchor", "margin", "level", "reserve", "screens", "keyboard", "open", "kind", "title", "app_id", "min_size", "rate", "captures", "agent"]),
    ("permissions", &["run", "services", "env"]),
    ("blur", &["passes", "values", "reach"]),
    ("shape", &["rotate", "stroke", "color", "opacity", "blend", "glass", "lens", "shine", "refraction", "dispersion", "dome", "ripple", "frost", "smoke", "milk", "tint", "vivid", "mirror", "highlight", "active", "show", "cursor", "carries", "carry_icon", "grow", "label", "agent", "role", "value", "checked", "selected", "handle", "yields", "place"]),
    ("ellipse", &["at", "radius", "scale"]),
    ("box", &["at", "from", "size", "corner"]),
    ("arc", &["at", "radius", "span", "width"]),
    ("line", &["from", "to", "width"]),
    ("path", &["at", "size"]),
    ("body", &["color", "gradient", "rim", "light", "shadow", "border", "glass", "lens", "shine", "refraction", "dispersion", "dome", "ripple", "frost", "smoke", "milk", "tint", "vivid", "mirror", "highlight", "opacity", "show"]),
    ("text", &["at", "anchor", "width", "size", "weight", "color", "opacity", "lines", "align", "line_height", "family", "measure", "show", "grow", "gradient", "outline", "shadow", "letter_move", "letter_opacity", "letter_scale", "selectable", "selection", "markdown", "place", "tight"]),
    ("image", &["at", "size", "opacity", "tint", "cell", "fit", "show", "grow", "place"]),
    ("window", &["at", "size", "ask", "opacity", "show"]),
    ("terminal", &["at", "size", "family", "font_size", "line_height", "colors", "cursor", "selection", "command", "folder", "env", "run", "opacity", "show", "label", "agent", "place", "grow"]),
    ("figure", &["at", "size", "scale", "rotate", "pivot", "color", "opacity", "blend", "stroke", "show", "grow", "place"]),
    ("shader", &["at", "size", "corner", "opacity", "show", "values", "colors", "grow", "place"]),
    ("particles", &["at", "area", "count", "life", "speed", "direction", "spread", "gravity", "drag", "size", "colors", "opacity", "shape", "emit", "burst", "show"]),
    ("input", &["at", "width", "size", "weight", "color", "opacity", "family", "placeholder", "selection", "secret", "passive", "show", "label", "agent"]),
    ("group", &["pivot", "rotate", "scale", "move", "opacity", "size", "show", "z", "grow", "span", "blur", "glow", "saturation", "brightness", "contrast", "hue", "mask", "mode", "shader", "place"]),
    ("popup", &["at", "size", "open"]),
    ("children", &["move"]),
    ("grid", &["at", "columns", "gap", "width", "row", "show", "opacity"]),
    ("layout", &["at", "anchor", "gap", "padding", "align", "fill", "glass", "lens", "shine", "refraction", "dispersion", "dome", "ripple", "frost", "smoke", "milk", "tint", "vivid", "mirror", "highlight", "corner", "show", "opacity", "cursor", "view", "step", "stick", "content", "wrap", "size", "grow", "place"]),
];

pub const FUNCTIONS: &[&str] = &["min", "max", "abs", "floor", "ceil", "sin", "cos", "clamp", "smooth", "mix", "if", "vel", "sqrt", "pow", "fract", "mod", "sign", "round", "exp", "log", "tan", "atan2", "length", "noise", "random", "pick", "rgb"];
/// Inside a hole of a text.
pub const TEXT_FUNCTIONS: &[&str] = &["upper", "lower"];
/// What can go after `on`. Any other word is the name of an event.
pub const TRIGGERS: &[&str] = &["press", "release", "scroll", "drag", "hold", "enter", "leave", "hover", "away", "idle", "key", "submit", "focus", "blur", "drop", "carry", "change", "still"];
/// The effects with a word of their own. Also: `prop: value ~spring` and `fact = expr`.
pub const EFFECTS: &[&str] = &["toggle", "emit", "impulse", "ripple", "play", "focus", "blur", "close", "promote", "fullscreen", "minimize", "restore", "float", "tile", "launch", "send", "swap", "workspace", "pick", "dock", "accept", "quit"];
pub const CURVES: &[&str] = &["linear", "in_quad", "out_quad", "in_cubic", "out_cubic", "in_out_sine", "out_back", "bezier"];
/// What a keyframe can carry besides a curve.
pub const KEYFRAME_OPTIONS: &[&str] = &["hold", "emit"];
pub const CLASSES: &[&str] = &["ambient", "reflex", "asked", "state"];
/// Of a model's field. Also: an enumeration, `low | normal | critical`.
pub const TYPES: &[&str] = &["text", "number", "bool", "image"];
/// Of a fact. Also: an enumeration.
pub const FACT_TYPES: &[&str] = &["number", "bool"];
/// One line per word, for the language server: what the editor shows when
/// hovering over it. `run-tests.sh` checks that no statement is missing.
pub const HELP: &[(&str, &str)] = &[
    ("translations", "`translations es { \"Control center\" = \"Centro de control\" }` — the scene's texts in another language. With them comes the fact `locale` (`en | es`), which starts as the system's language and switches every text at once."),
    ("surface", "`surface { size: full, 44; anchor: top }` — a window this scene asks the system for. Several, with a name, share everything."),
    ("permissions", "`permissions { run: \"date\"; services: \"audio\", \"audio.*\"; env: \"PATH\" }` — what the logic may touch. Undeclared, nothing. Listening is not commanding. `env:` names the variables `run` and `spawn` may set that are not let through on their own."),
    ("model", "`model rows max 14 { label: text }` — a list of records the logic fills. Creates `rows.count`, `rows.total` and `rows.K.field`."),
    ("service", "`service clock as now { time: text }` — a system service by name. What it reports lands in facts and texts with no logic at all."),
    ("spring", "`spring bouncy = 170, 12` — a spring of your own: stiffness, damping. Built in: lively, calm, quick, slow, gentle, pose."),
    ("prop", "`prop orb.x = 360 ~lively` — an animated property. The render moves it; the logic never sees it."),
    ("pose", "`pose eyes = 14` — a property a gesture carries by the hand."),
    ("fact", "`fact open = false` — something that is true for a while. The logic and the rules set it. Can be typed: `fact mode: low | normal | critical`."),
    ("event", "`event confirmed` · `event view ->` — something that happens. With `->`, the logic hears it too."),
    ("text", "Declares a live text (`text title = \"…\"`) or paints one (`text title { size: 14 }`). In a painted one, `\"{a} · {b}\"` has holes."),
    ("image", "`image fox = icon \"firefox\", 48, 48` — an image by icon name, by file, or from a text that says which."),
    ("figure", "`figure hat = file \"hat.svg\"` — an svg as geometry: its layers are paths, by the `id` of each one."),
    ("font", "`font file \"fonts/pixel.ttf\"` — a font the scene brings with it: `family:` names it as if it were installed, and nothing is put in the system."),
    ("particles", "`particles { at: 200, 100; count: 300; life: 0.6s .. 1.4s; speed: 40 .. 160; direction: -90deg; spread: 60deg; gravity: 0, 120; size: 5, 0; colors: mint, #fff; emit: open }` — an emitter. Each particle is worked out on the card from its number and the time, so thousands cost what their pixels cost. `burst: event` lets them all out at once instead."),
    ("shader", "Declares one of the scene's own shaders (`shader aurora = file \"aurora.wgsl\"`, with `fn shade(s: Shader) -> vec4<f32>`) or paints a box with it (`shader aurora { at: 360, 60; size: 400, 120; values: glow; colors: mint }`)."),
    ("blur", "The frosting behind every glass of the scene, as passes of its own: `blur = file \"blur.wgsl\" { passes: down 0.5 repeat 4, up 2 repeat 3, grade 1; values: 1.5, 0.02; reach: 34 }`, each pass a `fn down(s: Blur) -> vec4<f32>` reading `prev`, `input` and `kept`."),
    ("lens", "`lens = image \"~/.cache/wallpaper.png\"` — the glass of the scene bends this picture (the wallpaper, any size; looked at again when the file changes) instead of a capture of what is behind: no screencopy, but windows never show through the glass. `lens every 250ms` — at most one capture of what is behind that often (50ms without it). `lens hold while opening` — no captures while it holds, one as soon as it stops."),
    ("measure", "`measure label` — creates `label.width` and `label.height`, filled by the text that carries `measure: label`."),
    ("let", "`let panel.x = orb.x + 62` — a name for an expression, or for a colour. It has to come before whoever uses it."),
    ("zone", "`zone box whole { at: …; size: … }` — a shape that is not painted: it only catches the mouse."),
    ("body", "`body { color: …; shadow: … }` — one silhouette out of several shapes, melted together, with its paint, rim, light and shadow."),
    ("ellipse", "`ellipse { at: 40, 40; radius: 12 }` — a circle, or an ellipse with `scale`."),
    ("box", "`box { from: 0, 0; size: 40, 20; corner: 8 }` — a rectangle, rounded if you want."),
    ("arc", "`arc { at: …; radius: 20; span: 2.4; width: 3 }` — an arc like `∩`, open as much as `span` says."),
    ("line", "`line { from: 0, 0; to: 30, 12; width: 2 }` — a line with round ends."),
    ("path", "`path { move 0, 0; line 20, 10; curve 40, 0 via 32, 10; close }` — a broken or curved line. Closed, it is filled."),
    ("input", "`input query { width: 260 }` — a one-line text field. What gets typed reaches the logic as `text:query`."),
    ("clip", "`clip [inset n] shape` — cuts everything that comes after, until the end of its group."),
    ("group", "`group { rotate: …; opacity: … }` — several things moved, turned or faded as one."),
    ("popup", "`popup menu { at: …; size: …; open: shown }` — a little window of its own that can go outside the surface."),
    ("component", "`component Row(r: record, tone: color = mint) { … }` — something to copy, with typed parameters and named holes."),
    ("children", "`children` · `children header` — where a copy's children go, inside a component."),
    ("repeat", "`repeat i in 1..10 { … }` — the same thing several times, with `$i` to build names."),
    ("for", "`for r in rows { … }` · `for r in rows from first { … }` — once per record of a model."),
    ("row", "`row { gap: 8; align: center }` — children side by side, with gap, padding, fill and scroll (`view:`)."),
    ("grid", "`grid { columns: 2; gap: 12; width: 456; row: 106 }` — children in cells, left to right and down; `span: 2` takes two. Inside each, `cell.w` and `cell.h` are its cell."),
    ("windows", "`windows win max 6` — a compositor inside the scene: programs started with `launch \"kitty\"` open in it. Per slot: `win.$i.open`, `.title`, `.app`, `.width`, `.height`, `.focused`, `.place` (its turn in the layout of its monitor, −1 if closed), `.screen`, `.fullscreen`, `.dialog`, `.minimized`, `.floating`, `.framed` (it lets the compositor draw its title bar), `.held` · `.edges` (it asks to be carried, 1, or stretched, 2, by those edges) (`float win.$i`: over the layout, out of `.place`, `.among` and `on.$s`), `.workspace`, `.among` (how many share its monitor and workspace), `.rank` (its turn among the ones shown, −1 if hidden); and `win.count`, `win.on.$screen`, `win.shown.$screen` (the workspace each monitor shows), `win.focus`, `win.socket`."),
    ("window", "`window win.$i { at: x, y; size: w, h }` — that slot's window, drawn there and answering the mouse and the keyboard. `ask: w, h` is the size it is told to have (by default `size`): let `size` travel on a spring and `ask` be where it goes. It is also a zone: `on press win.$i`, `win.$i.hover`."),
    ("terminal", "`terminal term { at: 0, 40; size: w, h - 40; family: \"JetBrains Mono\"; font_size: 13 }` — a shell on a terminal of its own, its screen drawn there: the keys go to it once it is pressed (or `focus term`), the mouse selects, Ctrl+Shift+C and Ctrl+Shift+V copy and paste. `command:` what runs (the user's shell if not said), `folder:` where, `run:` while it lives (hidden, it keeps living: a tab behind another), `colors:` ink, paper and the sixteen ANSI ones. What it says, if the scene declares them: `term.title`, `term.folder` (texts), `term.alive`, `term.bell`, `term.focus`, `term.cols`, `term.rows` (facts). It is also a zone: `on press term`, `term.hover`."),
    ("pages", "`pages settings { header: 20, 45; page menu \"Settings\" { … } page look \"Her look\" { … } }` — one page at a time, sliding in; `settings` is a fact with the pages' names (`settings = look`), and with `header:` comes the ← and the title, and Esc goes back."),
    ("column", "`column { gap: 8 }` — children one under the other. Same properties as `row`."),
    ("space", "`space 12` — a gap of that size inside a layout."),
    ("between", "`between i { … }` — what goes between every two children of a layout, with its position."),
    ("layer", "`layer mouth { smile while happy }` — who wins when several want the same thing, and how it hands over."),
    ("on", "`on press hit { open = true }` — a rule: when something happens, this. Also `on some_event`."),
    ("every", "`every 2s { … }` · `every 2s..6s while awake { … }` — a rule on a clock."),
    ("blink", "`blink lid every 2.4s..6s for 170ms` — from 1 to 0 and back, now and then."),
    ("wave", "`wave breath = 3 at 1.7` — amplitude · sin(1.7 t)."),
    ("spin", "`spin angle by 0.9` — adds that much per second."),
    ("follow", "`follow on = if(active == i, 1, 0)` — a property that follows an expression, with its spring."),
    ("look", "`look eye.x, eye.y at pointer reach 8, 5 within 260` — eyes that follow something."),
    ("ripple", "`ripple hit` — a rule sends a ring of light from the centre of that zone through the glass, without pressing it. As a glass property, `ripple: 100%` sets its strength."),
    ("gesture", "`gesture cheer { … }` — a timeline of poses: the render plays it whole."),
    ("posture", "`posture tired { … }` — a still face: where the poses rest."),
    ("import", "`import \"common/palette.plm\"` — brings in a library: its colours, components and springs. `import \"menu.plm\" as menu` calls its components `menu.Row`."),
    ("scene", "`scene Bar { … }` — everything a window shows, and what it reacts to."),
    ("library", "`library Palette { … }` — what several scenes share. With a `.luau` next to it, it is a plugin: its own frontier and its own permissions."),
    ("include", "`include \"pages/wifi.plm\"` — a piece of this scene kept in a file of its own, put right here as if it were written here: the same names, inside the same group."),
    ("part", "`part Wifi { … }` — a piece of a scene, in a file of its own: anything a scene can hold. It is not opened: a scene includes it (`include \"…\"`)."),
    ("language", "`language 0.1` — the version of the language this file needs. It is the first line."),
];

/// The services that can be asked for with `service`, and what each one reports. The scene
/// picks which fields it wants from those there are; asking for one that is not there is an error on load.
/// A field that is a list (`LIST_FIELDS`) goes into a model the scene declares:
/// `service network as net { networks: nets }`, `service tray { list: icons }`.
pub const SERVICES: &[(&str, &[&str])] = &[
    ("clock", &["hour", "minute", "second", "day", "month", "year", "weekday", "time", "date"]),
    ("clock.seconds", &["hour", "minute", "second", "day", "month", "year", "weekday", "time", "date"]),
    ("audio", &["volume", "muted", "input", "input_muted", "outputs", "inputs", "apps"]),
    ("audio.level", &["level"]),
    ("battery", &["present", "percent", "charging"]),
    ("brightness", &["present", "level"]),
    ("network", &["online", "kind", "name", "strength", "wifi", "error", "networks"]),
    ("bluetooth", &["present", "powered", "discovering", "devices", "pairing", "pairing_id", "pairing_address", "pairing_name", "pairing_code"]),
    ("media", &["playing", "title", "artist", "album", "length", "position", "rate", "can_seek", "art", "player", "players"]),
    ("window", &["title", "class", "monitor", "fullscreen", "maximized", "list"]),
    ("thumbnails", &["list", "capturing"]),
    ("workspaces", &["active", "list"]),
    ("apps", &["list"]),
    ("tray", &["list"]),
    ("notifications", &["list"]),
    ("notification_history", &["list"]),
    ("polkit", &["ready", "pending", "asking", "message", "action", "icon", "user", "prompt", "secret", "info", "error"]),
];

/// The fields of a service that are lists: each one goes into a model, never into a fact.
/// `list` is the service's whole report, for the ones that are only a list.
pub const LIST_FIELDS: &[&str] = &["networks", "devices", "outputs", "inputs", "apps", "players", "list"];

/// The steps of a path: where it goes through. `curve … via …` is a quadratic Bézier.
pub const PATH_COMMANDS: &[&str] = &["move", "line", "curve", "close"];

/// What a model can carry inside besides fields: another list of records.
pub const MODEL_TYPES: &[&str] = &["list"];
/// What a component can ask for: `component Row(r: record, chosen: event, tone: color = mint)`.
pub const PARAMETER_TYPES: &[&str] = &["number", "bool", "color", "text", "record", "event", "image", "gesture", "spring"];
pub const SPRINGS: &[&str] = &["lively", "calm", "quick", "slow", "gentle", "pose"];
pub const UNITS: &[&str] = &["px", "%", "deg", "ms", "s"];
pub const CURSORS: &[&str] = &[
    "default", "pointer", "text", "grab", "grabbing", "ew_resize", "ns_resize", "nwse_resize", "nesw_resize", "move", "not_allowed", "crosshair", "context_menu", "help", "progress", "wait", "cell",
    "vertical_text", "alias", "copy", "no_drop", "e_resize", "n_resize", "ne_resize", "nw_resize", "s_resize", "se_resize", "sw_resize", "w_resize", "col_resize", "row_resize", "all_scroll", "zoom_in",
    "zoom_out", "dnd_ask", "all_resize",
];
pub const SURFACE_ANCHORS: &[&str] = &["top", "bottom", "left", "right", "top_left", "top_right", "bottom_left", "bottom_right", "center"];
pub const LEVELS: &[&str] = &["background", "bottom", "top", "overlay"];
/// Which kind of window a surface asks for: stuck to an edge, or a normal one.
pub const SURFACE_KINDS: &[&str] = &["panel", "window", "lock"];
pub const KEYBOARD_MODES: &[&str] = &["none", "on_demand", "exclusive"];
/// Whether what a surface shows goes into captures of the monitor too (a
/// screenshot, a recording, a remote desktop), or only to the monitor.
pub const CAPTURES: &[&str] = &["shown", "hidden"];
/// `agent:` on a zone, a field or a surface: what an agent may do with it.
pub const AGENT: &[&str] = &["yes", "no", "hidden"];
/// `role:` on a zone: what it is, when what its rules do does not say it.
pub const ROLES: &[&str] = &["button", "toggle", "slider", "tab", "link", "item", "list", "region"];
pub const TEXT_ALIGNS: &[&str] = &["left", "center", "right"];
pub const STACK_ALIGNS: &[&str] = &["start", "center", "end"];
/// How a group with effects blends: covering, or adding light.
pub const GROUP_MODES: &[&str] = &["normal", "add", "screen", "multiply"];
/// How an image fills the box it is drawn in: stretched, whole inside it, or cut to cover it.
pub const IMAGE_FITS: &[&str] = &["fill", "contain", "cover"];
/// What each particle looks like.
pub const PARTICLE_SHAPES: &[&str] = &["dot", "square", "spark"];

/// The properties of an element. Blowing up here is a mistake of whoever programs, not of whoever writes the scene.
pub fn properties(of: &str) -> &'static [&'static str] {
    PROPERTIES.iter().find(|(n, _)| *n == of).map(|(_, p)| *p).unwrap_or_else(|| panic!("the vocabulary does not know what properties '{of}' has"))
}

/// Everything, as text: one line per list. It is what `pleamar --grammar` prints
/// and what the reference has copied, so that they can be compared.
pub fn to_text() -> String {
    let mut s = format!("language: {}.{}\n", super::VERSION.0, super::VERSION.1);
    let mut line = |name: &str, list: &[&str]| s.push_str(&format!("{name}: {}\n", list.join(" ")));
    line("statements", STATEMENTS);
    line("library", LIBRARY_STATEMENTS);
    for (element, props) in PROPERTIES {
        line(&format!("properties.{element}"), props);
    }
    line("functions", FUNCTIONS);
    line("text_functions", TEXT_FUNCTIONS);
    line("triggers", TRIGGERS);
    line("effects", EFFECTS);
    line("curves", CURVES);
    line("frame", KEYFRAME_OPTIONS);
    line("classes", CLASSES);
    line("field_types", TYPES);
    line("fact_types", FACT_TYPES);
    line("model", MODEL_TYPES);
    line("path", PATH_COMMANDS);
    // The words the editor knows how to explain: `run-tests.sh` demands that all statements be there.
    line("documented", &HELP.iter().map(|(n, _)| *n).collect::<Vec<_>>());
    line("services", &SERVICES.iter().map(|(n, _)| *n).collect::<Vec<_>>());
    for (service, fields) in SERVICES {
        line(&format!("services.{service}"), fields);
    }
    line("parameter_types", PARAMETER_TYPES);
    line("springs", SPRINGS);
    line("units", UNITS);
    line("cursors", CURSORS);
    line("surface.anchor", SURFACE_ANCHORS);
    line("surface.level", LEVELS);
    line("surface.kind", SURFACE_KINDS);
    line("surface.keyboard", KEYBOARD_MODES);
    line("surface.captures", CAPTURES);
    line("agent", AGENT);
    line("role", ROLES);
    line("text.align", TEXT_ALIGNS);
    line("layout.align", STACK_ALIGNS);
    line("group.mode", GROUP_MODES);
    line("image.fit", IMAGE_FITS);
    line("particles.shape", PARTICLE_SHAPES);
    s
}
