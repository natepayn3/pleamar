# What's new

The highlights of each line of releases; every release, with what changed, is in [Releases](https://github.com/k4ditano/pleamar/releases).

- **0.8 (experimental, on the `exp2` branch)**: several scenes in one process
  that talk to each other, checked at both ends (`import scene`, `uses scene`,
  events that say what they carry); text as people and agents write it
  (markdown with tables, links, pictures and code blocks, fields of several
  lines with undo, an input method, mentions and paste); lists with identity
  and windows that draw their own frame, popups out of popups, `pleamar:ui`'s
  menus and tables; colours with their transparency; a real blur and a finer
  glass; it runs on more graphics cards and says why when one cannot; and
  `--film` to see a scene without a screen. Try it with
  `git clone -b exp2 https://github.com/k4ditano/pleamar`.
- **0.6 — what the language would not let you write**: a quote inside a text
  (`\"`), a block that opens on the line after its header, and a colour chosen
  with `pick` or `if` inside a `let`.
- **0.5 — a round costs what changed**: a big scene only works out again what
  changed in it (a pleamar-wm round went from 1.2 ms to 0.4 ms), edges are
  sharper and blended as light would blend them, layouts can hold a chat
  (`stick: end`, `place:`, `tight:`), `motion.reduced` and `color.scheme` come
  from the desktop, and a scene paints on the same card the compositor paints with.
- **0.5.1 – 0.5.3**: the mouse's side buttons reach a scene, and apps and icons
  installed while a shell runs show up without restarting it.

Every release, with what changed: [Releases].


[Releases]: https://github.com/k4ditano/pleamar/releases
