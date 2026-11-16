// What the glass cannot see behind it, filled from what it can (push-pull).
//
// The unmixed background has holes: under what the scene paints opaque on
// its glass —a card, a picture, a text— the capture is all ours and nothing
// behind can be got out of it (alpha 0), and along such a thing's edge only a
// little (alpha between 0 and 1: see `unmix_at`). The frosting filled small
// holes with what is around them, but not one as big as a card: under it the
// frosted backdrop stayed unknown, and a glass that looks a little to one
// side —its bevel, and most of all `dome:`, which magnifies towards the
// middle— showed it as a dark copy of the card beside the card.
//
// So before it is frosted, the background is averaged down level by level
// (each pixel of a level the mean of four of the one above, in premultiplied
// alpha, so what is unknown weighs nothing), until a level is a few pixels;
// and then, from the smallest up, each level takes from the one below it what
// it does not know itself. What comes out knows everything: a hole gets the
// colours round it, blending smoothly from one side to the other, and what
// was known stays as it was.
//
// Only round the glasses that need it (a `dome:`; see `Lens::prepare`): the
// first level is read from that box of the background (`place`), and the last
// step up writes only that box; the levels are the box's, anchored at its
// corner, so a pixel of a level is exactly two by two of the one above.

struct VertexOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) v: u32) -> VertexOut {
    let p = vec2<f32>(f32((v << 1u) & 2u), f32(v & 2u));
    var s: VertexOut;
    s.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    return s;
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var coarse: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
// The box of the background that is filled, in its pixels: corner and size.
struct Place {
    origin: vec2<f32>,
    size: vec2<f32>,
};
@group(0) @binding(3) var<uniform> place: Place;

// The mean of the (up to) four pixels at `p` of `src` that are inside `[lo, hi]`
// (premultiplied: what is unknown, alpha 0, adds nothing). Past an odd edge
// there is no pixel, and it counts for nothing: the last one is not taken twice.
fn mean4(p: vec2<i32>, lo: vec2<i32>, hi: vec2<i32>) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    var n = 0.0;
    for (var k = 0; k < 4; k++) {
        let q = p + vec2<i32>(k & 1, k >> 1);
        if (all(q >= lo) && all(q <= hi)) {
            sum += textureLoad(src, q, 0);
            n += 1.0;
        }
    }
    return sum / max(n, 1.0);
}

// Down from the background: its box only, from its corner.
@fragment
fn down_first(e: VertexOut) -> @location(0) vec4<f32> {
    let lo = vec2<i32>(place.origin);
    let hi = min(lo + vec2<i32>(place.size), vec2<i32>(textureDimensions(src))) - 1;
    return mean4(lo + vec2<i32>(e.pos.xy) * 2, lo, hi);
}

// Down: the mean of the four pixels of the level above.
@fragment
fn down(e: VertexOut) -> @location(0) vec4<f32> {
    return mean4(vec2<i32>(e.pos.xy) * 2, vec2<i32>(0), vec2<i32>(textureDimensions(src)) - 1);
}

// The smallest level: what it knows, as if it knew all of it (its colour over
// its alpha). If it knows nothing, nothing.
@fragment
fn top(e: VertexOut) -> @location(0) vec4<f32> {
    let l = textureLoad(src, vec2<i32>(e.pos.xy), 0);
    if (l.a < 1e-5) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(l.rgb / l.a, 1.0);
}

// What is known at `l`, and where it is not, the level below read smoothly at
// `at` (its own pixels: half of these), so the filling has no blocks.
fn pulled(l: vec4<f32>, at: vec2<f32>) -> vec4<f32> {
    let c = textureSampleLevel(coarse, linear_sampler, at / vec2<f32>(textureDimensions(coarse)), 0.0);
    // (normalised: the level below is filled, but a bilinear read at the edge of what it knows can be a little short of 1)
    let filled = select(vec4<f32>(0.0), vec4<f32>(c.rgb / c.a, 1.0), c.a > 1e-5);
    return l + (1.0 - l.a) * filled;
}

// Up: this level, and where it does not know, the level below it (already filled).
@fragment
fn pull(e: VertexOut) -> @location(0) vec4<f32> {
    return pulled(textureLoad(src, vec2<i32>(e.pos.xy), 0), e.pos.xy * 0.5);
}

// The last step up, into the background's box: from its corner.
@fragment
fn pull_last(e: VertexOut) -> @location(0) vec4<f32> {
    return pulled(textureLoad(src, vec2<i32>(e.pos.xy), 0), (e.pos.xy - place.origin) * 0.5);
}
