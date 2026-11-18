# How it works today

What runs today, inside. Rust, `wgpu` 30, `cosmic-text` for the text, `resvg` and `image` for the images; and, on Linux only, `smithay-client-toolkit` 0.21 (layer-shell).

## Three threads

| Thread | Does | Does not do |
| --- | --- | --- |
| **Platform** (`platform/wayland.rs`) | One layer-shell surface per monitor (and for the ones plugged in later), its scale, and the mouse | Nothing else |
| **Logic** (`logic_luau.rs`; `logic.rs` for the test benches written in Rust) | Runs the scene's `.luau` in a sandbox: receives named events, reports facts, texts, models and events, asks for gestures. Plus one thread per plugin and one per service | Does not animate. Knows nothing about coordinates. With `--stall` it blocks on purpose, for the trial |
| **Renderer** (`render.rs`) | Owner of the springs and of the clock. Evaluates, paints and reports | Does not know what it is painting |

The mouse goes **to the renderer**, which is the one that knows what is underneath; the logic gets it already as `Enter("view")` or `Press("orb")`.

## A scene is data (`scene.rs`)

- **Properties** with a name. Each one is a spring (position, velocity, target). If the scene is replaced, the ones with the same name survive.
- Pure **expressions** (`Expr`): constants, properties, velocities, `+ − × ÷`, `min`, `max`, `abs`, `smoothstep`. In Rust they are written with overloaded operators: `orb_x + 62.0`.
- **Draw list**, in order:

| Instruction | What it does |
| --- | --- |
| `Group` | Starts a body; optionally with a shadow |
| `Shape` | Adds an ellipse or a box, blended into what came before (`fusion` = radius of the smooth minimum) |
| `Fill` | Paints the accumulated body: shadow, color, light and rim |
| `Clip` | What follows is clipped to this shape |
| `Solid` | A loose shape in flat color |
| `Text`, `Field`, `Image` | A piece of the atlas, placed on screen (see below) |

