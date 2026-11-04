// The glass looks at where the edge points with `dpdx`/`dpdy` of the
// distance. All the pixels of a quad belong to the same element, so the
// branches by kind do not split a 2×2 block in two: the derivative holds.
diagnostic(off, derivative_uniformity);

// One quad per element. Each element brings its bounding box, and a pixel only
// runs the shapes of the element that covers it: the scene can grow without
// every pixel paying for all of it.
//
// An element is a body —one or several shapes blended by a smooth minimum,
// with its paint, border, light and shadow— or a piece of the atlas. Everything
// is signed distances and premultiplied alpha.

struct U {
    header: vec4<f32>,   // logical width and height, time, scale (real pixels per logical pixel)
    hud: vec4<f32>,      // origin x, period in ms, logic stalled, origin y: where this surface looks at the scene from
    times: array<vec4<f32>, 30>,
    backdrop: vec4<f32>, // x: there is an unmixed background to read (the lens) · yz: where it was pressed · w: how much light it lets through
    ripple: vec4<f32>,   // where it was last pressed (x, y), seconds since, and whether it still runs
};

struct Shape {
    a: vec4<f32>,      // kind, blend, stroke (0 = filled), -
    b: vec4<f32>,      // centre x, y · half width, half height (segment: vector to the other end)
    c: vec4<f32>,      // radius, rotation, scale x, scale y (arc: radius, rotation, half aperture, -; path: first point, rotation, how many, closed)
    t0: vec4<f32>,     // what is inherited, already inverted: from screen to local (2×2 matrix)…
    t1: vec4<f32>,     // …its translation, and how much it stretches distances
};

struct Element {
    header: vec4<f32>,    // kind, first shape (or layer no.), no. of shapes (or "tinted"), alpha
    bounds: vec4<f32>,    // x0, y0, x1, y1
    color0: vec4<f32>,    // r, g, b, edge
    color1: vec4<f32>,    // -, -, -, gradient (0 no, 1 linear, 2 radial)
    line: vec4<f32>,      // gradient: from (x, y) to (x, y); radial: centre (x, y), radius, -
    light: vec4<f32>,     // amount, from y, height, border thickness
    border: vec4<f32>,    // r, g, b, -
    shadow: vec4<f32>,    // dx, dy, blur, alpha
    clips: vec4<f32>,     // up to four shapes it is clipped to (-1 = none)
    dest: vec4<f32>,      // texture: x, y, width, height · gradient: first stop, how many, -, -
    uv: vec4<f32>,
    t0: vec4<f32>,        // from screen to local, as in the shapes
    t1: vec4<f32>,
    glass2: vec4<f32>,    // glass: towards its light (x, y), whether it has one, dome
    glass3: vec4<f32>,    // glass: dispersion, ripple, centre (x, y)
    glass4: vec4<f32>,    // glass: tint, saturation, mirror, highlight
};

const ELLIPSE: u32 = 0u;
const RECT: u32 = 1u;
const ARC: u32 = 2u;
const SEGMENT: u32 = 3u;
const PATH: u32 = 4u;

const BODY: u32 = 0u;
const TEXTURE: u32 = 1u;
const LAYER: u32 = 2u;
// One of the scene's own shaders: `user_shader`, added after this file (see shaders.rs).
const SHADER: u32 = 3u;
// A particle emitter: see `vs_particle`.
const PARTICLES: u32 = 4u;
const HUD: u32 = 9u;

const FAR: f32 = 1e6;

// 0 · the scene, the same for all surfaces.
@group(0) @binding(0) var<storage, read> shapes: array<Shape>;
@group(0) @binding(1) var<storage, read> elements: array<Element>;
@group(0) @binding(2) var atlas: texture_2d<f32>;
@group(0) @binding(3) var atlas_sampler: sampler;
// The points of the paths, in x, y pairs, relative to the centre of each one.
@group(0) @binding(4) var<storage, read> points: array<f32>;
// The gradient stops: r, g, b and where each one falls, in order.
@group(0) @binding(5) var<storage, read> stops: array<vec4<f32>>;
// The windows of the scene's compositor (`windows`), one per layer.
@group(0) @binding(6) var windows: texture_2d_array<f32>;
// A picture too big for the atlas, a texture of its own: bound only while
// the element that shows it is drawn.
@group(0) @binding(7) var picture: texture_2d<f32>;
// 2 · what belongs to each surface: its size and its scale.
@group(2) @binding(0) var<uniform> u: U;
// Groups with opacity are painted separately, here, and blended at once.
@group(1) @binding(0) var layers: texture_2d_array<f32>;
// 3 · what is behind the glass, already unmixed: sharp and frosted.
@group(3) @binding(0) var backdrop_sharp: texture_2d<f32>;
@group(3) @binding(1) var backdrop_blurred: texture_2d<f32>;
@group(3) @binding(2) var backdrop_sampler: sampler;

// The lens: how much of the glass is bent background (the rest, what is behind
// as is, which the compositor puts underneath and lets the next capture unmix)
// and how much tint.
const LENS_ALPHA: f32 = 0.88;
const LENS_TINT: f32 = 0.22;
// The thickness of the glass, in bevel widths: how much it bends.
const LENS_THICKNESS: f32 = 1.4;
// Its refractive index (crown glass), and how much of what is around the rim
// mirrors (Schlick's F0, ((n − 1) / (n + 1))²).
const LENS_IOR: f32 = 1.5;
const LENS_F0: f32 = 0.04;

// The height of the bevel across it, t from 0 at the edge to 1 where the flat
// top begins: (1 − (1 − t)³)^⅓, vertical at the edge and easing into the top
// over the whole bevel. (A squircle's fourth power bent nearly all of the view
// in the bevel's outer third and stopped short: where it stopped read as a
// line round the inside of the glass.) The profile, the cap below and the
// rim's reflection follow True Glass for Omarchy, by Eddie175, itself on
// hyprnux's HyprGlass.
fn rim_height(t: f32) -> f32 {
    let s = 1.0 - t;
    return pow(max(1.0 - s * s * s, 0.0), 1.0 / 3.0);
}

// Its slope, dheight/dt.
fn rim_slope(t: f32) -> f32 {
    let s = 1.0 - t;
    return s * s * pow(max(1.0 - s * s * s, 1e-4), -2.0 / 3.0);
}

// The index of a colour of light, for a dispersion: Cauchy's n = A + B/λ²,
// fitted to n at the sodium line and to an Abbe number. `dispersion: 100%` is
// an Abbe number of 10 —a lot more than real glass, which would show none at
// this size—; 0 is none.
fn ior_for(lambda2: f32, dispersion: f32) -> f32 {
    if (dispersion <= 0.0) { return LENS_IOR; }
    let abbe = 10.0 / dispersion;
    let b = (LENS_IOR - 1.0) / (abbe * 1.9105);
    return LENS_IOR - b / 0.34527 + b / lambda2;
}

