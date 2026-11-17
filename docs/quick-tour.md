# The language in five minutes

After [your first scene](../README.md#your-first-scene); the [guide](guide.md) goes step by step.

## The language in five minutes

A `.plm` file is a scene: what is seen, and how it reacts.

| | |
| --- | --- |
| `surface { size: full, 44; anchor: top }` | the window it asks for. Several per scene, and `kind: window` for a normal one |
| `fact open = false` · `text title = "…"` | what the logic may report. Typed too: `fact mode: low \| normal \| critical` |
| `prop x = 360 ~calm` | something that moves. Every property is a spring |
| `service audio { volume: number; muted: bool }` | a system service, by name, with no logic |
| `model rows max 14 { label: text }` · `for r in rows { … }` | a list the logic fills |
| `box`, `ellipse`, `arc`, `line`, `path`, `text`, `image`, `input` | what gets drawn |
| `figure hat = file "hat.svg"` | an svg **as geometry**: its layers are paths, so they melt, tint and turn |
| `body { color/gradient/rim/light/shadow }` | several shapes melted into one silhouette |
| `row` / `column` | layout, with gap, padding, alignment, `view:` to scroll and `wrap:` for a grid |
| `component Row(r: record) { … }` | something to copy, with typed parameters and named slots |
| `on press hit { open = true }` · `every 2s { … }` | rules, run by the renderer |
| `follow`, `blink`, `wave`, `spin`, `look` | movement that carries itself |
| `layer`, `gesture`, `posture` | who wins a slot, and timelines |
| `permissions { run: "date"; services: "audio" }` | undeclared, the logic cannot |

The full reference, with its grammar and its checked examples, is in
[`docs/11-language-reference.md`][Reference]. To start from zero,
[`docs/guide.md`][Guide]; to copy and paste, [`docs/recipes.md`](recipes.md).

## The logic

If there is a `bar.luau` next to `bar.plm`, that is its logic: Luau in a
sandbox, on its own thread, which the renderer never waits for. It can only cross
the boundary the scene declares — `fact.open = true`, `text.title = …`,
`model.rows = {…}`, `emit`, and listening with `on(…)` — plus timers, `sys` for
services and `run` for system commands, all behind permissions. Reference in
[`docs/10-luau-logic.md`](10-luau-logic.md).

A library with its own `.luau` next to it is a **plugin**: its boundary lives
under its name (`Clock.now`), it runs on its own thread, and its permissions are
approved by whoever uses it, with `pleamar --approve`. Unapproved, it runs
touching nothing.


[Guide]: guide.md
[Reference]: 11-language-reference.md
