# The logic, in Luau

**Status:** implemented (`src/logic_luau.rs`). If there is a `marea.luau` next to `marea.plm`, that is its logic. It reloads itself when saved, just like the scene.

Examples: `examples/marea.luau` (notices coming in) and `examples/tray.luau` (a list of data that grows and shrinks).

## What it can do, and what it cannot

The logic **does not animate, does not paint and knows nothing about coordinates**. It lives on its own thread; the renderer never waits for it. Only this crosses the boundary:

```lua
fact.open = true                          -- say what is true. It reads back as what it is: `true`, a number, or `"critical"`
text["notice.title"] = "Meeting in 5 min" -- say what a live text says
emit("confirmed")                         -- that something has happened
-- and from the scene, `emit moved(x: local.x, y: local.y)` arrives as a table: on("moved", function(p) … p.x, p.y … end)
play("joy")                               -- ask for a gesture (the scene will grant it or not, by its class)

on("view_event", function(n) … end)       -- an event the scene lets out (`event x ->`), with its payload
on("press:view", …)  on("release:view", …)  on("enter:orb", …)  on("leave:orb", …)
on("scroll:sound", function(notches) … end)   on("key", function(name, text) … end)
on("paste:draft", function(p) … end)      -- a paste in that field: p.text (nil without text), p.image, p.uris
on("text:query", function(value) … end)   -- someone typed in the `query` field; `text.query` already holds it
on("submit:query", function(value) … end) -- Enter in the field
on("focus", …)  on("blur", …)             -- the surface gains or loses the keyboard
on("drop:tray", function(data, mime) … end)  -- something was dropped from another application: text, or a list of `file://…`
json.decode(text)  json.encode(t)  -- what a program prints (`run`'s output) as tables, and back
focus("query")                            -- give the typing caret to a field; `focus()` takes it away
on("layer:card", function(claim) … end)   -- a layer changed hands
on("fact:open", function(v) … end)        -- a scene RULE changed a fact
on("demo", …)                             -- the tick of `--demo`
on("frame", function(dt) … end)           -- every frame, before it is made: see «A logic that plays»
on("key_down", function(name) … end)  on("key_up", …)   -- a key going down (once) and coming up
held("Left")   mouse.x  mouse.left   pad(1)   hit.circles(…)   sound.tone({ … })   -- the same section

local t = every(1000, function() … end)   -- timers, in milliseconds
after(500, function() … end)
cancel(t)

run("date", { "+%H:%M" }, function(out, code) … end)   -- a system command; answers when it finishes
run("wl-copy", { "--type", "image/png" }, nil, { stdin = path, output = false })   -- how: a file on its input; and not waiting for what it writes
run("sudo", { "-S", "-p", "", "-v" }, function(_, code) … end, { input = pw .. "\n" })   -- a text on its input, which never touches a file, its arguments or its environment
local id = spawn("wf-recorder", { "-f", file }, function(line) … end, function(_, code) … end)   -- one that does not end: a call per line, and one when it HAS ended
spawn("pacman", args, on_line, on_exit, { input = "…", errors = true, env = { LC_ALL = "C" } })   -- how, too: a text on its input, its error output among the lines, and variables of its own (`env` and `errors` also for `run`: its error output after the rest; a variable that decides which code runs —`PATH`, `LD_PRELOAD`, `NODE_OPTIONS`…— only if `permissions { env: "…" }` names it, and a plugin's only `LANG`, `LC_*`, `TZ` and colours unless it names more)
local id = spawn("node", { "worker.mjs" }, on_line, on_exit, { stdin = "open" })   -- its input left open:
write(id, '{"type":"prompt"}\n')                       -- a line to it while it runs: handed to its own writer, never waiting for it to read (false if it is gone, or 8 MiB are still waiting); write(id) closes it, after what is waiting
run("python3", { "tools/x.py" }, on_done, { cwd = "tools" })   -- programs run in the scene's folder (`cwd`: another one, relative to it or absolute); lines read from them are UTF-8 with what is not replaced, up to 16 MiB a line
kill(id, "int")                                       -- asks it to stop (or "term"); kill(id) alone kills it and forgets it
local id = spawn("pactl", { "subscribe" }, function(line) … end)   -- one that does NOT finish: one call per line
kill(id)

sys.watch("workspaces", function(w) … end)   -- a system service; returns whether this system has it
sys.call("workspaces.focus", 3)               -- ask a service for something
local menu = sys.ask("tray.menu", key)        -- ask it something and wait for the answer (the logic may wait)
log("whatever", 42)
tr("Good morning")                   -- the scene's `translations`, in the language `locale` says
busy(600)                                 -- fake work, to see that the renderer does not care
```

**Facts have a type**, the one the scene gave them: a yes-or-no is read and written with `true` and `false`; an enum (`fact mode: low | normal | critical`), with the name of its value; everything else, numbers. `on("fact:mode", function(m) … end)` receives the same. A value that does not exist is an error that says which ones there are: `'mode' cannot be 'critcal'. Did you mean 'critical'?`

A misspelled name is an error there and then, with a suggestion: `the scene has no fact called 'opne'. Did you mean 'open'?`

## The logic in your editor

An editor that speaks Luau through [luau-lsp](https://github.com/JohnnyMorganz/luau-lsp)
does not know the globals pleamar gives a logic, and calls them undefined.
`pleamar --luau-types` writes them down for it, typed; given the scene, it adds
that scene's own names: `fact.` completes its facts, each as what it is
(`boolean`, `number`, `"low" | "normal" | "critical"`), `text.` and `model.`
theirs, and `emit` knows its events.

```sh
pleamar --luau-types marea.plm > pleamar.d.luau   # again when the scene's facts change
```

Then tell luau-lsp to load it, on the standard platform (not Roblox's):

- **VS Code** (the *Luau Language Server* extension), in the settings:
  `"luau-lsp.platform.type": "standard"` and the file in
  `"luau-lsp.types.definitionFiles"`.
- **Neovim** ([luau-lsp.nvim](https://github.com/lopi-py/luau-lsp.nvim)):
  `platform = { type = "standard" }, types = { definition_files = { "pleamar.d.luau" } }`.
- **Any other** (Helix, Zed…): start the server as
  `luau-lsp lsp --definitions=@pleamar=pleamar.d.luau`.

The same file checks a logic from a terminal:
`luau-lsp analyze --platform standard --definitions=@pleamar=pleamar.d.luau marea.luau`.

## A logic that plays

Everything above is for a logic that decides now and then. A game decides
sixty times a second: where the ball is, what it has hit, what sounds. These
are for that, and they change nothing for a logic that does not use them.

```lua
local x, vx = 240, 0
on("frame", function(dt)                  -- dt: seconds since the last one it heard of
    vx = (held("Right") and 300 or 0) - (held("Left") and 300 or 0)
    local p = pad(1)                      -- a game controller, or nil
    if p then vx += p.lx * 300 end
    x = math.clamp(x + vx * dt, 20, 460)
    fact.x = x                            -- the scene draws it there: `at: x, 260`
end)
on("key_down", function(name)
    if name == "space" then sound.tone({ wave = "square", freq = 440, to = 880, ms = 120 }) end
end)
```

**`on("frame", function(dt) … end)`** is called once for each frame, right
before it is made, with the seconds gone by since the last one it was told of
(never more than a tenth: a pause does not become a jump). The render gives it
three milliseconds to answer, and what it says in that time —a `fact`, a
`text`, a `model`— goes into that very frame: the ball is drawn where it is
now, not where it was. If it takes longer, the frame is made without waiting,
as always, and what it said shows in the next; while it has not finished with
one frame it is not told of another, and the time it missed comes in the next
`dt`. Listening to frames is what keeps them coming: a scene whose logic does
not, still rests when nothing moves. A mistake in the handler is said once and
it stops being called, until the logic is saved again.

What it moves, it moves through facts: `fact.x = 132.5`, and the scene draws
with them (`ellipse { at: x, y; radius: 9 }`). Many things at once are a
`model`, handed over whole each frame; only what changed travels.

| | |
| --- | --- |
| `held("Left")` | whether that key is down right now, by the name a rule gives it (`Left`, `space`, `Escape`); a letter, in lower case whatever Shift says |
| `on("key_down", function(name) … end)` · `on("key_up", …)` | a key going down —once, however long it is held, where `on("key", …)` repeats— and coming up. Losing the keyboard lets go of all of them. The surface has to ask for the keyboard (`keyboard: on_demand`) |
| `mouse.x` · `mouse.y` | where the mouse is on the scene; `nil` when it is not over it |
| `mouse.over` · `mouse.left` · `mouse.right` · `mouse.middle` | whether it is over the scene, and which of its buttons are down |
| `pad(1)` | how the first game controller is right now, or `nil` if there is none: `name`; the sticks `lx`, `ly`, `rx`, `ry` from −1 to 1 (up is negative, like the screen); the triggers `lt`, `rt` from 0 to 1; and whether each button is down: `a`, `b`, `x`, `y`, `lb`, `rb`, `back`, `start`, `guide`, `ls`, `rs`, and the cross as `up`, `down`, `left`, `right`. A stick at rest rarely says exactly 0: leave it a little room (`if math.abs(p.lx) > 0.15`). With `services: "gamepad"` |
| `sys.watch("gamepad", function(pads) … end)` | every pad plugged in, as a list of those same tables: told when one arrives or goes, and when a button goes down or up (not for a stick moving: that is read each frame) |
| `hit.circles(ax, ay, ar, bx, by, br)` · `hit.boxes(ax, ay, aw, ah, bx, by, bw, bh)` · `hit.circle_box(cx, cy, r, bx, by, bw, bh)` · `hit.point(px, py, bx, by, bw, bh)` | whether two things touch. A box is said as the scene says it: its centre and its size |
| `hit.pairs(these, those)` | every pair that touches between two lists, as `{ { i, j }, … }`: each record has `x`, `y` and either `r` (a circle) or `w` and `h` (a box). Found in one go, not in a loop of the logic's |
| `hit.first(x, y, r, those)` | the first of `those` that a circle touches, or `nil` |

**Sound**, with `services: "sound"`. A file from the logic's folder (or one
inside it) —a WAV or an Ogg Vorbis—, or a tone made on the spot, which is most
of what a small game needs and wants no files:

```lua
local id = sound.play("coin.wav", { volume = 0.8, pitch = 1.2, pan = -0.5 })
local tune = sound.play("tune.ogg", { loop = true, volume = 0.4 })
sound.tone({ wave = "square", freq = 440, to = 880, ms = 120, volume = 0.5 })   -- a jump
sound.tone({ wave = "noise", freq = 900, to = 80, ms = 350, release = 300 })    -- a burst
for i, hz in ipairs({ 523, 659, 784, 1047 }) do                                  -- a tune: every
    sound.tone({ freq = hz, ms = 110, delay = (i - 1) * 120, duty = 0.25 })       -- note asked for
end                                                                              -- now, each in its place
sound.stop(tune)        -- one; sound.stop() is every one this logic started
sound.load("tune.ogg")  -- read ahead of time: a long file takes a moment the first time
sound.volume(0.6)       -- how loud everything this program plays is
```

| | |
| --- | --- |
| `volume` | 1 is as it is |
| `pitch` | 1 is as it is; 2, an octave up and half as long (files only) |
| `pan` | −1 all to the left, 1 all to the right |
| `loop` | it starts again when it ends, until it is stopped |
| `delay` | milliseconds to wait before it starts. They are counted by the mixer, to the sample, not by the frames: the notes of a tune asked for all at once keep their time however late a frame is, and those still waiting take none of the 48 voices. `sound.stop` stops them before they start too |
| `wave` | `sine`, `square` (the default), `saw`, `triangle`, `noise` |
| `freq` · `to` | the pitch it starts at and the one it slides to, in hertz (`to` left out, it stays) |
| `ms` · `attack` · `release` | how long it lasts, how long it takes to rise and how long to fall, in milliseconds |
| `duty` | of a square wave, how much of each turn it is up: 0.5 is even, 0.2 thin and nasal |

Everything a program plays is mixed here and goes to the sound server as one
stream (PulseAudio's protocol, which PipeWire speaks too), opened the first
time something sounds and let go after a few seconds of silence. Up to 48
things sound at once (and up to 4096 wait their turn); what a logic left
sounding, or waiting, stops when it is reloaded.
On a machine with no sound server it says so once, and nothing is heard.

## The system services

`sys.watch(name, fn)` listens to something that happens in the system. The function receives the state right now and then every change, as a table. **The names are the same on every system**; who answers is `src/platform/`'s business. If this system does not have that service, `sys.watch` returns `false` and the scene decides what to do without it.

| Service | What it reports | Who provides it today |
| --- | --- | --- |
| `workspaces` | `{ active = 3, list = { { id, name, windows, monitor, active }, … } }`; each one's `active` says whether it is the active one **on its monitor**, which is what a bar per screen needs | Hyprland, over its sockets (without running `hyprctl`) |
| `window` | `{ title, class, monitor, fullscreen, maximized }` of the one with the focus; elsewhere than Hyprland also `list = { { id, title, class, monitor, active, minimized, fullscreen, maximized }, … }`, every window in the order it came. `fullscreen` is what a bar hides for, or keeps quiet during; whether a window floats, is pinned or where it is, wlr-foreign-toplevel does not say | Hyprland; any compositor with wlr-foreign-toplevel (pleamar-wm, sway, niri…) |
| `sys.call("workspaces.focus", n)` | go to a workspace | Hyprland |
| `sys.call("window.restore", id)` · `("window.minimize", id)` · `("window.activate", id)` · `("window.close", id)` | a window, by the `id` `window`'s `list` gives it: brought back (with the keyboard), put away, given the keyboard, asked to close | wlr-foreign-toplevel (pleamar-wm, sway, niri…) |
| `audio` | `{ volume = 0.54, muted = false, input, input_muted, outputs, inputs, apps }`: the default output and input (0..1, as the mixer shows it), the devices for each, `{ { id, name, default }, … }`, and what is playing, `apps = { { id, name, icon, binary, title, volume, muted, playing }, … }`: one per stream (a browser has one for each tab that sounds), `icon` its icon name (or its program's, in lower case, if it does not say), `title` what it plays, `playing` false while it is paused. Event sounds (a click, a notice's chime) are left out | Linux: PulseAudio's protocol spoken directly (PipeWire speaks it too), told of changes; nothing is spawned. Without a sound server to speak to, `wpctl` |
| `sys.call("audio.volume", 0.5)` · `("audio.step", -0.05)` · `("audio.mute")` · `("audio.input", 0.8)` · `("audio.input_mute")` · `("audio.default", id)` · `("audio.app_volume", id, 0.5)` · `("audio.app_mute", id[, true])` | set it, move it one step, silence it (or `("audio.mute", true)`), the same for the input, where it plays or listens (an `id` from the lists), and one application's volume or silence (an `id` from `apps`) | `apps`, `app_volume` and `app_mute` need a sound server (PulseAudio or PipeWire); with `wpctl` alone there are no `apps` |
| `audio.level` | `{ level = 0.31 }`: how loud what plays on the default output is, 0..1, the loudest sample of each moment, up to 30 times a second and only when it changes (a silence is told once). It is the signal, not the volume: a meter or a bar that moves with the music. It is the peak of the server's mono mix of the channels, so stereo in opposite phase, or on one side only, reads lower than either channel would. After 250 ms with no reading, or when the output changes or goes away, it is 0 at once. It follows the default output when that changes. Listening takes `services: "audio"`; nothing runs until a scene or a logic asks for it | Linux: a record stream on the default output's monitor with the server's peak detection, as pavucontrol's meters do: the server measures, one number comes each time. Without a sound server, not available |
| `battery` | `{ present, percent, charging }`; a desktop answers `{ present = false }` | Linux: `/sys/class/power_supply`, told of changes by the kernel (a charger plugged in or out shows within a second), and looked at again every 20 s |
| `brightness` | `{ present, level }`; with no backlight, `{ present = false }` | Linux: `/sys/class/backlight` |
| `sys.call("brightness.level", 0.6)` | set it | Linux: `brightnessctl`, which is who has the permission |
| `network` | `{ online, kind = "wired" \| "wifi" \| "none", name, strength, wifi, error, networks = { { ssid, strength, secure, known, active }, … } }`: the connection in use, whether the wifi radio is on, and the networks in range (one per name, strongest first; `known`: used before). `error`: why the last `network.connect` did not get in, `none` if it did —`password` (wrong, or none given), `timeout`, `unsupported` (an enterprise network), `busy`, `failed`—; with iwd only, so far: with NetworkManager it is always `none` | Linux: NetworkManager over D-Bus, told of changes; without it, iwd over D-Bus, the same way, with `online` and `kind` from the kernel; with neither, the kernel alone (`/proc/net`, only `online`, `kind`, `name`, `strength`) |
| `sys.call("network.wifi", true)` · `("network.scan")` · `("network.connect", ssid[, password])` · `("network.disconnect")` · `("network.forget", ssid)` | the radio on or off, look for networks, join one (a known one as it was saved; a new one with its password), leave, forget a saved one. Joining takes seconds and does not wait: how it went comes through `network` —the network turns `active`, or `error` stops being `none` and says why not—. With iwd a known network takes no new password: `network.connect` says so, and `network.forget` it first | `services: "network.*"`; NetworkManager or iwd |
| `bluetooth` | `{ present, powered, discovering, devices = { { name, address, paired, connected, battery, icon }, … }, pairing, pairing_id, pairing_address, pairing_name, pairing_code }`: the first adapter and the devices it knows or sees, connected first; `battery` 0..1, or -1 if the device does not say. With no adapter, `{ present = false }`. `pairing`: what pairing a device asks of whoever sits there, `none` when nothing —`pin` or `passkey`, a code to type (the one a keyboard or the device shows; a passkey is up to six digits); `confirm`, whether `pairing_code` is the number the device shows; `display`, `pairing_code` is to be typed on the device; `authorize`, whether to let it pair at all—, with `pairing_id`, which question it is, and the device's `pairing_address` and `pairing_name`. Only while a `bluetooth.connect` of this process pairs, one thing at a time, as BlueZ asks, and only to a scene that declares `pairing` and may answer (`services: "bluetooth.*"`, or all three of `bluetooth.answer`, `bluetooth.confirm`, `bluetooth.cancel`): watching `bluetooth` alone asks nothing | Linux: BlueZ over D-Bus, told of changes; while pairing, this process is BlueZ's agent for it, at its own path for each `connect`, and takes calls on it only from BlueZ |
| `sys.call("bluetooth.power", true)` · `("bluetooth.scan", true)` · `("bluetooth.connect", address)` · `("bluetooth.disconnect", address)` · `("bluetooth.forget", address)` | the radio, looking for devices, connecting (it pairs first if it has to, and trusts it; it takes seconds and the change arrives through the service; while one pairs, any `connect` that would pair, to the same device too, is an error, `busy`), leaving, forgetting | `services: "bluetooth.*"`; BlueZ |
| `sys.call("bluetooth.answer", id, code)` · `("bluetooth.confirm", id)` · `("bluetooth.cancel", id)` | while pairing, each naming the `pairing_id` it answers (one for a question that is over is an error, and answers nothing): answer a `pin` (1 to 16 letters or digits, as text: `"0000"`, since a number loses its leading zeros) or a `passkey` (up to six digits, text or number); an error if the code is not one, and it can be given again · say yes to a `confirm` or an `authorize` · say no to what is asked (BlueZ is told `Rejected`, which fails the pairing; a `display` waits for no answer, so the pairing is called off with `CancelPairing`). A scene that declares `pairing` must answer, or pairing fails when BlueZ gives up (60 s for a `confirm` or an `authorize`, `REQUEST_TIMEOUT` in BlueZ's `src/agent.c`) or, for anything else, after 90 s, when pleamar calls it off: `pairing` goes back to `none` and the connection is not made. When the last scene that declares it stops declaring it, what it was asked is over too. With none, it says yes to everything as before (as a device with no screen nor keys). One request at a time: a newer one ends the older, and BlueZ's own `Cancel` does not say which device it is about | `services: "bluetooth.*"` |
| `thumbnails` | `{ capturing, list = { { id, title, app, picture, stale }, … } }`: every window of the desktop, the scene's or not. `picture` is a small PNG of it (400 px at most), a path with a version that `image … = from` paints and reloads: empty until it is asked for. A window asked for live (`thumbnails.live`) skips the file: `picture` names its last frame, kept in memory (`thumbnails:3?12`), which `image … = from` paints at the size it is drawn (a very large one a little softer, as any live image); once it is no longer asked for live, `picture` is the PNG of that last frame. `stale`, the compositor said it stopped copying it and the picture is the last one; a window it does not copy without saying so (Hyprland, for one off every monitor) keeps its last picture all the same, not stale. A copy with the same pixels as the last one changes nothing. `capturing` false, the compositor lists its windows but cannot copy them | Linux: `ext-foreign-toplevel-list` and `ext-image-copy-capture` (pleamar-wm, Hyprland, sway, niri…); a window is copied only when it changes, a few times a second at most. A live window is copied on the card when the compositor offers it (a card and BGRA layouts in its capture session: Hyprland does) and the card is the one pleamar draws with: made small there, without passing through memory; anywhere else, or if the compositor refuses the buffer, through memory as before. On the card, a window whose alpha the compositor gives keeps it |
| `sys.call("thumbnails.want", ids)` · `("thumbnails.want", "all")` · `("thumbnails.want")` | which windows to copy: the ones on screen, all of them, or none (the default). Copying costs: ask for the ones shown | |
| `sys.call("thumbnails.live", ids)` · `("thumbnails.live", "all")` · `("thumbnails.live")` | which windows to copy live, for an overview that shows them moving: up to 30 times a second, and only when they change, straight to the image that draws them, at its size. The same arguments as `thumbnails.want`, and the two do not mix: a window in either is copied, live if it is in this one. A window moved between the two keeps its picture until it next changes, except that one no longer live becomes a file straight away. Live costs more: ask for the ones shown | |
| `sys.ask("capture.pixel", x, y)` · `("capture.pixel", x, y, screen.name)` | the colour at that point, `"#rrggbb"`: a colour picker. Without a monitor, `x` and `y` are on the desktop, in the compositor's logical units, and the monitor that holds them is found; with one, inside that monitor, as a full-screen surface's `pointer.x` and `pointer.y` read them. A point on no monitor, or outside the one named, is an error that says so | `services: "capture"`; Linux: wlr-screencopy (wlroots compositors, Hyprland, niri, pleamar-wm), a small region around the point; where there is none, ext-image-copy-capture's output source. Either way the copy is made upright here and the pixel is the one the point is on at the monitor's scale (fractional too). Each call waits for the compositor's next frame, so ask once per pick, not every frame. On a connection of its own |
| `sys.ask("capture.freeze", screen.name)` | the whole monitor as it is now, kept in memory: answers its name, `capture:DP-1?3`, which `image … = from` paints like a live window's frame (a very large one a little softer, as any live image). What a region picker draws under its rectangle, so that what is being picked does not move. A monitor's last freeze is kept until the next one of that monitor replaces it. An unknown monitor is an error that names the ones there are | `services: "capture"`; ext-image-copy-capture's output source, or wlr-screencopy where there is none; made upright on a turned monitor. A surface with `captures: hidden` is left out of both, by the compositor that honours it (pleamar-wm) |
| `media` | `{ playing, title, artist, album, length, position, rate, can_seek, art, player }`; `length` and `position` in seconds (0 if the player does not say), `position` as of the report —carry it on at `rate` while `playing`, see the recipe *How far into the song*—, `can_seek` whether the player lets `media.seek` move it (a live stream does not), `art` the cover as a path a scene can paint (`image cover = from art_text`): a `file://` URL as its path, a `data:` one written once into `$XDG_RUNTIME_DIR/pleamar/media/`, an `http(s)` one left as the player said it; with no players, `player = ""`; `players = { { id, name, playing, chosen }, … }`, all of them, `name` being the player's `Identity` | Linux: MPRIS over D-Bus (`zbus`), without asking every so often |
| `sys.call("media.toggle")` · `("media.next")` · `("media.previous")` · `("media.seek", 95)` · `("media.step", -10)` · `("media.choose", id)` | to the player being reported; `media.seek` goes to that second of the song and `media.step` that many seconds from where it is, back if negative, both held inside the song where the player says how long it is (with no `length`, only above 0) —an error if it cannot seek (`can_seek`), or does not say where it is—, and the new `position` comes back as a report; `media.choose` pins which one that is (an `id` from `players`), `""` lets it choose again | `services: "media.seek"`, `"media.step"` or `"media.*"`; MPRIS's `SetPosition` for the song playing, or `Seek` where the player gives no `mpris:trackid` |
| `notifications` | `{ { id, app, title, body, icon, image, urgency, time, actions = { { key, label } } }, … }`, the newest first. `icon` is the application's; `image` the picture it brings —the sender's photo—, a file even when it arrives as pixels (`image-data`); `time` when it arrived, in seconds since 1970. There can only be one receiver: the newest shell takes them from an older one (which gets them back when it leaves); behind a daemon that will not give them up, it waits in the queue. **Returns `false` only without a session bus** | Linux: pleamar is the `org.freedesktop.Notifications` server |
| `sys.call("notifications.dismiss", id)` · `("notifications.invoke", id, "default")` · `("notifications.clear")` | dismiss, press one of its buttons (the application finds out), empty | |
| `sys.ask("auth.check", password)` | is that the password of whoever owns the session? `true` or `false`. It is what a lock screen needs to open. A wrong one takes a couple of seconds, on purpose, and the system counts it like any other failed login | `services: "auth"` |
| `polkit` | `{ ready, pending, asking, message, action, icon, user, prompt, secret, info, error }`: watching it makes this process the session's polkit authentication agent —the one asked for the password when a program wants root—. `ready`: it is (one per session); `pending`: a program waits, with polkit's `message`, the `action`'s name and whose password it takes (`user`); `asking`: an answer is awaited, with the `prompt` and whether it is `secret`; `info`: what the system says meanwhile; `error`: `none`, or `wrong` after an answer that was not it (asked again, three times at most). Requests are attended one at a time, in the order they came | Linux: polkit over D-Bus; the answer is checked by polkit's own helper (its socket since polkit 126, the program before), not here |
| `sys.call("polkit.answer", text)` · `("polkit.cancel")` | answer what is being asked (an error if nothing is) · give up on the request being attended: the program is told it was cancelled | `services: "polkit.*"` |
| `sys.call("notifications.keep", true)` | they stop expiring on their own. Six seconds and gone is what a bar of bubbles wants; a notification **centre** —a tray, a history— keeps them until somebody decides, and the standard leaves that to the server. It is not only how long they show: once one is closed its application stops listening, so an expired one can no longer be opened from here | |
| `notification_history` | the same shape: the ones that went away **on their own**, unseen, newest first, the last 50 (not the ones someone closed or opened: those were seen). It lives while the process does. With `notifications.keep(true)` nothing expires, so it stays empty: then the list itself is the centre | Linux |
| `sys.call("notifications.forget", id)` · `("notifications.clear_history")` | one out of the history, or all of it | |
| `tray` | `{ { key, id, title, status, icon, menu, tooltip, tooltip_body }, … }`; `icon` is a name or a path, ready as it comes for `image … = from`; `tooltip` and `tooltip_body` are what the application says on hover (`""` if nothing), the body as plain text: its markup is dropped, an `<img>` is its `alt` and a `<br>` is a new line | Linux: `StatusNotifierItem`. Watcher if there is no other; if there is, host to it |
| `sys.call("tray.activate", key)` · `("tray.secondary", key)` · `("tray.context", key)` · `("tray.scroll", key, 1)` | the click, the middle one, asking it to show its menu (if it knows how), the wheel | |
| `sys.ask("tray.menu", key)` → `{ { id, label, enabled, separator, checked, children }, … }` · `sys.call("tray.menu_click", key, id)` | an icon's menu, as a tree, and choosing something from it | Linux: `com.canonical.dbusmenu` |
| `apps` | `{ { name, exec, icon, id, wmclass, local_name, categories, generic_name, comment, keywords, terminal, actions }, … }`, in alphabetical order; `id` is the `.desktop` file's name (`org.gnome.Calculator`, which is the app id its windows say) and `wmclass` its `StartupWMClass`, to tell whose a window is; `local_name` its name in the user's language (`Name[es]`: «Calculadora»; the name itself if it has none), `categories` its `Categories=` as written (`Utility;Calculator;`); `generic_name`, `comment` and `keywords` what to search besides the name, in the user's language like `local_name` and `""` if it has none (Firefox: «Web Browser», `Internet;WWW;Browser;Web;`); `terminal` true for one that runs in a terminal (`btop`); `actions` what else it offers, `{ { id, name, local_name, exec, icon }, … }` in the order its `Actions=` says (Firefox: «New Private Window»), `icon` `""` if the action names none | Linux: the `.desktop` files in `XDG_DATA_HOME` and `XDG_DATA_DIRS` (without the hidden ones), told again when one is installed, removed or changed (looked at every two seconds) |
| `sys.call("apps.launch", exec)` | launch one, or one of its `actions` by its `exec`, loose from the program; a `terminal` one in the user's terminal | Linux: `setsid -f sh -c`, in a scope of its own (`systemd-run --user --scope`) when there is a user systemd, so it outlives a restart of pleamar's unit; a `terminal` one through `xdg-terminal-exec`, an error if it is not installed |
| `sys.call("say", scene, command)` | tell another running scene one of `pleamar --say`'s commands, down its socket: `sys.call("say", "bar", "emit card_landed 3")`. What it answers is not brought back; it is an error when that scene is not running (the error names those that are) or does not understand the order. One call, one command, on one line, under 4096 bytes. Finding the scene, connecting and the answer take about a second at most: a scene that does not take the order by then is an error, and the order is then not delivered (unless the error says it may have been, when the second ran out mid-write; a write that ends just after it counts as delivered); and a `wait` still pending then returns as a delivered order does, so it does not say the condition held. The scene reached is the one `--say` reaches, and a note in the log says when it is shown on another display | `services: "say:bar"` for that scene, `"say:*"` for any; Unix sockets, where `--say` reaches (`PLEAMAR_SOCKETS`, or `$XDG_RUNTIME_DIR/pleamar`) |

Whatever is not a service yet can be got out with `spawn` and `run`, but that ties the script to one system: it is a stopgap until the service exists. `bar.luau` used to read the volume that way; it no longer calls anything of Linux's.

On exit, the program stops everything the logic left running; on reloading the logic, too. And if it is killed outright, it all goes with it just the same (on Linux the kernel is asked to see to it).

## Plugins: one logic per library

A library with a `.luau` next to it is a plugin (see [Language reference — version 0.1](11-language-reference.md), §5). Its logic is a script like any other, with four differences:

- **It only sees its own.** `text.now` is the scene's `Clock.now`; `fact.secret`, if `secret` belongs to the scene, does not exist: `plugin 'Nosy' has no fact called 'secret'. A plugin only sees what its library declares`. The same with `model`, `emit` and what it listens to: `on("tapped", …)` is `Clock.tapped`, and `on("key", …)` hears nothing.
- **Its permissions are approved by whoever uses it.** `pleamar --approve scene.plm` shows what each plugin asks for and asks about it (`--plugin Name --yes` approves that one without a terminal; `--json` says, for a program, what each asks for and whether it is approved); unapproved, it runs with none (`plugin 'Clock' wants to run 'date', but nobody has approved its permissions`), and if its code, what it asks for or a file of its folder that its commands name (`node tools/usage.js`) changes, it goes back to unapproved.
- **Its permissions are the ones in its own `.plm`**, not those of the scene using it: `plugin 'Nosy' has no permission to run 'sh'. If it should be able to, declare it in its own .plm (the scene's do not count)`.
- **It does not ask for gestures or move the typing caret**: that belongs to the scene. If it wants something to happen, it emits an event of its own, and the scene decides (`on Clock.tapped { play nod }`).

**The scene's logic owns its plugins' boundary**, by its full name. What one of the two logics writes there the other reads and hears; neither hears its own writes, as always:

| the scene's logic | the plugin's logic |
| --- | --- |
| `text["Weather.out"]`, `on("text:Weather.out", f)` | `text.out = "sunny"` |
| `fact["Weather.degrees"] = 30` | `fact.degrees`, `on("fact:degrees", f)` |
| `model["Weather.hours"] = { { h = "12:00" } }`, `on("model:Weather.hours", f)` | `model.hours`, `on("model:hours", f)`, and the other way round |
| `emit("Weather.ask", { city = "Málaga" })` | `on("ask", function(d) … d.city … end)` |
| `on("Weather.found", function(d) … end)` | `emit("found", { degrees = 23 })` |

What an event brings —a number, a text, a table (six levels, without functions)— reaches the other logic; the scene's rules hear the event, without it. The plugin still names nothing outside its boundary.

**`pleamar --check scene.plm` reads the logics too**, the scene's and each plugin's: a mistake of writing is an error with its file and line (`weather/weather.luau:4: Expected ')' (to close '(' at line 2), got 'end'`), and a name of the boundary written as it is that is not there is a warning with its line (`weather.luau:7:30: warning: there is no text 'cuidad' in the boundary of plugin 'Weather'. Did you mean 'ciudad'?`, or `'title' is a text in the scene, not a fact` for an `on("fact:title", …)` that would never fire). A name made up while running (`"rows." .. k`) is not looked at. The `.plm` of a plugin already said its errors with its own file and line.

Each plugin has its own Luau state —its memory, its timers, its processes— **and its own thread**: one that gets stuck does not hold back the others or the scene's logic. The scene may have no logic at all.

`require("lib/format")` loads `lib/format.luau` from that logic's folder (the scene's, or the plugin's), once; whatever it returns is the module. No `..`, no whole paths: it does not leave its folder. Saving a module reloads the logic, like saving the main file. A module shares the logic's globals (`on`, `fact`, `sys`…), not its `local`s: what it needs from the main file goes to it as an argument, `require("pages/wifi")(shared)`.

A **scene brought into another** (`import scene "bar.plm" as Bar`, see the
reference) keeps its logic: `bar.luau` runs as a plugin's does, in its own
Luau state, where `fact.volume` is `Bar.volume` and `on("rang", …)` hears
`emit("Bar.rang")`. Its permissions are its own `permissions { … }` and need
no `--approve`: opening the scene that brings it in is already deciding.
It also names the other scenes beside it whole, as its `.plm` does:
`fact["Bar.volume"]`, `text["Bar.title"]`, `model["Bar.rows"]` are read,
`on("Bar.rang", …)` and `on("fact:Bar.volume", …)` heard (also when Bar's
own logic wrote or emitted them), and `emit("Bar.ping")` tells Bar. Writing
another's is an error: tell its owner with an event.

## Permissions

The sandbox closes off `io` and `os`; what stays open to the system is `run`, `spawn` and the services, and **each scene declares which ones it uses**, in its `.plm`:

```
permissions { run: "date";  services: "workspaces", "workspaces.focus", "window", "audio", "audio.*" }
```

Undeclared is nothing. **A command can be declared with its first arguments**: `run: "node tools/usage.js"` lets the logic run `node tools/usage.js …` and nothing else with `node` —an interpreter declared bare runs whatever it is told, and the startup line warns about it—. **Listening is not commanding**: `"audio"` allows `sys.watch` and `sys.ask`; for `sys.call("audio.volume", …)` it takes `"audio.volume"`, or `"audio.*"` for everything in that service. A command or a service that is not there is an error there and then, which says what to write: `the scene gives no permission to run 'sh'. If it should be able to, declare it in the .plm: permissions { run: "sh" }`. It is in the scene and not in the script so that it can be read at a glance, before anything runs; it is printed at startup (`logic · permissions · commands: date · services: none`), and reloading the scene applies the new ones. `apps.launch` only launches applications the `apps` service has reported. **Talking to another scene is by its name**: `"say:bar"` lets the logic `sys.call("say", "bar", …)` and no other scene, `"say:*"` any, which the startup line marks with a ⚠ (it can tell any scene to `quit`); what the other scene does with the order is up to it, as it is with `--say`.

## The sandbox

- **No `io`, no `os.execute`**: it is Luau's `sandbox` mode. The only door to the system is `run`.
- **Memory ceiling**: 64 MB.
- **Counted seconds**: a handler that has gone more than 2 s without finishing is cut off, and the logic stays alive. Tested with a `while true do end`.
- **An error takes nothing down**: it is reported on the console and the rest of the handlers carry on.

> **`fact`, `text` and `sys` are always read there and then.** In its sandbox, Luau takes for granted that a global does not change and keeps what it read on load: `text.query` was worth `""` forever inside a handler. The compiler is told those three are mutable. It took an afternoon to find.

## Events with a payload

In the scene, `emit opened(i)`; in the logic, `on("opened", function(i) … end)`. Before, it took one event per index.

With several values, `emit moved(x: local.x, y: local.y)` (or by place, `emit pair(a, b)`, up to 8): the logic gets a table, by name (`p.x`) or by place (`t[1]`). An event can say what it carries, `event moved(x, y) ->`, and then every `emit` of it, in the scene or from another scene brought in, has to give all of it. From the logic, `emit("moved", { x = 3, y = 4 })` hands the table to the logics that hear it.

What the person does outside the scene's own shapes reaches the logic too: a paste in a field (`on("paste:draft", function(p) … end)`, with `p.text`, `p.image`, `p.uris`), a link pressed in a markdown text (`on("link", function(href) … end)`), a picture copied (`sys.ask("clipboard.image")`, and `sys.call("clipboard.image.save", path)` to keep it), and the desktop's own file chooser (`sys.ask("files.pick", "open", "*.md,*.png")`, `"save"`, `"folder"`), which belongs to the scene's window.

## Lists that come from data

```lua
model.rows = { { label = "Open", enabled = true }, { label = "Quit" } }     -- the whole list, at once
local r = model.rows[i + 1]                                                 -- and it reads back just as it was put in
```

The scene declares the shape (`model rows max 14 { label: text; enabled: bool = true }`) and walks it (`for r in rows`). From each record the declared fields are taken and the rest is ignored, so **a service's list is handed over as it comes**: `sys.watch("tray", function(list) model.icons = list end)`. Whatever is missing is worth its default value; a `bool` is written with `true` and `false`, an enum with the name of its value (or its number), and an inner list (`list items`) with another table, which is handed out the same way; and where a number is expected, a list counts as how many it holds (`children: number` with a submenu inside). Only what has changed since the previous time travels to the renderer. **The assignment is atomic**: if one record is wrong, it is an error and the list that was there stays as it was.

Reading it back gives the original table, with everything it carried (`model.icons[1].key`), not just the scene's fields: it is where the logic keeps what the scene does not need to see.

## Cross-platform

Luau is compiled from its C++ sources, and `mlua` supports it on all three systems. There is no cross-compiler here, so `./portable.sh` checks Windows and macOS **without** the `luau` feature and says so. The glue is Rust with nothing of the system in it.