// Smoked (`smoke`) or milky (`milk`) glass: what is seen through it, taken
// down the brighter it is, or lifted the darker it is, its colour kept.
fn glass_tone(c: vec3<f32>, smoke: f32, milk: f32, frost: f32) -> vec3<f32> {
    let luma = vec3<f32>(0.2126, 0.7152, 0.0722);
    var o = c * (1.0 - smoke * 0.58 * (0.35 + 0.65 * smoothstep(0.1, 0.85, dot(c, luma))));
    let base = mix(0.15, 0.25, frost);
    let lift = milk * min(base + 0.4 * (1.0 - smoothstep(0.3, 0.85, dot(o, luma))), 0.85);
    return mix(o, vec3<f32>(1.0), lift);
}

// Where the ray seen through this point comes from: the ray straight down
// enters the glass through its surface, tilted by the bevel (`slope` towards
// `outward`), bends by Snell's law, and crosses the glass's height `z` down to
// what is behind. Eased into `cap` —half the bevel—, not clipped at it: at a
// corner the bends of two edges add up, and uncapped the view folded back on
// itself into a dark curl; clipped, it went from growing to flat in a pixel, a
// crease along the glass.
fn refract_offset(outward: vec2<f32>, slope: f32, ior: f32, z: f32, cap: f32) -> vec2<f32> {
    let n = normalize(vec3<f32>(outward * slope, 1.0));
    let t = refract(vec3<f32>(0.0, 0.0, -1.0), n, 1.0 / ior);
    if (t.z > -1e-3) { return vec2<f32>(0.0); }
    let o = t.xy * (z / -t.z);
    let l = length(o);
    if (l < 1e-4) { return o; }
    return o * (cap * tanh(l / max(cap, 1e-3)) / l);
}

// How deep a point is inside a rounded box, measured so the bevel has no
// crease. The distance to the edge draws, going in, boxes with ever sharper
// corners, and a bevel that follows them folds on the diagonal: a seam at 45°
// in what the corner bends. Here the contour at depth d is the box brought in
// by d with a corner of r + d —ever rounder—, so the bevel turns the corner
// the way a polished edge does. In the corner that is |a + 2d| = r + d, a
// quadratic in d; along the sides, the plain distance. (The idea is
// HyprGlass's, by hyprnux.)
fn crease_free_depth(p: vec2<f32>, half: vec2<f32>, radius: f32) -> f32 {
    let r = min(radius, min(half.x, half.y));
    let a = abs(p) - half + vec2<f32>(r);
    var d = min(r - a.x, r - a.y);
    let b = 4.0 * (a.x + a.y) - 2.0 * r;
    let c = dot(a, a) - r * r;
    let disc = b * b - 28.0 * c;
    if (disc >= 0.0) {
        let dc = (-b + sqrt(disc)) / 14.0;
        if (a.x + 2.0 * dc >= 0.0 && a.y + 2.0 * dc >= 0.0) { d = dc; }
    }
    return d;
}

struct VertexOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) element: u32,
};

@vertex
fn vs(@builtin(vertex_index) v: u32, @builtin(instance_index) i: u32) -> VertexOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let c = elements[i].bounds;
    // A particle emitter is not drawn here but by `vs_particle`: nothing.
    var p = mix(c.xy, c.zw, corners[v]);
    if (u32(elements[i].header.x) == PARTICLES) { p = c.xy; }
    var s: VertexOut;
    let q = p - u.hud.xw;
    s.pos = vec4<f32>(q.x / u.header.x * 2.0 - 1.0, 1.0 - q.y / u.header.y * 2.0, 0.0, 1.0);
    s.element = i;
    return s;
}

fn to_local(p: vec2<f32>, t0: vec4<f32>, t1: vec4<f32>) -> vec2<f32> {
    return vec2<f32>(dot(t0.xy, p), dot(t0.zw, p)) + t1.xy;
}

