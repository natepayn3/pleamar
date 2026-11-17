---
name: pleamar
description: Build anything on the desktop with pleamar — shells (bars, docks, launchers, panels, notification centres, widgets, lock screens), apps and windows, animations and effects, and the pleamar-wm window manager (its look, layouts, key bindings, actions) — from the user's own ~/.config/pleamar. Use when asked to make, change or fix a bar, a widget, a shell, a panel, a launcher, a desktop app, a key binding, a window-manager behaviour or a theme with pleamar, or when touching any .plm or .luau file.
---
<!-- pleamar skill {VERSION}. Written by `pleamar --install-skill` and rewritten when pleamar updates: put your own notes in another skill. -->

# Writing pleamar scenes

pleamar is a desktop runtime in Rust with its own declarative language. A `.plm`
file is a scene: what is drawn and how it reacts. Everything declared there is
run by the **renderer** — springs, rules, layers, gestures — so it keeps moving
even if the logic stalls. An optional `.luau` file next to it is the logic, and
it only reports facts; it never draws.

**It is not QML.** Do not carry over QtQuick names, properties or idioms. There
is no `Rectangle`, no `anchors.fill`, no `Behavior`, no JavaScript in the scene.

## The rule that matters

**Never guess a keyword or a property. Check it.**

```sh
pleamar --grammar        # every word the compiler accepts, by category
pleamar --check x.plm  # reads the scene, says whether it is fine, exits
```

`--grammar` prints the real vocabulary: statements, the properties of each
element, functions, triggers, effects, springs, units, services and what each
service reports. If a word is not in that list, it does not exist. The compiler
suggests the closest one ("did you mean…?"), so a failed `--check` usually
tells you the right word.

After writing or editing any scene, run `--check` on it. Always. It is
instant and it catches everything: unknown names, wrong types, a property that
belongs to another element, a service asked for something it does not report.

## Where the answers are

The documentation travels with the binary, so it always matches the pleamar
that is installed:

```sh
pleamar --docs reference   # the reference: grammar, every element and property, rules, layers, gestures, services, files
pleamar --docs guide       # from zero to a bar, six steps
pleamar --docs recipes     # whole scenes that work: lists, grids, gradients, paths, saved settings, windows, popups
pleamar --docs logic       # what the .luau logic can and cannot do
pleamar --docs measuring   # measuring cost and animations
pleamar --docs limitations # what does not work yet (and what was fixed, when, how it was checked)
pleamar --grammar          # every word the compiler accepts
```

Read the reference before writing anything non-trivial; search it
(`pleamar --docs reference | grep -n -i "shadow"`) instead of guessing.

## Where the user's things go

Everything of the user's lives in **one folder, `~/.config/pleamar/`** —the
one they keep in their dotfiles—. Put what you make there, never in a
repository or a system path:

```text
~/.config/pleamar/
  shells/NAME/NAME.plm    a shell or app of theirs (+ NAME.luau for its logic,
                          + anything it imports, relative to it)
  autostart               what starts with the desktop, one command a line
  theme.plm               a palette their scenes can import, if they want one
  session.conf            pleamar-wm only: monitors, keyboard, pointer, idle
  keys.conf               pleamar-wm only: key bindings and touchpad gestures
  wm/session.plm          pleamar-wm only: their own window manager
```

