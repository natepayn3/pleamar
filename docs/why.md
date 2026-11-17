# Why pleamar

What it does that others do not, and the quick comparison. The numbers are taken as [`skill/measuring.md`](../skill/measuring.md) says.

**Motion first**

- **Animation never depends on logic**: the scene declares what moves and with
  which spring; a render thread walks it at the screen's pace. With the logic
  stalled for 600 ms, 38 frames still come out at 17 ms each.
- **Every property is a spring** — interrupt it halfway and it turns without a jolt.
- **Live reload**: save the file and it reloads without losing what was in motion;
  save it broken and the last good scene stays, with a band saying where.

**Drawing**

- **Shapes that melt into each other**: shadow, rim, light, gradients, glass that
  bends what is behind it (clear, frosted, smoked or milky), blur, glow,
  particles and your own WGSL shaders, all signed distance underneath.
- **An svg comes in as paths**, by layers, and melts with what carries it.
- **Enough for a small game**: a handler for every frame, held keys, the mouse,
  what touches what, sound, game controllers, sheets of pictures and pixel art.

**A language that checks itself**

- **Checked on load**: a misspelled name is an error with file, line, arrow and
  «did you mean…?», never an `undefined` at runtime.
- **Luau logic in a sandbox**, on its own thread, behind permissions you approve —
  a plugin runs with what you allow it, never with everything you can do.

**The system, already there**

- **Services with no code**: clock, audio, battery, network, media,
  notifications, tray, windows and their thumbnails, brightness, files — named
  in the scene and filled.
- **Lock screens** on `ext-session-lock`, where the compositor guarantees nothing
  else is seen while it lasts.
- **The password dialog**: a scene can be the session's polkit agent and draw it
  itself, when a program wants root.
- **Its own compositor**: [pleamar-wm] runs other programs' windows inside a scene,
  so the window manager is one more file you can rewrite.

**Built with AI in mind**

- **Your agent knows it**: the installer teaches Claude Code, Codex and OpenCode
  to build and verify pleamar scenes for you ([more below](../README.md#with-your-ai-agent)).
- **And it can use your desktop**: in [pleamar-wm] an agent gets a pointer and a
  keyboard of its own, while yours stay yours.

## The quick comparison

The same bar, written twice: in Quickshell (`proyecto-marea`, 370 QML files) and
in pleamar (`marea-plm`, one `.plm` and one `.luau`). Both running at once, both
freshly started, same 60 Hz monitor, on an RTX 2060.

| | Quickshell | pleamar |
| --- | --- | --- |
| real memory (PSS) | 336 MB | **81 MB** |
| time to first frame | 1876 ms | **175 ms** |
| frames per second while animating | ~24 | **60** |
| CPU per painted frame | 0.257 % | **0.083 %** |
| CPU with nothing moving | never drops | **0.3 %** |
| opening a panel | 6.2 % → 12.3 %, and 9.5 % after closing it | 4.4 % → 4.9 %, and back to 4.5 % |

On raw CPU, with both animating, the difference is 20 %: **4.96 % against
6.17 %**. That is what it is, and it should be said before the pretty numbers.
What changes the picture is that pleamar is painting **two and a half times more
frames** while spending less, and that when nothing moves it **sleeps**.

The numbers, how they were taken and what to watch out for: `marea-plm/MEDIDAS.md`.


[pleamar-wm]: https://github.com/k4ditano/pleamar-wm