- **Behaviors** the renderer runs by itself: `Blink`, `Wave`, `Advance` (a hand going round), `Gaze`.
- **Zones**: a named shape and an `active` condition.
- **Facts and events**: the boundary. The logic sends `Fact("recording", true)`, `Signal("confirmed")` or `Gesture("nod")`, and nothing else. Facts are read in expressions (`Expr::H`), with `and`, `or`, `not`, `gt`.
- **Layers**: claims in order of priority — `while <expr>`, `N ms after <event>`, `from … until`, or the default one. The first one that holds wins. Each claim has a **presence** (a property that goes to 1 when it wins), to paint according to who is in charge, and it can **pin** properties with their spring and their delay: that is a choreography.
- **Gestures**: keyframes with a duration, a curve (`OutBack`, `InQuad`…) and a hold, over the **pose** properties. Whatever a keyframe does not name goes back to its base; when it ends, the springs pick the pose up. **Classes**: `State > Asked > Reflex > Posture > Ambient`; one only cuts off another of its own class or lower. A **posture** repeats on its own while something is true.
- **Rules**: `Enter`, `Leave`, `Press`, `Above{duration}`, `Away{duration}`, `Idle{duration}`, `Every{between}`, `On(signal)` → effects (`Animate`, `Fact`, `Toggle`, `Signal`, `Impulse`, `Gesture`). **The renderer runs all of it.**
- **Reduced motion** (`--reduced-motion`, or the system's preference as the fact `motion.reduced`, which changes it while the scene runs): the springs settle, each gesture shows the keyframe furthest from the base held still, and what carries itself goes quiet — a `blink` stays open, a `wave` rests at the middle of its travel and a `spin` stops where it was. Nothing is left going round on its own.

## The renderer: one quad per element (`render.rs`, `shape.wgsl`, `shapes.rs`)

Every frame, the renderer walks the draw list and **composes** it into two tables it uploads to the GPU:

- **shapes**, with their expressions already evaluated (16 numbers each);
- **elements** (48 numbers): a *body* —one or several blended shapes, with their paint, border, light and shadow— or a piece of the atlas. Each one carries its **bounding box**, computed on the CPU from the same geometry, widened by the shadow and the blend and cut down by its clips.

It is painted with **a single** instanced call: one quad per element, in order, with premultiplied blending. A pixel only runs the shapes of the element that covers it. What is invisible (`alpha` 0, empty boxes) does not even produce a quad.

| Primitive | Notes |
| --- | --- |
| `Ellipse`, `Rect` | as before |
| `Arc` | like an "∩"; `opening` is a half-angle in radians |
| `Segment` | a line with round ends |
| `.stroke(thickness)` | the outline only: a circle becomes a **ring** |
| `.rotated(angle)` | about its center; positive is clockwise |
| `Transform` | rotates, scales and moves everything that follows around a pivot. It is a **stack that composes**: affine matrices multiplied together, so a rotation inside a group that scales inside another that rotates does what one expects. Mouse zones can live under the same transforms (`zone_under`) |
| `Opacity` | everything inside is painted **apart, on a layer**, and blended as one single thing. Without a layer, blending piece by piece lets what is behind show through what is in front. It only spends a layer while it is half-blended (neither 0 nor 1); up to four at a time |
| `Clip` | now a **stack** (up to four): a child is clipped to its parent and to its grandparent |
| `Paint::Gradient`, `border` | a gradient between two points; an inner border on the body |

`shapes.rs` holds the geometry once for three uses: encoding it for the GPU, knowing what is under the mouse, and computing boxes.

**Measured** (RTX 2060, 720×300, `--scene swarm --no-vsync`, ms per frame):

| shapes | per-pixel interpreter (before) | per element |
| --- | --- | --- |
| 12 | 0.27 | 0.25 |
| 60 | 0.53 | 0.28 |
| 200 | 1.37 | 0.30 |
| 600 | 3.66 | 0.42 |
| 2000 | did not fit (1024 ceiling) | 0.57 |

Mind what this table says: **the old interpreter was not doing as badly as feared** at this surface size; 600 shapes still fit in a frame with room to spare. Where it would have broken is full screen (9.6 times the pixels) or on an integrated GPU. The new one, on top of that, has no ceiling any more.

## Text and images (`text.rs`)

Everything that ends up as a piece of the atlas. Three pure-Rust crates that exist on all three systems: `cosmic-text`, `image` and `resvg`.

- **`Instr::Text`**: fixed content or **live** (a named text the logic changes with `c.text("notice.title", …)`), a point and an `anchor` —which part of the text lands on it: (0.5, 0.5) centers it—, an optional width for breaking into lines, and a `Style` (family, size, weight, color, line height, alignment, maximum lines with an ellipsis).
- The **typesetter** shapes the text —ligatures, right to left, fallback fonts, color emoji— and keeps the layout: as long as text, style and width do not change, it is not redone. Each glyph is painted once, **at the scale of the finest render surface**, into an atlas: a letter is a mask —how much of each pixel it covers— that the shader tints, so the letters have an atlas of their own of one byte a pixel, and an emoji, which brings its own color, goes in the RGBA one with the images. Both are 2048 wide and as tall as what they hold on the card (from 64 rows, doubling), and the workshop may fill them to 2048² and taller —up to 2048 × 8192— when what is seen does not fit.
- Each glyph is one more element: it is clipped, transformed and blended like everything else. The text origin is rounded to real pixels so that it does not come out soft.
- **`Instr::Image`**: a path or an icon by name (`ImageSource::Icon("firefox")`; finding it is the platform's business). SVGs are painted at the exact size for the scale; with `tint`, the shape is painted in a single color: what a symbolic icon wants.
- **The workshop.** Shaping, painting glyphs and decoding images happen on its own thread, which is also the one that reads the system fonts at startup. The renderer **asks and does not wait**: while a layout is on its way it shows the last one there was in that spot, and whatever is already in the scene is ordered before the window even exists. First frame at ~170 ms, and none slow after it.
- **Measuring.** A `Text` with `measure` leaves its width and its height in two read-only properties. With `Behavior::Follow` —a property that chases an expression with its spring— a box grows with its label.
- There is a permanent snitch: any frame longer than 2.4 periods is printed to the console with its time.

## The platform boundary (`src/platform/`)

The only part of the program that knows what Wayland is. A platform puts up the surfaces a scene asks for and hands them to the renderer as render surfaces; it tells it about the mouse, the scale and the monitors coming and going; it gives it a `PlatformWindow` (`update_input_region` and a few more methods); and it knows how to find an icon by its name. Today there is only `wayland.rs`. On any other system the core compiles and says it does not know how to put up windows yet. **`./portable.sh` checks it against Linux, Windows and macOS**, and it is the guard that nothing belonging to one system slips out of here.

## Services (`platform/mod.rs`, `platform/hyprland.rs`)

What happens in the system reaches the logic through `platform::service(name, notify)`: a thread that listens and reports with a `SysValue` —a small JSON: null, yes/no, number, text, list, map— that Luau receives as a table. `platform::command(name, args)` is the way back. Today Hyprland answers, spoken to over its two sockets with `std` and nothing else: one to ask and to command (`j/workspaces`, `dispatch …`), another over which it tells what is happening. A system without that service says it does not have it.

Besides Hyprland: `platform/system.rs` (audio, battery and network: one thread per service, which only reports if something changes) `platform/mpris.rs` (what is playing, over D-Bus with `zbus`, listening to signals) and `platform/thumbnails.rs` (every window of the desktop and a small picture of each, over the standard Wayland protocols ext-foreign-toplevel-list and ext-image-copy-capture, copied only while the logic wants them and only when they change). Two services do not listen but **are** the server, both with `zbus`: `platform/notifications.rs` (the notifications: whoever holds the name `org.freedesktop.Notifications` receives them; the newest shell takes it from an older one, which gets it back when the newest leaves, and behind a daemon that will not let it go the service waits in the queue) and `platform/tray.rs` (the tray: watcher if nobody else is, host to whatever watcher there is if there is one). Whatever has to be said to the bus outside the call that caused it —a signal, an expiry— is done by a separate thread, over a channel.

Everything that is spawned goes through `platform::die_with_parent`: on Linux, `PR_SET_PDEATHSIG`.

## Surfaces (`platform/wayland.rs`, `gpu.rs`)

The scene declares the surface it wants: size in **logical** pixels, anchor, margin, level (background, bottom, top, overlay), how much room it reserves, and on which screens (`All` or a list). The Wayland thread puts one on each monitor it applies to, removes them when the monitor goes away and puts them up when one arrives. Each one reaches the renderer as a **render surface** when the compositor configures it.

- **Scale.** With `wp_fractional_scale` and `wp_viewporter`: the logical size does not change, and the render surface is painted with `size × scale` real pixels. The shader divides the position by the scale and smooths the edges in real pixels, so at scale 2 nothing comes out soft —except the text, which is a bitmap—. Without those protocols, it falls back to the old integer scale.
- **Pace.** Only one render surface —the one on the fastest monitor— waits for the screen; the rest present without blocking. If they all waited, a 60 Hz monitor would hold back a 165 Hz one.
- **Input region.** On every frame in which they change, the boxes of the active zones are handed to the compositor as the input region, right before presenting: the rest of the surface, even though it is its own, lets the click through.
- The GPU (device, pipeline, buffers) is created with the first render surface that arrives.
- **Which card.** On a machine with two (a laptop's integrated one and its discrete one), the scene has to paint on the card the compositor paints with, or the compositor cannot read its frames and the scene is a black rectangle. The compositor says which one it is (`linux-dmabuf` v4, its main device), and pleamar takes the adapter at that PCI address. Where the compositor does not say, it asks for the frugal one. `PLEAMAR_GPU=high` asks for the fast one instead (any other value, the frugal one), and either way the compositor's word is no longer followed. For a compositor that has no surface of its own (pleamar-wm), `PLEAMAR_BACKEND=gl` or `vulkan` chooses the driver: OpenGL is taken by itself on cards whose Vulkan cannot read other programs' buffers (AMD's GFX6 to GFX8 before Linux 7.3 and Mesa 26.3).

## Typing (`render.rs`, `text.rs`)

An `input` is edited by **the renderer**, not by the logic: the key arrives, `Editing` changes the text and the caret, and it shows in that same frame even if the logic has been stuck for a second. With every layout the workshop returns where each letter falls (`cursors`), and with that the caret is placed, the selection is painted and it is known which letter is under the mouse. The logic receives the text already changed (`Event::Text`) and the Enter (`Event::Submit`).

Key repeat is ours (400 ms, then one every 33): that way it is the same on every system. The clipboard is `arboard`, which exists on all three.

## Commands from outside (`platform/mod.rs`)

A thread listens on `$XDG_RUNTIME_DIR/pleamar-SCENE.sock`; `pleamar --say SCENE "emit toggle"` writes one line and leaves (2 ms); with several commands it writes one line each, on the same connection, and one thread reads them in order. A question (`get open`) goes to the renderer with a channel back (`ToRender::Query`) and is answered over the same socket. The command enters the renderer as `ToRender::ExternalSignal`, through the same door as the logic's events. It is `cfg(unix)`: on Windows it will be a named pipe.

## Dropping from another application (`platform/wayland.rs`)

`wl_data_device`: when a drag enters, it looks at whether it brings `text/uri-list` or `text/plain`, and when it is dropped it is read on a separate thread —whoever offers it may take a while— and arrives as `ToRender::Dropped`. The renderer looks at which zone is underneath and fires `on drop zone`.

## Several files (`language/mod.rs`)

`import` is resolved **before** compiling, by splicing trees: each file is tokenized and parsed on its own, what the libraries declare goes in front of the scene's body, and `compiler` compiles a single tree without knowing there was more than one. So that an error knows which file it comes from without loading every token with a name, the line number carries it inside: line 12 of the third file is 2,000,012, and it is undone when the error is shown. `read_file` returns, along with the scene, the list of files it is made of: that is what hot reload watches.

## Models (`language/compiler.rs`, `logic_luau.rs`)

The renderer does not know there are lists. `model rows max 14 { label: text; enabled: bool }` declares, underneath, fourteen live texts `rows.k.label`, fourteen facts `rows.k.enabled`, plus `rows.count` and `rows.total`. `for r in rows` unfolds fourteen copies, each one in a scope where `r` is another name for `rows.k` (the same aliasing mechanism as components) and with a condition: `rows.count > k`. That condition turns off the drawing, the slot in the layout and the zones. In Luau, `model.rows = list` hands each field to its text or to its fact and sends only what changed.

It is a deliberate decision: the language is already the definitive one —a list is written and walked— and the implementation can change underneath (copies born at runtime, G12) without touching a scene.

## Popups (`platform/wayland.rs`, `gpu.rs`, `shape.wgsl`)

A `popup` is not another scene: it is **another window onto the same one**. What is inside is compiled under a translation that takes it far away (10,000 px per popup), and its render surface carries an *origin*: the shader subtracts that origin when placing the quads and adds it back when computing each pixel. Everything else —properties, zones, rules, the atlas— is shared, and that is why a menu animates and is clicked just like the bar it comes out of. When composing, an element survives if it touches the main surface or the piece some open popup is showing.

The renderer decides when (the `open:` fact and the geometry, looked at after the rules) and calls `platform::popup`; the platform creates the `xdg_popup` from that same thread —Wayland's objects allow it—, and when the compositor configures it, the render surface reaches the renderer like any other. The mouse inside a popup arrives with the origin already added. On closing, the renderer first lets go of what it was painting and then the surface is destroyed.

## Several scenes in one process (`language/mod.rs`, `compiler.rs`, `logic_luau.rs`)

`import scene "bar.plm" as Bar` reads another scene into the one opened, as one
scene: everything it declares under its name (the plugins' boundary, by file),
its loose drawing on its own surface `Bar`, its logic as one more Luau state
with its own permissions, and one more socket for `--say`. What one scene's logic
writes or emits crosses to the opened scene's logic with who sent it, which
passes it to the others. `uses scene` reads only another's declarations, so a
scene that names one still opens alone.

## Which card (`gpu.rs`)

The card the compositor paints with, then the others that show on the surface
(the frugal first, software last), then OpenGL: each is tried by building
pleamar's own shader on it, and asked only for the limits it has. The effects
(blur, glass) are built apart, and a card that refuses them paints without them.
A card lost while running ends the process (70), for a supervisor to restart it.

## Doing only what changed

A round of a big scene costs what changed in it, not how big it is. Each result a round works out is kept with **what it read** (`scene::Seen`: the facts, the properties and the speeds it reached, not the ones it names) and is worked out again only when one of those changed:

- **The drawing** (`render.rs`, `ComposeMemo`): nothing it read changed, and the list already on the card is the one.
- **Stretches of the drawing** (`gpu.rs`, `Kept`): when something did change, the list is put together again stretch by stretch. A stretch is a run of whole groups —whatever it opens, it closes— of up to about 400 instructions. One that read nothing that changed, comes in with the same move, clips and opacity, and starts at the same place in every list as the frame before, is copied from it instead of being made: the same numbers at the same places, so every index in it still holds. Anything that moves by itself (particles, a shader that reads the time, an image that moves) is always made. Anything the whole drawing depends on besides facts and properties —the size, the order of `z:`, the texts, the windows' textures, the atlas— changing, and nothing is copied that frame. An agent's light breathing in a window manager's title bar made its 32 000 instructions again 165 times a second; now it makes its stretch, and copies the other ninety.
- **The shapes of a stretch made again** (`gpu.rs`, `ShapeMemo`): a stretch is made again when anything in it changed, or when what it is under moves. Each body shape in it is kept as it came out —before what it is under moves it—, with its fusion and what it read; one whose reads hold comes out as it came out then, without evaluating its expressions. A panel sliding in made each of its shapes again every frame, for the same numbers: in Vitreus, two thousand shapes and a hundred thousand expression nodes a frame.
- **The zones** (`render.rs`, `ZoneMemo`): whether each one is alive and its box. The mouse is tested against the kept boxes, and against a zone's shape only inside its box; the input region comes from them too. When something they read changed, only the zones that read it are worked out again. And the `z:` of the groups.
- **`follow` and `bind`** (`Readers`): only those whose inputs changed are asked again; a `bind` nothing it reads changed for is not even looked at (its property, a big `let`'s, is written by nobody else), and a `follow` writes its target only when it differs.
- **The rules** (`RuleIndex`): the event ones —a press, a key, a zone entered— are looked at only in a round with that kind of event; an `on change` or `on still` is worked out again only when something it reads changed since the round before (an expression depends on nothing else: `random` and `noise` are seeded; one that reads an event's payload is always worked out); and in a round where no fact nor property changed at all, those `on change`s are not looked at.
- **The copies for monitors that are not there** (`screens: each max 4` with one or two monitors): their rules and their `follow`s, `bind`s and `sort`s are left out of the lists a round goes through, made again when what is asleep changes.

`PLEAMAR_CHECK_SHORTCUTS=1` does each of these anyway and says any difference (`check  · …`): to try a scene with them; it is slow. `PLEAMAR_DEBUG_STRETCHES=1` says how a scene's drawing was cut into stretches. `PLEAMAR_FONT_HINTING` (`none`, `slight`, `full`) says how letters are hinted over what fontconfig says for sans-serif (`hintstyle`, as GTK and Chrome read it; slight without fontconfig on Linux, none elsewhere); with `PLEAMAR_DEBUG_TEXT=1` the log says which it took. `PLEAMAR_TIMING=1` says, every 300 rounds, how many stretches were copied and how many made.

**How many malloc arenas** (Linux with glibc, #153): glibc gives each thread that allocates an arena of its own (up to eight per core), and what one thread frees is kept for it; the render, the logic and the workshop each keep their own leftovers. `PLEAMAR_MALLOC_ARENAS=1` sets one arena for the whole process when pleamar starts (`2`, two; `0`, as glibc would have it, which is what pleamar does by default); without it, a `MALLOC_ARENA_MAX` or `GLIBC_TUNABLES=glibc.malloc.arena_max=…` set in the environment is left as it is. It depends on the scene and the machine: a small menu filmed on an Intel UHD 620 that opened 17 pages and was reloaded three times went from 110 to 151 MB and to 130 with one arena, at the same CPU; Vitreus on an RTX 2060 under a headless pleamar-wm, eight settings pages and three hot reloads, ended at 448–480 MB with glibc's and 462–463 with one arena, and its pointer along the bar cost 2.4–2.9 s of CPU in 10 s against 3.3–3.4 (frame times the same). So it is not on by default: try it on your own shell, and keep it if its memory goes down.

`PLEAMAR_LENS_LOG=1` says what a glass with a lens is given and what it does with it: every capture of what is behind it, how it came back (failed, nothing new, taken in, refused, and how many of its pixels did not fit the frame it was judged against), what a still glass does about it, why a capture could not be asked for, and every two seconds how many of each. Set to a folder instead of `1`, the first thirty captures are kept there as pictures, each beside the frame it was judged against (`capture-007.png`, `canvas-007.png`). It is for when a glass shows something other than what is behind it on someone's machine.

## At rest

If no property is moving and no behavior is alive, the renderer does not paint: it waits for a message, for a delay to come due, or for the next blink. Zero frames.

## How it is tested

```sh
./target/release/pleamar                 # Marea; right click closes it
./target/release/pleamar --scene island   # the island
./target/release/pleamar --scene face   # Marea's face: layers and gestures, with its script
./target/release/pleamar --scene showcase   # gradient, border, nested transforms, rotated texture, group opacity
./target/release/pleamar --demo --seconds 9
./target/release/pleamar --naive       # the logic blocks the renderer, like QtQuick
./target/release/pleamar --mouse "360,90@500 500,172@1100 click@3200 out@4500"
```

To see it without a screen, film it: on a virtual clock, so the same scene and script give the same frames on any machine, and as fast as the card paints them:

```sh
./target/release/pleamar --scene launcher.plm --film /tmp/look --seconds 2      # a PNG a frame
./target/release/pleamar --scene launcher.plm --film demo.mp4 \
    --mouse "emit:toggle@300 type:fire@900 key:Down@1700 key:Escape@2400"     # a video, keys drawn on it
./target/release/pleamar --scene launcher.plm --film /tmp/look --record sel   # its values, frame by frame, the same every time
```

A film is not a sandbox: the logic does what it does, so an Enter in a launcher launches the program.

To know whether it is laid out well without looking at it, ask for its layout: it is filmed a moment the same way, **with no permissions** (its logic runs nothing, uses no service and reaches no network, and its terminals start nothing, whatever it declares: `--no-permissions` does the same for a film), and the last line it prints is one JSON object with each text as painted (`bounds`, and how well its letters stand out from what is behind them, `contrast`) and the problems found: `text_overlap` (two texts over each other), `text_outside` (past the edge of its surface), `low_contrast` (under 3:1), `shadow_cut` and `drawing_cut` (a shadow or a shape that needs more room than its surface has; not on a surface that covers its monitor, `size: full, full`, whose edges are the screen's). And `warnings`, what is nearly right but not quite: `misaligned` (texts put by their left edge whose left edges are 0.5–2.5 px apart, meant to be on one edge; centred texts are left out), `uneven_spacing` (in a column of texts, a gap 1–3 px off the others), `tight_margin` (a text less than 8 px from an edge of its surface, unless that edge is the screen's) and `lines_overlap` (a text on fewer lines than its `lines:` allow, with another under it where the rest would go).

```sh
./target/release/pleamar --layout widget.plm | tail -n 1 | jq '.problems'
```

It comes up where the scene asks; `--screen NAME` puts it on another monitor. The graph at the bottom is one bar per frame; the red band is the time with the logic blocked. **Do not measure while capturing with `grim`**: it causes dropped frames.

## Known debts

All of them are in [Known limitations](08-limitations.md), each with its plan. The ones closest to this note:

- **A surface does not grow with what it holds** (S1): its size is the scene's, so a shadow or a panel that outgrows it is cut, and the renderer says so while it runs.
- **Copies are expanded at load time** (G12): a model unfolds its `max` records, and a list of thousands is a window the scene declares; copies that are born and die on their own are still to come.
- **On Windows and macOS the springs move by when each frame is made** (P9): on Wayland they move by when it will be seen, from `wp_presentation`. And **one frame in ~400 drops to 33 ms** (P10), not investigated.
- **Presenting a frame costs ~0.5 ms of CPU whatever it draws** (P11).
- A group with opacity inside another one gets no layer of its own: it multiplies (P4). With a different scale on each axis, the smoothing of the edge is approximate (P5). The gradient and the light live in the group's space, not in the shape's (P6).