/// Pixel art made bigger, however much a group scales it: inside each of its
/// pixels the same texel, and only across the one screen pixel where two of
/// them meet are they blended —sharp at any size, and even at sizes that are
/// not a whole number of times its own—. How many texels a screen pixel spans
/// comes from the element's box and its transform: derivatives are not to be
/// had in a branch that differs from element to element.
fn sharp_texel(el: Element, at: vec2<f32>) -> vec2<f32> {
    let per_local = abs(el.uv.zw - el.uv.xy) / max(el.dest.zw, vec2<f32>(0.0001));
    let per_px = max(per_local * vec2<f32>(length(el.t0.xz), length(el.t0.yw)), vec2<f32>(0.0001));
    let p = at - 0.5;
    let i = floor(p);
    let g = clamp((p - i - 0.5) / per_px + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
    return i + g + 0.5;
}

fn rotate_point(p: vec2<f32>, pivot: vec2<f32>, angle: f32) -> vec2<f32> {
    if (angle == 0.0) { return p; }
    let q = p - pivot;
    let c = cos(angle);
    let s = sin(angle);
    return vec2<f32>(c * q.x + s * q.y, -s * q.x + c * q.y) + pivot;
}

fn rounded_rect(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// A path: the distance to the nearest segment, signed if it is closed (the
// winding number says whether the point is inside). It is the SDF of a polygon.
fn path_distance(p: vec2<f32>, first: u32, n: u32, closed: bool) -> f32 {
    if (n < 2u) { return FAR; }
    var best = FAR;
    var inside = 1.0;
    let segments = select(n - 1u, n, closed);
    for (var i = 0u; i < segments; i++) {
        let j = (i + 1u) % n;
        let a = vec2<f32>(points[(first + i) * 2u], points[(first + i) * 2u + 1u]);
        let b = vec2<f32>(points[(first + j) * 2u], points[(first + j) * 2u + 1u]);
        let e = b - a;
        let w = p - a;
        let h = clamp(dot(w, e) / max(dot(e, e), 1e-6), 0.0, 1.0);
        best = min(best, length(w - e * h));
        if (closed) {
            // Crossings of the horizontal through the point: even, outside; odd, inside.
            let below_a = p.y >= a.y;
            let above_b = p.y < b.y;
            let side = e.x * w.y > e.y * w.x;
            if ((below_a && above_b && side) || (!below_a && !above_b && !side)) { inside = -inside; }
        }
    }
    return best * inside;
}

// The colour of a gradient at `t`: between the two stops around it.
fn between_stops(first: u32, n: u32, t: f32) -> vec3<f32> {
    var prev = stops[first];
    if (t <= prev.w || n < 2u) { return prev.rgb; }
    for (var i = 1u; i < n; i++) {
        let cur = stops[first + i];
        if (t <= cur.w) {
            let d = max(cur.w - prev.w, 1e-4);
            return mix(prev.rgb, cur.rgb, (t - prev.w) / d);
        }
        prev = cur;
    }
    return prev.rgb;
}

/// A tile of a body's table (see `fs`): where its list of shapes starts in
/// `points` and how many it has. Outside the table, none.
fn tile_entry(t: u32, at: vec2<f32>) -> vec2<u32> {
    let size = points[t + 2u];
    let cell = floor((at - vec2<f32>(points[t], points[t + 1u])) / size);
    let cols = points[t + 3u];
    let rows = points[t + 4u];
    if (cell.x < 0.0 || cell.y < 0.0 || cell.x >= cols || cell.y >= rows) { return vec2<u32>(0u, 0u); }
    let slot = t + 5u + u32(cell.y * cols + cell.x) * 2u;
    return vec2<u32>(u32(points[slot]), u32(points[slot + 1u]));
}

fn smooth_min(a: f32, b: f32, k: f32) -> f32 {
    if (k < 0.5 || a > FAR * 0.5 || b > FAR * 0.5) { return min(a, b); }
    let h = max(k - abs(a - b), 0.0) / k;
    return min(a, b) - h * h * k * 0.25;
}

fn shape_distance(k: u32, point: vec2<f32>) -> f32 {
    let f = shapes[k];
    // First what is inherited from the group, then its own rotation about its centre.
    let p = rotate_point(to_local(point, f.t0, f.t1), f.b.xy, f.c.y) - f.b.xy;
    var d = FAR;
    switch u32(f.a.x) {
        case ELLIPSE: {
            let e = f.c.zw;
            d = (length(p / e) - f.c.x) * min(e.x, e.y);
        }
        case RECT: {
            if (f.b.z >= 0.5 && f.b.w >= 0.5) { d = rounded_rect(p, f.b.zw, f.c.x); }
        }
        case ARC: {
            // An arc like "∩", open as much as the half aperture says.
            let sc = vec2<f32>(sin(f.c.z), cos(f.c.z));
            let q = vec2<f32>(abs(p.x), -p.y);
            if (sc.y * q.x > sc.x * q.y) { d = length(q - sc * f.c.x); } else { d = abs(length(q) - f.c.x); }
        }
        case SEGMENT: {
            let h = clamp(dot(p, f.b.zw) / max(dot(f.b.zw, f.b.zw), 0.0001), 0.0, 1.0);
            d = length(p - f.b.zw * h);
        }
        case PATH: {
            d = path_distance(p, u32(f.c.x), u32(f.c.z), f.c.w > 0.5);
        }
        default: {}
    }
    // The stroke turns any shape into its outline: a circle into a ring.
    if (f.a.z > 0.0 && d < FAR * 0.5) {
        let kind = u32(f.a.x);
        // What is already a line only gets thicker; what encloses something stays as its outline.
        let is_line = kind == ARC || kind == SEGMENT || (kind == PATH && f.c.w <= 0.5);
        if (is_line) { d = d - f.a.z * 0.5; } else { d = abs(d) - f.a.z * 0.5; }
    }
    if (d > FAR * 0.5) { return d; }
    return d * f.t1.z;
}

// How much of a REAL pixel the shape covers: a straight ramp one pixel wide,
// which is what a pixel's box sees of an edge (at scale 2, half that in logical
// pixels, or everything would come out blurry). An edge on the pixel grid is
// one pixel from one colour to the other, and a curve keeps its steps soft. It
// was a smoothstep over a pixel and a half —three pixels from a sky to a black
// pill, soft— and over one it was steeper than a pixel sees: the curves stepped.
fn coverage(d: f32) -> f32 {
    return clamp(0.5 - d * u.header.w, 0.0, 1.0);
}

// How much of the pixel a shape of this colour covers at its edge, as light
// would mix it. Blended in sRGB, a pixel half covered comes out at half the
// number between the two colours, which the eye sees as darker than half the
// light: a dark shape on a light picture —a black pill on a sky— gets a harder,
// steppier edge than it should, and a light one on a dark picture a thinner
// one. What is behind is the blending's business, not ours, so the curve goes
// by the shape's own brightness alone: a dark one gives a little of its edge
// pixel back, a light one takes a little more, a middle grey is left as it is.
// Only the edge pixel moves: a fill, an opacity, a gradient stay what they were.
// (Text has its own curve, in fs_shape.)
const EDGE_GAMMA: f32 = 1.8;
fn edge_coverage(d: f32, color: vec3<f32>) -> f32 {
    let cv = coverage(d);
    if (cv <= 0.0 || cv >= 1.0) { return cv; }
    let lum = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let g = 1.0 + (EDGE_GAMMA - 1.0) * abs(lum * 2.0 - 1.0);
    if (lum < 0.5) { return 1.0 - pow(1.0 - cv, 1.0 / g); }
    return pow(cv, 1.0 / g);
}

fn over(below: vec4<f32>, color: vec3<f32>, a: f32) -> vec4<f32> {
    return vec4<f32>(color * a, a) + below * (1.0 - a);
}

fn instruments(p: vec2<f32>) -> vec4<f32> {
    var c = vec4<f32>(0.0);
    let hp = p - vec2<f32>(60.0, u.header.y - 68.0);
    let panel = rounded_rect(hp - vec2<f32>(286.0, 30.0), vec2<f32>(334.0, 38.0), 12.0);
    c = over(c, vec3<f32>(0.05, 0.055, 0.055), 0.78 * coverage(panel));
    if (hp.x >= 0.0 && hp.x < 600.0 && hp.y >= 0.0 && hp.y <= 60.0) {
        let j = u32(hp.x / 5.0);
        let v = u.times[j / 4u][j % 4u];
        let ms = abs(v);
        let height = min(ms * 2.0, 60.0);
        let in_bar = step(60.0 - height, hp.y) * step(fract(hp.x / 5.0), 0.8);
        var tone = vec3<f32>(0.62, 0.84, 0.74);
        if (ms > u.hud.y * 1.6) { tone = vec3<f32>(0.95, 0.35, 0.3); }
        if (v < 0.0) { c = over(c, vec3<f32>(0.95, 0.35, 0.3), 0.16); }
        c = over(c, tone, in_bar * step(0.01, ms));
        let line_y = 60.0 - u.hud.y * 2.0;
        c = over(c, vec3<f32>(1.0), 0.25 * step(abs(hp.y - line_y), 0.5));
    }
    let pilot = length(p - vec2<f32>(38.0, u.header.y - 38.0)) - 5.0;
    let pilot_tone = mix(vec3<f32>(0.62, 0.84, 0.74), vec3<f32>(0.95, 0.3, 0.26), u.hud.z);
    return over(c, pilot_tone, coverage(pilot));
}

@fragment
fn fs(e: VertexOut) -> @location(0) vec4<f32> {
    // The position arrives in real pixels; the scene thinks in logical ones.
    let p = e.pos.xy / u.header.w + u.hud.xw;
    let el = elements[e.element];
    let kind = u32(el.header.x);
    if (kind == HUD) { return instruments(p); }

    // A big body is cut into tiles (`tile_bodies`, gpu.rs), each with the
    // shapes that can reach it: a pixel measures only those, and where none
    // does it is not painted. That is decided for the whole 2×2 quad the pixel
    // is in, so the edges' derivatives always have their four pixels.
    var tile = vec2<u32>(0u, 0u);
    var tiled = false;
    if (kind == 0u) {
        let table = shapes[u32(el.header.y)].a.w;
        if (table > 0.5) {
            let t = u32(table) - 1u;
            let q0 = (floor(e.pos.xy * 0.5) * 2.0 + 0.5) / u.header.w + u.hud.xw;
            let q1 = q0 + vec2<f32>(1.0 / u.header.w);
            let quad = tile_entry(t, q0).y + tile_entry(t, vec2<f32>(q1.x, q0.y)).y + tile_entry(t, vec2<f32>(q0.x, q1.y)).y + tile_entry(t, q1).y;
            if (quad == 0u) { discard; }
            tile = tile_entry(t, p);
            tiled = true;
        }
    }

    var clip = 1.0;
    for (var k = 0; k < 4; k++) {
        let r = el.clips[k];
        if (r >= 0.0) { clip *= coverage(shape_distance(u32(r), p)); }
    }
    let alpha = el.header.w * clip;
    if (alpha <= 0.001) { discard; }

    var c = vec4<f32>(0.0);
    if (kind == LAYER) {
        // A plain opacity group: its layer as is. With effects, `layer_with_effects`.
        if (el.border.y < 0.5) { return textureLoad(layers, vec2<i32>(e.pos.xy), i32(el.header.y), 0) * alpha; }
        return layer_with_effects(el, e.pos.xy, p, alpha);
    }
    if (kind == SHADER) {
        return user_shader(el, p) * alpha;
    }
    let local = to_local(p, el.t0, el.t1);
    if (kind == TEXTURE) {
        let q = local;
        // A letter with effects draws outside itself too: its outline and its shadow.
        if (el.header.z > 1.5 && el.light.w > 0.5) { return text_with_effects(el, q, alpha); }
        let uv01 = (q - el.dest.xy) / max(el.dest.zw, vec2<f32>(1.0));
        if (uv01.x < 0.0 || uv01.x > 1.0 || uv01.y < 0.0 || uv01.y > 1.0) { discard; }
        // Another program's window: its layer of the windows' texture, as it drew it.
        if (el.header.z < -0.5) {
            let w = textureSampleLevel(windows, atlas_sampler, mix(el.uv.xy, el.uv.zw, uv01), i32(el.header.y), 0.0);
            // −2: its alpha means nothing (XRGB); it covers.
            if (el.header.z < -1.5) { return vec4<f32>(w.rgb, 1.0) * alpha; }
            return w * alpha;
        }
        var at = mix(el.uv.xy, el.uv.zw, uv01);
        if (el.color1.x > 0.5) { at = sharp_texel(el, at); }
        // Above 0 in the second slot: a picture of its own, not a piece of the atlas.
        var t: vec4<f32>;
        if (el.header.y > 0.5) {
            t = textureSampleLevel(picture, atlas_sampler, at / vec2<f32>(textureDimensions(picture)), 0.0);
        } else {
            t = textureSampleLevel(atlas, atlas_sampler, at / vec2<f32>(textureDimensions(atlas)), 0.0);
        }
        // Tinted: the piece is a mask —a letter (2), a symbolic icon (1)— and the element provides the colour.
        // A letter (2) gets its edges firmed up. Blended as they are, in sRGB,
        // a stem that falls across two pixels —most of them: the letters are
        // placed at quarters of a pixel and the font is only hinted
        // vertically— comes out as two greys, and light text on a dark
        // background reads thin and washed out. This is the contrast curve
        // DirectWrite and Skia apply to their coverage: stronger the lighter
        // the letter, because dark text on light already gains body from the
        // sRGB blend. Measured on an «I» in #f6f6f5 over #1b1b1c: #bc + #67
        // became #d4 + #8a —the same width, but a letter and not a smudge.
        if (el.header.z > 1.5) {
            let lum = dot(el.color0.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
            let k = mix(0.3, 1.0, lum);
            let a = t.a * (k + 1.0) / (t.a * k + 1.0);
            return vec4<f32>(el.color0.rgb * a, a) * alpha;
        }
        if (el.header.z > 0.5) { return vec4<f32>(el.color0.rgb * t.a, t.a) * alpha; }
        return t * alpha;
    }

    let first = u32(el.header.y);
    let n = u32(el.header.z);
    var d = FAR;
    var d_shadow = FAR;
    let has_shadow = el.shadow.w > 0.0;
    let count = select(n, tile.y, tiled);
    for (var j = 0u; j < count; j++) {
        let k = first + select(j, u32(points[tile.x + j]), tiled);
        let blend = shapes[k].a.y;
        d = smooth_min(d, shape_distance(k, p), blend);
        if (has_shadow) { d_shadow = smooth_min(d_shadow, shape_distance(k, p - el.shadow.xy), blend); }
    }
    // Glass: from 0 to 1, in the `uv` slot, which a body does not use.
    let glass = el.uv.x;
    if (has_shadow) {
        // Its colour is the three slots of `color1`; if nothing is said, black.
        // Under glass the shadow does not show through: only around it.
        let covered = 1.0 - coverage(d) * glass;
        c = over(c, el.color1.rgb, el.shadow.w * alpha * covered * (1.0 - smoothstep(-8.0, el.shadow.z, d_shadow)));
    }
    var tone = el.color0.rgb;
    if (el.color1.w > 0.5) {
        var t = 0.0;
        if (el.color1.w > 1.5) {
            // Radial: from the centre outwards, up to the radius.
            t = clamp(length(local - el.line.xy) / max(el.line.z, 0.0001), 0.0, 1.0);
        } else {
            let axis = el.line.zw - el.line.xy;
            t = clamp(dot(local - el.line.xy, axis) / max(dot(axis, axis), 0.0001), 0.0, 1.0);
        }
        tone = between_stops(u32(el.dest.x), u32(el.dest.y), t);
    }
    let light = clamp(1.0 - (local.y - el.light.y) / max(el.light.z, 1.0), 0.0, 1.0) * el.light.x;
    tone += vec3<f32>(light) + vec3<f32>(el.color0.w) * smoothstep(-2.2, -0.4, d);
    // Where the edge points: towards where the distance grows.
    let g = vec2<f32>(dpdx(d), dpdy(d));
    let normal = g / max(length(g), 1e-6);
    let inside = max(-d, 0.0);
    // A box alone —the usual card— has two better measures than its distance.
    // What the lens bends follows a bevel without a crease, and the light
    // reads a rounder box: with a tight corner the normal turns 90° in three
    // pixels and the highlight pinches into a bright dot there, while a
    // rounder one turns it gently and the lit edge stays an even stroke.
    var lens_inside = inside;
    // How far a square corner's point is (its bend fades out there).
    var mitre_reach = 1e6;
    var lens_normal = normal;
    var light_normal = normal;
    let lone = shapes[first];
    if (n == 1u && u32(lone.a.x) == RECT && lone.a.z <= 0.0 && lone.b.z >= 0.5 && lone.b.w >= 0.5) {
        let lp = rotate_point(to_local(p, lone.t0, lone.t1), lone.b.xy, lone.c.y) - lone.b.xy;
        var depth = crease_free_depth(lp, lone.b.zw, lone.c.x) * lone.t1.z;
        // A square corner, cut as a glass block's: the contours above close to
        // a point on its diagonal, and the bend converging there drew a spike
        // and gathered what is behind into a dark curl. Its depth is a 4-norm
        // of the distances to its two edges instead —the edges straight, the
        // contours rounding off near the diagonal—, and its bend fades out
        // towards the corner's point.
        if (lone.c.x < 1.0) {
            let e = lone.b.zw - abs(lp);
            if (e.x > 0.0 && e.y > 0.0) {
                let q = e / min(e.x, e.y);
                depth = min(e.x, e.y) * pow(pow(q.x, -4.0) + pow(q.y, -4.0), -0.25) * lone.t1.z;
            }
            mitre_reach = max(max(e.x, e.y), 0.0) * lone.t1.z;
        }
        let gd = -vec2<f32>(dpdx(depth), dpdy(depth));
        if (depth > 0.0) {
            lens_inside = depth;
            lens_normal = gd / max(length(gd), 1e-6);
        }
        let round = rounded_rect(lp, lone.b.zw, max(lone.c.x, min(18.0, min(lone.b.z, lone.b.w)))) * lone.t1.z;
        let gl = vec2<f32>(dpdx(round), dpdy(round));
        light_normal = gl / max(length(gl), 1e-6);
    }
    // The light of the glass, the same with a lens or without it. It comes from
    // the top left: the edge that faces it shines thin and strong; the opposite
    // one, weaker, is the light coming out. The closer to the edge, the
    // brighter —the glass seen edge-on—. And when pressed it lights up from the
    // finger.
    // With `shine:` the light is a point in the scene, and it comes from it:
    // one direction per shape, from its centre —like the sun— or a light near
    // the edge would fan out into a cone.
    let to_light = select(normalize(vec2<f32>(-0.55, -0.83)), el.glass2.xy, el.glass2.z > 0.5);
    let rim = 1.0 - smoothstep(0.4, 3.0, inside);
    let facing = pow(max(dot(light_normal, to_light), 0.0), 1.6);
    let away = pow(max(-dot(light_normal, to_light), 0.0), 1.6);
    let fresnel = exp(-inside / 10.0);
    let shine = rim * (0.85 * facing + 0.4 * away + 0.12) + 0.14 * fresnel;
    let finger_dist = distance(p, u.backdrop.yz);
    var finger_light = u.backdrop.w * exp(-finger_dist * finger_dist / (2.0 * 70.0 * 70.0)) * 0.3;
    // A press sends a ring of light through the glass that spreads and fades,
    // and flexes the glass outwards as it goes by.
    var ripple_push = vec2<f32>(0.0);
    if (u.ripple.w > 0.5 && el.glass3.y > 0.0) {
        let progress = clamp(u.ripple.z / 0.9, 0.0, 1.0);
        let dist = distance(p, u.ripple.xy);
        let reach = 150.0 * (1.0 - pow(1.0 - progress, 3.0));
        let fade = pow(1.0 - progress, 2.0);
        let ring = exp(-pow((dist - reach) / 16.0, 2.0));
        let fill = exp(-pow(dist / (reach * 0.6 + 1.0), 2.0)) * 0.5;
        finger_light += (ring + fill) * fade * 0.3 * el.glass3.y;
        ripple_push = (p - u.ripple.xy) / max(dist, 0.5) * ring * fade * 9.0 * el.glass3.y;
    }
    if (glass > 0.0 && u.backdrop.x > 0.5 && el.uv.z > 0.5) {
        // With what is behind at hand, the glass is that background bent by
        // the bevel, frosted, with a little of its tint. Red, green and blue
        // bend a hair differently: the colour fringe of a glass edge.
        let bevel = select(16.0, el.uv.y, el.uv.y > 0.5);
        // The bevel: how high the glass is here and how steep. `refraction:`
        // is how thick it is; a glass that appears does not fade in, it
        // starts bending the light.
        let t_rim = clamp(lens_inside / bevel, 0.0, 1.0);
        let thick = LENS_THICKNESS * bevel * glass * el.uv.w;
        let z = thick * rim_height(t_rim);
        let slope = select(0.0, min(thick * rim_slope(t_rim) / bevel, 40.0), t_rim < 1.0) * smoothstep(0.0, 1.5 * bevel, mitre_reach);
        let cap = 0.5 * bevel;
        let size = u.header.xy * u.header.w;
        // `dome:` a magnifying glass in the middle: towards the centre, it
        // shows what is behind a little bigger. It fades out in the bevel,
        // which already bends its own way.
        let flat = t_rim;
        let dome = (el.glass3.zw - p) * el.glass2.w * 0.14 * flat * u.header.w * glass;
        let base = e.pos.xy + dome + ripple_push * u.header.w;
        // Red, green and blue each bend as their own index says, so where
        // the bevel bends hard a bright edge splits into a faint rainbow
        // (red 611 nm, green 549, blue 464).
        let q = base + refract_offset(lens_normal, slope, ior_for(0.3014, el.glass3.x), z, cap) * u.header.w;
        var qr = q;
        var qb = q;
        if (el.glass3.x > 0.0 && slope > 0.01) {
            qr = base + refract_offset(lens_normal, slope, ior_for(0.3733, el.glass3.x), z, cap) * u.header.w;
            qb = base + refract_offset(lens_normal, slope, ior_for(0.2153, el.glass3.x), z, cap) * u.header.w;
        }
        // What is behind comes in premultiplied alpha —what is unknown, under
        // a text, has alpha 0 and weighs nothing—: it is divided by it.
        let br = textureSampleLevel(backdrop_blurred, backdrop_sampler, qr / size, 0.0);
        let bg = textureSampleLevel(backdrop_blurred, backdrop_sampler, q / size, 0.0);
        let bb = textureSampleLevel(backdrop_blurred, backdrop_sampler, qb / size, 0.0);
        let frosted = vec3<f32>(br.r / max(br.a, 0.001), bg.g / max(bg.a, 0.001), bb.b / max(bb.a, 0.001));
        let nr = textureSampleLevel(backdrop_sharp, backdrop_sampler, qr / size, 0.0);
        let ng = textureSampleLevel(backdrop_sharp, backdrop_sampler, q / size, 0.0);
        let nb = textureSampleLevel(backdrop_sharp, backdrop_sampler, qb / size, 0.0);
        let sharp = vec3<f32>(nr.r / max(nr.a, 0.001), ng.g / max(ng.a, 0.001), nb.b / max(nb.a, 0.001));
        let known = min(nr.a, min(ng.a, nb.a));
        // In the bevel what is bent looks fairly sharp; further in, frosted.
        // `frost:` how frosted it is inside the bevel: 1 as it always was, 0
        // clear —what is behind seen as it is, bent only at the edge—, and
        // `auto` (−1) frosted only where what is behind is busy, so words
        // behind never fight the ones on it, and clear over a wallpaper. In
        // the bevel what is bent looks fairly sharp whatever it says.
        var frost = clamp(el.border.w, 0.0, 1.0);
        if (el.border.w < -0.5) {
            // Busy for `auto` is what blurring takes away: how far the sharp
            // backdrop is from the frosted one, here and at four points round
            // it (a frosted text is a flat grey, so how much the frosted one
            // varies says nothing of it).
            // Wide apart: a wallpaper's one sharp edge weighs in a single spot,
            // where text has edges in all of them.
            let reach = 26.0 * u.header.w;
            let spots = array<vec2<f32>, 5>(vec2<f32>(0.0), vec2<f32>(reach, reach * 0.5), vec2<f32>(-reach, reach * 0.5), vec2<f32>(reach * 0.5, -reach), vec2<f32>(-reach * 0.5, -reach));
            var busy = 0.0;
            for (var k = 0; k < 5; k++) {
                let at = (q + spots[k]) / size;
                let ms = textureSampleLevel(backdrop_sharp, backdrop_sampler, at, 0.0);
                let mf = textureSampleLevel(backdrop_blurred, backdrop_sampler, at, 0.0);
                busy += abs(dot(ms.rgb / max(ms.a, 0.001) - mf.rgb / max(mf.a, 0.001), vec3<f32>(0.299, 0.587, 0.114)));
            }
            frost = smoothstep(0.02, 0.045, busy * 0.2);
        }
        let in_bevel = 1.0 - t_rim;
        // The bevel shows what is behind sharp only as much as it bends it
        // (`refraction:`): a flat glass is frosted to its edge, not ringed
        // with a band of what is behind as it is.
        let bent = clamp(el.uv.w, 0.0, 1.0);
        var background = mix(frosted, sharp, max(smoothstep(0.0, 0.6, in_bevel) * 0.85 * bent, 1.0 - frost) * known);
        // `smoke:` and `milk:` (after True Glass for Omarchy's dark and light
        // glass): smoked, what is seen through is taken down, the brighter the
        // more —a white backdrop to about 40 %, a dark one barely—, keeping
        // its colour, so light words on it read over anything; milky, it is
        // lifted towards white, the darker the more, so dark words read.
        // A frosted glass takes a little more milk.
        let raw_gray = dot(background, vec3<f32>(0.299, 0.587, 0.114));
        background = glass_tone(background, el.dest.z, el.dest.w, frost);
        let gray = dot(background, vec3<f32>(0.299, 0.587, 0.114));
        // A glass livens up a little what it lets through (`vivid:`, 118 %).
        // Each of what follows is skipped where its property makes it a no-op:
        // a glass with `tint: 0; vivid: 100%; mirror: 0; highlight: 0` shows
        // the scene's blur as it left it.
        var vivid = background;
        if (abs(el.glass4.y - 1.0) > 0.001) {
            vivid = clamp(mix(vec3<f32>(gray), background, el.glass4.y), vec3<f32>(0.0), vec3<f32>(1.0));
        }
        // The glass, with everything: tint, the darkening over text, and its
        // light. It all goes in the colour and nothing in the alpha, which stays
        // at LENS_ALPHA: the next capture is unmixed by dividing by 1 − α, and
        // with an alpha of 0.96 —a small ball is almost all edge— the rounding
        // got multiplied by 25, the background never came out and the ball
        // stayed black inside.
        var glass_color = vivid;
        if (el.glass4.x > 0.0) {
            // Over something bright, or over something with a lot of detail,
            // more tint: what is written on top can still be read. Apple puts it
            // this way: the shadow of the glass rises over text. A clear glass
            // takes less of it: that it is clear is what it was asked for.
            let bright = smoothstep(0.45, 0.9, gray);
            // How busy what is behind is —text, lines, a window—: how much the
            // frosted background varies a few steps from here, which on a flat
            // background is nothing.
            let step_px = 14.0 * u.header.w;
            let around = array<vec2<f32>, 4>(vec2<f32>(step_px, 0.0), vec2<f32>(-step_px, 0.0), vec2<f32>(0.0, step_px), vec2<f32>(0.0, -step_px));
            var variation = 0.0;
            for (var k = 0; k < 4; k++) {
                let m = textureSampleLevel(backdrop_blurred, backdrop_sampler, (q + around[k]) / size, 0.0);
                let l = dot(m.rgb / max(m.a, 0.001), vec3<f32>(0.299, 0.587, 0.114));
                variation += abs(l - raw_gray);
            }
            let detail = smoothstep(0.03, 0.14, variation * 0.25);
            // `tint:` the base share of its own colour (22 %); the extra over
            // bright or busy backdrops scales with it.
            glass_color = mix(vivid, tone, el.glass4.x);
            let extra = max(bright * 0.5, detail * 0.55) * (0.35 + 0.65 * frost) * (el.glass4.x / LENS_TINT);
            glass_color = mix(glass_color, tone, clamp(extra, 0.0, 1.0));
        }
        // The bevel, tilted away from the eye, mirrors what is round the glass
        // (Fresnel, as Schlick puts it): a little on the slope, most where it
        // turns over at the edge, and nothing on the flat top. It starts a
        // couple of pixels in: the edge's partly covered pixels would mirror
        // almost everything. `mirror:` how much (100 %).
        if (slope > 0.01 && el.glass4.z > 0.0) {
            let cv = 1.0 / sqrt(1.0 + slope * slope);
            let m1 = 1.0 - cv;
            let m2 = m1 * m1;
            let fr = LENS_F0 + (1.0 - LENS_F0) * m2 * m2 * m1;
            let edge_in = smoothstep(0.0, 2.0, lens_inside * u.header.w);
            let fw = clamp(fr * 0.9 * el.glass4.z, 0.0, 0.85) * edge_in;
            if (fw > 0.002) {
                // Four taps close together, outside the glass: a polished
                // surface mirrors sharply.
                let eo = e.pos.xy + lens_normal * 20.0 * u.header.w;
                let es = 6.0 * u.header.w;
                var env = vec3<f32>(0.0);
                for (var k = 0; k < 4; k++) {
                    let o = vec2<f32>(select(-es, es, (k & 1) == 1), select(-es, es, k >= 2));
                    let m = textureSampleLevel(backdrop_sharp, backdrop_sampler, (eo + o) / size, 0.0);
                    env += m.rgb / max(m.a, 0.001) * step(0.5, m.a);
                }
                glass_color = mix(glass_color, glass_tone(env * 0.25, el.dest.z, el.dest.w, frost), fw);
            }
        }
        // The light it catches on its rim (`highlight:`, 100 %).
        if (el.glass4.w > 0.0) {
            glass_color = mix(glass_color, vec3<f32>(1.0), clamp((shine + finger_light) * el.glass4.w, 0.0, 1.0));
        }
        // Partly glass, partly its fill: as without a lens, where less glass
        // is more tint. It covers as much as its opacity says whatever the
        // glass: weighed by it too, `glass: 0.5` let two thirds of the sharp
        // background through and read as clear, and `glass: 0` —a plain
        // fill— was a jump from almost nothing (#40).
        glass_color = mix(tone, glass_color, glass);
        // What has to be seen is this glass. But the compositor puts the real
        // background, sharp, under our alpha, and it would leak through like a
        // ghost of the text behind: it is subtracted beforehand, since we know
        // it. What is seen: ours + (1 − α)·behind = glass · coverage.
        let seen = coverage(d) * alpha;
        let db = textureSampleLevel(backdrop_sharp, backdrop_sampler, e.pos.xy / size, 0.0);
        let underneath = db.rgb / max(db.a, 0.001) * step(0.5, db.a);
        let lens_alpha = seen * LENS_ALPHA;
        let rgb = clamp(seen * (glass_color - (1.0 - LENS_ALPHA) * underneath), vec3<f32>(0.0), vec3<f32>(lens_alpha));
        c = vec4<f32>(rgb, lens_alpha) + c * (1.0 - lens_alpha);
    } else {
        // Without it, the fill is a tint: it lets what is behind show through
        // (which the compositor blurs) and keeps its colour; and on top, its light.
        c = over(c, tone, edge_coverage(d, tone) * alpha * mix(1.0, 0.36, glass));
        // Smoke and milk over what the compositor blurs, which we never see:
        // a veil of black or of white, as near as it gets.
        c = over(c, vec3<f32>(0.0), coverage(d) * alpha * glass * el.dest.z * 0.35);
        c = over(c, vec3<f32>(1.0), coverage(d) * alpha * glass * el.dest.w * 0.3);
        if (glass > 0.0 && el.glass4.w > 0.0) {
            c = over(c, vec3<f32>(1.0), clamp((shine + finger_light) * el.glass4.w, 0.0, 1.0) * glass * coverage(d) * alpha);
        }
    }
    if (el.light.w > 0.0) {
        c = over(c, el.border.rgb, edge_coverage(abs(d + el.light.w * 0.5) - el.light.w * 0.5, el.border.rgb) * alpha);
    }
    return c;
}

// ── a group's effects ─────────────────────────────────────────────
// Its layer, blurred over a disc of `radius` real pixels: 32 samples on a
// golden-angle spiral, weighted like a gaussian. Bilinear reads between them
// fill what 32 points leave; what is outside what was painted is transparent.
fn layer_blurred(layer: i32, at: vec2<f32>, radius: f32) -> vec4<f32> {
    let dims = vec2<f32>(textureDimensions(layers));
    var sum = vec4<f32>(0.0);
    var weights = 0.0;
    for (var k = 0; k < 32; k++) {
        let f = (f32(k) + 0.5) / 32.0;
        let r = radius * sqrt(f);
        let angle = f32(k) * 2.39996323;
        let w = exp(-2.0 * f);
        sum += textureSampleLevel(layers, atlas_sampler, (at + vec2<f32>(cos(angle), sin(angle)) * r) / dims, layer, 0.0) * w;
        weights += w;
    }
    return sum / weights;
}

// `hue: 120deg` arrives in radians, like every angle written with `deg`.
fn hue_rotate(c: vec3<f32>, a: f32) -> vec3<f32> {
    let k = vec3<f32>(0.57735027);
    return c * cos(a) + cross(k, c) * sin(a) + k * dot(k, c) * (1.0 - cos(a));
}

// color0: glow colour, and how much · color1: saturation, brightness, contrast, hue
// line: the mask's points · light: blur, glow radius, mask kind (1 linear, 2 radial), add
// border: whether the glow has its own colour, whether there are effects
fn layer_with_effects(el: Element, at: vec2<f32>, p: vec2<f32>, alpha: f32) -> vec4<f32> {
    let layer = i32(el.header.y);
    let scale = u.header.w;
    var c = textureLoad(layers, vec2<i32>(at), layer, 0);
    // A shader of the scene's: what it holds, as that shader sees it.
    if (el.uv.x > 0.5) { c = user_layer_shader(el, p, layer); }
    if (el.light.x > 0.25) { c = layer_blurred(layer, at, el.light.x * scale); }
    // The glow goes under what is inside: light that spills from its edges.
    if (el.light.y > 0.25 && el.color0.w > 0.0) {
        let g = layer_blurred(layer, at, el.light.y * scale);
        var glow = g * el.color0.w;
        if (el.border.x > 0.5) { glow = vec4<f32>(el.color0.rgb, 1.0) * g.a * el.color0.w; }
        c = c + glow * (1.0 - c.a);
    }
    // Colour: on the straight colour, not the premultiplied one.
    let tone = el.color1;
    if (any(tone != vec4<f32>(1.0, 1.0, 1.0, 0.0)) && c.a > 0.0) {
        var rgb = c.rgb / c.a;
        rgb = rgb * tone.y;
        rgb = (rgb - vec3<f32>(0.5)) * tone.z + vec3<f32>(0.5);
        let gray = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        rgb = mix(vec3<f32>(gray), rgb, tone.x);
        if (tone.w != 0.0) { rgb = hue_rotate(rgb, tone.w); }
        c = vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * c.a, c.a);
    }
    // The mask, where the group is (it moves with it): whole at the start, nothing at the end.
    var m = 1.0;
    let local = to_local(p, el.t0, el.t1);
    if (el.light.z > 0.5 && el.light.z < 1.5) {
        let axis = el.line.zw - el.line.xy;
        m = 1.0 - smoothstep(0.0, 1.0, dot(local - el.line.xy, axis) / max(dot(axis, axis), 0.0001));
    } else if (el.light.z > 1.5) {
        m = 1.0 - smoothstep(el.line.z, max(el.line.w, el.line.z + 0.001), distance(local, el.line.xy));
    }
    c = c * m * alpha;
    // Added: light that adds up instead of covering. Premultiplied with no alpha
    // is exactly that, here and in the compositor.
    if (el.light.w > 0.5 && el.light.w < 1.5) { return vec4<f32>(c.rgb, 0.0); }
    // `screen` and `multiply` are blended by their own pipelines: see `BLENDS` in gpu.rs.
    return c;
}

// ── particles ─────────────────────────────────────────────────────
// No particle is stored: each one is worked out here from its number and the
// time. Its emitter's element brings everything (see `Instr::Particles` in gpu.rs):
// color0 colour and opacity at birth · color1 at death · line where, and the box
// it is born in · light life a..b, speed a..b · border direction, spread, drag,
// shape · shadow gravity, size at birth and at death · dest now, switched on,
// switched off, last burst · uv the emitter's velocity, whether it bursts.
const PARTICLE_SLOTS: u32 = 4096u;

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

// A number from 0 to 1, always the same for the same three.
fn chance(a: u32, b: u32, k: u32) -> f32 {
    return f32(pcg(a ^ pcg(b ^ pcg(k)))) / 4294967295.0;
}

struct ParticleOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) element: u32,
    @location(1) quad: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) world: vec2<f32>,
    // Its half size along and across, in pixels, and its shape.
    @location(4) @interpolate(flat) extent: vec3<f32>,
};

