# Language reference — version 0.6

**What this note is.** The complete, exact description of what the language accepts. [The language — the guide](09-language-v0.md) is the guide —read straight through, with the reason behind each thing—; this is where a doubt gets looked up. It comes from the compiler (`src/language/`), not from memory, and **it cannot fall behind without `./run-tests.sh` saying so**: its whole examples compile, and its vocabulary (§17) is compared against the one the compiler consults.

```sh
pleamar --version                  # pleamar 0.7.7 · language 0.9
pleamar --check scene.plm      # reads it, with whatever it imports; says whether it is fine, exits
./run-tests.sh                        # tests/*.plm, examples/*.plm and the examples in this note
```

## 1. Version

The language has a number of its own, apart from the program's: **0.9**. The first changes when something already written stops being valid; the second, when something is added. A file can say which one it needs, on its first line:

```
language 0.1
```

If it asks for a different first number, or a second one higher than the program understands, it is an error on load —`this file asks for language 0.99, and this pleamar understands 0.9`— and not a half-built scene. A file that asks for 0.1 is still read by 0.9: what 0.2 added —a group's own `shader:`, workspaces, `pick`, `import … as`, `drag.over`— and what 0.3 added —what a zone tells an agent: `label:`, `agent:`, `role:`, `value:`, `checked:`, `selected:`—, what 0.4 added —a sheet of pictures' `cell:` and `pixels` for pixel art—, and what 0.5 added —a list that sticks to its end (`stick: end`), a child placed on its own across its layout (`place:`), a text as wide as what it says (`tight: true`), a `lines:` that changes, `motion.reduced` and `color.scheme`, a `launch` with holes, a `text` chosen with `if`, and `hover`/`away` over several zones—, and what 0.6 added —a quote inside a text (`\"`), a block opened on the line after its header, and a `let` that is a colour chosen with `pick` or `if`—, what 0.7 added —a terminal drawn by the scene (`terminal`, §10.4)—, what 0.8 added —a window put aside, on no workspace (`send win(k) aside`)—, and what 0.9 added —a zone that yields to what is below it (`yields: true`)— it simply does not use. Without that line, it is read with whatever is there. While the first is 0, nothing is promised: this is a language still being made.

## 2. What it guarantees

It is a **declarative language that always terminates**. No free loops, no recursion, no variables that mutate: `repeat` and `for` unfold on load, with a ceiling. Everything written is run by the renderer, on its own, at the cadence of the screen; the logic (Luau, apart) only sets facts, texts, models and events. **Every name is checked on load**: a misspelt one is an error with its file, its line, its arrow and a "did you mean…?", never a failure while running.

## 3. Lexicon

| | |
| --- | --- |
| Comment | `//` to the end of the line |
| End of statement | a newline or `;`. Inside parentheses, a newline ends nothing; nor does one before a `{`: a block may open on the line after its header (`group` and, below it, `{`) |
| Name | letters, digits, `_` and `.`; starts with a letter or `_`. Dots are part of the name: `orb.x`, `note.0.title`. Inside a `repeat`, `$i` is replaced by the number of the turn: `hit.$i` |
| Number | `40`, `0.5`, `-3`. With a unit: `40px` (= 40), `34%` (= 0.34), `138deg` (to radians) |
| Duration | `320ms`, `14s`. Where a duration is expected, a number without a unit is not valid |
| Color | `#151616` or `#fff` |
| Text | `"in quotes"`. A quote inside it is `\"`, and a backslash before a quote or another backslash, `\\`; any other backslash is itself (`"C:\Users"`). With slots, see §11 |
| Symbols | `{ } ( ) [ ] , : ; = ~ + - * / < > <= >= == != .. -> % \|` |

