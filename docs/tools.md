# Tools

## In the editor

`pleamar --lsp` is a language server over stdio, with this same compiler behind
it: mistakes as you type, which words fit here, what the word under the cursor
means, go to where a name was declared, where it is used, and renaming it.
`pleamar --highlight vim|vscode` writes the syntax file straight from the
vocabulary, so it cannot fall behind. Both, already generated, in
[`editor/`](../editor/).

For the logic, `pleamar --luau-types scene.plm > pleamar.d.luau` writes a
definitions file for [luau-lsp](https://github.com/JohnnyMorganz/luau-lsp): the
globals a logic is given (`fact`, `on`, `sound`, `sys`…), typed, and that
scene's own facts, texts, models and events, so `fact.` completes and
`fact.mode = "critcal"` is underlined. How to point an editor at it:
[docs/10-luau-logic.md](10-luau-logic.md#the-logic-in-your-editor).

## From the source

```sh
cargo build --release
./target/release/pleamar --scene examples/bar.plm       # a bar: workspaces, window, time and volume
./target/release/pleamar --scene examples/long-list.plm # five thousand rows in sixteen copies
./target/release/pleamar --scene examples/paths.plm     # paths, curves and gradients
./target/release/pleamar --scene examples/window.plm    # a normal window, with its frame
./target/release/pleamar --scene examples/icons.plm     # the system tray; right-click opens an icon's menu
./target/release/pleamar --scene examples/settings.plm  # saves what you pick in its own folder
./target/release/pleamar --scene examples/tide.plm      # a night sea that rises: a shader, glass, svgs by layers
./target/release/pleamar --scene examples/catch.plm     # a small game: frames, keys, a game controller, sound
./target/release/pleamar --check examples/bar.plm       # reads it, says whether it is fine, exits
./run-tests.sh                                          # the language tests, and the examples in its reference
```

Rebuild pleamar itself and the running one starts again with the same arguments.

## Day to day

| | |
| --- | --- |
| `--screen A,B` | which monitors |
| `--say NAME …` | talk to a running scene from outside, or from a compositor shortcut; of two scenes with one name, it reaches the one on your terminal's display |
| `--mouse "360,90@500 click@3200"` | a pretend mouse, to rehearse without touching the real one |
| `--stall MS` | stall the logic on purpose and watch the screen carry on |
| `PLEAMAR_GPU=high` | on a laptop with two cards, paint on the fast one. By default pleamar paints on the card the compositor says it paints with, or on the frugal one |
| `--report` | if something stutters: measures every scene for 30 seconds while you use the desktop and writes down what it saw —frame times, where the late frames went, the card, the CPU's clock and temperature, what else took the CPU— in `~/pleamar-report-….md`, with no personal data. Send it to us in an issue |