@vertex
fn vs_particle(@builtin(vertex_index) v: u32, @builtin(instance_index) instance: u32) -> ParticleOut {
    var out: ParticleOut;
    let k = instance / PARTICLE_SLOTS;
    let i = instance % PARTICLE_SLOTS;
    let el = elements[k];
    out.element = k;
    out.pos = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    let seed = u32(el.header.y);
    let count = el.header.z;
    let now = el.dest.x;
    // When it was born, and in which round —each round, another particle—.
    var born = 0.0;
    var round = 0u;
    let lifetime_most = max(el.light.x, el.light.y);
    if (el.uv.z > 0.5) {
        born = el.dest.w + chance(seed, i, 7u) * 0.06;
        round = u32(max(el.dest.w, 0.0) * 1000.0);
        if (el.dest.w < 0.0) { return out; }
    } else {
        // A steady stream: each slot is born once per `lifetime_most`, spread evenly.
        let phase = f32(i) / count * lifetime_most;
        let since = now - el.dest.y - phase;
        if (since < 0.0) { return out; }
        var n = floor(since / lifetime_most);
        born = el.dest.y + phase + n * lifetime_most;
        // Switched off: the ones born after it are not there; the last ones finish.
        if (born > el.dest.z) {
            n -= 1.0;
            born -= lifetime_most;
            if (n < 0.0) { return out; }
        }
        round = u32(n);
    }
    let life = mix(el.light.x, el.light.y, chance(seed, i, round * 11u + 1u));
    let age = now - born;
    if (age < 0.0 || age > life) { return out; }
    let t = age / life;
    // Where it was born: somewhere in the box, where the emitter was then.
    let box = (vec2<f32>(chance(seed, i, round * 11u + 2u), chance(seed, i, round * 11u + 3u)) - 0.5) * el.line.zw;
    let start = el.line.xy + box - el.uv.xy * age;
    let angle = el.border.x + (chance(seed, i, round * 11u + 4u) - 0.5) * el.border.y;
    let speed = mix(el.light.z, el.light.w, chance(seed, i, round * 11u + 5u));
    let v0 = vec2<f32>(cos(angle), sin(angle)) * speed;
    let g = el.shadow.xy;
    let drag = el.border.z;
    var at = vec2<f32>(0.0);
    var velocity = vec2<f32>(0.0);
    if (drag > 0.001) {
        let slow = exp(-drag * age);
        let f = (1.0 - slow) / drag;
        at = start + v0 * f + g * (age - f) / drag;
        velocity = v0 * slow + g * (1.0 - slow) / drag;
    } else {
        at = start + v0 * age + 0.5 * g * age * age;
        velocity = v0 + g * age;
    }
    let size = mix(el.shadow.z, el.shadow.w, t);
    let alpha = mix(el.color0.w, el.color1.w, t) * el.header.w;
    if (size <= 0.01 || alpha <= 0.001) { return out; }
    // Its quad: a square around it, or a streak along where it goes.
    var along = vec2<f32>(1.0, 0.0);
    var extent = vec2<f32>(size * 0.5, size * 0.5);
    let shape = el.border.w;
    if (shape > 1.5) {
        let fast = length(velocity);
        if (fast > 0.001) { along = velocity / fast; }
        extent = vec2<f32>(size * 0.5 + fast * 0.03, max(size * 0.22, 0.6));
    }
    let across = vec2<f32>(-along.y, along.x);
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    let q = corners[v];
    // One pixel more on each side, for the edge's softness.
    let pad = 1.0 / u.header.w;
    let local = at + along * q.x * (extent.x + pad) + across * q.y * (extent.y + pad);
    // Back from the group's space to the scene's: `to_local` inverted.
    let det = el.t0.x * el.t0.w - el.t0.y * el.t0.z;
    let d = local - el.t1.xy;
    let world = vec2<f32>(el.t0.w * d.x - el.t0.y * d.y, -el.t0.z * d.x + el.t0.x * d.y) / det;
    let screen = world - u.hud.xw;
    out.pos = vec4<f32>(screen.x / u.header.x * 2.0 - 1.0, 1.0 - screen.y / u.header.y * 2.0, 0.0, 1.0);
    out.quad = q * (extent + vec2<f32>(pad)) ;
    out.color = vec4<f32>(mix(el.color0.rgb, el.color1.rgb, t), alpha);
    out.world = world;
    out.extent = vec3<f32>(extent, shape);
    return out;
}

