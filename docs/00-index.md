# pleamar — Index

**What it is:** a desktop runtime —what would be "a better Quickshell"— and the language its scenes are written in. Born on 19 Sep 2026 out of a conversation about why QtQuick cannot animate while the logic works.

**Where it lives:** [github.com/k4ditano/pleamar](https://github.com/k4ditano/pleamar). These notes travel with the program too: `pleamar --docs` prints the ones of the version installed.

## Start here

| Note | What for |
| --- | --- |
| [The guide](guide.md) | From zero to a bar, step by step |
| [Recipes](recipes.md) | Whole scenes that already work, ready to copy |
| [Language reference — version 0.6](11-language-reference.md) | **The reference**: lexicon, EBNF grammar, every element with what it accepts, and the version number. Its examples are compiled by `./run-tests.sh`, and its vocabulary is compared against the compiler's |
| [The logic, in Luau](10-luau-logic.md) | The boundary seen from the logic: what a `.luau` can do, and its sandbox |
| [Scenes an agent can read](12-agents.md) | The scene as a tree, acting by name, `wait`, and `agent: no` |
| [Screen readers](13-screen-readers.md) | The same tree to Orca over AT-SPI (AccessKit): what it is told, what it can do, and that it costs nothing without one |

## How it is made, and how it got here

| Note | What for |
| --- | --- |
| [How it works today](02-how-it-works.md) | Inside: the threads, the scene as data, the renderer, doing only what changed, which card it paints on |
| [Parity with Quickshell — what is actually missing](07-whats-missing.md) | The honest inventory, measured against real configurations, and the order of work |
| [Known limitations](08-limitations.md) | Everything that is half-done, with its severity and its plan; and, below, everything fixed, with its date |
| [The language — the guide](09-language-v0.md) | The language read straight through, with the reason for every choice |
| [Why it exists](01-why.md) | The problem, the idea, and what was measured |
| [Decisions and open questions](05-decisions.md) | What was decided, and why |
| [Test: does the real Marea fit in the language?](04-marea-test.md) | Marea's `ExpressionController` against the language. Historical |
| [The language — draft 0](03-language-draft-0.md) · [Three sketches](06-sketches.md) | The sketches from before it was implemented. Historical; note 09 and the reference win |

## Where it stands (pleamar 0.6.0, 31 Oct 2026)

- **The renderer animates on its own**: with the logic blocked for 600 ms, 38 frames at ~17 ms. The same trial in QML: a 600 ms gap. And a round of a big scene costs what changed in it, not how big it is (0.5).
- **The language, version 0.6**: scenes, components with typed parameters and slots, `repeat` and `for`, layouts with `grow:`, `wrap:`, `view:` and grids, layers, gestures, rules, enums, arrays and sorting, texts with slots, translations, imports and libraries. Checked on load, with «did you mean…?», and written down in the reference.
- **The logic in Luau**, in a sandbox, on its own thread, behind permissions; **plugins** with their own boundary, thread and permissions, approved by whoever uses them.
- **Drawing**: blended shapes with shadow, rim and light, gradients, paths, svgs as geometry, glass that bends what is behind it, group effects, particles and the scene's own WGSL shaders. Images and windows drawn smaller than they are are averaged under each pixel, not read once ([minify](minify.md)). A program's window drawn at its own size lands on whole pixels ([pixelsnap](pixelsnap.md)).
- **The system**: audio, battery, brightness, network, bluetooth, media, notifications and their history, tray, windows and their thumbnails, workspaces, apps, files, clock and polkit — named in the scene and filled.
- **Surfaces**: panels on any layer-shell compositor, one per monitor with its own state, popups, plain windows and lock screens.
- **For whoever writes it**: hot reload that loses nothing, a language server, highlighting for vim and VS Code, types for luau-lsp, `--report` to measure on someone else's machine, and a skill that teaches AI agents to write and check scenes.
- **[pleamar-wm](https://github.com/k4ditano/pleamar-wm)**: a compositor whose window manager is a pleamar scene, with hands for AI agents.
- **0.8, experimental (`exp2`, Nov 2026)**: several scenes in one process, talking with checked events (`import scene`, `uses scene`); markdown, fields of several lines with an input method, mentions and paste; lists with identity; windows with their own frame and popups out of popups; `pleamar:ui`'s menus and tables; a real blur and a finer glass; any card that can paint it does, and `--film` films a scene with no screen. What each brought and how it was checked: [08](08-limitations.md), «Fixed».
- ⬜ Still missing: copies born at runtime, deferred loading, an input method outside Wayland, and a whole screen live inside the scene. See the next step.

Every version, with what it brought: [Releases](https://github.com/k4ditano/pleamar/releases).

## Next step

**The direction:** this is **an alternative to Quickshell**, not the engine of one particular shell. The yardstick is [Parity with Quickshell](07-whats-missing.md), measured against real configurations, and its §4 says the order of work: copies that are born and die on their own (G12), then deferred loading, then IME. **Still to be tried by real hands**: the popups and the tray menu (S11), the input field with a real keyboard (E8), the wheel (E1), and notifications and the tray as the only bar (B12, B14).
