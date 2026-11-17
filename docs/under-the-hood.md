# Under the hood

Three threads that never wait for each other: **platform** (windows and input),
**logic** (Luau) and **render** (owner of the springs and the clock). Plus one
per plugin, one per service, and a workshop thread for text and images.

| | |
| --- | --- |
| `language/` | From text to scene: `tokens` tokenises, `tree` groups without knowing what anything means, `compiler` gives it meaning and checks the names, `vocabulary` is the list of what exists |
| `scene.rs` | The contract: properties, expressions, drawing instructions, rules, layers, gestures, zones, surfaces |
| `render.rs` | The interpreter. Still, it does not paint a single frame |
| `gpu.rs` · `shape.wgsl` | One quad per element; each pixel only runs the shapes of the element covering it |
| `shapes.rs` | The geometry, written once, used three times: the GPU, the mouse and the bounding boxes |
| `logic_luau.rs` | A scene's logic: Luau in a sandbox, with the boundary and nothing else |
| `text.rs` | Real text (`cosmic-text`) and images (SVG, PNG, JPEG) in an atlas at the monitor's scale |
| `platform/` | The only part that knows about the system: Wayland, the services, files, the clock |
| `lsp.rs` | The language server and the highlighters, both drawn from the vocabulary |

## What is missing

Measured against two real Quickshell configs (648 QML files between them), in
[`docs/07-whats-missing.md`](07-whats-missing.md): a live view of a whole
screen inside the scene (windows come as thumbnails), input methods for Japanese
or Chinese, and list copies that are born and die on their own.

Known limitations, **each one with its plan to fix it**, in
[`docs/08-limitations.md`](08-limitations.md). They are written down as
they show up, not at the end.