@fragment
fn fs_particle(e: ParticleOut) -> @location(0) vec4<f32> {
    let el = elements[e.element];
    var clip = 1.0;
    for (var k = 0; k < 4; k++) {
        let r = el.clips[k];
        if (r >= 0.0) { clip *= coverage(shape_distance(u32(r), e.world)); }
    }
    // Its distance to its own edge, in logical pixels, like any shape's.
    var d = 0.0;
    let x = e.extent.xy;
    if (e.extent.z > 1.5) {
        // A streak: a capsule along x.
        let h = max(x.x - x.y, 0.0);
        let q = vec2<f32>(max(abs(e.quad.x) - h, 0.0), e.quad.y);
        d = length(q) - x.y;
    } else if (e.extent.z > 0.5) {
        let q = abs(e.quad) - x;
        d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
    } else {
        d = length(e.quad) - x.x;
    }
    let a = e.color.a * coverage(d) * clip;
    if (a <= 0.001) { discard; }
    return vec4<f32>(e.color.rgb * a, a);
}

// ── a text's effects ──────────────────────────────────────────────
// How much of the letter covers this point; nothing outside its piece of the atlas.
fn glyph_at(el: Element, q: vec2<f32>) -> f32 {
    let uv01 = (q - el.dest.xy) / max(el.dest.zw, vec2<f32>(1.0));
    if (uv01.x < 0.0 || uv01.x > 1.0 || uv01.y < 0.0 || uv01.y > 1.0) { return 0.0; }
    return textureSampleLevel(atlas, atlas_sampler, mix(el.uv.xy, el.uv.zw, uv01) / vec2<f32>(textureDimensions(atlas)), 0.0).a;
}

