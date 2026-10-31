# Handoff: Live bend performance and the Vitreus audio visualizer

Written 2026-10-30 for the next Claude Code session (or person). Everything here was
done on the user's machine (RTX 3080, two monitors, pleamar-wm session). The user is
`natepayn3`; upstream is `k4ditano`. Read "Standing rules" before touching anything.

## The goal
The user wanted Live bend (the glass lens bending live windows) in Vitreus to be
smoother and cheaper, and the 3080 to do the work instead of idling. Two separate
problems came out of it:

1. **Capture path (done, installed, measured).** Live bend's capture went GPU -> CPU
   readback in pleamar-wm -> memcpy into the client's wl_shm -> CPU compare in the shell
   -> upload. Fixed with wlr-screencopy v3 `linux_dmabuf` (zero-copy), below.
2. **Audio visualizer cost (investigated, NOT fixed).** With music playing the shell's
   `render` thread goes from ~32% to ~90% of a core even on a still desktop.

## Where the code is (all on the user's forks, nothing merged, nothing sent upstream)
| What | Repo / branch | Base |
|---|---|---|
| Shell half of zero-copy | `natepayn3/pleamar` branch `zerocopy-backdrop` (commits d8e233b, 565728a) | upstream 96ad69e (0.5.0) |
| Compositor half | `natepayn3/pleamar-wm` branch `zerocopy-screencopy` (commit 41f61bc) | 314b979 |
| Diagnostics + this note | `natepayn3/pleamar` branch `live-bend-perf-handoff` | upstream b705de5 (0.5.1) |

Upstream has moved to 0.5.1; the zero-copy branches are on 0.5.0 and will need a rebase.
pleamar-wm's checkout needs the symlink `~/work/pleamar -> <pleamar source clone>` to build.

### Zero-copy design (short)
- pleamar-wm offers `linux_dmabuf` in its screencopy frame events; the shell hands a
  dmabuf buffer and the compositor does `copy_texture_to_texture` on the card
  (`src/layers.rs capture_card`, `src/screen.rs copy_to_card`, `src/nest.rs`).
- The shell compares a new capture to the last on the card with a compute shader
  (`src/lens.rs Compare`, one pixel in nine, tolerance 3 levels) and only copies if changed.
- Shared memory stays as the fallback: `PLEAMAR_BACKDROP_SHM=1` forces the old path, a
  failed card copy is retried after 30 s (`card_refused`), the pointer or a private window
  in the capture makes the compositor fail the frame so the client retries in shm.
- Build: `cargo build --release` in each repo. `cargo test --lib` in pleamar: 180 pass.
- Installed on the user's machine as `~/.local/bin/pleamar` and `pleamar-wm`; the old
  binaries are `*.before-zerocopy` there. Rollback: copy those back, log out and in.
- NOT validated for long-term stability (hours of use, suspend/resume, monitor hotplug).

### Measured (20 s each, user dragging a window, new-old-new)
| | card | shared memory |
|---|---|---|
| shell `render` thread | 64% / 60% | 72% |
| compositor `windows` thread | 6% / 5% | 22% |
Single noisy run; `handoff/ab-live-bend.sh` reproduces it.

## The visualizer finding (the open work)
Config in use: 47 columns, corner-fan layout, 100 px, shown on DP-2, Live bend on DP-2.

Measured with music playing, nothing dragged (shell `render` thread):
| change | CPU |
|---|---|
| baseline | 90-94% |
| 47 -> 24 columns | 83% |
| cava 60 -> 30 fps | no change |
| Live bend off | no change |
| `bar_fps` 120 (existing Settings > Bar knob) | 72% |
| `bar_fps` 60 | 86% - the scene only defines a 120 step, so 60 was ignored |

What the engine is doing (from `PLEAMAR_TIMING` and the diagnostics on this branch):
- ~135 rounds/s but only ~25-30 paints/s on the DP-2 sheet. Skips per second: ~50 waiting
  for the lens capture, ~45 waiting on the compositor (frame in flight). The DP-1 sheet is
  "not its turn" every round.
