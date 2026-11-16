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

// Down: the mean of the four pixels of the level above (premultiplied: what
// is unknown, alpha 0, adds nothing). An odd edge repeats its last pixel.
@fragment
fn down(e: VertexOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(e.pos.xy) * 2;
    let last = vec2<i32>(textureDimensions(src)) - 1;
    let a = textureLoad(src, min(p, last), 0);
    let b = textureLoad(src, min(p + vec2<i32>(1, 0), last), 0);
    let c = textureLoad(src, min(p + vec2<i32>(0, 1), last), 0);
    let d = textureLoad(src, min(p + vec2<i32>(1, 1), last), 0);
    return (a + b + c + d) * 0.25;
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

// Up: this level, and where it does not know, the level below it (already
// filled), read smoothly so the filling has no blocks.
@fragment
fn pull(e: VertexOut) -> @location(0) vec4<f32> {
    let l = textureLoad(src, vec2<i32>(e.pos.xy), 0);
    let uv = e.pos.xy / vec2<f32>(textureDimensions(src));
    let c = textureSampleLevel(coarse, linear_sampler, uv, 0.0);
    // (normalised: the level below is filled, but a bilinear read at the edge of what it knows can be a little short of 1)
    let filled = select(vec4<f32>(0.0), vec4<f32>(c.rgb / c.a, 1.0), c.a > 1e-5);
    return l + (1.0 - l.a) * filled;
}