Words of the language (they cannot be used as a name of one's own without confusing the reader, though the compiler does not forbid it): `language import scene library surface permissions model service spring prop pose fact event text image measure let zone body ellipse box arc line path input clip group popup component repeat for row column space layer on every blink wave spin follow look gesture posture`, and inside their statements `in max while for after from until at by reach within rest every inset right middle back forward move curve close via as radial radius to change wrap true false and or not`.

## 4. Grammar

In EBNF: `[ x ]` is optional, `{ x }` zero or more times, `|` alternatives, `"x"` literally. `end` is a newline or `;`.

```
file         = [ "language" number end ] { "import" text [ "as" name ] end } ( scene | library ) ;
scene        = "scene" name "{" { statement } "}" ;
library      = "library" name [ "strict" ] "{" { let | spring | component | boundary | inner | translations } "}" ;
boundary     = permissions | fact | event | live_text | model | image_decl ;    (* lives under the library's name: `Clock.now` *)
inner        = property | gesture | layer ;                                     (* what moves inside; also under its name *)

statement    = declaration | drawing | structure | layer | rule | behaviour | gesture ;

declaration  = surface | permissions | model | spring | property | fact | event
             | live_text | image_decl | figure_decl | shader_decl | measure | let | zone | translations ;
translations = "translations" name "{" { text "=" text end } "}" ;            (* name: a language code, `es`, `pt_br` *)
surface      = "surface" [ name ] "{" { element_prop | statement } "}" ;   (* named: one of several, with its own things inside *)
permissions  = "permissions" "{" { ( "run" | "services" | "env" ) ":" text { "," text } end } "}" ;
model        = "model" name [ "max" number ] "{" { field | list } "}" ;
service      = "service" service_name [ "as" name ] "{" { field } "}" ;     (* the services, and their fields, are in §17 *)
field        = name ":" type [ "=" literal ] end ;
list         = "list" name [ "max" number ] ( "{" { field | list } "}"          (* records inside the record *)
                                            | "depth" number ) ;                (* …or like the outer one, down to that depth: a tree *)
type         = "text" | "number" | "bool" | "image" number "," number | enum ;
enum         = name "|" name { "|" name } ;
spring       = "spring" name "=" number "," number ;
property     = ( "prop" | "pose" ) name "=" number [ "~" spring_ref ] ;
fact         = "fact" name [ ":" ( "number" | "bool" | enum ) ] "=" ( number | "true" | "false" | name ) ;
event        = "event" name [ "->" ] ;
live_text    = "text" name "=" text ;
image_decl   = "image" name "=" ( "icon" text | "file" text | "pixels" text | "from" name ) "," number "," number ;
figure_decl  = "figure" name "=" "file" text ;                                  (* an svg, by its layers *)
shader_decl  = "shader" name "=" "file" text ;                                  (* WGSL with `fn shade(s: Shader) -> vec4<f32>`: §8.1 *)
measure      = "measure" name ;
let          = "let" name "=" ( expr | color ) ;
array        = "let" name "[" expr "]" [ "[" expr "]" ] ;                                   (* its length, known when loading: `let tw[win.max]` *)
sorted       = "let" name "[" expr "]" "=" "sort" "(" name "in" ( name | number ".." number ) "by" expr { "," expr } ")" ;
element      = name "[" expr "]" { "[" expr "]" } "=" expr ;                                (* one element, in a `repeat` at the scene's level *)
fold         = name ( "=" | "+=" | "-=" | "*=" ) expr ;                     (* a link of a fold, in a `repeat` at the scene's level *)
zone         = "zone" shape ;
spring_ref   = name | "spring" "(" number "," number ")" | duration ;   (* ~620ms: gets there in that long *)

drawing      = body | shape | text | image | figure | shader | particles | field | clip | group | popup ;
body         = "body" "{" { element_prop | shape } "}" ;
shape        = ( "ellipse" | "box" | "arc" | "line" ) [ name ] "{" { element_prop } "}"
             | "path" [ name ] "{" { element_prop | step } "}" ;
step         = "move" point | "line" point | "curve" point "via" point | "close" ;
text         = "text" ( text | name | "number" "(" expr [ "," number [ "," text ] ] ")" ) "{" { element_prop } "}" ;
image        = "image" name "{" { element_prop } "}" ;
figure       = "figure" name [ "." name ] "{" { element_prop } "}" ;            (* whole, or one layer *)
shader       = "shader" name "{" { element_prop } "}" ;
particles    = "particles" [ name ] "{" { element_prop } "}" ;
field        = "input" name "{" { element_prop } "}" ;
clip         = "clip" [ "inset" number ] shape ;
group        = "group" "{" { element_prop | statement } "}" ;
popup        = "popup" name "{" { element_prop | statement } "}" ;

structure    = component | copy | children | slot_block | repeat | for | layout | space | separator ;
component    = "component" name [ "(" [ parameter { "," parameter } ] ")" ] "{" { element_prop | statement } "}" ;
parameter    = name [ ":" param_type ] [ "=" argument ] ;
param_type   = "number" | "bool" | "color" | "text" | "record" | "event" | "image" | "gesture" | "spring" ;
copy         = Name [ "(" [ arguments ] ")" ] [ "{" { element_prop | statement } "}" ] ;   (* the statements are its children *)
children     = "children" [ name ] [ "{" { element_prop } "}" ] ;                          (* only inside a component *)
slot_block   = name "{" { statement } "}" ;                                                (* in a copy: what goes into the slot of that name *)
separator    = "between" [ name ] "{" { element_prop | statement } "}" ;                   (* only inside a layout; with several things, it needs `size:` *)
arguments    = argument { "," argument } { "," name ":" argument }
             | name ":" argument { "," name ":" argument } ;
argument     = expr | color | text | name ;
repeat       = "repeat" name "in" integer ".." integer "{" { statement } "}" ;
for          = "for" name "in" name [ "from" expr ] "{" { statement } "}" ;
layout       = ( "row" | "column" ) [ name ] [ "~" spring_ref ] "{" { element_prop | statement } "}" ;
space        = "space" expr ;

layer        = "layer" name [ "~" spring_ref ] "{" { claim } "}" ;
claim        = name [ when ] [ "{" { transition } "}" ] ;
when         = "while" expr | "for" duration "after" events | "from" events "until" events ;
events       = name { "," name } ;
transition   = name ":" expr [ "~" spring_ref ] [ "after" duration ] end ;

rule         = "on" trigger [ "while" expr ] "{" { effect } "}"
             | "every" duration [ ".." duration ] [ "while" expr ] "{" { effect } "}" ;
trigger      = "press" [ "right" | "middle" | "back" | "forward" ] zone_ref | "release" zone_ref | "scroll" zone_ref
             | "drag" zone_ref | "hold" zone_ref "for" duration
             | "enter" zone_ref | "leave" zone_ref
             | ( "hover" | "away" ) zone_ref "for" duration
             | "idle" "for" duration
             | "key" key | "submit" name | "focus" | "blur" | "drop" zone_ref | "carry" zone_ref
             | name ;                                (* an event *)
key          = name { "+" name } ;                   (* Escape · Ctrl+k · Super+Alt+s *)
effect       = transition | name "=" expr | "toggle" name
             | "emit" name [ "(" expr ")" ] | "impulse" name expr | "ripple" name
             | "play" name | "focus" name | "blur" ;

behaviour    = "blink" name "every" duration [ ".." duration ] "for" duration
             | "wave" name "=" expr "at" number
             | "spin" name "by" expr
             | "follow" name "=" expr
             | "look" name "," name "at" point "reach" number "," number "within" number [ "rest" point ] ;

gesture      = "gesture" name ( "ambient" | "reflex" | "asked" | "state" ) "{" { frame } "}"
             | "posture" name "while" expr "{" { frame } "}" ;
frame        = duration { curve | "hold" duration | "emit" name } [ "{" { name ":" expr end } "}" ] ;
curve        = "linear" | "in_quad" | "out_quad" | "in_cubic" | "out_cubic" | "in_out_sine" | "out_back"
             | "bezier" "(" number "," number "," number "," number ")" ;   (* CSS's cubic-bezier *)

element_prop = name ":" value { "," value } end ;       (* which ones are valid, depending on the element: §8 *)
point        = expr "," expr ;

expr         = or ;
or           = and { "or" and } ;
and          = not { "and" not } ;
not          = "not" not | sum [ ( "<" | ">" | "<=" | ">=" | "==" | "!=" ) sum ] ;
sum          = product { ( "+" | "-" ) product } ;
product      = unary { ( "*" | "/" ) unary } ;
unary        = "-" unary | "(" expr ")" | number | duration | "true" | "false"
             | name | function "(" [ expr { "," expr } ] ")" ;
color        = "#" hex | name | "mix" "(" color "," color "," expr ")" | "if" "(" expr "," color "," color ")" ;
```

`Name` in `copy` is that of an already declared component: by convention capitalised, which is what tells it apart at a glance from a statement of the language.

## 5. Files, order and names

**A file is a scene or a library.** A scene is opened; a library is imported. `import "path.plm"` goes before `scene` or `library`, and the path is relative **to the file that imports it**. A library imported by two routes is read once; a circle is an error that says its route. A library only declares: `let`, `spring` and `component`. What is imported behaves as if it were written at the start of the scene.

**`import "menu.plm" as menu`** gives the library's components a surname: from the scene they are `menu.Row(r)`, and inside the library they keep calling each other by their own names (`Row` inside `menu.Pair` is `menu.Row`). That is how two libraries that each have a `Row` are used together. A library has one name in a scene: imported once with `as menu` and again with `as other`, or without it, is an error. The surname is for components; a library's facts, texts and events already live under the library's name (`Menu.open`), and its `let`s and springs are still shared, so the scene can override a colour.

**A big scene goes in pieces with `include`.** `include "pages/wifi.plm"` puts there, where the line is, what a **part** holds —`part Wifi { … }`, in a file of its own—, as if it had been written there: the same names (a part's `fact` is the scene's, and it reads the scene's lets, and those of the group it is included in), the same order (it paints where it is included). It can go inside a `group`, and a part can include others; its path is relative to the file that includes it. A part does not import: the scene imports the libraries, and its parts use them. Its errors say its own file and line, what it draws finds its images next to it, and saving it reloads the scene like any of its files. A part is not opened: `pleamar --check` on one says so. Unlike a library it has no name of its own to live under; it is the same scene, in more files.

```
scene Marea {
    include "pages/declarations.plm"
    group {
        let lx = card.x - 228
        include "pages/wifi.plm"       // part Wifi { … }, which reads lx
    }
}
```

**`library Name strict { … }`**: its components can only read what they ask for by parameter, what they declare themselves, what belongs to their library (and to whatever it imports), and the names that always exist. Reading a fact, a color or an event of the scene without asking for it is an error on load —`'Nosy' belongs to a `strict` library and reads 'secret', which is the scene's, without asking for it`—: that way someone else's library does not depend on what things are called in the scene, nor does it touch them. Without `strict`, a component sees everything belonging to whoever uses it, which is the comfortable thing for one's own libraries.

**A plugin is a library with its logic next to it**: `clock.plm` and `clock.luau`. It can also declare its own boundary —`fact`, `text`, `model`, `event`— and its `permissions`:

```
library Clock strict {
    permissions { run: "date" }           // ITS logic's, not the scene's
    text now = "--:--"
    fact seconds = false
    event tapped ->
    component Clock(tone: color = ink) {
        row face { padding: 9; fill: coal; corner: 14;  text now { size: 14; color: tone } }
        on press face { emit tapped }
    }
}
```

- Its boundary **lives under the name of the library**: inside it is written `now`; from the scene, `Clock.now`. Two plugins can each have their own `count`, and neither treads on the scene's.
- Its logic runs in **its own Luau state**, and there `text.now` is `Clock.now`: it cannot name —cannot read, write or emit— anything that is not its own, neither the scene's nor another plugin's. It does not hear the keyboard, or the mouse, or anybody else's events; it does not ask for gestures or move the writing cursor.
- What it touches of the system is set by **its** `permissions`. The scene's are no good to it, and its own are no good to the scene. On start-up this is printed: `logic  · plugin 'Clock' · permissions · commands: date · services: none`.
- A scene may have no logic of its own and use plugins that do. Saving a plugin's `.plm` or `.luau` reloads hot, like everything else.

Asking for permissions without a `.luau` next to it is an error: there is nobody to use them.

**A plugin's permissions are approved by whoever uses it.** Declaring them is not having them: `pleamar --approve scene.plm` shows what each plugin of that scene asks for, and asks. What is approved is stored outside the plugin, with the fingerprint of its logic, of what it asked for and of the files of its folder that its commands name (`node tools/usage.js`): if any of them changes, it goes back to unapproved. **Unapproved, a plugin runs with no permissions at all**, and its errors say why and how to approve it. An interpreter (`sh`, `python`…) comes out flagged: it is asking for everything; and so does a variable in `env:` that decides which code runs (`PATH`, `LD_PRELOAD`, `NODE_OPTIONS`…), which is the same thing. The scene one opens does not go through this: opening it is already deciding.

A library can also bring **what moves inside** —`prop`, `pose`, `gesture`, `posture`, `layer`— and **images and figures** (`image logo = file "logo.png", 16, 16`, `figure hat = file "hat.svg"`: the path is relative to the file that writes it, so the piece travels with it). All under its name, like its boundary. What it cannot do is draw outside a component, or hold loose rules: that belongs to the scene.

**The scene talks to a plugin by emitting one of its events** (`on press button { emit Face.cheer }`), which the plugin's components hear (`on cheer { … }`) and its logic too (`on("cheer", …)`); and it can read and set the facts of its boundary (`Face.happy = false`): the scene is the owner. A plugin has no surface of its own: if something needs one, it is a scene.

**The file is read in four passes** —declarations; `let` and layers; drawing; rules—, so the order of what is written is whatever suits the reader: a rule can come before the shape it names, and a `prop` at the end. Two exceptions: a `let` has to come before whoever uses it, and **things are painted in the order they are written** (and of two zones, the one declared later ends up on top).

**Every name is global**, except inside a component or inside one turn of a `repeat` or a `for`: there, what is declared belongs to that copy (two copies of `Note` each have their own `lit` and their own `hit` zone), and the inner things are looked up first —parameters, the component's `let`— and then the outer ones. A `let` of the scene with the name of an imported one treads on it: that is how a tone is changed. Two components with the same name do not coexist. **A `group` does not keep its `let`s**: one written inside a group is seen by everything after it, outside too, so a group's `let` with the name of one written outside any group is an error (it would have changed that one in everything below). It is so too when they are in different parts, or in a scene copied per monitor. Inside a component, a `repeat` or a `for` the `let` is theirs.

**Names that always exist**, read like facts: `screen.width`, `screen.height` (what the monitor measures, as the scene's main surface sees it: the one without a name, or, in a scene whose surfaces all have one, the first; inside a `screens: each` surface, its own copy's monitor), `screen.index` (which monitor copy this is, with `screens: each`; 0 otherwise), and during a rule, the mouse's: `pointer.x`, `pointer.y` (on the surface), `local.x`, `local.y` (where the zone is written: with every `move`, `scale` and layout slot above it undone, but not its own `from:` — a zone at `from: 236, 0` reads 236 at its left edge), `drag.dx`, `drag.dy` (since the press), `wheel` (notches; positive is upwards). And the `demo` event, which `--demo` fires.

**`cursor.x`, `cursor.y` are the mouse wherever it is**: over another window, on
another monitor, far from the scene. They are in the scene's own coordinates,
like `pointer.x`, so a pair of eyes computes the same way whether the mouse is
over them or across the desk: `atan2(cursor.y - cy, cursor.x - cx)`. A surface
on Wayland is not told where the mouse is when it is not over it —on purpose—,
so pleamar asks whoever knows, and **only if the scene names them**: on
Hyprland, its socket, about thirty times a second; in a pleamar-wm session,
the `cursor.sock` it keeps for its programs, which says the mouse as it
moves. Elsewhere they are the pointer's while it is over the scene, and keep
their last value when it leaves.

A named surface draws from its own corner, so it has its own pair:
**`nook.cursor.x`, `nook.cursor.y`** are the same mouse in the coordinates the
surface `nook` draws with. That is what a camera in a corner needs to point at
the mouse: `atan2(nook.cursor.y - cy, nook.cursor.x - cx)`.

```plm
scene Watching {
    surface { size: 120, 120 }
    prop gaze.x = 0 ~gentle
    prop gaze.y = 0 ~gentle
    //  With the mouse near, the eyes follow it; far, they rest looking at it.
    let far = max(length(cursor.x - 60, cursor.y - 60), 1)
    look gaze.x, gaze.y at 60, 60 reach 6, 4 within 200 rest 6 * (cursor.x - 60) / far, 4 * (cursor.y - 60) / far
    ellipse { at: 60 + gaze.x, 60 + gaze.y; radius: 8; color: #f5f7f5 }
}
```

## 6. Declarations

| Statement | What it declares |
| --- | --- |
| `surface { … }` · `surface panel { …; …drawing… }` | The windows it asks for. See below |
| `permissions { run: "date"; services: "audio", "audio.*"; env: "PATH" }` | What the logic may touch of the system. Undeclared, nothing. **Listening is not commanding**: `"audio"` allows knowing the volume (`sys.watch`, `sys.ask`); changing it needs `"audio.volume"`, or `"audio.*"`. **`env:`**, the variables `run` and `spawn` may set (`{ env = … }`) that do not go through on their own: one that decides which code a program runs —`PATH`, `LD_*`, `NODE_OPTIONS`, `PYTHON*`, `PERL5OPT`, `BASH_ENV`, `HOME`…— never does, since with it `run: "node tools/usage.js"` would run something else; and a plugin's logic sets only `LANG`, `LC_*`, `TZ`, `NO_COLOR` and the terminal's, unless it names more |
| `service clock as now { time: text; hour: number }` | A system service, by its name. Whatever it reports fills `now.time` and `now.hour` **without a line of logic**. See below |
| `model rows max 14 { label: text; enabled: bool = true; depth: number }` | A list of records that the logic fills. `max`: how many fit (1 to 1024; 16 if unsaid). Creates `rows.count`, `rows.total` and, per record, `rows.K.field` |
| `prop orb.x = 360 ~lively` | An animated property: a spring. Without `~`, `lively` |
| `pose eyes = 14` | A property of the pose: the kind a gesture leads by the hand |
| `fact open = false` · `fact tries: number = 3` · `fact mode: low \| normal \| critical = normal` | Something that is true for a while. The logic and the rules set it. See **Types**, below |
| `event confirmed` · `event view_event ->` | Something that happens. With `->`, it also reaches the logic |
| `text notice.title = "Meeting"` | A live text: the logic changes it, or an `input` |
| `image fox = icon "firefox", 48, 48` | An image, and the largest logical size it is painted at. `icon "name"`, `file "path"`, or `from some_text`: whichever that text says (an icon name, or a path if it starts with `/`). A `file` that moves —a **GIF, an animated PNG or an animated WebP**— plays by itself, looping; the render wakes up only when it changes frame, it stops while the image is not seen, and with reduced motion it stays on its first frame. Its frames go to the atlas at the size it is painted, up to a quarter of it: one that asks for more keeps one frame in two —or three…—, at the same pace. **The declared size fixes the shape of the image's cell**: the picture is fitted inside a box of that shape, keeping its proportions, and that box is then stretched to the `size:` it is drawn at. Declare it with the proportions it is drawn at, or a picture of another shape is squashed, or leaves empty bands. Declared `0, 0`, it is the file's own size and shape (see `fit:` below) |
| `figure hat = file "hat.svg"` | An svg **as geometry**: its layers become paths, each one named by the `id` of its group in the file. The path is relative to the file that writes it, so a library takes its pieces with it |
| `shader aurora = file "aurora.wgsl"` | One of the scene's **own shaders**: a function in WGSL that paints a box, point by point. It is read and checked when the scene is read, and touching the file reloads it, like an svg. How it is written, in §8.1 |
| `measure label` | Creates `label.width` and `label.height`, filled by the text that carries `measure: label` |
| `let panel.x = orb.x + 62` · `let mint = #9ed6bd` | A name for an expression, or for a color. A small one is substituted where it is named; **a big one is computed once a frame** and what is named is that, so a chain of them —each naming the one before— costs a sum and not a product. A colour is the same, channel by channel. It is a colour when what it says is one: `#hex`, a colour's name, `rgb(…)`, `mix(…)` of colours, and `pick(i, #a, #b, …)` or `if(cond, #a, #b)` whose first choice is a colour, whole (`if(t == 0, skin == lens, …)` compares, and is numbers) |
| `spring bouncy = 170, 12` | A spring of one's own: stiffness, damping. From the house: `lively`, `calm`, `quick`, `slow`, `gentle`, `pose`. Inline: `~spring(170, 12)`, or **`~620ms`**: the spring that gets there in that long without overshooting |
| `zone box whole { at: …; size: …; active: expr }` | A zone that is not painted |

**The header says what it is; the block, what it holds.** A declaration's header names the thing and gives the few words that shape it: which one it is (`service clock`), what it is called (`as now`), how many fit (`max 8`), how deep it goes (`depth 3`). Its block holds what it contains: **fields** when it describes data —in a `model` or a `service`, `title: text` is a field called `title` that holds text—, **properties** when it sets something up, as in a `surface`, a `state` or a `zone`. So an option never takes the name of a field: a model can have one called `max` or `type`. And `windows win max 6` or `list children max 6 depth 3`, which have no block, say all they need in the header.

**Several windows in one process.** `surface { … }` with no name is the scene's, and draws whatever is loose. With a name, `surface panel { … }` is one of several and **carries inside it what it draws**:

```
surface { size: full, 40; anchor: top; screens: all }
box knob { … }
on press knob { toggle open }

surface panel {
    size: 300, 160;  anchor: top_right;  margin: 48, 12, 0, 0;  level: overlay
    open: open                                   // it is there while that is true: a fact, or any expression
    body { … };  box close { … }
    on press close { open = false }
}
```

They all **share properties, facts, models and rules**: a bar and its panel talk through a fact, without a round trip through the system and without knowing about each other. Inside, each one looks at a different slice of the same plane, just like a popup. A closed surface is not seen and cannot be pressed. `open:` takes any expression, so a surface can stay while what it holds finishes leaving: `open: tuck > 0.01`, with `tuck` a spring, closes the window once the thing has slid out of it and not before. **A named surface starts clean**: a loose `clip` of the scene ends where a named surface begins, instead of reaching into another window.

**A surface does not grow with what it draws**, and a shadow counts: `shadow: 0, 18, 44` reaches 62 px below its shape, so a card that ends 20 px from the bottom edge has its shadow cut there, and the cut is a straight line where a fade should be. Declare the surface with that room; the input region lets the click through the empty part, so nothing is lost by declaring it large. When a shape that is well clear of an edge has its shadow cut against it, the renderer says so, once, as soon as nothing is moving:

```
render · a shadow is cut: it needs 30 px below more than this 760 x 520 surface has. The shape fits; its shadow does not
render · this scene does not keep up: 32.3 ms a frame against the 16.7 the screen gives, and 31.0 of those go in reading the scene, not in drawing it
render · a drawing is cut: it needs 36 px below more than this 820 x 580 surface has. Almost all of it is inside, so it looks like the surface is the one that fell short
```

The second one is for the shape itself, which is what happens when a panel grows
past what its surface has. It is only said of what **almost** fitted —three
quarters of it inside, so a shape that hangs off an edge on purpose is left
alone— and never of an edge the surface is anchored to, where there is no room
to ask for. It waits until it has been cut for three seconds, or until nothing
is moving: crossing an edge on the way somewhere is not a layout mistake.

**Types.** For the renderer everything is numbers; types are for whoever writes and for whoever talks to the scene from outside. A fact is a number, a yes or no (`bool`; with no type, that is what one born `true` or `false` is) or an **enum**: `fact mode: low | normal | critical = normal`. The names of its values are valid in any expression (`mode == critical`, `mode = low` in a rule) and they are their position: `low` is 0. **An enum is compared against its own values, and the compiler checks it**: `mode == fast`, if `fast` belongs to another one, is an error that says which ones are valid; and arithmetic is not done with an enum (`mode + 1` means nothing; with a yes or no it does: `r.separator * 21`). The same name can be in two enums: compared against its fact, each one is its own; on its own, if it means different numbers, it is an error that asks for the long form, `mode.normal`, which is always valid. In a slot of a text, an enum is shown by its name: `"mode: {mode}"` → `mode: critical`. The logic reads and writes them as what they are —`fact.open` is `true`, `fact.mode` is `"critical"`—, and so does `--say`.

The fields of a model have those types and two more: **`image w, h`** —an icon name or a path, and the image that says: `image r.icon { … }` without declaring anything else— and **`list`**, records inside the record:

```
model menu max 8 {
    label: text
    icon: image 20, 20
    kind: plain | checked | danger = plain
    list items max 6 { label: text; enabled: bool = true }
}
for m in menu { …  for it in m.items { text it.label { … } } }
```

**A file that is written again under the same name** —a cover, a capture, a picture made for the moment— is asked for with a version: `"/tmp/cover.jpg?3"`, whatever follows the last `?` being a number. Each new number loads the file again **into the place the last one had** in the atlas, so changing it a thousand times costs the same as once; a new name, instead, takes new room. An image asked for bigger than a quarter of the atlas (2048² real pixels), or wider or taller than 2046, is not put in it: it is a texture of its own, with the file's own pixels —never made bigger on the way, only smaller where it is bigger than it is ever painted— up to what the card allows on a side (8192 at most). A wallpaper comes out as sharp as its file. Each is megabytes on the card, so a live one that goes unseen for 120 frames is let go and loaded again if it comes back.

An inner list is walked with `for it in m.items`, and it has its `m.items.count` and its `m.items.total`. **`list children max 6 depth 2`**, with no block, are records like the outer one, nested inside each other down to that depth (1 to 6): a tree, like the menu of an application. It is walked with as many `for` as levels are to be shown. Everything unfolds on load: 8 × 6 is 48 records, and the ceiling across all the lists of a model is 4096.

**Services, with no logic.** `service` asks the system for something by its name and says which of the fields that service brings it wants. Each field is an ordinary fact or text —of whatever type it is given— with the name in front, and it fills itself whenever the system reports something:

```
permissions { services: "clock" }                      // without permission it is not set up
service clock { time: text = "--:--"; date: text }     // without `as`: clock.time, clock.date
service clock.seconds as tick { second: number }       // with `as`: tick.second
text clock.time { size: 14; color: ink }
```

What each service brings is in the vocabulary (§17), and **asking it for what it does not have is an error on load**, with its "did you mean…?". What the numbers mean:

| | |
| --- | --- |
| `audio.volume` | 0 to 1 |
| `audio.level` · `level` | how loud what plays is right now, 0 to 1: the loudest sample, up to 30 times a second. Not the volume: a silence is 0 whatever the slider says. Its own service, `service audio.level as vu { level: number }`, under the same `services: "audio"`; only with a sound server (PulseAudio or PipeWire) |
| `battery.percent` | 0 to 100 |
| `brightness.present` · `brightness.level` | the screen's backlight, 0 to 1. A machine with none says `present = false` |
| `network.strength` | 0 to 100 |
| `clock.hour` · `minute` · `second` · `day` · `month` · `year` | as they are read |
| `clock.weekday` | 0 is Sunday |
| `clock.time` · `clock.date` | already written out: `10:41`, `Sun 20 Sep` |
| `audio.input` · `audio.input_muted` | the microphone: the same two, for what comes in |
| `network.kind` | `none`, `wired` or `wifi` |
| `polkit.error` | `none`, or `wrong` after an answer that was not it |
| `bluetooth.pairing` | `none`, or what pairing a device asks: `pin`, `passkey`, `confirm`, `display` or `authorize`; `bluetooth.pairing_code` the code to compare or type, `pairing_id` which question it is, and `pairing_address`, `pairing_name` which device. Declaring `pairing` (with `services: "bluetooth.*"`) is what makes pairing ask this scene; a scene that does not declare it is never asked. Answered from the logic, naming the `pairing_id`: `bluetooth.answer`, `bluetooth.confirm`, `bluetooth.cancel` |
| `network.error` | why the last attempt to join a network did not get in: `none` (it did, or none was made), `password` (wrong, or none given), `timeout`, `unsupported`, `busy` or `failed`. With iwd; with NetworkManager it stays `none`, and a network that was not joined is one that never turns `active` |
| the `bool` ones | `muted`, `charging`, `present`, `online`, `playing`, `can_seek` |
 Whatever does not come in a report stays as it was.

**Lists, into a model, with no logic.** What a service reports as a list goes
into a model the scene declares **before** it, record by record and by field
name —as `model.x = …` would from the logic—: its field takes the model's name
as its type. For the services that are only a list —`apps`, `tray`,
`notifications`, `notification_history`— the field is `list`:

```
model nets max 12 { ssid: text; strength: number; known: bool; active: bool }
service network as net { online: bool; networks: nets }

model icons max 12 { key: text; title: text; icon: image 18, 18 }
service tray { list: icons }
```

| service | its lists |
| --- | --- |
| `network` | `networks`: `{ ssid, strength, secure, known, active }`, the strongest first |
| `bluetooth` | `devices`: `{ name, address, paired, connected, battery, icon }` |
| `audio` | `outputs` · `inputs`: `{ id, name, default }`; `apps`: `{ id, name, icon, binary, title, volume, muted, playing }`, what is playing, one per stream |
| `media` | `players`: `{ id, name, playing, chosen }`, by bus name |
| `window` | `list`: `{ id, title, class, monitor, active, minimized, fullscreen, maximized }` (compositors with wlr-foreign-toplevel) |
| `workspaces` | `list`: `{ id, name, windows, monitor, active }` |
| `apps` | `list`: `{ name, exec, icon, id, wmclass, local_name, categories, generic_name, comment, keywords, terminal, actions }`; `actions`: `{ id, name, local_name, exec, icon }` |
| `tray` | `list`: `{ key, id, title, status, icon, menu, tooltip, tooltip_body }` |
| `notifications` | `list`: `{ id, app, title, body, icon, image, urgency, time, actions }` |
| `notification_history` | `list`: the same records, the ones that expired unseen, newest first, the last 50 |

The model only gets the fields it declares; the rest of each record is left
out. `nets.count` is how many fit, `nets.total` how many came.

**Which speaker and which microphone.** `outputs` and `inputs`, above: each one
`{ id, name, default }`. `sys.call("audio.default", id)` switches to one, and the
report comes back with the new `default` set. A desktop sound panel needs this:
without it only the volume of whatever was already there can be moved.

**Which player.** `media` reports one player: the one playing; if none is, the one that played last (a pause does not move it); before anything has played, the first.
`players`, above, is all of them, and `name` is what each calls itself, which
tells two phones through KDE Connect apart where `player` cannot.
`sys.call("media.choose", id)` pins one: it is reported, and `media.toggle`,
`next`, `previous`, `seek` and `step` go to it, while it is there; `sys.call("media.choose", "")`
goes back to that default. The choice is the process's, not the scene's.
`playerctld` is left out: it only mirrors another player.

**Saying goodbye.** `session` is commands only —it reports nothing— and it is
what a desktop needs to close itself: `sys.call("session.lock")`, `"suspend"`,
`"logout"`, `"reboot"`, `"poweroff"`, behind `services: "session.*"`. The
permission matters more here than anywhere else, and for a different reason:
**these do not undo**. The worst an `audio.volume` can do is deafen you for a
second. Underneath they are `loginctl` and `systemctl`.

The permissions are the scene's, and they are the same as `sys.watch`'s: `services: "clock"`. Without them the scene loads all the same, says why on the console and that field stays as it was born. `clock` reports when the minute changes and `clock.seconds` every second; neither of the two asks anybody for the time nor wakes the machine up to see whether it is time yet.

**Files.** A scene has **its own folder**, and does not leave it: no paths, no `..`, like `require`. On Unix the folder is created with mode 0700 (an older one is tightened to it on the next write) and each file `files.write` saves gets 0600, so other users of the machine cannot read what a scene keeps there, though other programs of the same user still can. It is used from the logic, with permission `services: "files"` to read and `"files.write"` to write:

| | |
| --- | --- |
| `sys.ask("files.read", "settings.json")` | whatever it says, or `nil` if it is not there |
| `sys.ask("files.read", n, "json")` | that same thing, already as a table. If the file is broken, it is an error with its place, not a half table |
| `sys.ask("files.exists", n)` · `sys.ask("files.list")` · `sys.ask("files.folder")` | whether it is there · what is there · where |
| `sys.call("files.write", n, text)` · `sys.call("files.remove", n)` | write (all or nothing: first alongside, then into place) · remove |
| `sys.call("files.write", n, { tone = 2 })` | a table is saved as JSON, with its newlines: what is saved can also be read by hand |
| `sys.watch("files:settings.txt", f)` | reports when that file changes, including when somebody else touches it |

**The environment and the clipboard**, with their permissions (`services: "env"`, `"clipboard"`, `"clipboard.set"` to write):

| | |
| --- | --- |
| `sys.ask("env", "HOME")` | an environment variable, or `nil` |
| `sys.ask("clipboard")` | whatever has been copied, as text |
| `sys.call("clipboard.set", t)` | copy that. On Wayland, whoever copies has to stay alive: the platform takes care of that |

A plugin has its own, under its name: what it saves the scene does not see, nor the other way round.

**Talking to another scene.** A shell made of a bar, its notices and an OSD is
better as separate scenes, each one able to restart on its own. One tells
another what `pleamar --say` would, down the same socket and with the same
words: `sys.call("say", "bar", "emit card_landed 3")`, with `services:
"say:bar"`, or `"say:*"` for any scene. What the other one answers is not
brought back. A scene that is not running is an error for the caller, which
names the ones that are, and so is an order it does not understand. One call
is one command, on one line, under 4096 bytes. The scene reached is the one `--say` reaches: the one shown on
the caller's display, or, with none there, one shown on another, which the
caller's log then says. Finding it, telling it and hearing back take about a
second at most: a scene that has not taken the order by then is an error, and
is not told it later (unless the error says it may have the order, the second
having run out while it was being written), and a `wait` still waiting then,
or a `watch`, is not an error: it says nothing about
whether what was waited for came. `"say:*"` is shown with a ⚠ at startup: it
can tell any scene anything, `quit` too. Two scenes that each answer the
other's `emit` with one of their own tell each other forever, as they would
with `run("pleamar", "--say", …)`. The two share no state: the one that owns a
value keeps it as a fact, and tells the others with `emit` when it changes.

**One per monitor.** `screens: each [max N]` repeats the surface on every monitor (4 at most, unless said otherwise), and **each copy has its own**: its properties, its zones and its rules. Inside:

| | |
| --- | --- |
| `$screen` | its number, to interpolate into a name: `mon.$screen.active`, like `$i` in a `repeat` |
| `screen.index` | the same, as a number |
| `screen.name` | the name of **its** monitor, set by the renderer (`HDMI-A-1`) |
| `screen.width` · `screen.height` | what **its** monitor measures |
| `screens.count` | how many monitors are showing something |

With the scene's surface (the one with no name), what is repeated is the loose drawing. `--screen A,B` spreads the copies across those monitors, which is how two are rehearsed without having two.

Its loose `prop`s and `fact`s, though, are **one for all the copies**: they are not drawing. So what a `follow` follows must not depend on which copy it is —`follow a = screen.index` ends up as the last copy's in every one—; say which monitor shows something in its `show:` instead, which is drawing and is read per copy.

```
surface { size: full, 44; anchor: top; screens: each }
model mon max 4 { active: number = 1; title: text }     // one record per monitor, from the logic
text mon.$screen.title { … }
repeat i in 1..10 { Desk(i, mon.$screen.active) { show: ws.$i.there } }
```

**An ordinary window.** `kind: window` asks for a window with its frame and its cross, one of those the compositor places, instead of a panel stuck to an edge; `title:` is what it shows. What belongs to a panel —`anchor`, `level`, `reserve`, `screens`, `margin`— is no good to it, and **`screen.width` and `screen.height` are what it measures**, not its monitor: they change when somebody stretches it, so a window is drawn against them.

```
surface { size: 460, 320;  kind: window;  title: "pleamar · settings" }
box { from: 0, 0;  size: screen.width, screen.height;  color: coal }
```

**`surface`**: `size: width, height` (`full` as the width is the whole monitor, and as the height too: `size: full, full` is all of it; a named surface publishes what it really measures as `name.width` and `name.height`, which with `full` is the only way to know it) · `kind:` `panel` `window` `lock` · `title:` (a window only) · `app_id: "org.me.notes"` (a window only: its name to the desktop, which is what its `.desktop` file is called, so the dock gives it that file's icon and groups its windows; without it, every window of pleamar is `pleamar`) · `anchor:` `top` `bottom` `left` `right` `top_left` `top_right` `bottom_left` `bottom_right` `center` — or **the name of a fact whose values are anchors** (`fact corner: top_left | top_right = top_right`, `anchor: corner`), and then it moves from edge to edge while it runs, without being recreated · `margin: n` or `top, right, bottom, left` · `level:` `background` `bottom` `top` `overlay` — or two, **`level: top, overlay while open`**: the first as a rule, the second while that holds, changed on the fly (a panel above the rest while it has something open, so a full-screen catcher can sit under it; in its place the rest of the time, so a full-screen video still covers it) · `reserve: n` (the room windows leave it; a surface `size: full, full` anchored to one edge keeps it too —a bar and its panels on one surface as tall as the monitor—: it sticks to that edge and the two across it, not to all four, where the compositor would ignore any reserve), or **`reserve: n while expr`**, only while that holds (a shell that lets you choose whether windows go under it or leave it its strip) · `rate: 60` (at most that many frames a second, on any monitor: what a scene costs is then the same on a 60 Hz screen and on a 165 Hz one; without it, the monitor's), or **`rate: 60, 20 while asleep`**, fewer still while that holds (a character breathing in its sleep, a clock nobody looks at: slow motion does not need sixty frames a second, and each one is paid for) — and as many steps as the scene has paces, each lower than the one before it (`rate: 60, 20 while asleep, 5 while away`): **the first whose condition holds is the one**, so they are written from the busiest down. **Each surface has its own**: the scene's step is that of its fastest open surface (the monitor's, if one of them says none), and a slower one skips the rounds in between —what changed meanwhile is painted when its turn comes—; a surface that does not write `rate:` keeps the first one's. A slow surface saves its painting, not the scene's rounds: a background breathing at 15 beside a bar at 60 still goes round at 60, so a shell that rests should lower the first surface's `rate:` while it is still (`rate: 60, 15 while calm`) · `screens: all` or `"HDMI-A-1", "DP-3"` · `captures:` `shown` `hidden` (hidden: seen on the monitor but left out of what is captured of it —a screenshot, a recording, a shared screen, a remote desktop—, for what is meant only for whoever sits in front of it; pleamar-wm does it, other compositors show it in captures too) · `agent: hidden` (what it holds is left out of what the scene tells an agent: §13, *Told to an agent*) · `keyboard:` `none` `on_demand` `exclusive`, and with `while expr` it only asks for it while that is true. **Each surface by its own**: a surface with its own `while` obeys only it (a bar asking for it while its calendar is open, a finder while it is shown); one that writes `keyboard:` without a `while` follows the scene's condition (the last `while` written, as before); one that does not write `keyboard:` asks for it as the first surface does, its mode and its condition (so a `while` on a sheet does not become the bar's). When it comes, the field typed into is one on a surface that asked for it exclusively, if one did: a sheet with no field of its own gets the keys itself (Esc reaches its rules), not the first field drawn elsewhere. With `on_demand` the compositor hands the keyboard over on a click; so when the scene does `focus` on a field without having the keyboard —a search opened from a shortcut, which nobody clicked— it is asked for as `exclusive` for as long as the `while` holds, and let go when it stops: that is how a launcher behaves, and Esc closing it is what gives it back.

**A lock screen: `kind: lock`.** It is not a surface painted over everything: it is `ext-session-lock`, where the *compositor* guarantees that nothing else is seen or touched while it lasts, on every monitor. So it does not exist until its `open:` is true —which is mandatory: without it the session would be locked from the start— and it goes when `open:` stops being true. Its `size:` is the box that gets centred on each monitor; what lies around it shows too, so paint the backdrop large. `lock.held`, a name that always exists, is 1 once the compositor **confirms** the session is locked: a drawn padlock certifies nothing, this does. The password goes in an `input` with `secret: true` and is checked by the logic, `sys.ask("auth.check", text.password)`. It has one copy per monitor, and its `name.width` and `name.height` are the **largest** of them, each way: a backdrop that big, centred on the box, covers every monitor, a portrait one beside a landscape one included. `screens:` puts it only on those monitors (`screens: "DP-2"`): the protocol still wants every monitor covered, so the others get plain black, and the pointer over them touches nothing. If none of those is plugged in, it goes on all of them: a lock with nowhere to type the password would be no way back.

```plm
language 0.1
scene Lock {
    surface { size: 200, 50; anchor: top }
    fact locked = false
    surface screen_lock {
        kind: lock
        size: 600, 300
        open: locked
        box { from: -4000, -4000; size: 8600, 8300; color: #101214 }
        text "locked" { at: 300, 140; anchor: center; size: 26; color: #ffffff; opacity: lock.held }
    }
    box lock_it { at: 100, 25; size: 180, 36; corner: 12; color: #9ed6bd; cursor: pointer }
    on press lock_it { locked = true }
    on still locked for 5s while locked { locked = false }
}
```

If whoever locks dies with the lock held, the session **stays locked**. That is on purpose —the opposite would mean killing the locker unlocks— and it is the reason to try a lock screen inside a nested compositor first, never against a live session.

**Asking for the password: the `polkit` service.** When a program wants to do something as root —mounting a disk, a system tool, `pkexec`— somebody has to ask whoever sits there for the password: the session's *authentication agent*. A scene that declares `service polkit` **is** that agent while it runs, and draws the dialog itself. `polkit.ready` says it got to be it (there is one per session: with another program's already there, it stays false). It **stays** the agent across polkit's restarts: when polkit leaves the system bus `ready` falls, and the scene registers again as soon as polkit is back; while another agent holds the session, it asks again now and then (every few seconds at first, at most a minute apart) and takes the slot when it frees, saying so once in the log. `polkit.pending`, that a program is waiting to be let through, and then `message` is what polkit says is wanted, `action` its name and `user` whose password it takes; `polkit.asking`, that an answer is awaited —not while the last one is being checked—, with `prompt` what is asked («Password: ») and `secret` whether to hide what is typed; `info`, what the system says meanwhile (a finger on the reader); `polkit.error`, `none` or `wrong` after an answer that was not it, which is asked again, three times at most. The answer is the logic's to carry: `sys.call("polkit.answer", text.answer)` and `sys.call("polkit.cancel")`, with `services: "polkit.*"`. **The scene is the messenger**: the answer goes to polkit's own helper, which asks the system and tells polkit; pleamar never learns whether it was right other than by being told, and gains no privilege. Mark the field and the button that lets a program through `agent: no`: that is for a person to do. The whole dialog is in `examples/password.plm`.

## 7. Expressions

They are numbers. **True is more than 0.5**; `true` is 1 and `false` is 0. The renderer evaluates them, on every frame that needs it.

From weakest to strongest: `or` · `and` · `not` · `< > <= >= == !=` (they do not chain: `a < b < c` is not valid) · `+ -` · `* /` · `-` in front. `==` means "equal to within a thousandth": these are numbers with a decimal point, and a spring never quite arrives.

| Function | |
| --- | --- |
| `min(a, b)` `max(a, b)` `abs(x)` | |
| `floor(x)` `ceil(x)` | to the integer below or the one above |
| `sin(deg)` `cos(deg)` | sine and cosine, **in degrees, as a plain number**: what it takes to put something on an arc. Animate the angle and not the x and the y, and the thing travels along the arc instead of cutting across it. `sin(30deg)` is an error: `deg` turns a number into radians, which is what `rotate` and `span` want —and so `atan2(…) * 1deg` is how an angle worked out here turns something— |
| `clamp(x, a, b)` | x, between a and b |
| `smooth(a, b, x)` | from 0 to 1 while x goes from a to b, easing in and out |
| `mix(a, b, t)` | between a and b. Also between two colors |
| `if(cond, a, b)` | a if the condition holds, b if not: only that side is evaluated. With a spring as the condition (`if(hot, a, b)`), it goes between the two, like `mix(b, a, hot)`. Also with two colors: `if(urgent, amber, mint)` |
| `vel(prop)` | the velocity of a spring, which only the renderer knows |
| `sqrt(x)` `pow(a, b)` `exp(x)` `log(x)` | square root (of 0 or more), power, e to the x, natural logarithm. A power that would not be a number (`pow(-8, 0.5)`) is 0 |
| `tan(deg)` `atan2(y, x)` | tangent, and the angle of the point (x, y): **in degrees**, like `sin` and `cos`. `atan2(pointer.y - cy, pointer.x - cx)` is where the mouse is, seen from (cx, cy) |
| `length(x, y)` | how long the vector (x, y) is: `length(pointer.x - cx, pointer.y - cy)` is how far the mouse is |
| `pick(i, a, b, c…)` | the one at place `i` (0 is the first; rounded, and kept within the list). With an enum fact it reads as a table: `pick(mode, 40, 150, 260)`. It also chooses **colours** —`color: pick(mode, #9ed6bd, #f0b85a, #ef7a66)`— and **texts** —`text pick(skin, "Liquid", "Light liquid", "Classic")`, each one translated like any other, or the name of a live text—: one text instead of three with `show:`. Two texts by a condition read better as `if`: `text if(online, "wifi", "wifi_off")` is `pick(online, "wifi_off", "wifi")` |
| `rgb(r, g, b)` | a **colour** from three numbers, 0 to 1 each, any of which can be a fact or a property: `let ink = rgb(pal_r, pal_g, pal_b)`. Where `mix` and `pick` choose among colours the scene already has, this one lets the logic hand over a colour that did not exist when the scene was read (a palette taken from the wallpaper), by setting three facts, without a file to rewrite and a scene to reload. Only as a colour: it is not a number |
| `fract(x)` `mod(a, b)` | the part after the point, and the remainder **always positive**: `mod(-1, 3)` is 2, which is what something going round in a circle needs |
| `round(x)` `sign(x)` | to the nearest integer; −1, 0 or 1 |
| `noise(x)` `noise(x, y)` | smooth noise from −1 to 1: the same input, the same value, and it never jumps. `noise(time)` is a wobble that never repeats; `noise(k * 0.3, time)` a different one for each `k` |
| `random(seed)` | a number from 0 to 1 that looks random and is always the same for the same seed: in a `repeat`, `random(k)` scatters without anything moving |

**`time`** is the seconds since the scene started. It exists if the scene names it —and has not declared a `time` of its own—, and while it does, the scene never rests: that is its point. With reduced motion (`--reduced-motion`) it stops, like `spin`.

**`motion.reduced`** and **`color.scheme`** are what the system says about motion and colour, in every scene. `motion.reduced` is 1 when it asks for less motion, and then the scene is as with `--reduced-motion` **from that frame on, without starting again**: every spring settles where it was going, and from then on at once; the loops go quiet. Back to 0, it moves again. `color.scheme` is 0 with no preference, 1 dark, 2 light: nothing changes by itself, a scene reads it (`color: if(color.scheme == 2, paper, coal)`). Both come from the desktop portal (`org.freedesktop.portal.Settings`, `org.freedesktop.appearance`: `reduced-motion` and `color-scheme`), at the start and each time they change; where the portal does not say `reduced-motion` —GTK's backend—, GNOME's `enable-animations` says it the other way round. Only read: pleamar changes no setting. Without a portal both stay 0. They are facts like any other: the logic or `--say … fact motion.reduced 1` can set them too (a shell's own switch), and the next change in the system says its own again. `--reduced-motion` keeps it on whatever they say; `PLEAMAR_NO_PORTAL=1` does not ask the portal.

Valid as a name: a `let`, a `prop`, a `fact`, a measure (`label.width`), how much a named layout takes up and how many children it has in view (`list.width`, `list.height`, `list.count`, and with `view:` `list.content`: they can also be read before the point where it is declared, and by rules), the numeric field of a record (`r.depth`, `r.index`, `rows.count`), and the presence of a claim (`shape.rec`: 1 while it wins).

**Reading by place: `a[i]`.** `a[i]` is the element of `a` at place `i`, and it reads any name with numbers in it the same way: `win[i].screen` is `win.$i.screen`, `screen[s].width` is `screen.$s.width`, `win.reserved[s].top`, `win.order[p]`. With a place known when loading (a number, a `repeat`'s counter) it is that name; with one that changes, a `pick` over every element, which evaluates only the one chosen. Before the first element or past the last, it is **-1**, also with a place known when loading (`win[win.max].screen`): `ord[p - 1]` at `p = 0` reads nothing, and says so. Without a declared length (§10), there are as many elements as names that exist from 0 up. Texts are read the same way (§11).

## 8. Drawing

Each element accepts these properties and no others; another one is an error, with a suggestion.

| Element | Properties |
| --- | --- |
| `ellipse` | `at` · `radius` · `scale: sx, sy` |
| `box` | `at: cx, cy` or `from: x, y` · `size: w, h` · `corner` |
| `arc` (like "∩") | `at` · `radius` · `span` (the whole angle it covers) · `width`. It opens **upwards and symmetrically**; a progress ring is `span: p * 360deg` with `rotate: p * 180deg` |
| `line` | `from` · `to` · `width` |
| `path` | `at` (what its points hang from) · `size: w, h` (what it takes up in a layout), and inside it its steps: `move x, y` (once, the first one) · `line x, y` · `curve x, y via cx, cy` · `close`. Closed, it is filled; open, or with `stroke`, it is a line |
| …and every shape | `color` · `opacity` · `rotate` · `stroke` (the outline only) · `blend` (inside a `body`: how much it melts into what came before) · `active` · `cursor` · `carries` · `carry_icon` (below) · `show` · `label` · `agent` · `role` · `value` · `checked` · `selected` (§13, *Told to an agent*) |
| `body` | `color` or `gradient` (below) · `rim` · `light: amount, from_y, height` · `shadow: dx, dy, blur, alpha[, color]` · `border: width, #color` · `glass` · `lens` · `opacity` · `show`, and inside it its shapes, melted into one silhouette |
| `text` | `at` · `anchor` · `width` · `lines` · `size` · `weight` · `color` · `opacity` · `align:` `left` `center` `right` · `line_height` · `family` · `measure` · `show` · and its effects: `gradient` · `outline` · `shadow` · `letter_move` · `letter_opacity` · `letter_scale` (§8.4) · `selectable` · `selection` · `markdown` |
| `image` | `at` (its **top-left corner**, not its centre: it is a rectangle of pixels, not a shape) · `size` · `opacity` · `tint` · `cell: which, columns, rows` (one picture of a sheet: below) · `fit: fill`, `contain` or `cover` (below) · `show` |
| `particles` | `at` · `area` · `count` · `life` · `speed` · `direction` · `spread` · `gravity` · `drag` · `size` · `colors` · `opacity` · `shape` · `emit` or `burst` · `show`: §8.3 |
| `shader` | `at` (its top left corner) · `size: w, h` · `corner` · `opacity` · `show` · `values: a, b, …` (up to eight numbers, any expression) · `colors: c1, c2` (up to two). What it is and how it is written: §8.1 |
| `figure` | `at` (where the piece's centre goes) · `size: w, h` or `scale:` (without either, one unit of the svg is one pixel) · `pivot: x, y` (in the svg's units, from its centre: the point it **turns** around, which does not move it) · `rotate` · `color` (instead of the one in the file) · `opacity` · `blend` · `stroke` · `show` |
| `input` | `at` · `width` · `size` · `weight` · `color` · `opacity` · `family` · `placeholder` · `selection` · `secret` · `passive` · `show` · `label` · `agent` |
| `group` | `pivot` · `rotate` · `scale: s` or `sx, sy` · `move: dx, dy` · `opacity` (they melt as a single thing) · `size` (for whoever lays it out) · `show` · `z` (see below) · and its **effects**: `blur` · `glow` · `saturation` · `brightness` · `contrast` · `hue` · `mask` · `mode` (§8.2) |
| `popup` | `at` (inside the surface) · `size` · `open:` a fact |
| `row` `column` | `at` · `anchor` · `gap` · `padding` · `align:` `start` `center` `end` · `fill` · `corner` · `opacity` · `cursor` · `show` · `size: w, h` · `view: w, h` · `step` · `content` · `wrap: n` |
| `grid` | `at` · `columns` · `width` · `gap` · `row` (the height of every row) · `show` · `opacity`; and on its children, `span`. What it is: below |

`anchor` of a text: `left` `center` `right` and `top` `center` `bottom`, one or both (`anchor: left center`). Of a layout: `left` `center` `right` and `top` `middle` `bottom` —with no anchor, `at` is its top left corner—. `cursor:` `default` `pointer` `text` `grab` `grabbing`, and for stretching and carrying `ew_resize` `ns_resize` `nwse_resize` `nesw_resize` `move`, `not_allowed`, `crosshair`; and any other standard one, with `_` between its words (`help`, `progress`, `wait`, `context_menu`, `zoom_in`, `col_resize`…: the cursor-shape protocol's list). Or **one that changes while it runs**: an expression in which each cursor's name is that cursor, `cursor: if(held, grabbing, grab)` or `cursor: pick(tool, pointer, text, wait)`. It is read while the zone is dragged too —the cursor of a drag is the pressed zone's, wherever the mouse goes—, so a hand can close for as long as it holds something. Inside a `cursor:` a bare cursor name is the cursor before anything of the scene's (`pointer.x` is still the mouse), and an `if` takes one or the other, never a blend: halfway, the nearer one.

**A text's `size:` and `weight:`, and a body's `rim:`, are expressions** like
its `at:` and its `opacity:`: `size: 21 * u` is 21 times `u`, `size: 12 + 4 * lane`
follows `lane` as it changes, `weight: 500 + 200 * if(today, 1, 0)` is bold on one
day, and `rim: 10% + 12% * hover` lights up under the mouse. A text's `lines:` too:
`lines: if(open, 12, 2)` lays it out again when `open` changes, and `lines: 0`
(or anything below one) is as many lines as it has. What is read only
once, when the scene loads —an input's `size:` and a text's `line_height:`—
takes a number or one worked out from numbers and `let`s of numbers
(`40 * 2`, `1.5 * rung`), and says so if it is given something that changes.
The slots that take a plain number —a surface's `margin:` and `reserve:`, an
image's declared size— say so too when something follows it, instead of reading
its first number and dropping the rest.
And any value read with something left after it —`width: 200 200`, `at: 10,
10, 10`, a `;` forgotten between two properties— is an error too, with where
(`this is left over here: what follows the value would be dropped`): until
0.4.20 it was only a note, and the scene loaded drawn without it.

**A shadow can be of any colour, and everything about it is an expression.**
Black if no colour is said, which is what a shadow is on paper. On a desktop of
dark windows a black shadow has nothing to darken and reads as dirt, and a
light halo (`shadow: 0, 0, 8, 22%, #9ed6bd`, no offset, hugging the outline)
paints outside the silhouette, where it shows. Often what a small shape needs
there is its own `rim`, which is light **inside** the silhouette and touches
nothing behind it. And since the offset, the blur and the alpha are expressions
too, a shadow can show up only when there is something to cast one: `shadow: 0,
2 * open, 12 * open, 32% * open` gives a card the shadow of a card and leaves
the thing it grew out of with none. With the count at zero there is no shadow
to work out, and the renderer does not look at it.

**`glass` turns a body into glass**, from `0%` to `100%`, and like everything
else it is an expression, so it can come and go. Three things happen. The fill
becomes a tint: its `color` is still there, but what is behind shows through.
Below `100%` it is partly glass and partly its fill —at `50%`, a glass tinted
half with its colour—, with a lens or without one, and it covers as much as its
`opacity` says: going from `0` to `100%` the plain fill turns into glass with no jump.
The edge catches the light: a thin highlight on the side facing the top left,
a softer one on the opposite side, and the glass getting lighter towards its
edge, like glass seen side-on; all of it comes from where the edge points,
which a signed distance knows. And the body's `shadow` stops showing through
it: only around it. And **it bends what is behind it, like a lens**:
near the edge the background is pulled in: the ray seen through each point
enters the glass through a bevel that is vertical at the edge and eases into
the flat top, bends there by Snell's law (index 1.5) and crosses the glass's
height to what is behind; red, green and blue each bend by their own index,
so a bright edge splits into a faint rainbow where the bevel bends hard. The
bend eases into a limit of half the bevel, so where two edges meet the view
never folds back on itself, and a square corner is cut like a glass block's,
its bend fading out towards the point. The bevel also mirrors a little of what
is round the glass, most where it turns over at the edge. Further in it is
frosted. A bigger piece of glass bends more, as
thicker glass would. Pressing it lights it from the pointer, and over something
bright it tints itself a little more so what is written on it still reads. To
bend the background pleamar needs its pixels, and on Wayland only the
compositor has them: it asks for a screenshot of what is under the surface
(`wlr-screencopy`) —with the surface on top, the only thing it can give— and
works the background out, because it knows exactly what it painted itself:
screenshot = ours + (1 − our alpha) · background. It takes one after presenting,
at most every 50 ms while something moves, and when nothing moves it waits for
something behind to change: still, it costs nothing. The scene can set that
pace (`lens every`, `lens hold while`: see `lens = image` below). `lens: false` keeps the glass
and leaves the bending out: the compositor blurs what is behind it instead,
which costs less; like `glass`, it is an expression, so a setting can switch
it while the scene runs. It works on layers, normal
windows and popups; where each one is on its monitor is asked of Hyprland,
and worked out from the anchor elsewhere. In pleamar-wm's own session the scene
is the whole screen and there is nothing behind it to ask for: right before a
glass is painted, what the scene has painted so far under it (the wallpaper,
the windows) is taken as its background, frosted and bent the same way. And
on any surface, a glass with something of the scene's own drawn under it —a
picture, a text, another shape— sees that as well, in front of what is behind
the surface: it is what the scene painted itself, so it is known exactly, and
a lens over an opaque picture bends the picture. (A glass with nothing of the
scene's under it costs what it did.) Where it cannot be done (another
system, a compositor without screencopy),
**the compositor is asked to blur what is
behind the silhouette** instead, following its shape in 2 px strips —a round thing
gets round blur, not a square—, through the standard `ext-background-effect`
protocol (Hyprland and KWin have it). Nobody has to write a blur rule for the
compositor: the scene asks. Where the compositor cannot do it, the glass is
still a tint with its light, just with nothing blurred behind. How strong the
blur is belongs to the compositor's settings. A loose shape takes `glass` too
(`box { …; color: #4a5057; glass: 100% }`), and so does the `fill` of a `row`
or a `column`: that is how a glass card holds glass plates —a mid grey at 36 %
over dark glass lightens it just enough, and each plate gets its own lit edge.
Inside a `body`, the body says it, not its shapes. Below 30 % of glass (times
opacity) no blur is asked for. The `fill` of a layout takes an opacity after the colour (`fill: ink 9%`):
inside a glass card, the plates are veils like that, not more glass —glass on
glass is what Apple says not to do—.

```
body {
    color: #1b2127
    glass: 100%
    shadow: 0, 10, 28, 30%
    box { at: 250, 160; size: 420, 220; corner: 36 }
    ellipse { at: 520, 90; radius: 46; blend: 26 }
}
```

**And how it is made is yours.** Twelve things go next to `glass` —on a body, a
loose shape or the `fill` of a layout—, all of them expressions, so they can
follow a fact, a spring or the pointer:

| | |
| --- | --- |
| `shine: x, y` | where the light comes from: a point in the scene. The edge that faces it lights up, and moving the shape or the point moves the highlight round the rim. `shine: pointer` is the light in your hand; it turns in steps of 2°, so a big body is not painted again for every pixel the pointer moves. Without it, from the top left |
| `refraction: 100%` | how thick the glass is: how much its edge bends what is behind. `0` is flat glass, `200%` a thick lens |
| `dispersion: 100%` | how far red, green and blue come apart where it bends: the rainbow on the edge (an Abbe number of 10 at `100%`, of 5 at `200%`). `0` is none |
| `dome: 0%` | the middle as a magnifying glass: what is behind looks bigger towards the centre (negative, smaller) |
| `ripple: 100%` | pressing the glass sends a ring of light through it that spreads and fades, flexing it as it goes by. `0` is none; with reduced motion there is none either |
| `frost: 100%` | how frosted it is inside its bevel. `0` is clear glass: what is behind seen as it is, bent only at the edge. `auto` is frosted only where what is behind is busy —words, a window— and clear over a wallpaper, so the words behind never fight the ones on it |
| `smoke: 0%` | smoked glass: what is seen through it is taken down, the brighter the more —a white page to about 40 %, a dark one barely—, its colour kept, so light words on it read over anything |
| `milk: 0%` | milky glass: what is seen through it is lifted towards white, the darker the more, so dark words on it read over anything. Both are numbers, so a light and a dark look can go from one to the other: `smoke: dk; milk: 1 - dk` |
| `tint: 22%` | how much of its own `color` is laid over what is seen through it, and (scaled with it) the extra over something bright or busy that keeps words on it readable. `0` shows what is behind as the blur left it |
| `vivid: 118%` | the saturation of what is seen through it: `100%` as it is |
| `mirror: 100%` | how much its rim mirrors what is around the glass (Fresnel). `0` none |
| `highlight: 100%` | how bright the light caught on its rim is (`shine:` says where it comes from). `0` none |

`frost`, `smoke` and `milk` belong to the glass that bends (`lens`): where
the compositor blurs instead, `frost` has no say in how much, and smoke and
milk are a veil of black or white over its blur.

A single `box` also bends without a crease: the bevel turns its corners the
way a polished edge does, and its highlight stays an even stroke there
instead of pinching into a dot. Without `glass`, any of the five is an error:
there is nothing for it to do.

```plm
scene Lens {
    surface { size: 420, 240 }
    prop held = 0 ~calm
    body {
        color: #1b2127
        glass: 100%
        shine: pointer
        refraction: 150%
        dispersion: 250%
        dome: 40% * held
        box { at: 210, 120; size: 320, 180; corner: 28 }
    }
}
```

**A sheet of pictures: `cell:`.** An image can be a grid of pictures —the
frames of a walk, a set of tiles— and `cell: which, columns, rows` draws one of
them: which one, counted along the rows from 0, and how the sheet is divided
(`rows` left out, it is one row). `which` is an expression, so the clock walks
it —`cell: floor(time * 10), 8`— or the logic does, with a fact; past the last
one it starts again. Declare the image with the size of the **whole sheet** as
it is drawn —eight frames of 64 across are `512, 64`— and draw it with the
size of one. Each picture wants a pixel of nothing round it, as sheets
usually have, or its neighbour shows at its edge when it lands between two
pixels of the screen.

**Its own shape, and a box of another: `0, 0` and `fit:`.** Declared `0, 0`,
an image is the file's own size and shape: its pixels as they are, whatever
the scale (an SVG, its size times the scale). Drawn, it is stretched to its
`size:` (`fit: fill`, the default) unless it says otherwise: `fit: contain`
keeps its shape whole inside the box, `fit: cover` keeps its shape and cuts
what overflows the box, centred. A wallpaper is one picture covering each
monitor, whatever its shape, from the file's own pixels:
`image back = from wall, 0, 0` and `image back { at: 0, 0; size:
screen.width, screen.height; fit: cover }`. The shape `fit:` keeps is the
image's cell: with a declared size, that one (and a sheet's `cell:`).

**Pixel art: `pixels "hero.png"`** instead of `file`. A picture made bigger
is smoothed, which is what a photograph wants and what a 16 × 16 knight does
not: with `pixels` each of its pixels stays a sharp square. Declare it at a
whole number of times its real size, and draw it at the size it is declared.

```plm
scene Walk {
    surface { size: 200, 120 }
    image hero = pixels "pieces/spin.gif", 96, 96
    image hero { at: 52, 12; size: 96, 96; cell: floor(time * 6), 2, 2 }
}
```

**A font of its own: `font file "fonts/pixel.ttf"`.** A scene can bring the
lettering it is drawn with —a game its pixel font, a theme its face— instead of
asking whoever runs it to install it. The file (TrueType or OpenType, `.ttf`,
`.otf` or `.ttc`) is read where the scene is, and its family is then used by
name like any installed one: `family: "Press Start 2P"`. Nothing is put in the
system, and the system's fonts are all still there.

```
font file "fonts/PressStart2P-Regular.ttf"
text "1UP" { at: 16, 16; size: 8; family: "Press Start 2P"; color: #fcfcfc }
```

**A figure is an svg read as geometry, not as a stamp.** An image is rasterised
into an atlas: it shows, but it is a sticker —it melts into nothing, it cannot
be tinted by parts nor animated by layers, and scaling it is pixels—. `figure
hat = file "hat.svg"` reads the same file as **paths**, so each layer is a shape
like any other: inside a `body` it is one silhouette with it, it takes rim,
light and shadow, and it turns around its own pivot.

`figure hat { … }` draws the whole piece and `figure hat.brim { … }` one of its
layers, **in its place inside the piece**: two layers drawn separately still fit
together, which is what lets one of them turn while the others stay. What comes
across is filled and stroked paths with their colour; gradients, masks, filters
and text do not, and it says so when it reads the file instead of pretending.

A **path** is a broken or curved line. `close` closes it, and then it is filled —concave too, and crossing itself too—; unclosed, or with `stroke`, it is a line of that width with round caps. `curve` is a quadratic Bézier, and it is split into as many segments as the detour is long. Inside it is the same signed distance as the other shapes: it melts with `blend`, and it has shadow, rim, light and border like any other.

```
path {
    at: 26, 40;  stroke: 2.5;  color: mint
    move 0, 56;  line 26, 36 + breath * 5;  line 52, 44;  line 78, 12
}
```

A path carries **a single stroke** (one `move`, the first) and up to 192 already flattened points; for several, several `path`. In a layout it has to be told what it takes up with `size:`, because its box is not known until it is evaluated.

**Lists longer than what is unfolded.** A model unfolds its `max` records on load, and that is the ceiling (1024). For a list of thousands, the scene declares only the **window** —what is seen and a little more— and says how long the whole list is:

| | |
| --- | --- |
| `content: total * 34` | in a layout with `view:`, the real length: the scrolling runs over it, not over what is unfolded |
| `for r in rows from first` | copy 0 is record `first` of the real list, so `r.index` is the number that belongs to it |
| `move: 0, first * 34` | puts the copies in their place within the whole list |
| `list.scroll` | besides being read, it **is written** like any property: `on press top { list.scroll: 0 ~calm }` |
| `on change floor(list.scroll / 34) { emit slid(list.scroll) }` | that is how the logic learns it has to be sent another slice, wherever the movement comes from |

A layout with `view:` **is dragged** without declaring anything: on being pressed it notes where it was and follows the mouse, with its spring. And it is grabbed **from inside**: dragging is not the business of the topmost zone, but of any zone that was underneath at the press, like the wheel. That is how a list is moved by grabbing it by one of its rows.

**`size: w, h`** says how big the layout is, instead of it being however much its children came to. Its background and its zone are that size even if the children do not reach it, and —this is what it is for— it is what **`grow:`** shares out. Across the axis it is also the box that `align:` aligns in, so `align: center` in a card with `size: 173, 66` centres its contents in those 66 and not around the tallest child.

**`place:`** on a child puts **it alone** across the layout —its own `align:`—: `start`, `center`, `end`, or a number from 0 (start) to 1 (end) that may change. In a chat's column, `place: if(m.mine, 1, 0)` sends mine to the right and the rest to the left, each bubble as wide as its text. Across, the layout is as wide as its widest child, or what its `size:` or `view:` says —with `view:`, `align:` and `place:` work across what is seen—. A `tight` text in it takes the slot of what it measures.

**`grow: 1`** on a child asks for the room that is left along the layout's axis, divided among those who ask in proportion to what they asked (`grow: 2` takes twice as much). What is left is the layout's size minus its padding, its gaps and whatever the children that do not grow take. Without `size:` on the layout it is an error: there is no *left over* if nobody said of how much.

A child that grows gets **the slot**; what it then draws is still its own business, so a `box` inside a `group` does not stretch. What does follow is **a text**: a growing text, or the texts inside a growing `group` or `column` that did not say their own `width`, take it from there. That is the case that bites — a name running under the switch beside it instead of ending in an ellipsis:

```
row card { size: 164, 66; gap: 10; padding: 12; fill: #1b1b1c; corner: 14
    group { size: 22, 22;  …the icon… }
    column { grow: 1; gap: 2
        text title { size: 13; color: ink }
        text subtitle { size: 11.5; lines: 1; color: #8b8f95 }   // it ends in "…" on its own
    }
    group { size: 40, 23;  …the switch… }
}
```

**`wrap: 5`** turns a layout into a **grid**: five per line and on to the next. The cell is as big as the largest child, and **what is not seen leaves no gap**, so the rest move up, with the layout's spring if it has one. It is what a `Flow` is in Quickshell.

**`grid` is a grid of cells you design with.** It shares its `width` among its
`columns`, with `gap` between them, and places its children left to right
and then down. A child takes one cell, or several with **`span:`** —a number,
which inside a `repeat` may depend on it—. It does not have to say its size:
inside it **`cell.w` and `cell.h`** are the size of its cell, so it is drawn
in its own coordinates, from its corner. With `row:` every row is that tall;
without it, each child says its height (`group { size: cell.w, 90 }`) and a
row is as tall as its tallest child. Its zones go where it goes. With a
`for` inside, a cell the list does not reach is not there —neither seen nor
pressed—, and **its place stays**: the cells never move to fill it, so six
cards that become two leave four empty places, not a reshuffled grid.

And **inside any layout —`row`, `column`, `grid`— a `prop`, a `let`, a
`fact` or a rule is not a child**: it is read where it is written, with the
names of the `repeat` around it. That is what lets each tile carry its own
spring and its own `on press` beside its drawing.

```plm
scene Tiles {
    surface { size: 480, 300 }
    fact chosen = -1
    grid {
        at: 12, 12; columns: 2; gap: 12; width: 456; row: 80
        repeat t in 0..5 {
            group {
                span: if(t == 4, 2, 1)
                on press tile.$t { chosen = t }
                box { at: cell.w / 2, cell.h / 2; size: cell.w, cell.h; corner: 14
                      color: mix(#25282b, #2f3a37, tile.$t.hover) }
                box { at: cell.w / 2, cell.h / 2; size: cell.w, cell.h; corner: 14; stroke: 1.2; color: #9ed6bd
                      opacity: if(chosen == t, 0.9, 0) }
                text pick(t, "Home", "Look", "Language", "Lock", "Wardrobe") { at: 16, 30; anchor: left center; size: 13; color: #f5f7f5 }
                zone box tile.$t { at: cell.w / 2, cell.h / 2; size: cell.w, cell.h; corner: 14; cursor: pointer }
            }
        }
    }
}
```

`examples/long-list` is five thousand rows in sixteen copies: 0.49 ms per frame, and the same sixteen groups and nineteen zones however many there are.

`clip [inset n] shape` clips everything that comes after, to the end of its `group` —or, loose in the scene, up to the next named `surface`—. Up to four nested ones clip by their shape; the outer ones, by their box.

**Gradients.** In a `body`, `gradient:` takes where it goes from and to and then its colors, separated by commas. Each color can say **where it falls** (`sand 40%`); the ones that do not say it are spread evenly. From two to eight.

```
gradient: 0, 0, 0, 44, mint, coal                        // from one point to another
gradient: 0, 0 to 0, 44, mint, sand 30%, #e86a9a, coal   // the same, with stops
gradient: radial 100, 160 radius 60, ink, mint 40%, coal // from a center outwards
```

**A named shape is a zone** if some rule names it, if it carries `active`, or if it was declared with `zone`. A name put there only to read better does not stop a click. A zone inherits the transforms of the groups it is in, and **what is not there —a false `show:`, an `opacity:` that has reached zero, a record that does not exist— is not a zone**: what cannot be seen cannot be pressed. A `row` or `column` with a name is one too: its whole box, underneath those of its children.

**A zone declared with `zone` brings its own two springs**: `hit.hover` goes
from 0 to 1 while the pointer is over it, and `hit.pressed` while it is
pressed on it, each in 140 ms. They are read like any property, without a
rule: `color: mix(ink, mint, hit.hover)`. Only the zones whose `.hover` or
`.pressed` is named get them, and they can be used above the zone —which is
where what it lights goes, since the press belongs to the zone declared last—.
Inside a component or a `repeat` each copy has its own: `touch.hover`,
`hit.$k.hover`.

```plm
scene Chips {
    surface { size: 300, 80 }
    repeat k in 0..3 {
        box { at: 60 + k * 90, 40; size: 80, 34; corner: 17
              color: mix(#2a2d30, #9ed6bd, chip.$k.hover); opacity: 1 - 0.25 * chip.$k.pressed }
        zone box chip.$k { at: 60 + k * 90, 40; size: 80, 34; corner: 17; cursor: pointer }
    }
}
```

### 8.1. The scene's own shaders

What the language did not foresee can still be drawn: a scene can bring its own
shaders, in WGSL —the language wgpu speaks, on Linux, Windows and macOS alike—.
Whoever writes one writes **one function**: a colour for each point of its box.
Everything else is pleamar's: where the box goes, its rounded corners, its
opacity, the clips it is under, and painting it only when something changed.

```
shader aurora = file "shaders/aurora.wgsl"
…
shader aurora { at: 30, 30; size: 300, 200; corner: 20; values: glow, open; colors: mint, #7a6cff }
```

```wgsl
fn shade(s: Shader) -> vec4<f32> {
    let band = exp(-pow((s.uv.y - 0.4 - 0.1 * sin(s.uv.x * 6.0 + s.time)) * 6.0, 2.0));
    return vec4<f32>(mix(s.color.rgb, s.color2.rgb, s.uv.x), band * s.a.x);
}
```

It returns **the colour and how much of it covers** (straight, not premultiplied).
What it receives, in `s`:

| | |
| --- | --- |
| `s.pos` · `s.size` · `s.uv` | the point inside its box, in logical pixels from the top left corner; the size of the box; and the two divided (0 to 1) |
| `s.time` | seconds since the scene started. **Only if it reads it** does the scene keep painting it: a shader that does not read `time` costs nothing while nothing changes. And only **while it is drawn**: hidden (a `show:` that does not hold, an opacity of 0), the clock stands still and the scene rests, unless the scene itself names `time` |
| `s.pointer` · `s.hovered` | where the mouse is, in the same coordinates as `s.pos`, and 1 while it is over the box. Only if it reads them is it painted again when the mouse moves |
| `s.a` · `s.b` | the eight `values:`, four and four (`s.a.x` is the first) |
| `s.color` · `s.color2` | the two `colors:` |
| `s.scale` | real pixels per logical pixel: for lines one real pixel wide |
| `behind(s, at)` · `behind_frosted(s, at)` | **what is behind the surface** at a point (in `s.pos` coordinates), sharp or frosted: the colour, and in its alpha how much of it is known. Calling it is what asks the surface to capture it, like a `lens:` glass |
| `inside(s, at)` | only for a **group's shader** (§8.2, `shader:`): what the group holds at that point, straight colour and alpha. Anywhere else it is transparent |

Three rules, checked when the scene is read, with the file and the line of the
mistake: it has `fn shade(s: Shader) -> vec4<f32>` exactly; it declares no
bindings, no entry points and no variables outside its functions —all it can
read is `s` and `behind`—; and it is valid WGSL. Its names are its own: two
shaders can each have their own `fn wave`. A loop that never ends is still the
author's business; the card's driver will cut it, the scene with it.

What `behind` sees is what is behind the whole surface, so under the box there
must be nothing of ours that covers it: where something of the scene is solid,
it comes back unknown. And the box itself should not cover it all either —an
alpha of 0.88, like the lens— or the next frame does not know what is behind
it. It is read with the same capture as the glass, so the same thing applies:
on a compositor that does not let the screen be captured, it is always unknown. Until
the first capture of its box arrives it is unknown too, and nothing is put in
its place: a glass with a lens is blurred by the compositor meanwhile, a shader
draws what it draws with what it knows.

**The frosting: `blur`.** What a glass shows through it, and what
`behind_frosted` reads, is frosted by a chain of passes the scene can write
itself, in WGSL as well. Without one it gets the frosting pleamar always had: a
gaussian at half size, across and then down.

```
blur = file "shaders/blur.wgsl" {
    passes: down 0.5 repeat 4, up 2 repeat 3, grade 1
    values: radius, 0.02
    reach: 32
}
```

```wgsl
fn down(s: Blur) -> vec4<f32> {
    let o = s.a.x / s.source;
    return (prev(s, s.uv + o) + prev(s, s.uv - o) + prev(s, s.uv + vec2(o.x, -o.y)) + prev(s, s.uv - vec2(o.x, -o.y))) * 0.25;
}
```

Each pass names a function of the file, `fn down(s: Blur) -> vec4<f32>`, and
how big its picture is next to the one before (the first, next to what is
behind): `0.5` halves it, `2` doubles it, `1` keeps it, up to `4`. `repeat n`
runs it n times in a row, and `keep k` (0 to 3) keeps the picture the pass ends
with, for any later pass to read. What the last pass leaves is what the glass
shows. The pictures are 16-bit floats, in premultiplied alpha. The order and
the sizes are said in the scene because they have to be known before a frame
is drawn; only the colour of each pixel is the shader's. What it receives, in
`s`:

| | |
| --- | --- |
| `s.pos` · `s.size` · `s.uv` | the pixel, in real pixels of this pass's picture; the size of that picture; and the two divided (0 to 1) |
| `s.source` | the size of the picture before, which `prev` reads |
| `s.scale` | real pixels per logical pixel |
| `s.index` · `s.count` | which run of a `repeat` this is (from 0), and how many there are |
| `s.step` | which pass of the whole chain this is (from 0) |
| `s.a` · `s.b` | the eight `values:`, four and four: expressions like any other, worked out every frame |
| `prev(s, uv)` · `input(s, uv)` · `kept(s, k, uv)` | the picture of the pass before, the sharp backdrop, and what a pass kept with `keep k` |

`reach:` is how far the frosting reaches past a glass, in logical pixels: how
much of what is around it is captured with it (22 without a `blur`). It is
checked when the scene is read, with the file and the line of the mistake: a
pass that names a function the file does not have, one that is not exactly
`fn name(s: Blur) -> vec4<f32>`, or a size out of range is an error. A scene has
one `blur`.

**What the glass bends: `lens = image`.** A glass with a lens bends what is behind
it, and on a layer surface only the compositor has those pixels: each time
something moves behind the glass, a screencopy, a readback and an upload are
paid. On a weak or hybrid GPU that is most of what a bent frame costs. A scene
can instead say what to bend:

```
lens = image "~/.cache/myshell/wallpaper.png"
```

The glass then bends that picture (the wallpaper, any size, stretched over the
surface) with the rim and the dispersion of any other, and nothing is captured.
The file is looked at again twice a second at most and read again when it
changes, so a program that rewrites it with each new wallpaper is followed; a
half-written file is tried again. `~/` is the home folder, and any other
relative path is next to the scene. The file is not part of the scene: changing
it does not reload it. What it costs: windows never show through the glass, and
a surface that is not the whole monitor is slightly misaligned with it. A scene
has one `lens =`.

`lens = image "file" while expr` bends the picture only while `expr` holds; when
it does not, the glass captures what is behind it, as a scene without `lens =`
does. It is looked at every frame, so a switch in the scene can choose between the
cheap picture and the live windows.

**How often the glass captures: `lens every` and `lens hold while`.** A glass
that bends what is behind it takes at most one capture every 50 ms, whether it
was asked after a frame of the scene or while watching for something to change
behind; each one is a screencopy, a readback and an upload. A scene can make
that slower, or stop it for a while:

```
lens every 50ms + 950ms * slow
lens hold while open > 0.01 and open < 0.99
```

`lens every` is the least time between two captures, an expression looked at
every frame, so a setting can move it while the scene runs: from `16ms` to
`60s`, written with its unit (a constant out of that range is an error). Slower,
the bend trails what moves fast behind the glass; it still catches up with the
last of it once the interval is over. `lens hold while` takes none at all while
its condition holds —a panel opening or closing, so the readback does not
compete with the animation— and one as soon as it stops holding. Neither applies
to a glass bending a picture (`lens = image`), which captures nothing, nor to a
lens's first moments, when it captures back to back to see what is behind it.
A scene has one of each: a second one replaces the first.

**`z:` — what goes on top.** Groups with `z:` that are side by side (in the
same group, or loose in the scene, even from different iterations of a
`repeat`) are drawn from the smallest `z` to the biggest, whatever the order
they were written in; with the same `z`, as written. Their zones follow them:
where two overlap, the press goes to the one drawn on top. It is how windows
stack —the one you touch comes forward: `z: stamp.$i` with a fact the press
bumps— and it moves nothing else: what has no `z:` stays where it was
written. A group with `z:` inside another one with `z:` is an error: only one
level is sorted.

### 8.2. Effects on a group

A `group` can do something to everything it holds, as ONE thing, when it is
blended: what is inside is painted apart and the effect is applied to the whole.

```plm
language 0.1
scene Effects {
    surface { size: 400, 160; anchor: top }
    let mint = #5ef2b0
    group {
        glow: 14, 90%, mint                      // a halo of that colour around it
        text "online" { at: 80, 40; anchor: center; size: 16; weight: 700; color: mint }
    }
    group {
        blur: 6                                  // out of focus
        saturation: 0                            // and grey
        ellipse { at: 200, 40; radius: 20; color: #ff4d6d }
    }
    group {
        mask: 300, 20 to 300, 140                // whole at the top, gone at the bottom
        box { at: 330, 80; size: 80, 120; corner: 12; color: #7a6cff }
    }
}
```

| | |
| --- | --- |
| `blur: r` | out of focus, over `r` pixels |
| `glow: r, amount` · `glow: r, amount, color` | a light spilling from its edges, under what it holds: with a colour, a halo of that colour; without one, a *bloom* of its own colours. `amount` can go over 100 % |
| `saturation: s` | 0 is grey, 1 as it is, more than 1 more vivid |
| `brightness: b` · `contrast: k` | 1 is as it is |
| `hue: angle` | turns every colour round the colour wheel: `hue: 120deg` makes red green |
| `mask: x1, y1 to x2, y2` | whole at the first point, gone at the second, along that line |
| `mask: radial x, y radius r` · `… radius r1 to r2` | whole at the centre (or up to `r1`), gone at `r` (or `r2`) |
| `shader: rain, a, b, …` | one of the scene's own shaders (§8.1) **over what the group holds**: it reads it with `inside(s, at)` and what it returns is seen instead. Its box is the group's, the numbers after the name are `s.a` and `s.b`, and `s.time` runs if it reads it. That is how a window becomes wet glass: drops that bend what the program shows. **`shader: rain, wet while raining`**: only while that holds; the rest of the time the group is painted as if it had none, and without a pass of its own if nothing else asks for one —a shader whose numbers at rest give back what the group holds still costs that pass, the size of the group, every frame—. It goes before the other effects (a `saturation:` greys what the shader returns); `blur` and `glow` read the group as it was. It cannot call `behind` |
| `mode: add` · `mode: screen` · `mode: multiply` | how it blends with what is under it, as one thing. `add` **adds light**: what it holds brightens whatever is under it, also the desktop behind the surface. `screen` lightens —never past white, never darker—; over nothing it is simply itself. `multiply` darkens what **the scene** painted under it, tinting it; the desktop behind the surface is not in pleamar's hands, so over nothing it paints nothing (rather than black). `mode: normal` is the default |

All of them are expressions, so they animate like anything else: `blur: 8 * (1
- open)` brings something into focus as it opens. The mask moves with the group
(`move:`, `rotate:`). Each group with effects is painted into a layer right
before it is blended, one after another, so there can be as many as the scene
wants; what they cost is the pixels they cover, and `blur` and `glow` read 32
points per pixel.

Two groups with effects cannot go **one inside the other** —only the outer one
would get them, and so it is an error—; side by side, as many as needed. A group
with only `opacity` around one with effects is fine: it fades each thing inside
instead of the whole, and the effects keep their layer.

### 8.3. Particles

An emitter: sparks, snow, confetti, a trail. **No particle is kept anywhere**:
each one is worked out on the graphics card from its number and the time —when
it was born, where it went, how it fell—, so thousands cost what their pixels
cost and nothing in the processor.

```plm
language 0.1
scene Fountain {
    surface { size: 300, 260; anchor: top }
    fact open = true
    event pop
    group {
        glow: 8, 90%                                       // they are light: a glow suits them
        particles sparks {
            at: 150, 240; count: 400
            life: 0.8s .. 1.6s; speed: 120 .. 220
            direction: -90deg; spread: 40deg; gravity: 0, 260
            size: 3, 1; colors: #ffd166, #ff4d6d; opacity: 100%, 0%
            shape: spark; emit: open
        }
    }
    particles { at: 150, 120; count: 200; life: 1s .. 2s; speed: 150 .. 380; spread: 140deg
                gravity: 0, 420; drag: 1.4; size: 6, 5; colors: #5ef2b0, #3dd6ff; shape: square; burst: pop }
}
```

| | |
| --- | --- |
| `at: x, y` · `area: w, h` | where they are born: a point, or anywhere in a box that size centred on it. If `at` moves —`at: pointer.x, pointer.y`— what was born stays where it was born: it leaves a trail |
| `count: n` | how many at most, 1 to 4096. A number, not an expression: it is what the card reserves |
| `life: a .. b` | how long each one lives, in seconds, each its own between the two. A steady stream is born at `count / b` per second |
| `speed: a .. b` · `direction: angle` · `spread: angle` | how fast they leave, where to (`-90deg` is up) and how open the fan is (`360deg`, all round, is the default) |
| `gravity: x, y` · `drag: k` | what pulls them, in pixels per second squared, and how much the air brakes them (0: nothing) |
| `size: birth, death` · `colors: birth, death` · `opacity: birth, death` | from one to the other over their life; one value, the same all their life. By default they fade out |
| `shape: dot \| square \| spark` | a disc, a square, or a streak along where it goes that stretches with its speed |
| `emit: condition` | born while it holds —`true` by default—. Switched off, no more are born and the ones in the air finish |
| `burst: event` | instead, all at once each time that event happens |

They carry themselves, so the scene does not rest while one is in the air —and
rests again when the last one dies—. With reduced motion there are none. An
emitter with `shape: spark` inside a group with `glow:` is most of what a
firework is.

### 8.4. Text with effects

A `text` can have a gradient across its letters, an outline and a shadow, and
each letter can move, fade and grow on its own.

```plm
language 0.1
scene Title {
    surface { size: 420, 160; anchor: top }
    prop reveal = 0 ~2s
    follow reveal = 1
    text "PLEAMAR" { at: 210, 50; anchor: center; size: 48; weight: 800; color: #fff
                     gradient: 100, 0 to 320, 0, #5ef2b0, #7a6cff, #ff6f91
                     outline: 2, #0b0f14; shadow: 3, 5, 6, 70%, #000000 }
    text "one letter at a time" { at: 210, 110; anchor: center; size: 18; color: #f5f7f5
                                  letter_opacity: clamp(reveal * letters - letter, 0, 1) }
}
```

| | |
| --- | --- |
| `gradient:` | like a `body`'s (§8): `x1, y1 to x2, y2, colours…` or `radial x, y radius r, colours…`, in the scene's coordinates. It replaces `color` for the fill |
| `outline: width, color` | the letters grown by `width`, in that colour, under them |
| `shadow: dx, dy, blur, alpha` · `…, color` | the letters moved and blurred, under everything; black unless a colour is given |
| `letter_move: dx, dy` · `letter_opacity: o` · `letter_scale: s` | each letter on its own. Inside them, **`letter`** is which one (from 0) and **`letters`** how many there are: `letter_move: 0, sin(time * 300 + letter * 35) * 5` is a wave, `letter_opacity: clamp(reveal * letters - letter, 0, 1)` a typewriter as `reveal` goes from 0 to 1, `letter_scale: 1 + 0.3 * max(0, sin(time * 240 - letter * 25))` a ripple of growth |

A letter scaled up covers its neighbours: scale is around its own centre, the
line does not open up for it. Colour letters (emoji) take the movement, fading
and scale, not the outline, shadow or gradient.

**Text that can be copied.** `selectable: true` lets the mouse select a text,
as in a browser: dragging over its letters (across lines too), a double click
for a word, a third for the whole text; **Ctrl+C** copies it, unless the field
being typed into has a selection of its own. What is selected is painted behind
the letters in `selection:` (a muted blue if not said). A press on its letters
belongs to the selection, not to the zones under it —dragging over the text of
a `view:` list selects instead of scrolling it; the wheel still scrolls—, but a
zone with `cursor: pointer`, a field or something that `carries:` keeps its
click. Over the letters the cursor is the text one. Pressing anywhere else lets
go of the selection.

```
text r.text { at: 2, 2; width: 520; size: 13.5; color: ink; selectable: true; selection: #2f5a50 }
```

**As wide as what it says.** With a `width:`, a text breaks its lines there and takes up that width, even if it says «Ey». With **`tight: true`** it takes up only what its longest line does (up to its width): a bubble around it is as wide as its words. Only a text aligned to the left: centred or to the right, its width is what places it.

```
row { padding: 10; fill: #2f5a50; corner: 14; place: end
    text m.text { width: 420; tight: true; size: 13.5; color: ink }
}
```

**What a model writes.** With `markdown: true` a text shows what its marks mean instead of the marks: `**bold**`, `*italic*` (and `_italic_` outside a word), `` `code` `` in the monospace font, `# a title` in bold, `- a point` as `• a point`, `> a quote` in italics, `[a link](…)` as its words, and a block between ` ``` ` lines in monospace. A star between spaces (`2 * 3`) or an underscore inside a name stays as it is, and `\*` is a star. A mark still open at the end —a model halfway through `**this`— does not show. Everything else reads the text without its marks: its letters (`letter_*`), what is selected and copied, and what an agent is told.

```
text r.text { at: 2, 2; width: 520; size: 13.5; color: ink; markdown: true; selectable: true }
```

## 9. Layouts

`row` and `column` place their children one after another: the place of each one is an expression, so if one grows or disappears, the rest move. With `~spring` in the header, they travel to their place instead of jumping. Every child has to know how much it takes up: a shape with `size` or `radius`, a text (it measures itself), an image, a `group` or a component with `size:`, another layout, or `space n`. `show: expr` on a child decides whether it is there: it takes up room and is seen, or neither of the two.

**Lists longer than the room they have.** `view: w, h` says what is seen; what is inside can be longer and **moves with the wheel**, clipped and never past what there is. `step:` is how much per notch (60 by default), and the layout's spring is the one it travels with. Besides `width`, `height` and `count`, it publishes `list.content` —how much there is— and `list.scroll` —where it is— which is what is needed to paint a little bar alongside:

```
column list { at: 16, 16;  view: 250, 208;  gap: 6
    for r in rows { Row(r) }
}
box { from: 276, 16 + list.scroll / max(list.content, 1) * 208
      size: 4, 208 * 208 / max(list.content, 208);  corner: 2;  color: ink;  opacity: 35% }
```

**`stick: end`** is a chat's, a log's: the list starts at its end and **stays there while what is inside grows**, unless it was scrolled away from it to read; scrolled back to the end, it sticks again. It publishes **`list.pinned`** —1 at the end, 0 when it was moved away—, which is what is needed for a «↓ new» button: `on press down { list.scroll: list.content - list.height ~calm; list.pinned = 1 }`.

```
column chat { at: 16, 16;  view: 268, 360;  stick: end;  gap: 8
    for m in said {
        row { padding: 10; fill: if(m.mine, #2f5a50, #232526); corner: 14
            text m.text { size: 13; width: 200; color: ink; markdown: true }
        }
    }
}
```

With `view:`, outwards it takes up what is seen, not what it carries inside.

`between { box { size: 272, 1; color: ink } }` puts that **between every two children that are there**: if one disappears, so does its line, and there is never one at the start or at the end. **It does not open another gap**: it goes centred in the `gap` that is already between its neighbours, so the distance between two children is the `gap` plus what it takes up. With a single thing inside, that thing says how much it takes up; with several, the `between` acts as a group and says it itself (`between { size: 10, 12; … }`). `between i { … }` gives it its position —1 after the first child, 2 after the second…—, so the first one can be different: `opacity: if(i == 1, 50%, 12%)`. A named layout publishes, besides `list.width` and `list.height`, **`list.count`**: how many children are there right now. All three can also be read before the point where it is declared.

## 10. Components, `repeat`, `for`

`component Name(parameters) { size: w, h; … }` declares; `Name(arguments)` places a copy. `size:` says how much it takes up, for whoever lays it out. What a copy declares —`prop`, named shapes, rules— is its own.

**A component says what it needs.** Each parameter can carry a type and a default value: `component Row(r: record, chosen: event, tone: color = mint, height: number = 30)`.

| Type | What is passed to it | Inside |
| --- | --- | --- |
| `number` | an expression | valid in any expression |
| `color` | `#fff`, a color `let`, `mix(…)` | wherever a color goes |
| `text` | `"in quotes"` (with slots, if it likes) or the name of a live text | `text name { … }`, and in a slot: `"{name}"` |
| `record` | a record: that of a `for`, or `rows.0` | `r.field`, `r.index` |
| `event` | the name of an event of the scene | `emit name(…)` and `on name { … }` talk about **that** event |
| `image` | the name of an image | `image name { … }` |
| `bool` | an expression (`true`, `false`, `count > 3`) | valid in any expression |
| `gesture` | the name of a gesture | `play name` |
| `spring` | the name of a spring, or `spring(170, 12)` | `~name` |

That way a library component does not take it for granted that the scene has an event called in a certain way: it asks for it. Arguments go **by position and then, if wanted, by name** (`Row(r, choose, height: 40)`); from the first one with a name, all with a name. The ones that have a default value can be omitted, and they go at the end. Whatever is missing, extra, repeated or not of the type is an error where it is used, showing the whole signature: `'Row' is missing 'chosen' (an event): it is Row(r: record, chosen: event, tone: color = …)`. The default value is read where the component is used, so `= mint` is the `mint` of that scene.

With no type (`component Dot(tone)`), the parameter is whatever the argument looks like: it is how they used to be written, and it is still valid.

**A slot for children: `children`.** What a copy brings inside its block —besides properties like `show:`— goes wherever its component says `children`:

```
component Card(title: text) {
    size: 300, 40 + inside.height            // as big as whatever is put into it
    body { color: #1b1c1c; box { from: 0, 0; size: 300, 40 + inside.height; corner: 12 } }
    text "{upper(title)}" { at: 12, 18; anchor: left center; size: 11; color: ink }
    column inside { at: 12, 32; gap: 4;  children }
}

Card("Alerts") {
    text title { size: 14; color: ink }      // the scene's `title`, not Card's parameter
    repeat i in 0..2 { text "row {i}" { size: 13; color: ink } }
}
```

Inside a `row` or a `column`, each child takes its place in the layout (and a `repeat` or a `for` from outside unfolds like the ones inside); loose, `children { move: x, y }` is a group. **Children are read with the names of whoever wrote them**: a component neither sees nor treads on what is put into it, and a parameter of its own covers nothing from outside. **Several slots, with names.** A component has at most one `children` with no name and as many with one as it likes: `children header`, `children footer`. In the copy, a block with that name is what goes into that slot —**even if the scene has a component called the same: inside the copy, the slot wins**—, and the rest goes to the one that has no name. A slot cannot be called like a word of the language.

```
Panel {
    header { text "Alerts" { … } }
    for n in notes { text n.title { … } }        // to the `children` with no name
    footer { box ok { … };  box no { … } }
}
```

A slot that does not exist is an error that says which ones there are (`'Panel' has no slot called 'heder': it has header, footer. Did you mean 'header'?`), and so is putting something into a component with no `children`: not a silence.

An error **inside** a component also says where it was used from —`(inside 'Badge', used at scene.plm:6)`—, because often what is wrong is what was passed to it.

`repeat i in 0..5 { … }` unfolds five turns on load (512 at most); inside, `i` is a number and `$i` is substituted into the names. What one at the scene's level declares (`prop glow.$i`, `fact open.$i`, an `event`, a `text`) is declared with the other declarations, so anything can read it wherever it is written; an `image` read again for each monitor's copy is still one image.

**Arrays: `let a[N]` and `a[i] = ...`.** A `let` declared in a `repeat` is that turn's own, and nothing outside it sees it. An array is declared once at the scene's level with its length, and each of its elements is defined **exactly once**, in a `repeat` there (or loose, with a number for its place); anything after it reads them, by place (§7):

```
let tw[win.max]
repeat i in 0..win.max {
    let share = if(win[i].open, 1, 0)
    tw[i] = share * (W - gap) / max(win.on.0, 1)
}
let first = tw[0]
```

The length is read when loading, like a `repeat`'s bounds: a number, a `let` of one, or `win.max`. An element is a `let` called `tw.3`, so the names written out by hand still work. An element missing, defined twice or outside the length is an error. Such a `repeat` is read in two goes: its `let`s and definitions with the scene's `let`s, so that what comes after it can read them, and the rest (its drawing, its rules) where it is written. In a scene copied per monitor, an element that reads its monitor (`screen.width`) is each copy's own, like any other `let`.

**Tables: `let a[N][M]`.** Two places, each element defined once as `a[i][j] = ...` (in a `repeat` inside a `repeat`), and read the same way: `a[s][k]`. Each row is an array of its own (`a.2` has `M` elements), so `a[s]` names a row where an array is expected, and `a.2.5` is the element's `let`.

**Folds: `x = ...` and `x += ...`.** In such a `repeat`, a `let` of the scene declared before it can be given a new value each turn: a sum, a count, the smallest so far and where it was. Nothing changes while the scene runs: each turn is one more `let`, made from the one before, and after the `repeat` the name is the last one. Inside it, each turn reads the value as it is at that point: what is written before a link, its drawing and its rules too, reads the one before it.

```
let total = 0
let best = 99999
let at = -1
let left[win.max]
repeat i in 0..win.max {
    left[i] = total                      // what the ones before it take
    total += win[i].open * wp[i]
    at = if(win[i].open and d[i] < best, i, at)
    best = if(win[i].open, min(best, d[i]), best)
}
```

**Inside a turn or a block** a fold works the same on that turn's own `let`: a window's `repeat` can add up over the others with one inside it, and what is written after the inner `repeat` reads the total.

```
repeat i in 0..win.max {
    let others = 0
    repeat j in 0..win.max {
        others += (j != i and win[j].screen == win[i].screen) * win[j].open
    }
    box { at: 10, 10 + 30 * i; size: 20 + 10 * others, 20; color: #9ed6bd }
}
```

`+=`, `-=` and `*=` are short for `x = x + ...`. An argmin carries two folds, the best value and where it was, and the one that says where is written before the one it compares with.

**Sorting: `sort`.** `let ord[win.max] = sort(i in win by row[i], win[i].place)` puts the slots in order of their keys: by the first, then by the next where it ties, and by slot where all tie. `ord[p]` is the slot at place `p`, and `ord.pos[i]` is the place of slot `i`. It is worked out while the scene runs, once for everyone who reads it, and again only when a key's inputs change. What to sort is `win` (its slots), an array (its length) or `0..N`, as many as the array has places. There is no filter: a leading key does it (`by not win[i].open, ...` puts the closed ones last). A fold over `ord` gives running sums in that order, and a slot reads its own through `ord.pos`:

```
let x = 0
let left[win.max]
repeat p in 0..win.max {
    let s = ord[p]
    x = if(row[s] != row[ord[p - 1]], 0, x)      // a new row starts again
    left[p] = x
    x += wp[s]
}
// slot i starts at left[ord.pos[i]]
```

`for r in rows { … }` unfolds one turn per record that fits in the model. Inside, `r.field` is the field of that record —a text wherever a live text goes, a number in any expression— and `r.index` its position from 0. **Each turn only exists if the list reaches that far.** Valid inside a layout, loose, and inside a `popup`.

### 10.1. Pages: `pages`

A panel with several pages shows one at a time: `pages` says so.

```
pages settings {
    header: 20, 45                       // where the title goes; optional
    page menu "Settings" { … }           // the first is where it starts
    page look "Her look" { … }
    page language "Language" { … }
}
```

**`settings` is a fact** with the pages' names —`menu | look | language`—,
declared from the start wherever the `pages` is written, so anything reads it
and sets it: `on press tile { settings = look }`, or the logic
(`fact.settings = "look"`). Each page **slides in** as it becomes the one —the
first from the left, the others from the right— and fades out as it goes, and
what is not seen is not pressed: its zones are only there while it is. With
**`header: x, y`**, at that point comes the page's title —the one written after
its name, translated like any other text— and, on any page but the first, a
**←** beside it that goes back to the first; **Esc** goes back too.

### 10.2. The pieces that come with pleamar: `pleamar:ui`

`import "pleamar:ui"` brings pleamar's own library, which comes inside the
program: it is there wherever the scene is and whoever runs it. Each piece
answers the mouse by itself —it lights up under the pointer and gives a little
when pressed— and says what happened through the event it is given: what that
does is the scene's business. They are drawn from their own corner, so they go
in a `row`, a `column` or a cell of a `grid`.

| | |
| --- | --- |
| `Switch(on, press)` | a switch: `on` is whether it is on (an expression), `press` the event it fires. `on flip { toggle following }` |
| `Radio(on, press)` | a round choice: the ring, and the dot when `on` |
| `Segmented(value, a, b, press, c: "…", count: 3, width: 240)` | two or three choices in a pill, with the chosen one's background sliding to it; `press` carries which one: `on pick_mode(v) { mode = v }` |
| `Tile(title, value, press, w, h: 106)` | a plate that answers the mouse, with its name, its value and a ›; what goes in its round badge is whatever is put inside it |
| `Card(w, h, padding: 16)` | a plate, and in a column inside it whatever is put inside it |

All of them take `accent:` (the colour of what is on), except `Card`.

```plm
import "pleamar:ui"
scene Pieces {
    surface { size: 480, 300 }
    fact following = false
    fact mode = 1
    event flip
    event pick_mode
    event opened
    on flip { toggle following }
    on pick_mode(v) { mode = v }
    grid {
        at: 12, 12; columns: 2; gap: 12; width: 456; row: 106
        Tile("Where she lives", pick(following, "Home", "Follows you"), opened, cell.w) {
            box { size: 22, 14; corner: 3; stroke: 1.6; color: #f5f7f5 }
        }
        Tile("Language", pick(mode, "System", "English", "Español"), opened, cell.w) {
            text "Aa" { size: 16; weight: 700; color: #f5f7f5 }
        }
    }
    Card(456, 120) {
        move: 12, 130
        row { gap: 12; align: center
            Switch(following, flip)
            text "Follow me" { size: 13; color: #f5f7f5 }
        }
        Segmented(mode, "System", "English", pick_mode, c: "Español", count: 3, width: 240)
    }
}
```

### 10.3. Other programs' windows: `windows`

`windows win max 6` puts a Wayland compositor inside the scene —with
[pleamar-wm](https://github.com/k4ditano/pleamar-wm), which is pleamar with
one inside; plain `pleamar` reads these scenes but holds no windows—. Programs
started with `launch "kitty"` —or by hand, with the `WAYLAND_DISPLAY` that
`win.socket` says— open in it, and each one takes a slot, `win.0` to `win.5`.
Where it goes, how big, how it arrives and how it leaves is the scene's: the
window manager is the `.plm` file, with its springs, rules and zones, and it
reloads on save while the programs in it keep running.

| per slot | |
| --- | --- |
| `win.$i.open` · `win.$i.focused` | whether there is a window in it, and whether it has the keyboard |
| `win.$i.title` · `win.$i.app` | texts: what the window calls itself, and its program (`kitty`) |
| `win.$i.width` · `win.$i.height` | the size it has drawn itself at |
| `win.$i.place` | its turn in the layout of its monitor: 0 leads, −1 if there is none. `promote` changes it |
| `win.$i.screen` | the monitor it is on: which copy of a `screens: each` scene lays it out |
| `win.$i.minimized` | it is put away: its minimize button, the scene (`minimize`), or whoever lists the windows (a dock, Marea). It is left out of the layout's order, as a dialog is; `restore` brings it back |
| `win.$i.floating` | it floats over the layout (`float`): `.place` is −1 and `.among` and `win.on.$s` do not count it, so the others close up as if it were a dialog; it keeps its turn in `win.order`, `.rank` and `win.count` (the keyboard still reaches it). The compositor keeps it: a reload of the scene does not lose it. `tile` puts it back; it ends when the window closes |
| `win.$i.dialog` | it is a dialog: it belongs to another window, or has a size of its own it cannot leave (a message, a file chooser). It is left out of the layout's order (`place` −1, not counted in `win.on`): float it over the rest, at its own size (`ask: 0, 0`) |
| `win.$i.urgent` | it asks for attention: it asked to come forward with nothing of the user's behind it (a message arrived while another window had the keyboard). Until it gets the keyboard. A dock makes its icon hop, as macOS does |
| `win.$i.framed` | it lets the compositor draw its frame (server-side decorations, X11 programs): draw its title bar. False, it draws its own —a GTK 4 header bar, a browser's tabs—: draw none |
| `win.$i.held` · `win.$i.edges` | what it asks from its own title bar or edges while the button is down: 1 to be carried with the mouse, 2 to be stretched by `edges` (1 top, 2 bottom, 4 left, 8 right, added: 10 is the bottom right corner); 0 when the button is let go. Floating windows follow `cursor.x`, `cursor.y` while it lasts |
| `yields: true` (on a zone) | the zone only counts a press that nothing below the scene takes. A window manager that puts programs' widgets under its own scene (pleamar-wm: a clock on the desktop, on layer-shell's bottom level) gives them the press first where the zone is; where there is none, the zone gets it. A desktop's right button over the whole screen, without taking the clicks of the widgets on it. Anywhere else, a zone like any other. It may be an expression |
| `handle: i` · `handle: i, left` (on a zone) | the zone is window `i`'s handle: by which it is carried, or stretched by that edge (`top`, `bottom`, `left`, `right`, `top_left`, `top_right`, `bottom_left`, `bottom_right`). When the window asks from its own title bar or edges (`win.$i.held`), the zone is pressed for it, on the monitor's copy the mouse is over, and let go with the button: its `press`, `drag` and `release` rules are the ones that carry or stretch it, the same as when the zone itself is pressed. An edge that has no zone of its own gets the one that shares the most of it; a zone that is not `active:` is not pressed |
| `win.$i.fullscreen` | it is fullscreen: it asked (a video, a game, F11) or the scene did. Where it goes is still the scene's: draw it over the whole monitor, and ask it for that size |
| `win.$i.workspace` | the workspace it is on, by its turn in its monitor's stack (from 1): the one its monitor showed when it opened, or the one it was sent to. It changes when one above it dries up |
| `win.$i.aside` | it is put aside (`send win(k) aside`): on no workspace —`win.$i.workspace` is 0— and in no monitor's count, not shown and not in a dock; its program keeps running and drawing. Given the keyboard by the compositor (`focus` with `dock programs`, or a program that asks to be brought forward), it comes to the workspace its monitor shows |
| `win.$i.pool` | the same workspace by its identity, which never changes: compare this one to tell whether two windows share a workspace, or whether it is the one shown (`win.pool.$s`) |
| `win.$i.among` | how many windows share its monitor **and** its workspace: what its layout is shared out among. `place` is its turn among them |
| `win.$i.rank` | its turn among all the windows shown, −1 if it is not (another workspace, put away, a dialog) |
| **for all of them** | |
| `win.count` · `win.focus` | how many are shown (on the workspaces their monitors show), and which slot has the keyboard (−1, none) |
| `win.max` | how many slots there are, the `max` of `windows`: a number known when loading, for `repeat i in 0..win.max` and `let tw[win.max]` |
| `win.$i.pending` | opened under `placement manual` and not accepted yet: out of the layout and of the order |
| `win.on.$s` | how many are shown on monitor `s` (0 to 3) |
| `win.shown.$s` · `win.pool.$s` | the workspace monitor `s` shows: its turn, and its identity |
| `win.pools.$s` · `win.used.$s` | how many workspaces monitor `s` has (the one shown among them, even empty), and how many hold windows |
| `win.$i.icon` | the icon of its program, from its `.desktop` (a generic one if nothing installed says it): `image i = from win.$i.icon, 64, 64` |
| `win.docks.$s` · `win.dock.$s.$k.icon` | monitor `s`'s dock: how many items it has (the programs the compositor has pinned, then the others with windows on the workspace it shows, as they opened), and each one's icon. With `windows win max 16 dock programs` the dock is **of the programs running, wherever their windows are** —any monitor, any workspace—, as a Mac's: every monitor's list is that one, an item counts all its program's windows, and every window has its `dockat` |
| `win.dock.$s.$k.windows` · `.focused` · `.pinned` · `.away` | that item's windows there, whether one of them has the keyboard, whether it is pinned, and how many of them are put away |
| `win.$i.dockat` · `win.$i.dockturn` · `win.$i.minat` · `win.mins.$s` | its item in its monitor's dock, its turn among that item's windows, and its turn among the ones put away there (−1: none); how many are put away on monitor `s` |
| `win.dock.$s.$k.name` · `win.dock.$s.$k.badge` | the item's program's name, and how much is unread for it: whoever keeps the notifications says it in `win.badges` (`pleamar-wm --say wm "text win.badges discord:3 telegram:1"`), by any of the names the program goes by |
| `win.reserved.$s.top` · `.right` · `.bottom` · `.left` | what other programs' bars keep on monitor `s` at that edge, in pixels (layer-shell's exclusive zones): lay the windows out around it |
| `win.order.$p` | which slot is at each place among the ones shown: `win.order.0` leads |
| `win.socket` | where programs connect |
| `win.picking` | what the compositor asks the scene to choose, 0 while it asks nothing: 1 a monitor, 2 a window, 3 either (a program wants to share the screen). The scene draws its chooser and answers with `pick` |

`window win.$i { at: x, y; size: w, h }` draws that slot's window in that box.
`ask: w, h` is the size it is told to have, by default `size` (`ask: 0, 0`: the one it chooses; `ask: -1, -1`: none, a picture of it drawn somewhere else —a preview— that must not ask it for another size than the copy that lays it out): let the box
travel on its springs and `ask` be where it is going, and the program is only
asked once, not every frame. While box and `ask` differ, the window is scaled
by as much as its box is; a program that cannot be that small comes out cut at
its box, not squashed. The window is a zone with its name —`on press win.$i`,
`win.$i.hover`— and the mouse and the keys reach it through it: the pointer in
its own pixels, the keys while it has the keyboard, except the ones the scene
has a rule for (`on key Alt+Return`, or `on key Escape while overview` while
that holds), which are the scene's.

| effect | |
| --- | --- |
| `launch "kitty"` · `launch "foot -e htop -p {pid.$i}"` | starts a program so that it opens here. The command is a text like any other: its holes are filled in when the rule fires (a monitor, a window, a fact), so one rule serves where four nearly equal ones were written |
| `focus win.$i` · `focus win(expr)` | the keyboard goes to that window |
| `close win.$i` · `close win(win.focus)` | asks it to close, as its own close button would |
| `promote win.$i` | it goes first in the layout: `place` 0 |
| `send win.$i to 1` · `send win(win.focus) to 0` | to that monitor |
| `swap win.$i with win.$j` · `swap win(a) with win(b)` | they change places: their turn in the layout, and their monitors |
| `minimize win.$i` · `restore win.$i` | put away, and back: the program and whoever lists the windows are told |
| `float win.$i` · `tile win.$i` | one window over the layout, and back into it (`win.$i.floating`); its monitor stays tiled |
| `fullscreen win.$i` · `fullscreen win(win.focus)` | to fullscreen, or back from it: the program is told (it hides its own bars) and `win.$i.fullscreen` says so |
| `workspace 3` · `workspace n on 1` | that workspace shown on the monitor the pointer is on, or on that one —no further than the empty one past the last that holds windows—. The keyboard goes to the window that last had it there; with none there, nobody has it |
| `send win(win.focus) to workspace 2` · `… to workspace 2 on 1` | that window to that workspace, on its monitor or on that one (the same limit) |
| `send win(k) aside` | that window to no workspace (`win.$i.aside`): out of sight and out of every count, alive. A desktop's helpers live there —a terminal of agents working for you— and come forward when asked: `send win(k) to workspace win.shown.0` puts it on one again. With the keyboard, the keyboard stays where it is seen |
| `dock 2 on 0` · `dock k` | that item of monitor `s`'s dock (the one the pointer is on, without `on`): its window —the next one, if one of them has the keyboard; brought back, if put away— or, with none there, its program started. With `dock programs`, the window of its program that had the keyboard last, wherever it is: its workspace is shown with it (and so is that of any window a `focus win…` gives the keyboard to) |
| `dock k on s with drop` · `… pin` · `… unpin` · `… close` · `… new` · `… hide` | the files just dropped on it (`on drop zone`) opened with its program; the program pinned to the dock, or unpinned (the compositor keeps it: pleamar-wm writes its `dock` line); its windows there asked to close; its program started again for another window, whether it has windows or not; its windows there put away |
| `close workspace 2` · `close workspace n on 1` | that workspace gone, from the monitor the pointer is on or from that one: its windows go to the one before it (to the next, from the first), which is shown if this one was. The only one a monitor has stays |
| `send workspace 2 on 0 to 1` · `send workspace n to 1` | the whole workspace, with its windows, to that monitor —from the one the pointer is on, without `on`—: it goes to the end of that one's stack and is shown there; the monitor it left shows the one beside it |
| `pick win.$i` · `pick win(x)` · `pick screen 1` · `pick none` | the answer to `win.picking`: that window, that monitor, or nothing (the program's request is turned down). The compositor stops asking: `win.picking` goes back to 0 |

**Workspaces that stay: `windows win max 16 workspaces kept`.** As they come, workspaces are tide pools (below): an empty one that is not shown dries up, and there is never more than one empty past the last with windows. With `workspaces kept` they are a Mac's desktops: the ones a monitor has are the ones made, empty or not, shown or not. One more is made by going one past the last (`workspace win.pools.0 + 1 on 0`), and one goes only when it is closed (`close workspace n`). `dock programs` and `workspaces kept` go in that order when both are written.

**Where a new window goes: `windows win max 16 placement manual`.** Without it, a window that opens counts at once: it is in `among`, `win.on.$s`, `rank`, `win.order` and `win.count` from its first frame, and the layout makes room for it before the scene has decided where it goes. With it, a window that opens is **pending** (`win.$i.pending` is true) and in none of those, until a rule takes it with **`accept win(i)`**: then it gets its place, and the others make room. What the scene decides in between —a row, a key to sort it by, a side to dock on— is set in that same rule, before `accept`: `on change win.$i.open while win.$i.pending { row.$i = ...; accept win(i) }`. Written with the others, it goes after `dock programs` and before `workspaces kept`.

**Workspaces** are a stack per monitor, each monitor its own, as tide
pools along a shore: there are as many as hold windows, plus the one shown
even when it is empty; one left empty dries up when the monitor shows another,
and the ones below it move up (their `workspace` changes, their `pool` does
not). Past the last one with windows there is always one more to go to.
What is on another workspace is still laid out —`place` and `among` count per
monitor and workspace—, so a scene can draw one leaving and the next arriving
(pleamar-wm's `session.plm` draws a wave crossing the monitor, and changes
the windows it shows when the water covers them: by `pool`, so nothing moves
when the numbers do). With a copy of the scene per monitor (`screens: each`), a window opens on the
monitor the pointer is on, and each copy draws the ones whose `screen` is its
own (`show: win.$i.screen == screen.index`); a window is only asked for a size
by the copy that shows it.

A window that closes leaves its last image in its slot: the scene can see it
leave, fading on a spring, instead of vanishing. Programs that draw with the
GPU hand over their frames on the card as they are; X11 programs open through
XWayland like any other. A whole window manager is in pleamar-wm's `examples/windows.plm`.

```plm
scene Nested {
    surface { size: 900, 600; kind: window; title: "nested" }
    windows win max 2
    // Alone it takes it all; with company, half each, sliding over on a spring.
    prop w0 = 900 ~lively
    follow w0 = if(win.count > 1.5, 450, 900)
    window win.0 { at: 0, 0; size: w0, 600; ask: if(win.count > 1.5, 450, 900), 600; show: win.0.open }
    window win.1 { at: w0, 0; size: 450, 600; show: win.1.open }
    on key Alt+Return { launch "kitty" }
    on key Alt+q { close win(win.focus) }
}
```

### 10.4. A terminal: `terminal`

**A shell of its own, drawn by the scene.** `terminal term { … }` starts the
user's shell (`$SHELL`, or `command:`) on a terminal of its own —a pty— and
draws its screen there, in cells of a monospaced font, with the engine's own
letters: as sharp as any text of the scene, and in its colours. It is the
emulator Alacritty and Zed use (`alacritty_terminal`): colours of 16, 256 and
24 bits, bold, italic, underlines, the other screen of vim and less, its
history, programs that ask for the mouse. The lines and corners of boxes
(`│ ─ ╭ ╮ ╰ ╯ ┼ ═`) and the blocks (`▀ ▄ █ ░ ▒ ▓`) are drawn by hand, so they
meet across cells whatever the font and the line height. It works on Linux,
macOS and Windows.

```plm
scene Term {
    surface { size: 900, 560; kind: window; title: "A terminal"; keyboard: on_demand }
    text term.title = ""
    text term.folder = ""
    fact term.alive = 0
    box { from: 0, 0; size: 900, 560; color: #16181a }
    terminal term { at: 14, 12; size: 872, 506; family: "JetBrains Mono"; font_size: 13.5 }
    text "{term.title} · {term.folder}" { at: 14, 536; size: 11; color: #8a8580 }
    on change term.alive while term.alive == 0 { quit }
}
```

| property | what it is |
|---|---|
| `at` · `size` | its box. Its columns and lines are as many cells as fit: when the box changes, the program in it is told its new size |
| `family` · `font_size` · `line_height` | the font of its cells (a monospaced one; `monospace` if not said), its size —an expression: `font_size: zoom`— and the height of a line in multiples of it (1.25) |
| `command: "htop"` or `"bash", "--norc"` | what runs in it: the user's shell if not said |
| `folder: "~/src"` or `folder: where` | where it starts: a folder between quotes or a text of the scene, read when it starts |
| `env: vars` | what its shell's environment has besides the program's: a text of the scene with `KEY=VALUE` lines, read when it starts (one tab's own name, `PLEAMAR_NAME=…`) |
| `run: expr` | while it holds, the shell lives —drawn or not: a tab behind another keeps its shell—; when it stops holding, the shell is ended. Once more true, a new one starts |
| `colors: ink, paper, c0, …, c15` | the ink, the paper and the sixteen ANSI colours (fewer: the rest are its own). The paper is not painted: a cell of the paper's colour lets the scene's background through, its glass and its mist |
| `cursor: color` · `selection: color` | the cursor's colour (the ink's if not said) and what is selected |
| `opacity` · `show` · `label` · `agent` | as anywhere |

**Who has the keys.** Pressing a terminal gives it the keys, and `focus term`
too (in a rule, or `focus("term")` from the logic): all of them go to its
program, Ctrl+C included, except **the combinations the scene has a rule for**
—`on key Ctrl+Shift+T { emit new_tab }` is the scene's—. A field pressed, or
`blur`, takes them away. Its own: **Ctrl+Shift+C** copies what is selected,
**Ctrl+Shift+V** pastes (between the marks of a bracketed paste if the program
asked for them), **Shift+PageUp** and **Shift+PageDown** go through the
history, and any key that writes brings it back to the end.
A program that asks for the **kitty keyboard protocol** (Claude Code, Neovim,
Helix, fish…) gets it: the keys that are the same in the old way —**Ctrl+Enter**
and **Shift+Enter** are a plain Enter there, Escape is the start of every
sequence— come as `CSI code ; modifiers u`, so it can tell them apart.

**The mouse.** Dragging selects (two clicks a word, three a line), the middle
button pastes, and the wheel goes through the history —or, in a program on
the other screen (less, man), sends it arrows—. A program that asked for the
mouse (vim, htop, a TUI) gets the buttons, the wheel and the dragging
instead; with Shift held, the terminal selects anyway.

**What it says**, where the scene declares it (all optional): `term.title`
(the title its program sets) and `term.folder` (the folder its shell is in)
are texts; `term.alive` (1 while its shell lives), `term.bell` (how many times
it rang), `term.focus` (1 while it has the keys), `term.cols`, `term.rows` and
`term.pid` (its shell's process, 0 once it ended: under pleamar-wm, the number
of its layer, `pleamar-wm term list`) and `term.back` (how many lines back
through its history it is shown: 0 at its end) are facts. A logic hears them as any other: `on("text:term.folder", …)`,
`on("fact:term.alive", …)`. It is also a zone: `on press term`, `term.hover`.

**Told to an agent.** What its screen shows is in what the scene tells an
agent (§13), as text. Under pleamar-wm the shell also goes through its layer
of terminals, so `pleamar-wm agent` reads it and runs commands in it as in any
other terminal.

**Its logic acts on it** with `sys.call("terminal.WHAT", "term", …)`, no
permission needed (they are the scene's own): `copy` what is selected,
`paste` the clipboard, `write` a text as if typed (`"ls\r"`), `forget` its
history, `select_all`, `find` a text up its history without minding capitals
(again: the next one up; it is selected and shown, and an error says it is not
there), `find_newer` the next one down, `find_end` to stop looking. And
`sys.ask("terminal.selection", "term")` answers what is selected in it, or
nil; `sys.ask("terminal.copied", "term", 120)`, what the program in it put on
the clipboard (OSC 52) in the last 120 seconds, or nil: a program that takes
the mouse (Claude Code, vim) selects by itself and copies, and then the
terminal has nothing selected.

**Several windows of one scene** (a terminal's, each its own process) are
told apart from outside with `--answer-as NAME`: `pleamar --say NAME …`
reaches that one.

**A scene that ends: `quit`.** In a rule, `quit` ends the program, as closing
its window would: a terminal whose last shell ended.

## 11. Text with slots

Inside a text in quotes that is the content of a `text` or the argument of a component:

| | |
| --- | --- |
| `{name}` | a live text, or the `text` field of a record |
| `{expr}` · `{expr, n}` | an expression, with n decimals (0 if unsaid) |
| `{expr, time}` | that many seconds, the way a clock writes them: `1:07`, and `1:02:07` past the hour |
| `{upper(name)}` · `{lower(name)}` | that text, in upper or lower case |
| `{win[i].title}` | the live text at that place, as a number is read by its place (§7); outside them, nothing. Also `text win[i].title { … }`, `label: win[i].title` and `{upper(win[i].title)}` |
| `{? … }` | a stretch that is only there if none of the texts inside it is empty |
| `{{` · `}}` | a real brace |

The names of a slot are resolved where the string is written, not where it is used.

### 11.1. Translations

A scene is written in one language —`en`— and **`translations`** give it others. Each
block is a language and, inside, each text as written and what it becomes; slots
included, and they may move:

```
translations es {
    "Control center" = "Centro de control"
    "{n} notices"    = "{n} avisos"
}
```

With at least one block, the fact **`locale`** exists by itself: an enum with `en`
and each translated language, in order (`locale == es`). It starts as the system's
language —`LC_ALL`, `LC_MESSAGES`, `LANG`; `es_ES.UTF-8` is `es`— or `en` if it is not
one of them, and **it can change while the scene runs**: a rule (`locale = es`) or the
logic (`fact.locale = "es"`) switch **every text at once**, without reloading anything.
That is how a Settings page offers a language.

What is translated: the text of a `text "…"`, with its slots; the value of a declared
`text name = "…"` (while the logic has not written something else into it); a
component's string argument; an `input`'s `placeholder`. What the logic writes itself
goes through **`tr("…")`**, which returns it in the language `locale` says, from the same
tables. A text with no letters (`·`, `{n} %`) is not asked for.

The tables can live anywhere in the scene, or in a library that only has them
(`import "lang/es.plm"`): they are read before anything else. A text some language
lacks is shown as written there, and loading the scene says so, once:
`translations · es: 2 texts without a translation, shown as written: «Wi-Fi», «Search»`.
The same text translated twice into a language, a translation into `en`, or a scene
fact of its own called `locale`, are errors.

## 12. Layers

`layer name [~spring] { claims }`. **The first claim that holds wins**, from top to bottom; when it stops holding, the next one is seen, on its own. A claim is a `name` followed by when —`while expr`, `for 700ms after event, other`, `from event until event`, or nothing (the default)— and, if it likes, a block with its choreography: where each property goes, with which spring and with which delay. The destination is an expression evaluated when its turn comes. `layer.claim` is 1 while it wins, and it can be read in any expression.

## 13. Rules

`on trigger { effects }` and `every 2s..7s [while expr] { effects }`.

A `repeat` among the effects unfolds into more effects of the same rule, in order: `on reset { repeat k in 0..16 { tre.w.$k = 0 } }` is one rule with sixteen effects, each seeing what the ones before it set, not sixteen rules.

A fact can be written by its place, as it is read (§7): `tre.x[tre.nw] = tre.ax` sets the one of `tre.x.0`, `tre.x.1`... that `tre.nw` says when the rule fires, and `put[i].x = 0` writes `put.3.x` the same way. The place and the value are worked out once; a place outside them changes nothing.

| Trigger | When |
| --- | --- |
| `press zone` · `press right zone` · `press middle zone` · `press back zone` · `press forward zone` | it is pressed. `back` and `forward` are a mouse's thumb buttons, the ones `key XF86Back` and `key XF86Forward` also hear: `on press back` is only over that zone. A button's word counts only when a zone's name follows it, so a zone called `back` or `right` is still that zone: `on press back { … }`. A scene that uses `right` for anything is never closed by a right click, nor is a `kind: window` or a `kind: lock`: the prototype's emergency exit is only for panels that do not use it |
| `release zone` | what was pressed there is released, wherever the mouse is by then |
| `hold zone for 500ms` | it has been held down that long |
| `enter zone` · `leave zone` | the mouse enters or leaves. A zone that only moves —a row laid out again, a panel that grows— under a pointer that does nothing is not entered or left by it: that is settled the next time the pointer moves, a button goes or the wheel turns (a press always lands on what is under it). One that appears or goes away under it is entered or left at once |
| `hover zone for 320ms` · `away zone for 420ms` · `away button, panel for 320ms` | it has been over it that long; it was over it and has been away that long. Several zones count as one —over any of them is over—: a button and the panel it opens close when the mouse has left both, not when it crosses the gap between them |
| `scroll zone` | the wheel, over any zone underneath it. Read in `wheel` |
| `drag zone` | it moves with the button down; it keeps going even if it leaves, until release. `local.x`, `drag.dx`. Like the wheel, it works for any zone that was underneath at the press, not only the topmost one: that is how a list is dragged by grabbing it by a row |
| `change expr` | that computation stops being worth what it was worth. Being born does not count: it fires on changing |
| `still expr for 1.1s` | that computation has been worth the same for that long. The reverse of `change`, and like it, being born does not count. Every change puts the clock back to zero, so a run —the volume key pressed six times— is one wait and not six, and what it fires happens once when the run ends |
| `key Escape` · `key Ctrl+k` | a key; the surface has to ask for the keyboard. Modifiers: `Ctrl+` `Alt+` `Super+` `Shift+`, in any order. `Shift+` counts with another modifier or a key that writes nothing (`Super+Shift+Left`, `Shift+F5`); a character already says it: `key question`, not `Shift+question`. The mouse's side buttons are `key XF86Back` and `key XF86Forward`, as the keys a keyboard has for them (a window under the pointer still gets them as buttons) |
| `submit field` | Enter inside that `input` |
| `focus` · `blur` | the surface gains or loses the keyboard |
| `drop zone` | something dragged from another application is dropped on it. While it is being carried over the surface, not yet let go, the fact `drag.over` is 1: a scene can open its arms before it arrives |
| `carry zone` | what that zone carries (`carries:`, below) has just been dragged out of it to another program |
| `idle for 14s` | nobody touches anything for that long. In a scene that names `cursor.x`, moving the mouse anywhere on the desktop counts as touching |
| `event_name` | that event happens: the logic emits it, or another rule, or a gesture, or it comes from outside |
| `event_name(v)` | the same, and inside the rule —in its `while` and its effects— `v` is the value it arrived with: `on chosen(v) { mode = v }`. It is how a component that says *which* (a segment, a row) is answered without logic |

**And the other way: `carries:`.** A zone with `carries: "{file.$k.path}"` can be dragged out to another program: pressed and moved more than a few pixels, the compositor takes the drag and whatever it is let go on gets that. A path or an address (`/…`, `file://…`, `https://…`, one per line for several) goes as a list of files or links —a browser uploads it, a file manager copies it— and as text too; anything else, as text. From then on the pointer is the compositor's, so the zone's own `drag` rules stop there; `on carry zone` says it has gone —a panel it came out of can close then, and let the drop reach what is underneath—. It works on any compositor with drag and drop (Hyprland, pleamar-wm); not yet from pleamar-wm's own scene.

**What is in the hand: `carry_icon:`.** A drag has no picture of its own, so without help nothing shows that something is being carried. `carry_icon: "{file.$k.icon}"` on the same zone is the path of a PNG, read when the drag starts and drawn under the pointer until it ends (up to 512 px a side, with its transparency; its top left sits a little up and to the left of the pointer). The scene has to have the picture ready by then, so a path that is empty or not there yet is a drag without one, not a failed drag.

**Told to an agent: `label:` and `agent:`.** A running scene can be asked what there is to read and touch: `pleamar --say notes describe` answers with each surface that is on screen and, inside it, every zone and field by its name, what it is (`button`, `slider`, `field`, `list`, `item` or `region`, from what its rules do), what it says, whether it is inactive or covered by another one, and its box in the surface's pixels, together with the words drawn outside every zone —a title, a status line—, in the order they were written; `describe json`, the same as data. What it says is the texts drawn inside it, or a field's text and its `placeholder`. When that is not enough —an icon with no word in it, a slider whose name is drawn beside it—, **`label:`** says it: a text, with holes if it needs them (`label: "Delete {n.title}"`), a live text, or `pick(k, "Brightness", "Volume")` in a copy. The first time it is asked, a zone that can be pressed and says nothing is named in the log, so that nobody has to guess what it is. The same names are used to **act, as a hand would**: `press save` (and `right`, `middle`, `back`, `forward`, a count), `hold`, `drag knob 0 -40`, `wheel list -3`, `type query some words`, `key escape`. The hand enters the zone, presses and leaves, so its springs move and its rules fire as with a mouse; a row scrolled out of its list is brought into sight first; and the answer is what happened —the events, the facts and texts that changed, the surfaces that opened—. What a person could not do is refused, saying why: inactive, covered, not on screen. And **`wait status == "Saved"`** answers as soon as that holds (`wait dirty == true 2s`; facts, texts and properties by name, `has`, `and`, `or`, `not`), and **`watch`** writes a line for each thing that happens for ten seconds (`watch 30`), and `watch 0 lay. free.` follows only the facts and texts whose names start so, one line per change (`fact lay.0 3`), for a program, for as long as it reads. **`agent: no`** keeps a zone or a field for a person's hand: it is described, but an agent cannot use it, and a field's text is not given; a field with `secret: true` never gives it either. **`agent: hidden`** leaves it out altogether, and on a `surface` everything it holds. When what it is cannot be told from its rules, a zone says it: **`role:`** `button` `toggle` `slider` `tab` `link` `item` `list` `region`; **`value:`** what it is worth, a text (`value: "{volume * 100} %"`) or a sum (`value: volume`; a fact with names says its name); **`checked:`** whether a toggle is on; **`selected:`** whether a row or a tab is the chosen one (`selected: sel == r.index`). A `kind: lock` surface and one with `captures: hidden` are never told. What else comes, and why: [note 12](12-agents.md).

```plm
scene Told {
    surface { size: 300, 120; kind: window; title: "told" }
    fact armed = false
    image trash = icon "user-trash", 24, 24
    box delete { from: 20, 40; size: 40, 40; cursor: pointer; label: "Delete"; agent: no }
    image trash { at: 28, 48; size: 24, 24 }
    on press delete { toggle armed }
}
```

**Any rule accepts `while expr`** at the end of its header: it is looked at at the moment of firing — **at the state the frame began with**, so two rules that fire in the same frame both see the same one, and the one declared last is the one whose value stays. In `idle` and `every` it also decides whether the wait counts.

| Effect | |
| --- | --- |
| `prop: value ~spring after 70ms` | that property heads there. Without `~`, **with its own spring**: the one it was declared with |
| `fact = expr` | evaluated on firing |
| `toggle fact` | |
| `emit event` · `emit event(expr)` | with a payload, which reaches the logic and the rules that read it with `on event(v)` |
| `impulse prop -620` · `impulse pop left * 5` | a shove: it adds to the velocity of the spring. The amount is evaluated on firing, so it can depend on what is going on: a countdown's bounce shrinks with the number left |
| `ripple zone` | a ring of light from the centre of that zone through the glass, without a press or a pointer move; nothing if the zone is hidden, inactive or off screen, or with reduced motion. A new ripple replaces the previous one. Naming a shape here makes it a zone, like any rule that names it: it stops the clicks under it |
| `play gesture` | it asks for it; it will be granted or not, depending on its class |
| `focus field` · `blur` | gives the writing cursor to an `input`, or takes it away. Focusing a field also has the compositor lend the keyboard to the surface (a launcher opened with a shortcut never gets it with `on_demand` otherwise), to the copy the field is on. An `input` with `passive: true` is only that: it takes no key, so the arrows, Tab and Enter still reach the rules. In a field, **Ctrl+V**, **Ctrl+Shift+V** (as in a terminal) and **Shift+Insert** paste plain text; **Ctrl+C** or **Ctrl+Insert** copy what is selected, **Ctrl+X** or **Shift+Delete** cut it, **Ctrl+A** selects it all |
| `text = "words"` · `text = other` | a live text made that at once: words as written (no slots), or what another live text says then. `label = ""` empties it; `label = draft`, after an `input draft`, keeps what was written |

A state change can give the same glass feedback as a click, entirely in the
renderer. In a workspace tab, for example:

```plm
scene Ripple {
    surface { size: 240, 64 }
    fact selected = false
    body { color: #24332d; glass: 100%; box { from: 0, 0; size: 240, 64; corner: 16 } }
    zone box tab { from: 16, 12; size: 80, 40 }
    on change selected while selected { ripple tab }
}
```

The zone's centre follows its current layout and transforms. The effect does
not fire its `on press` rules. The logic can report the state as usual, or a
shortcut can say `pleamar --say ripple "fact selected true"`.

## 14. Movement that carries itself

| | |
| --- | --- |
| `blink eyelid every 2.4s..6s for 170ms` · `blink lid every 5.2s for 120ms` | from 1 to 0 and back, every now and then. Without `..`, always the same wait. The period runs **start to start**: "every 5.2 s" is every 5.2 s, not 5.2 s after the eye closed |
| `wave breath = amplitude at 1.7` | amplitude · sin(1.7 t) |
| `spin angle by 0.9` | += 0.9 per second |
| `follow chip.w = label.width + 32` | chases the expression, with its spring |
| `look gx, gy at cx, cy reach 5, 3.2 within 140 rest rx, ry` | two properties that pull towards the mouse |

With reduced motion (`--reduced-motion`, or `motion.reduced`) the first three go quiet: `blink`
stays open, `wave` rests at the middle of its travel and `spin` stops where it
was. What carries itself is exactly what must not be left going round on its
own. `follow` and `look` are not loops —they chase something— so they stay,
with their springs settling at once like every other one.

## 15. Gestures

A gesture is a timeline over the properties of the pose (`pose`). `gesture name class { frames }`; classes, from weakest to strongest: `ambient` < postures < `reflex` < `asked` < `state`. **A gesture only cuts off another of its own class or lower.** A frame is a duration, and if it likes a curve —one of the named ones, or `bezier(x1, y1, x2, y2)` with the same two control points as CSS's `cubic-bezier`, so a curve from any design tool is copied as is; the x of both go from 0 to 1, the y can overshoot—, `hold 60ms` (holds there) and `emit event` —which fires when the frame **begins**: to say "done", give it a short frame of its own at the end—; its block says where each property goes, and whatever it does not name returns to its base. With no block, it is the return to the base. `posture name while expr { … }` repeats on its own while that is true.

**A spring can be said in time.** `~620ms` is the spring that arrives in 620 ms
and does not bounce: critically damped, worked out from the time asked for.
Animation contracts are written in milliseconds, and this is how they get
transcribed without stopping being springs — they keep their speed, they can be
interrupted halfway, and they survive a hot reload. Measured: `~620ms` reaches
99 % at 617 ms.

`~0ms` is not a spring: the value is there at once and still, with no speed
to carry, and whatever moves it next does so with its own spring. It is how a
thing is put where it already is before it glides away —a window let go
where the mouse left it: `dx: here - there ~0ms`, then `dx: 0 after 30ms`—.
Jumping there with a fast spring instead gives it that speed, and it overshoots.

**A gesture outranks whatever is ambient.** While it is holding a pose, any
`blink`, `wave` or `spin` on that same pose goes quiet, **and its clock stops with
it**: after the gesture, it picks up where it left off instead of firing at once
everything it owed. That is what lets a breath carry its own blink without the
regular one landing on top of it.

## 16. Checked examples

These compile with `./run-tests.sh`.

A list that comes from data, with a component, text with slots, and one rule per row:

```plm
language 0.1
scene Reference1 {
    surface { size: 320, 220; anchor: top; margin: 40 }
    let ink = #f5f7f5
    model notes max 4 { app: text; title: text; body: text; urgency: number = 1 }
    event opened ->

    text "{notes.total} alerts" { at: 20, 20; anchor: left center; size: 15; weight: 600; color: ink }
    column list ~calm { at: 20, 40; gap: 6
        for n in notes { Note(n) }
    }
    component Note(n) {
        size: 280, 40
        prop lit = 0 ~quick
        box hit { from: 0, 0; size: 280, 40; corner: 10; color: mix(#1b1c1c, #2b2d2d, lit); cursor: pointer }
        box { from: 0, 8; size: 3, 24; corner: 1.5; color: mix(#9ed6bd, #e8776a, n.urgency == 2) }
        text "{upper(n.app)}  {n.title}{? · {n.body}}" { at: 12, 20; anchor: left center; size: 13; width: 260; lines: 1; color: ink }
        on enter hit { lit: 1 ~quick }
        on leave hit { lit: 0 ~quick }
        on press hit { emit opened(n.index) }
    }
}
```

A layer that decides, a popup, and the keyboard only while it is needed:

```plm
language 0.1
scene Reference2 {
    surface { size: 300, 60; anchor: top; keyboard: on_demand while open }
    fact open = false
    fact busy = false
    event saved
    prop glow = 0
    prop tint = 0

    layer mood ~quick {
        done    for 600ms after saved { tint: 1 ~lively }
        working while busy            { tint: 0.5 ~calm }
        idle                          { tint: 0 ~slow }
    }
    box button { at: 150, 30; size: 120, 34; corner: 17; color: mix(#2e2f2f, #9ed6bd, max(tint, glow)); cursor: pointer }
    text "Save" { at: 150, 30; anchor: center; size: 14; color: #f5f7f5; opacity: 60% + mood.idle * 40% }

    popup confirm { at: 90, 56; size: 120, 16 + choices.height; open: open
        body { color: #1b1c1c; box { from: 0, 0; size: 120, 16 + choices.height; corner: 10 } }
        column choices { at: 8, 8; gap: 4
            box yes { size: 104, 28; corner: 7; color: #9ed6bd }
            box no  { size: 104, 28; corner: 7; color: #3a3b3b }
        }
    }

    on press button  { toggle open }
    on press yes     { open = false; emit saved }
    on press no      { open = false }
    on key Escape    { open = false }
    on hover button for 200ms while not open { glow: 1 ~quick }
    on leave button  { glow: 0 ~quick }
}
```

A pose, a gesture and movement that carries itself: the face is the example, but it works for anything that has states and moves between them.

```plm
language 0.1
scene Reference3 {
    surface { size: 200, 200; anchor: center }
    pose eyes = 14
    pose look.y = 0
    prop lid = 1
    prop breath = 0
    prop gaze.x = 0 ~gentle
    prop gaze.y = 0 ~gentle
    fact searching = false
    event shutter

    body { color: #151616; rim: 5%; shadow: 0, 8, 24, 30%
        ellipse face { at: 100, 100 + breath; radius: 60 } }
    group {
        clip inset 3 ellipse { at: 100, 100; radius: 60 }
        box { at: 82 + gaze.x, 96 + gaze.y + look.y; size: 9, eyes * lid; corner: 4.5; color: #f5f7f5 }
        box { at: 118 + gaze.x, 96 + gaze.y + look.y; size: 9, eyes * lid; corner: 4.5; color: #f5f7f5 }
    }

    blink lid every 2.4s..6s for 170ms
    wave breath = 1.2 at 1.7
    look gaze.x, gaze.y at 100, 100 reach 6, 4 within 160

    gesture nod reflex {
        130ms out_quad              { look.y: 4; eyes: 10 }
        170ms out_back hold 60ms emit shutter { eyes: 15 }
        160ms
    }
    posture scanning while searching {
        400ms in_out_sine { look.y: -3 }
        400ms in_out_sine { look.y: 3 }
    }
    on press face { play nod }
}
```

## 17. The vocabulary, just as the compiler consults it

This is the output of `pleamar --grammar`, copied. It is not a second list: these are the same tables (`src/language/vocabulary.rs`) the compiler consults to accept or reject a word. `./run-tests.sh` compares this block with what the program prints —if somebody adds a word and does not write it down here, it fails— and it also checks that **every word appears in some test**.

```vocabulary
language: 0.9
statements: surface permissions model service spring prop pose fact event text image figure font shader blur lens particles measure let zone body ellipse box arc line path input clip group popup component children repeat for row column grid pages space between layer on every blink wave spin follow look gesture posture translations windows window terminal
library: let spring component permissions fact text model service event image figure font shader prop pose gesture posture layer translations
properties.surface: size anchor margin level reserve screens keyboard open kind title app_id rate captures agent
properties.permissions: run services env
properties.blur: passes values reach
properties.shape: rotate stroke color opacity blend glass lens shine refraction dispersion dome ripple frost smoke milk tint vivid mirror highlight active show cursor carries carry_icon grow label agent role value checked selected handle yields place
properties.ellipse: at radius scale
properties.box: at from size corner
properties.arc: at radius span width
properties.line: from to width
properties.path: at size
properties.body: color gradient rim light shadow border glass lens shine refraction dispersion dome ripple frost smoke milk tint vivid mirror highlight opacity show
properties.text: at anchor width size weight color opacity lines align line_height family measure show grow gradient outline shadow letter_move letter_opacity letter_scale selectable selection markdown place tight
properties.image: at size opacity tint cell fit show grow place
properties.window: at size ask opacity show
properties.terminal: at size family font_size line_height colors cursor selection command folder env run opacity show label agent place grow
properties.figure: at size scale rotate pivot color opacity blend stroke show grow place
properties.shader: at size corner opacity show values colors grow place
properties.particles: at area count life speed direction spread gravity drag size colors opacity shape emit burst show
properties.input: at width size weight color opacity family placeholder selection secret passive show label agent
properties.group: pivot rotate scale move opacity size show z grow span blur glow saturation brightness contrast hue mask mode shader place
properties.popup: at size open
properties.children: move
properties.grid: at columns gap width row show opacity
properties.layout: at anchor gap padding align fill glass lens shine refraction dispersion dome ripple frost smoke milk tint vivid mirror highlight corner show opacity cursor view step stick content wrap size grow place
functions: min max abs floor ceil sin cos clamp smooth mix if vel sqrt pow fract mod sign round exp log tan atan2 length noise random pick rgb
text_functions: upper lower
triggers: press release scroll drag hold enter leave hover away idle key submit focus blur drop carry change still
effects: toggle emit impulse ripple play focus blur close promote fullscreen minimize restore float tile launch send swap workspace pick dock accept quit
curves: linear in_quad out_quad in_cubic out_cubic in_out_sine out_back bezier
frame: hold emit
classes: ambient reflex asked state
field_types: text number bool image
fact_types: number bool
model: list
path: move line curve close
documented: translations surface permissions model service spring prop pose fact event text image figure font particles shader blur lens measure let zone body ellipse box arc line path input clip group popup component children repeat for row grid windows window terminal pages column space between layer on every blink wave spin follow look ripple gesture posture import scene library include part language
services: clock clock.seconds audio audio.level battery brightness network bluetooth media window thumbnails workspaces apps tray notifications notification_history polkit
services.clock: hour minute second day month year weekday time date
services.clock.seconds: hour minute second day month year weekday time date
services.audio: volume muted input input_muted outputs inputs apps
services.audio.level: level
services.battery: present percent charging
services.brightness: present level
services.network: online kind name strength wifi error networks
services.bluetooth: present powered discovering devices pairing pairing_id pairing_address pairing_name pairing_code
services.media: playing title artist album length position rate can_seek art player players
services.window: title class monitor fullscreen maximized list
services.thumbnails: list capturing
services.workspaces: active list
services.apps: list
services.tray: list
services.notifications: list
services.notification_history: list
services.polkit: ready pending asking message action icon user prompt secret info error
parameter_types: number bool color text record event image gesture spring
springs: lively calm quick slow gentle pose
units: px % deg ms s
cursors: default pointer text grab grabbing ew_resize ns_resize nwse_resize nesw_resize move not_allowed crosshair context_menu help progress wait cell vertical_text alias copy no_drop e_resize n_resize ne_resize nw_resize s_resize se_resize sw_resize w_resize col_resize row_resize all_scroll zoom_in zoom_out dnd_ask all_resize
surface.anchor: top bottom left right top_left top_right bottom_left bottom_right center
surface.level: background bottom top overlay
surface.kind: panel window lock
surface.keyboard: none on_demand exclusive
surface.captures: shown hidden
agent: yes no hidden
role: button toggle slider tab link item list region
text.align: left center right
layout.align: start center end
group.mode: normal add screen multiply
image.fit: fill contain cover
particles.shape: dot square spark
```

`properties.shape` are the ones common to `ellipse`, `box`, `arc` and `line`; `properties.layout`, those of `row` and `column`.

## 17.1. In the editor

`pleamar --lsp` is a language server over the input and the output, with **this same compiler** behind it: the errors with their place while it is being written, which words are valid here, and what the one under the cursor means. `pleamar --highlight vim` and `--highlight vscode` write the syntax file, taken from the vocabulary above. Both, and how they are installed, are in `editor/`.

## 18. What this version does not have

So as not to look for it here: `import { Row } from …`, a line break in the layouts, times and plurals in the slots, and writing to the field of a record from a rule. It is all there, with its plan, in [Known limitations](08-limitations.md).