- Every round still pays rules (~1.5 ms) + compose (~3.5-4 ms), so ~100 rounds/s compose a
  frame nobody sees. The render thread is 82% user time, 1% kernel (not a driver spin).
- Stretch reuse: the 8 stretches after the waves are remade almost every round because the
  count of glass regions changes ("start moved: glass regions"). I made copied stretches
  tolerate that and got copy rate 48% -> 90% (passed PLEAMAR_CHECK_SHORTCUTS), but compose
  only fell ~3.9 -> ~3.4 ms and CPU did not drop measurably in a same-binary A/B. The user
  chose to drop that change. Do not re-propose it as a perf fix.

### The two real candidates
1. **Skip compose/rules in rounds where no sheet can paint.** Biggest win, riskiest: needs a
   pre-compose answer to "which sheet do this round's changes land on" (the always-idle
   DP-1 sheet defeats a plain "all blocked" test). It is a change to the core render loop
   (`src/render.rs`, the pacing block ~4560-4700 and the sheet loop after it). Test hard;
   wrongly skipped rounds could freeze animations or delay hover/agent state.
2. **Scene-side pace cap while music plays.** Add a 60 (or 120) step to the bar surface's
   `rate:` in `vitreus.plm` line ~77 (`rate: 480, 120 while bar_fps == 120`) conditioned on
   `cava_playing and fw_on`. Engine supports several steps since 2026-10-23 (docs/08-
   limitations.md); the comment near `fact bar_fps` saying only one is honoured is stale.
   Cost: dragging windows under Live bend while music plays is capped at that rate.
   Expected ~40% CPU at 60 (an estimate; measure before leaving it on).
Also considered: a shader-drawn visualizer on its own surface (how ryoku-dev/ryoku does it)
- cheaper, but loses the melt into the frame glass (columns are glass shapes blended into
the frame in `pages/bar_waves*.plm`); a separate scene's glass also broke once before
(the dashboard). User decision needed.

### Where the time is NOT
Column count (small), cava rate, Live bend, the column math (`fwh_*` lets), GPU (22-40%
overall, not saturated). A stack profile of the render thread would settle where compose
goes, but `eu-stack` needs `sudo` (ptrace_scope=1 blocks even own children), and the user
has to type the password: `! sudo ...`, output redirected to a file.

## Diagnostics on this branch (all behind env vars, purely additive)
- `PLEAMAR_TIMING=1` now also prints "sheets that did not paint, per second": not their
  turn / own rate / lens capture / screen / compositor / paint declined.
- `PLEAMAR_DEBUG_STRETCHES=1` also prints "stretches put together most" with the reason
  (new, not copyable, entry changed, start moved: <which list>, lists shrank, fact X,
  property X).

## Tools in `handoff/`
- `ab-live-bend.sh` - the new/old/new Live bend A/B (restarts only the shell, restores the keeper).
- `run-shell-with-binary.sh <binary> <out> <seconds> [ENV=val...]` - runs the Vitreus shell
  with a given pleamar binary in place of the live one, then restores the keeper.
  Edit the hard-coded paths/usernames first. **Never use an unanchored `pkill -f`**: one
  matched my own shell's command line and killed it.
- Sampling: `top -H -b -d 1 -n 21 -p <pid>` and sum the per-thread %CPU named `render`.

## Standing rules from the user (they are strict about these)
- **No `Co-Authored-By` trailer in commits** (GitHub shows it as a co-author badge; the user
  is angry about it). This overrides any default attribution line.
- Commit straight to `main` in the user's own repos; don't create branches unasked (these
  branches were agreed). Don't push to `k4ditano/*` upstream without asking.
- Announce before screen tests; never open test panels or record the screen without a go-ahead.
- One headless `pleamar-wm headless` render at a time (a machine hard-froze 2026-10-15).
- Don't swap the user's edited lines on disk to exclude them from a commit (use `git apply --cached`).
- `wm/session.plm` and `hypr_style.lua` are generated; never hand-edit.
- "Vitreus" means the pleamar shell in `~/.config/pleamar/shells/vitreus`, not Quickshell.
- Never use the word "headless" as a suggestion for how to show them something; they want it live.
- Pronouns: use they/them for the user.