A shell works on **any Wayland compositor** (Hyprland, Sway, niri, KDE,
pleamar-wm): it is a layer-shell surface. To start it with the desktop, add
`pleamar --scene ~/.config/pleamar/shells/NAME/NAME.plm --no-hud` to
`autostart`; on Hyprland, `exec-once = pleamar --autostart` runs that file
(lines starting with `wm:` only run in pleamar-wm's own session). A scene
reloads itself when its file is saved, so the user sees each change live.

### pleamar-wm, the window manager

When the user runs pleamar-wm (their session is `pleamar-wm session`, and
`pleamar-wm init` makes the folder above), the window manager itself is a
pleamar scene: layouts, window decorations, animations, what happens on drag.

- **Key bindings** go in `keys.conf`, never in the scene:
  `bind Super+b launch zen-browser`, `bind Super+q minimize`,
  `unbind Super+t`, `gesture swipe3_down close`. Start the file with
  `defaults` to keep pleamar-wm's; `pleamar-wm keys` prints them with the list
  of actions (`close`, `minimize`, `restore_last`, `fullscreen`, `toggle_free`,
  `overview`, `focus_next`, `focus_previous`, `move_left/right/up/down`,
  `narrower`, `wider`).
- **An action is an event the window manager's scene declares**, so a new
  behaviour is: declare `event my_action` in the scene, write
  `on my_action { … }`, and bind it in `keys.conf`. Anything can also fire it:
  `pleamar-wm --say wm "emit my_action"`.
- **Changing the window manager** means their own `wm/session.plm`: start from
  a copy of the one that comes with it (`pleamar-wm scene >
  ~/.config/pleamar/wm/session.plm`) and change that; pleamar-wm uses theirs
  when it exists.
- `pleamar-wm config` shows how it understood `session.conf`.
- **Using the windows themselves** —clicking and typing in a browser, an
  editor— is the other skill, `pleamar-desktop`: pleamar-wm gives an agent a
  pointer and a keyboard of its own (`agent on` in `session.conf`,
  `pleamar-wm agent …`).
- **Using this desktop from another computer**, in a browser, is
  `pleamar-wm remote` (`remote setup` for its password and codes, then put
  it behind something that encrypts it, such as Tailscale Funnel). What
  only whoever sits in front should see —its own mark that someone is in—
  goes on a surface with `captures: hidden`, which the picture sent leaves out.

## The shape of a scene

```
language 0.1              // optional, first line: which language version it needs
import "common/palette.plm" // libraries, relative to this file

scene Name {
    surface { size: full, 44; anchor: top }   // the window it asks for
    permissions { services: "clock" }         // without this, the logic can do nothing

    service clock as now { time: text }       // system services, by name
    fact open = false                         // what the logic may report
    text title = "…"
    model rows max 14 { label: text }
    event chosen ->                           // `->` means the logic hears it too
    event moved(x, y) ->                      // an event that says what it carries: every `emit` is checked
    prop x = 360 ~calm                        // something that moves: a spring
    let mint = #9ed6bd                        // a name for an expression or colour

    box { from: 0, 0; size: 200, 44; color: mint }   // what is drawn
    text title { at: 10, 22; anchor: left center; size: 12; color: #f5f7f5 }

    zone box hit { from: 0, 0; size: 200, 44; cursor: pointer }
    on press hit { toggle open }              // rules, run by the renderer
    follow x = if(open, 420, 360)             // movement that carries itself
}
```

## Things that are easy to get wrong

- **Ranges are exclusive at the end.** `repeat i in 1..10` gives 1 to 9. A model
  with `max 16` has records 0 to 15, and its logic fills `1..16` in Lua terms.
- **A `let` is a name for an expression, not a variable**: a small one is
  written out wherever it is named. A big one is computed once a frame instead,
  so chaining them (`let b = a * (1 - t) + 60 * t`) is fine — but keep each link
  naming the one before it **once**, which is what `a * (1 - t) + k * t` does
  and `a + (k - a) * t` does not.
- **`prop` is a spring, not a variable.** Do not set it every frame from the
  logic; declare where it goes (`follow`, a rule, `impulse`) and let it travel.
  **The spring is the property's**: `prop lid = 0 ~150ms` is what `lid: 1` uses,
  and a rule only travels differently if it says so (`lid: 1 ~40ms`).
- **A property belongs to its element.** `corner` is a `box` thing, `radius` an
  `ellipse` thing, `width`/`lines` are `text` things. `--check` lists the
  valid ones when you miss.
- **Inside a layout (`row`/`column`) a child must say how much room it takes.**
  Wrap loose shapes in `group { size: w, h; … }`.
- **`if()` and `mix()` work with colours too**: `if(urgent, amber, mint)`,
  `mix(ink, mint, open)` —a comparison works as `t`—. A colour the logic
  works out at runtime (a palette from the wallpaper) is `rgb(r, g, b)` over
  three facts, 0 to 1: no file to rewrite, no reload.
- **Text with holes** is `text "{a} · {b}"`, where `a` and `b` are live texts or
  facts. For a number with decimals, `text number(expr, 2, " %")`.
- **A surface does not grow with what it holds, and a shadow needs room.**
  `shadow: 0, 18, 44` reaches 62 px past its shape: leave it in `surface {
  size: … }` or the shadow is cut into a straight line. A panel that grows is
  the usual way to find this out: the renderer warns once, both for the shadow
  ("a shadow is cut: it needs 30 px below…") and for the drawing itself ("a
  drawing is cut: it needs 36 px below…"), once it has been cut for three
  seconds and never against an edge the surface is glued to. Only while it
  runs, though, never in `--check`: where a card ends is a sum that exists
  only while the scene is alive.
  `pleamar --layout x.plm` paints it a moment with no screen and says it in
  JSON (its last line), with texts drawn over each other, past their surface or
  hard to read: run it after `--check` on anything with text in it.
- **Zones are what catch the mouse.** A named shape only becomes a zone if a rule
  names it, if it carries `active`, or if it is declared with `zone`.
- **Say what a zone is when no word is drawn in it.** An agent or a screen
  reader reads a scene with `pleamar --say NAME describe`: every zone by its
  name, what it is and what it says, which is the texts drawn inside it. An
  icon-only button says nothing, and the log names it the first time someone
  asks: give it `label: "Close"` (or `label: pick(k, "Brightness", "Volume")`
  in a copy). Put `agent: no` on what only a person's hand should press
  —pay, delete, send—, and `agent: hidden` on what is private.
- **A press goes to the zone declared LAST**, not to the smallest one. So a
  grace zone —the big invisible rectangle that keeps a panel open while the
  pointer crosses a gap— goes **before** what it wraps, or it swallows every
  click inside it. This is the mistake that repeats: in marea-plm it happened
  five times, always the same way, and every time it looked like "the button
  does nothing". What is **hidden** does not catch it, though: a `show:` that is
  false, or an `opacity:` that has reached zero —on a group, on a layout—
  turns off the zones inside it. No need to repeat `active: open > 0.9` on
  every zone of a panel that fades in; keep `active:` for what is visible and
  still must not be pressed (a button mid-transition, a confirm that arms late).
- **Two rules in the same frame: the one declared LAST sets the value, and a
  `while` reads the frame BEFORE them.** A pointer that jumps from one row to
  another gives `leave` on the old one and `enter` on the new one in the same
  frame, so whatever clears has to be declared **before** whatever sets, or the
  row just entered is cleared by the row just left. Guarding it (`while thing ==
  what_it_set`) does not save it: the guard is reading the old value.
- **A loose `clip` reaches further than it looks**: it clips everything after it
  until the next named `surface`, not until the end of the block it seems to
  belong to. Something written further down comes out clipped to a shape that
  may be closed —that is, it does not come out at all—. Inside a `group`, it
  ends with the group.
- **A shadow is black unless you say otherwise** (`shadow: dx, dy, blur, alpha,
  colour`), and on a desktop of dark windows a black shadow has nothing to
  darken: it reads as a dirty ring around the thing it was meant to lift. What a
  small shape usually needs there is its own `rim`, which is light **inside** the
  silhouette and touches nothing behind it. Everything about a shadow is an
  expression, so it can show up only when there is something to cast one:
  `shadow: 0, 2 * open, 12 * open, 32% * open`.
- **An svg is `figure`, not `image`.** `image` rasterises it into an atlas —a
  sticker: it melts into nothing, it cannot be tinted by parts nor animated by
  layers—. `figure hat = file "hat.svg"` reads the same file as paths, and
  `figure hat.brim { … }` draws one layer (the `id` of its group in the file) in
  its place inside the piece, so two layers drawn apart still fit together.
- **Permissions are per service and listening is not commanding**:
  `services: "audio"` lets the logic know the volume; changing it needs
  `"audio.volume"` or `"audio.*"`.

## A shell of several scenes: `import scene`

A bar, its notices and an OSD are better as **separate scenes** —each opens and
restarts alone— and can still run **in one process** (one render, one graphics
card, one connection to the compositor). The scene that is opened brings the
others in:

```
import scene "bar.plm" as Bar           // shell.plm: nothing of its own needed
import scene "notices.plm" as Notices
scene Shell { }
```

```
uses scene "bar.plm" as Bar             // notices.plm: it talks to Bar, and still opens alone
scene Notices {
    surface toast { size: 320, 90; anchor: top_right }
    box card { from: 10, 10; size: 300, 70; corner: 14; color: #23272cf2 }
    text "{Bar.unread} unread" { at: 160, 45; anchor: center; size: 12; color: #f5f7f5 }
    on press card { emit Bar.landed(id: 7, x: local.x) }
}
```

- Brought in, **everything a scene declares is under its name**: its `unread` is
  `Bar.unread`, its zone `hit` is `Bar.hit`, its unnamed surface is `Bar`.
- It **reads** another's facts, texts and models (`Bar.unread`), hears its
  events (`on Bar.landed(c)`) and emits them (`emit Bar.landed(id: 7, x: 0)`):
  all checked when loading, at both ends. It **writes only its own**: to change
  another's value, emit an event that its owner answers.
- `uses scene "bar.plm" as Bar` is what lets a scene that names another **open
  alone**: then Bar's facts keep their first values and its events never come.
- Each one keeps its logic (`bar.luau`, under its name: `fact.unread` is
  `Bar.unread`; another's names whole: `fact["Notices.count"]`,
  `on("Notices.dismissed", …)`, `emit("Bar.landed", { id = 7, x = 0 })`), its own
  `permissions { … }` (no `--approve` needed) and its name for `--say`
  (`pleamar --say bar "emit landed"` still reaches it).
- They share one render: a slow one slows the others. Details: «Scenes brought
  in» in `pleamar --docs reference`.

## Text, fields, lists and windows

- **A text that is written by someone** (a chat, notes, an agent's answer):
  `markdown: true` shows what its marks mean —bold, italic, links that can be
  pressed (`on link`, or `on("link", …)` in the logic), lists, tables, quotes,
  checkboxes, `code`—; `code_block: #1b1e22` boxes the fenced blocks, `images:`
  shows the pictures it names, and `mark 4..20, #74b9ff` inside the text colours a stretch.
- **A field** is an `input`: `lines: 4` (or `height:`) for several lines, where
  Enter breaks the line and Ctrl+Enter sends (`send: enter` turns it round);
  `placeholder:` can hold live text; `mentions:` shows `@names` as chips;
  `paste: logic` lets the logic decide what a paste does (`on("paste:draft", …)`,
  with `p.text`, `p.image`, `p.uris`). Undo and redo, selection and an input
  method come with it.
- **A list that changes**: `for m in said key m.id { … }` keeps each record's
  copy (its springs, its hover) while it stays in the list. A layout with `view:`
  works out which rows are seen; `stick: end` keeps it at its end, as a chat.
- **Windows and popups**: `decorations: none` on a window's surface draws its
  own frame, and a zone with `handle: self` carries it (`handle: self, bottom_right` stretches it);
  a `popup` can take the keyboard (`keyboard: true`, Escape closes it) and open
  out of another (`parent: menu`).
- **Ready pieces**: `import "pleamar:ui"` brings `Button`, `Switch`, `Tabs`,
  `Select`, `Slider`, `Checkbox`, `Menu`/`MenuItem`, `Table`/`TableRow`,
  `Tooltip`, `Toast`, `Chip`, `Avatar`, `Card`, `Tile`… with a theme you can
  change (§10.2 of the reference).
- **Colours with their transparency**: `#rrggbbaa`, kept through `mix()`, `if()`
  and `pick()`. A row or column can have a `border:`.
- **Pictures the logic has**: `image me = data "me"` (bytes from the logic),
  `sys.ask("clipboard.image")` for a copied picture, `sys.ask("files.pick", "open",
  "*.png")` for the desktop's own file chooser.

## When the language falls short: your own shader

Before faking an effect with a hundred shapes, write it. `shader aurora = file
"aurora.wgsl"` declares one, `shader aurora { at: …; size: …; values: …; colors: … }`
paints a box with it. The file has ONE function, `fn shade(s: Shader) ->
vec4<f32>` —straight colour and coverage for each point—, and reads `s.pos`,
`s.size`, `s.uv`, `s.time`, `s.pointer`, `s.hovered`, `s.a`/`s.b` (the eight
`values:`), `s.color`/`s.color2`, and `behind(s, at)` / `behind_frosted(s, at)`
for what is behind the surface. Only what it reads costs: without `s.time` it
does not keep the scene painting. `--check` validates the WGSL with its line.
Contract and rules: §8.1 of `pleamar --docs reference`.

A `group` can also treat what it holds as one thing: `blur: 6`, `glow: 14, 90%,
mint` (or without a colour, a bloom), `saturation`, `brightness`, `contrast`,
`hue: 120deg`, `mask: x1, y1 to x2, y2` / `mask: radial x, y radius r1 to r2`,
`mode: add | screen | multiply` (multiply only darkens what the scene painted). All animatable; §8.2. Two groups with effects cannot nest.
`blur: r` is a real gaussian (r / 2, reaching 1.5 r) and a wide one costs no more than a narrow one.

Particles are an element: `particles { at: x, y; count: 400; life: 0.8s .. 1.6s;
speed: 120 .. 220; direction: -90deg; spread: 40deg; gravity: 0, 260; size: 3, 1;
colors: a, b; shape: dot | square | spark; emit: cond }` (or `burst: event`).
Worked out on the card, thousands are cheap; §8.3.

A `text` can have `gradient:` (like a body's), `outline: w, color`, `shadow: dx,
dy, blur, alpha[, color]`, and per letter `letter_move: dx, dy`, `letter_opacity`,
`letter_scale`, where `letter` is its index and `letters` the count; §8.4.

An `image … = file "x.gif"` (or an animated PNG/WebP) plays by itself.

For smaller things there is maths: `noise(x)`, `noise(x, y)`, `random(k)`,
`sqrt`, `pow`, `fract`, `mod`, `atan2`, `length`… and `time`, the seconds since
the scene started (naming it keeps the scene painting). Gesture frames take
`bezier(x1, y1, x2, y2)`, CSS's cubic-bezier.

## The logic, if there is any

`scene.luau` next to `scene.plm`. Sandboxed Luau, own thread, cut off if a
handler runs longer than two seconds. It can only cross the boundary the scene
declares:

```lua
fact.open = true                 -- a declared fact
text.title = "hello"             -- a declared live text
model.rows = { { label = "a" } } -- the whole list at once, atomically
emit("chosen", 3)                -- a declared event
emit("moved", { x = 3, y = 4 })  -- one that carries several values: a table
on("chosen", function(n) end)    -- listen: events, "fact:x", "text:x", "press:zone"
after(200, f)  every(1000, f)    -- timers
run("date", {"+%H:%M"}, f)       -- a command, if `permissions { run: … }` allows it
local id = spawn("node", {"worker.mjs"}, on_line, on_exit, { stdin = "open" })
write(id, '{"type":"ask"}\n')   -- a line to a program while it runs (a JSON-lines agent)
sys.watch("audio", f)            -- a service; sys.ask(…) asks, sys.call(…) commands
```

Everything else is a mistake: the logic does not draw, does not move properties
and does not know about coordinates.

## Before saying it is done

Check it, open it, and look at it:

```sh
pleamar --check scene.plm                     # always, after every edit
pleamar --scene scene.plm --seconds 8         # opens, closes by itself
pleamar --scene scene.plm --record open,x     # what a fact or spring is worth, frame by frame
pleamar --say NAME "fact open true"           # talk to a running scene (NAME: its scene name, lower case)
pleamar --say NAME "get open"                 # ask it
pleamar --say NAME describe                   # what it holds, by name: every zone should say what it is
```

To **see** it without taking over the user's screen, film it: no screen, no
compositor, a virtual clock (the same frames every time, as fast as the card
paints), and the keys and clicks of the script drawn on it:

```sh
pleamar --scene scene.plm --film /tmp/look --seconds 2         # /tmp/look/frame-00120.png is the end
pleamar --scene scene.plm --film /tmp/demo.mp4 --mouse "emit:toggle@300 200,40@600 click@800 type:hola@1200 key:Ctrl+z@2000"
```

Only the scene's own surfaces are filmed (no popups; a glass sees what its
scene painted under it, not the desktop). A film is not a sandbox: its logic runs for real, so a script that
presses Enter in a launcher launches the program. For those, or to see it beside programs' windows, run it inside a
screenless pleamar-wm and read the picture it writes:

```sh
echo "pleamar --scene $PWD/scene.plm --no-hud" > /tmp/autostart
PLEAMAR_WM_AUTOSTART=/tmp/autostart PLEAMAR_HEADLESS_AT=4 PLEAMAR_HEADLESS_PNG=/tmp/look.png \
  pleamar-wm headless --seconds 6
```

`PLEAMAR_HEADLESS_INPUT="900,40@2000 down@2500 up@2600 key:Super+q:16@3000"`
adds a mouse and keys there. Never drive the real mouse or keyboard.

Do not say an animation lasts what it was asked to last without measuring it
(`--record`, and `pleamar --docs measuring`).

## When the graphics card is the problem

pleamar picks the card the compositor paints with, asks only for what that card
has, and tries the next one (software last, then OpenGL) when a card cannot build
its shader; the log says which and why (`render · …`). Blur and glass a card
refuses are switched off, not fatal. To see a scene as it is painted where that
happens, `PLEAMAR_NO_EFFECTS=1`; to choose, `PLEAMAR_GPU=high` (the fast card of
a laptop) or `PLEAMAR_BACKEND=gl` / `vulkan`. A card lost while running (a
driver reset) ends the process with 70, for whatever supervises it to start it
again.
