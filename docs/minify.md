# Images and windows drawn smaller than they are

An icon asked for at 128 pixels and drawn at 41, a window shrunk to a
thumbnail, a wallpaper bigger than the screen: each screen pixel covers
several of the picture's. Read once there —as pleamar did before—, the picture
comes out soft in some places and grainy in others, and fine detail (text,
stripes, a grid) turns into a moiré that crawls when it moves.

So, **when a screen pixel spans more than 1.25 of the picture's**, pleamar
averages what falls under it: up to 4 × 4 bilinear samples spread over the
pixel's footprint, one for each picture pixel it spans (2 × 2 for 128 → 60, 4
× 4 from 128 → 32 down). It is what a mipmap does, without one: the pictures
are not stored again at smaller sizes (no extra memory), the images' atlas is
not rearranged, and a window, whose picture changes every frame, needs nothing
made for it first.

## When it does nothing

- **At its own size or larger** (up to 1.25 picture pixels a screen pixel):
  the one sample of always, the same bit for bit.
- **Letters**: they are laid out at the size they are drawn.
- **Pixel art** (an image pleamar keeps sharp): its pixels stay pixels.
- **A group's layer** (`opacity` with effects): painted at the screen's size.

The samples never leave the image's own piece of the atlas, so one image
takes nothing from its neighbour however small it is drawn. With a rotation
or a scale that is not the same both ways, the larger of the two decides.

## What it costs, and turning it off

Only what is drawn smaller pays: up to 16 samples a pixel where there was one.
Over a large area —more than a million screen pixels, a wallpaper twice the
screen's size— it takes at most 2 × 2, so a picture as big as the screen costs
4 samples a pixel, not 16.

| Variable | What it does |
|---|---|
| `PLEAMAR_MINIFY=off` | the one sample of always, everywhere (also `0`, `no`, `false`) |
| `PLEAMAR_MINIFY_AREA=N` | above `N` screen pixels, 2 × 2 at most (`1000000` by default; `0`: 2 × 2 always) |

Both are read when pleamar starts.

Measured on a software card (Mesa's lavapipe, so the times are not a graphics
card's: compare them with each other), as pleamar's CPU filming 60 frames:
a picture twice the screen's size drawn over all of it costs about 1 ms more
a frame; six windows shrunk to an overview (0.29 and 0.14 of their size), about
1.7 ms more. A whole desktop scene with a wallpaper larger than its screen, 3 to 8 %
more. If that matters more than the picture on a slow machine,
`PLEAMAR_MINIFY=off`.

*Possible later*: a property of each image (`smooth: off`) for a scene that
wants the old sampling in one place only. It would be a change to the
language, so it is not here yet.
