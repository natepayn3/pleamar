# Programs' windows on whole pixels

A program draws its window on whole pixels. Shown where a scene put it —a
position a spring worked out, a third of a monitor, a size that is a fraction
of a pixel off its buffer— it would fall between two rows of the screen's
pixels and be read halfway between them: its letters a little soft, the whole
window slightly out of focus.

So, **when a window is drawn at its own size** —a pixel of its buffer to a
pixel of the screen, give or take 0.2 %—, **without turning**, pleamar puts it
on whole pixels: its corner moves to a whole pixel (to the nearest across;
upwards, never down, so it does not leave a hairline under a title bar drawn
above it) and its size is made exactly its buffer's. It moves half a pixel at
the most, and nothing in the scene changes: its facts, its frame and its
shadow are where the scene says.

## When it does nothing

- A window drawn smaller or larger than it is (an overview, a window
  opening or closing, a monitor whose scale is a fraction): it is scaled as the
  scene says, and averaged when smaller ([minify](minify.md)).
- A window turned or mirrored.
- A picture of a window (`ask: -1, -1`).
- One already on whole pixels: the same picture, bit for bit.

While a window slides at its own size it moves a whole pixel at a time, which
at sixty frames a second is not seen.

## Turning it off

`PLEAMAR_PIXELSNAP=off` (also `0`, `no`, `false`), read when pleamar starts:
each window exactly where the scene puts it, as before.
