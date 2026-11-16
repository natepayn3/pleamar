// What is behind the glass, from a capture of the screen.
//
// The capture was taken with our surface on top, so it is the blend the
// compositor makes: capture = ours + (1 − our alpha) · background. We know
// ours, because it is the last canvas that was presented: the background gets
// unmixed. Where ours is almost opaque —text, an icon— it cannot be, and the
// background that was there stays; the same if the result is not a possible
// colour, which is what happens when the capture is of a different frame than
// the canvas (something was moving).

struct VertexOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) v: u32) -> VertexOut {
    // A triangle that covers the whole screen.
    let p = vec2<f32>(f32((v << 1u) & 2u), f32(v & 2u));
    var s: VertexOut;
    s.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    return s;
}

// ── unmix ─────────────────────────────────────────────────────────
@group(0) @binding(0) var capture: texture_2d<f32>;
@group(0) @binding(1) var canvas: texture_2d<f32>;
@group(0) @binding(2) var previous: texture_2d<f32>;
@group(0) @binding(3) var linear_sampler: sampler;
// Where the capture falls on the canvas, in pixels: x, y, width, height; and
// with what opacity the compositor blended ours.
struct Bounds {
    rect: vec4<f32>,
    opacity: f32,
    // 1 when the capture is the same as the one before it: see `settle`.
    same: f32,
};
@group(0) @binding(4) var<uniform> bounds: Bounds;

// What is unmixed at a pixel, and whether it says that the capture is not of
// this canvas: under our opaque parts the capture should be ours, and nowhere
// should what is behind come out as a colour that cannot be.
struct Unmixed {
    value: vec4<f32>,
    wrong: bool,
};

fn unmix_at(pos: vec2<f32>) -> Unmixed {
    let px = vec2<i32>(pos);
    // The compositor takes the capture at its own scale: it is read proportionally.
    let f = textureSampleLevel(capture, linear_sampler, (pos - bounds.rect.xy) / bounds.rect.zw, 0.0).rgb;
    // Ours, as the compositor blended it: times its opacity.
    let s = textureLoad(canvas, px, 0) * bounds.opacity;
    let a = textureLoad(previous, px, 0);
    let remaining = 1.0 - s.a;
    // Under our opaque parts —a text, an icon— nothing can be seen: it is
    // marked as unknown (alpha 0), and `fill.wgsl` fills it with what is
    // around. Keeping the last one seen left it stale forever.
    if (remaining < 0.03) {
        return Unmixed(vec4<f32>(0.0), any(abs(f - s.rgb) > vec3<f32>(0.1)));
    }
    let background = (f - s.rgb) / remaining;
    // A background that cannot be: the capture is not of this canvas. With a
    // margin for rounding, which grows when dividing by what remains: half a
    // level over what remains.
    let margin = 0.03 + 0.5 / 255.0 / remaining;
    if (any(background < vec3<f32>(-margin)) || any(background > vec3<f32>(1.0 + margin))) {
        return Unmixed(a, true);
    }
    // How far it can be trusted: dividing by what remains multiplies the
    // capture's rounding by as much, so along the edge of something opaque
    // —a card on the glass, where what remains runs from a glass's 12 % down
    // to nothing— the background came out as a ring of wrong colours that
    // passed for known, and a glass that looks a little to one side (a
    // `dome:`) showed it as an outline round the card. There it is known only
    // in part, and `fill.wgsl` takes the rest from round it. (Premultiplied,
    // like everything the frosting reads; what is known is alpha 1.)
    let sure = smoothstep(0.03, 0.09, remaining);
    return Unmixed(vec4<f32>(clamp(background, vec3<f32>(0.0), vec3<f32>(1.0)) * sure, sure), false);
}

@fragment
fn unmix(e: VertexOut) -> @location(0) vec4<f32> {
    return unmix_at(e.pos.xy).value;
}

// ── own ───────────────────────────────────────────────────────────
// A scene that is the whole screen (pleamar-wm's) has nothing behind it but
// what it painted itself before the glass: that, copied as it is. It is kept
// only round the glass: a pixel of it is the surface's at `own_at` from it.
@group(0) @binding(9) var<uniform> own_at: vec4<f32>;

@fragment
fn own(e: VertexOut) -> @location(0) vec4<f32> {
    return textureLoad(capture, vec2<i32>(e.pos.xy + own_at.xy), 0);
}

// A scene on a surface has both: what it painted itself before the glass, and
// behind that, wherever it left a gap, the unmixed background (`canvas` here).
// What is known of the result is what either of them knows.
@fragment
fn own_over(e: VertexOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(e.pos.xy + own_at.xy);
    let ours = textureLoad(capture, px, 0);
    let behind = textureLoad(canvas, px, 0);
    // (the background is premultiplied: see `unmix_at`)
    return ours + (1.0 - ours.a) * behind;
}
