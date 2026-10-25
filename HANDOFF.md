# Handoff: cheaper live glass bend (branch `capture-per-glass`)

Goal: make Vitreus' "Live bend" (glass that bends the live windows behind it) usable on weak GPUs.

## What was found (measured 2026-10-24, 1920x1080 laptop, Hyprland, Intel iGPU, Live bend on, desktop idle)

- A live lens captures what is behind the glass with `wlr-screencopy` into shared memory (compositor reads the
  screen back from the GPU), then pleamar copies, uploads, unmixes and blurs it. At most one capture every 50 ms
  while anything behind the glass moves (`lens::CAPTURE_INTERVAL`).
- Every capture was **2.07 Mpx = the whole monitor (100% of the surface)**, at 1.0 to 4.4 captures a second even
  with nothing being touched (2 to 9 Mpx/s). Cause: `render.rs` took the *bounding box* of all the glass on a
  surface as the one box to capture. Vitreus' glass is a frame round the screen, so its hull is the whole screen
  although the glass covers only about 2.6 to 7.9% of it (about 140 thin 2 px strips following the shape).
- Cheap mode that already exists: X-Bend bends a 960x540 wallpaper copy (`lens = image`), no capture at all.

## What this branch does

- `lens::capture_parts` (new, unit tested): turns the glass boxes into a few rectangles (16 px tiles, merged
  row runs, margin = blur reach). None when the box whole is as good (fewer than 2 or more than 12 parts, or parts
  that are more than 70% of the hull).
- `render.rs` computes them when the glass changes (`Sheet::glass_rects`, cached by `glass_src`).
- `platform/wayland.rs`: one screencopy request per part, copied into one hull-size buffer
  (`Parts`, `Assembled`), delivered as a normal `Backdrop` with `rects` listing what is real. Watching
  (`on_change`) delivers on the first part that changed; the render's repaint then asks for everything.
  `PARTS_BROKEN` falls back to the whole box if the compositor's sizes do not fit.
- `lens.rs`: `receive` uploads and samples only the parts; `prepare` unmixes every part, then frosts every part.
- `PLEAMAR_WHOLE_CAPTURE=1` turns it off (old behaviour). `PLEAMAR_TIMING=1` prints `lens ·` capture stats
  every 5 s and `lens boxes ·` when the glass boxes change.

## State

- Builds; `cargo test --release --lib` passes (152, including 3 new `parts_tests`).
- **Not tested on real Hyprland screencopy.** Hyprland has no headless switch here (`CBackend::create()` fails),
  and a nested Hyprland would open a window on the desktop. So nobody has yet looked at the glass.
  Do not merge or open a PR before that. The rig (`shells/vitreus/dev/rig`, pleamar-wm headless) cannot show it:
  pleamar-wm does not use screencopy.

## To verify (needs a Hyprland session with Vitreus)

1. `cargo build --release`, then run the shell with this binary and `PLEAMAR_TIMING=1` (the autostart line goes
   through `vitreus-keep`; stop its keeper first, or start the scene by hand).
2. Turn Live bend on (Settings > Glass). Look: edges bend windows behind, no stale or black patches, panels
   opening and closing, window drags near the bar.
3. Compare the `lens ·` lines with the numbers above (2.07 Mpx each). Expect far fewer Mpx per capture.
   Compare with `PLEAMAR_WHOLE_CAPTURE=1`.
4. Watch for: a frame with a wrong patch after the glass changes shape (new parts only unmix when captured),
   fractional scales (parts are 16 logical px; at scale 1.1 offsets round), rotated monitors (the capture path
   never handled transforms; parts assume upright like the old path).

## Next ideas (not started)

- Configurable capture interval / a "low" setting (50 ms is the fixed cap; under motion that is up to 20/s).
- dma-buf screencopy so the frame stays on the GPU (largest gain, most work, compositor dependent).
- Vitreus side: only bend live while a panel is open, or fall back to the wallpaper copy when the capture is large.
- `AI_POLICY.md`: a PR must say AI helped, be one focused change, and have been seen running.