// color0 the letter's colour · color1 the shadow's colour, and the gradient's kind ·
// line the gradient's points · light first stop, how many, -, «has effects» ·
// border the outline's colour and width · shadow its offset, blur and alpha
fn text_with_effects(el: Element, q: vec2<f32>, alpha: f32) -> vec4<f32> {
    // The letter, firmed up like any other (see the contrast curve above).
    let lum = dot(el.color0.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let k = mix(0.3, 1.0, lum);
    let raw = glyph_at(el, q);
    let fill = raw * (k + 1.0) / (raw * k + 1.0);
    var c = vec4<f32>(0.0);
    // The shadow: the letter moved and blurred, under everything.
    if (el.shadow.w > 0.0) {
        // Blurred over a disc of `blur`: 24 points on a golden-angle spiral,
        // weighted like a gaussian —with a 3 × 3 grid, a wide blur came out as
        // stacked copies of the letter—.
        let at = q - el.shadow.xy;
        let radius = el.shadow.z;
        var s = 0.0;
        var weights = 0.0;
        for (var i = 0; i < 24; i++) {
            let f = (f32(i) + 0.5) / 24.0;
            let r = radius * sqrt(f);
            let a = f32(i) * 2.39996323;
            let w = exp(-2.0 * f);
            s += glyph_at(el, at + vec2<f32>(cos(a), sin(a)) * r) * w;
            weights += w;
        }
        c = vec4<f32>(el.color1.rgb, 1.0) * (s / weights) * el.shadow.w;
    }
    // The outline: the letter grown by its width, in its colour, under the fill.
    if (el.border.w > 0.0) {
        let w = el.border.w;
        var o = raw;
        for (var i = 0; i < 12; i++) {
            let a = f32(i) * 0.5235988;
            let dir = vec2<f32>(cos(a), sin(a));
            o = max(o, glyph_at(el, q + dir * w));
            o = max(o, glyph_at(el, q + dir * w * 0.5));
        }
        c = over(c, el.border.rgb, clamp(o * 1.4, 0.0, 1.0));
    }
    // The fill: its colour, or its gradient at this point.
    var tone = el.color0.rgb;
    if (el.color1.w > 0.5) {
        var t = 0.0;
        if (el.color1.w > 1.5) {
            t = clamp(length(q - el.line.xy) / max(el.line.z, 0.0001), 0.0, 1.0);
        } else {
            let axis = el.line.zw - el.line.xy;
            t = clamp(dot(q - el.line.xy, axis) / max(dot(axis, axis), 0.0001), 0.0, 1.0);
        }
        tone = between_stops(u32(el.light.x), u32(el.light.y), t);
    }
    c = over(c, tone, fill);
    if (c.a <= 0.001) { discard; }
    return c * alpha;
}
