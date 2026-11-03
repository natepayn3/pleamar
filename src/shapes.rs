//! Shapes: what a scene declares (with expressions) and what is left once
//! they are evaluated (numbers). The same geometry serves to paint on the GPU,
//! to know what is under the mouse and to compute the box of each element.

use crate::scene::{Ctx, Expr, Point};

/// An affine transform: p' = M·p + t. Those of a group and those of its parents
/// multiply, so rotating something inside something that scales does what one
/// expects.
#[derive(Clone, Copy, Debug)]
pub struct Affine {
    pub m: [f32; 4],
    pub t: [f32; 2],
}

impl Affine {
    pub const IDENTITY: Affine = Affine { m: [1.0, 0.0, 0.0, 1.0], t: [0.0, 0.0] };

    /// Around a pivot: first scale, then rotate, then move.
    /// Radians, and positive is clockwise (y grows downwards).
    pub fn new(pivot: (f32, f32), rotation: f32, scale: (f32, f32), translate: (f32, f32)) -> Affine {
        let (s, c) = rotation.sin_cos();
        let m = [c * scale.0, -s * scale.1, s * scale.0, c * scale.1];
        let t = [
            pivot.0 + translate.0 - (m[0] * pivot.0 + m[1] * pivot.1),
            pivot.1 + translate.1 - (m[2] * pivot.0 + m[3] * pivot.1),
        ];
        Affine { m, t }
    }

    /// `self ∘ o`: `o` is applied and then `self`.
    pub fn mul(self, o: Affine) -> Affine {
        let (a, b) = (self.m, o.m);
        Affine {
            m: [a[0] * b[0] + a[1] * b[2], a[0] * b[1] + a[1] * b[3], a[2] * b[0] + a[3] * b[2], a[2] * b[1] + a[3] * b[3]],
            t: [a[0] * o.t[0] + a[1] * o.t[1] + self.t[0], a[2] * o.t[0] + a[3] * o.t[1] + self.t[1]],
        }
    }

    pub fn inverse(self) -> Affine {
        let m = self.m;
        let det = m[0] * m[3] - m[1] * m[2];
        let det = if det.abs() < 1e-6 { 1e-6 } else { det };
        let i = [m[3] / det, -m[1] / det, -m[2] / det, m[0] / det];
        Affine { m: i, t: [-(i[0] * self.t[0] + i[1] * self.t[1]), -(i[2] * self.t[0] + i[3] * self.t[1])] }
    }

    pub fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (self.m[0] * x + self.m[1] * y + self.t[0], self.m[2] * x + self.m[3] * y + self.t[1])
    }

    /// How much it stretches distances. Exact if the scale is the same on both
    /// axes; if not, an average that is enough for smoothing the edge.
    pub fn factor(self) -> f32 {
        (self.m[0] * self.m[3] - self.m[1] * self.m[2]).abs().sqrt()
    }

    pub fn is_identity(self) -> bool {
        self.m == Affine::IDENTITY.m && self.t == Affine::IDENTITY.t
    }

    /// The box that contains another one once transformed.
    pub fn bounds(self, c: [f32; 4]) -> [f32; 4] {
        if self.is_identity() {
            return c;
        }
        let e = [self.apply(c[0], c[1]), self.apply(c[2], c[1]), self.apply(c[0], c[3]), self.apply(c[2], c[3])];
        let (mut r0, mut r1) = (e[0], e[0]);
        for p in e {
            r0 = (r0.0.min(p.0), r0.1.min(p.1));
            r1 = (r1.0.max(p.0), r1.1.max(p.1));
        }
        [r0.0, r0.1, r1.0, r1.1]
    }

    pub fn encode(self, s: &mut [f32]) {
        let i = self.inverse();
        s[..8].copy_from_slice(&[i.m[0], i.m[1], i.m[2], i.m[3], i.t[0], i.t[1], self.factor(), 0.0]);
    }
}

#[derive(Clone, Debug)]
pub enum Shape {
    Ellipse { center: Point, radius: Expr, scale: Point },
    /// Rounded box. With half width or height below half a pixel, it does not
    /// exist: it is neither painted nor blended with anything.
    Rect { center: Point, half_size: Point, radius: Expr },
    /// An arc like "∩". `opening` is half the angle it spans, in radians:
    /// π/2 is half a circle.
    Arc { center: Point, radius: Expr, opening: Expr, thickness: Expr },
    /// A line with round ends.
    Segment { from: Point, to: Point, thickness: Expr },
    /// Rotated about its centre. Radians, and positive is clockwise.
    Rotated(Box<Shape>, Expr),
    /// Only the outline: a circle becomes a ring.
    Stroke(Box<Shape>, Expr),
    /// A broken or curved line. Closed it is a polygon, and gets filled; open,
    /// a line with thickness. The points are flattened here and the shader
    /// measures against them, so it comes out with the same signed distance as
    /// the others: it blends, it has shadow, edge and light.
    Path { origin: Point, steps: Vec<PathStep>, closed: bool },
}

/// Where a path goes through. The first one is its `move`; the rest, this.
#[derive(Clone, Debug)]
pub enum PathStep {
    Line(Point),
    /// A quadratic Bézier: the point it pulls towards and where it arrives.
    Curve { via: Point, to: Point },
}

/// How many points an already flattened path can have. Every pixel of its box
/// goes through all of them: it is the price of it being exact.
/// How many points a path has once its curves are split. What costs is per
/// covered pixel: each one goes through the points of the path that covers
/// it, and an accessory takes forty pixels a side. 64 was from when paths
/// were written by hand; a real SVG —a hat drawn in Inkscape— goes past that
/// without being complicated.
pub const MAX_POINTS: usize = 192;

impl Shape {
    pub fn circle(center: Point, radius: impl Into<Expr>) -> Shape {
        Shape::Ellipse { center, radius: radius.into(), scale: (1.0.into(), 1.0.into()) }
    }
    pub fn ring(center: Point, radius: impl Into<Expr>, thickness: impl Into<Expr>) -> Shape {
        Shape::circle(center, radius).stroke(thickness)
    }
    pub fn rotated(self, angle: impl Into<Expr>) -> Shape {
        Shape::Rotated(Box::new(self), angle.into())
    }
    pub fn stroke(self, thickness: impl Into<Expr>) -> Shape {
        Shape::Stroke(Box::new(self), thickness.into())
    }

    /// The same as `flatten`, but leaving the points of the paths in `pts`
    /// (x, y pairs, relative to the centre of their box). A path with nowhere
    /// to leave them stays as its box, which is what is enough for the mouse.
    pub fn flatten_into(&self, c: Ctx, pts: &mut Vec<f32>) -> FlatShape {
        match self {
            Shape::Path { origin, steps, closed } => {
                let mut ps: Vec<(f32, f32)> = Vec::with_capacity(steps.len() + 1);
                let mut d = (origin.0.eval(c), origin.1.eval(c));
                ps.push(d);
                for step in steps {
                    match step {
                        PathStep::Line(a) => {
                            d = (a.0.eval(c), a.1.eval(c));
                            ps.push(d);
                        }
                        PathStep::Curve { via, to } => {
                            let v = (via.0.eval(c), via.1.eval(c));
                            let f = (to.0.eval(c), to.1.eval(c));
                            // As many segments as the detour is long: a short curve does not spend 16.
                            let length = (v.0 - d.0).hypot(v.1 - d.1) + (f.0 - v.0).hypot(f.1 - v.1);
                            let n = ((length / 4.0) as usize).clamp(3, 24);
                            for k in 1..=n {
                                let t = k as f32 / n as f32;
                                let u = 1.0 - t;
                                ps.push((
                                    u * u * d.0 + 2.0 * u * t * v.0 + t * t * f.0,
                                    u * u * d.1 + 2.0 * u * t * v.1 + t * t * f.1,
                                ));
                            }
                            d = f;
                        }
                    }
                }
                ps.truncate(MAX_POINTS);
                let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for (x, y) in &ps {
                    (x0, y0, x1, y1) = (x0.min(*x), y0.min(*y), x1.max(*x), y1.max(*y));
                }
                let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                let first = pts.len() / 2;
                for (x, y) in &ps {
                    pts.push(x - cx);
                    pts.push(y - cy);
                }
                FlatShape {
                    kind: 4,
                    cx,
                    cy,
                    mx: (x1 - x0) * 0.5,
                    my: (y1 - y0) * 0.5,
                    radius: first as f32,
                    rotation: 0.0,
                    ex: ps.len() as f32,
                    ey: *closed as u8 as f32,
                    stroke: 0.0,
                    affine: Affine::IDENTITY,
                }
            }
            Shape::Rotated(f, angle) => {
                let mut p = f.flatten_into(c, pts);
                p.rotation += angle.eval(c);
                p
            }
            Shape::Stroke(f, thickness) => {
                let mut p = f.flatten_into(c, pts);
                p.stroke = thickness.eval(c).max(0.0);
                p
            }
            other => other.flatten(c),
        }
    }

    pub fn flatten(&self, c: Ctx) -> FlatShape {
        let mut p = FlatShape { kind: 0, cx: 0.0, cy: 0.0, mx: 0.0, my: 0.0, radius: 0.0, rotation: 0.0, ex: 1.0, ey: 1.0, stroke: 0.0, affine: Affine::IDENTITY };
        match self {
            Shape::Ellipse { center, radius, scale } => {
                (p.cx, p.cy) = (center.0.eval(c), center.1.eval(c));
                p.radius = radius.eval(c).max(0.0);
                (p.ex, p.ey) = (scale.0.eval(c).max(0.01), scale.1.eval(c).max(0.01));
            }
            Shape::Rect { center, half_size: half, radius } => {
                p.kind = 1;
                (p.cx, p.cy) = (center.0.eval(c), center.1.eval(c));
                (p.mx, p.my) = (half.0.eval(c).max(0.0), half.1.eval(c).max(0.0));
                p.radius = radius.eval(c).min(p.mx).min(p.my).max(0.0);
            }
            Shape::Arc { center, radius, opening: aperture, thickness } => {
                p.kind = 2;
                (p.cx, p.cy) = (center.0.eval(c), center.1.eval(c));
                p.radius = radius.eval(c).max(0.0);
                p.ex = aperture.eval(c);
                p.stroke = thickness.eval(c).max(0.0);
            }
            Shape::Segment { from, to, thickness } => {
                p.kind = 3;
                (p.cx, p.cy) = (from.0.eval(c), from.1.eval(c));
                (p.mx, p.my) = (to.0.eval(c) - p.cx, to.1.eval(c) - p.cy);
                p.stroke = thickness.eval(c).max(0.0);
            }
            Shape::Rotated(f, angle) => {
                p = f.flatten(c);
                p.rotation += angle.eval(c);
            }
            Shape::Stroke(f, thickness) => {
                p = f.flatten(c);
                p.stroke = thickness.eval(c).max(0.0);
            }
            // With nowhere to leave the points, a path is its box: that is good
            // enough for the mouse, and the GPU always gets it through `flatten_into`.
            Shape::Path { .. } => {
                let mut pts = Vec::new();
                p = self.flatten_into(c, &mut pts);
                p.ex = 0.0;
                p.kind = 1;
            }
        }
        p
    }

    #[allow(dead_code)]
    /// The same distance the shader computes, to know whether the mouse is
    /// inside without asking the GPU.
    pub fn distance(&self, c: Ctx, x: f32, y: f32) -> f32 {
        self.flatten(c).distance(x, y)
    }
}

/// A shape with its expressions already evaluated.
#[derive(Clone, Copy, Debug)]
pub struct FlatShape {
    pub kind: u8,
    pub cx: f32,
    pub cy: f32,
    /// Box: half width and height. Segment: the vector to the other end.
    pub mx: f32,
    pub my: f32,
    pub radius: f32,
    pub rotation: f32,
    /// Ellipse: its scale. Arc: `ex` is the half aperture.
    pub ex: f32,
    pub ey: f32,
    pub stroke: f32,
    /// What is inherited from the group and its parents, already multiplied.
    pub affine: Affine,
}

fn rotate_point(x: f32, y: f32, pivot: (f32, f32), angle: f32) -> (f32, f32) {
    if angle == 0.0 {
        return (x, y);
    }
    let (qx, qy) = (x - pivot.0, y - pivot.1);
    let (s, c) = angle.sin_cos();
    (c * qx + s * qy + pivot.0, -s * qx + c * qy + pivot.1)
}

impl FlatShape {
    pub fn distance(&self, x: f32, y: f32) -> f32 {
        self.distance_with(x, y, &[])
    }

    /// The same computation the shader does. `pts` are the points of the paths,
    /// the ones `flatten_into` left; without them, a path is its box.
    pub fn distance_with(&self, x: f32, y: f32, pts: &[f32]) -> f32 {
        let (x, y) = self.affine.inverse().apply(x, y);
        let (x, y) = rotate_point(x, y, (self.cx, self.cy), self.rotation);
        let (px, py) = (x - self.cx, y - self.cy);
        let mut d = match self.kind {
            0 => ((px / self.ex).hypot(py / self.ey) - self.radius) * self.ex.min(self.ey),
            1 => {
                if self.mx < 0.5 || self.my < 0.5 {
                    return f32::MAX;
                }
                let (qx, qy) = (px.abs() - self.mx + self.radius, py.abs() - self.my + self.radius);
                qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - self.radius
            }
            2 => {
                let (s, c) = self.ex.sin_cos();
                let (qx, qy) = (px.abs(), -py);
                if c * qx > s * qy { (qx - s * self.radius).hypot(qy - c * self.radius) } else { (qx.hypot(qy) - self.radius).abs() }
            }
            3 => {
                let h = ((px * self.mx + py * self.my) / (self.mx * self.mx + self.my * self.my).max(0.0001)).clamp(0.0, 1.0);
                (px - self.mx * h).hypot(py - self.my * h)
            }
            // A path: to the nearest segment, and signed if it is closed.
            _ => {
                let (first, n) = (self.radius as usize * 2, self.ex as usize);
                if n < 2 || first + n * 2 > pts.len() {
                    let (qx, qy) = (px.abs() - self.mx, py.abs() - self.my);
                    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0)
                } else {
                    let point = |i: usize| (pts[first + i * 2], pts[first + i * 2 + 1]);
                    let (mut best, mut inside) = (f32::MAX, 1.0f32);
                    let segments = if self.ey > 0.5 { n } else { n - 1 };
                    for i in 0..segments {
                        let (a, b) = (point(i), point((i + 1) % n));
                        let (ex, ey) = (b.0 - a.0, b.1 - a.1);
                        let (wx, wy) = (px - a.0, py - a.1);
                        let h = ((wx * ex + wy * ey) / (ex * ex + ey * ey).max(1e-6)).clamp(0.0, 1.0);
                        best = best.min((wx - ex * h).hypot(wy - ey * h));
                        if self.ey > 0.5 {
                            let c = [py >= a.1, py < b.1, ex * wy > ey * wx];
                            if c.iter().all(|x| *x) || c.iter().all(|x| !*x) {
                                inside = -inside;
                            }
                        }
                    }
                    best * inside
                }
            }
        };
        if self.stroke > 0.0 {
            // What is already a line (arc, segment, open path) only gets thicker;
            // what encloses something stays as its outline.
            let line = self.kind == 2 || self.kind == 3 || (self.kind == 4 && self.ey <= 0.5);
            d = if line { d - self.stroke * 0.5 } else { d.abs() - self.stroke * 0.5 };
        }
        d * self.affine.factor()
    }

    /// To clip with an inward margin.
    pub fn shrink(mut self, m: f32) -> FlatShape {
        match self.kind {
            0 => self.radius = (self.radius - m).max(0.0),
            1 => {
                self.mx = (self.mx - m).max(0.0);
                self.my = (self.my - m).max(0.0);
                self.radius = (self.radius - m).max(0.0);
            }
            _ => {}
        }
        self
    }

    /// The box that surely contains the shape. `None` if it does not exist.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let half = self.stroke * 0.5;
        let (mut hx, mut hy) = match self.kind {
            0 => (self.radius * self.ex, self.radius * self.ey),
            1 => {
                if self.mx < 0.5 || self.my < 0.5 {
                    return None;
                }
                (self.mx, self.my)
            }
            2 => (self.radius, self.radius),
            3 => {
                let l = self.mx.hypot(self.my);
                (l, l)
            }
            _ => (self.mx, self.my),
        };
        if self.rotation != 0.0 {
            let r = hx.hypot(hy);
            (hx, hy) = (r, r);
        }
        let local = [self.cx - hx - half, self.cy - hy - half, self.cx + hx + half, self.cy + hy + half];
        Some(self.affine.bounds(local))
    }

    /// The shape as `encode` left it in a draw list (the affine goes in inverted).
    pub fn decode(s: &[f32]) -> FlatShape {
        let inverse = Affine { m: [s[12], s[13], s[14], s[15]], t: [s[16], s[17]] };
        FlatShape { kind: s[0] as u8, stroke: s[2], cx: s[4], cy: s[5], mx: s[6], my: s[7], radius: s[8], rotation: s[9], ex: s[10], ey: s[11], affine: inverse.inverse() }
    }

    /// Boxes that surely hold what an outlined box paints: its four sides and its
    /// four rounded corners, where `bounds` is the whole box. A frame round the
    /// screen drawn as an outline has the screen for bounds, and a change in it
    /// (its shadow, its edge light) only touches the band. `None` for any other
    /// shape, or when the band is most of the box anyway.
    pub fn outline_cover(&self) -> Option<[[f32; 4]; 8]> {
        if self.kind != 1 || self.stroke <= 0.0 || self.rotation != 0.0 || self.mx < 0.5 || self.my < 0.5 {
            return None;
        }
        // Painted within half the stroke of the outline: the straight sides lie on
        // the box's edges, each corner's curve inside a square of its radius.
        let h = self.stroke * 0.5;
        let c = self.radius.clamp(0.0, self.mx.min(self.my)) + h;
        let (x0, y0, x1, y1) = (self.cx - self.mx, self.cy - self.my, self.cx + self.mx, self.cy + self.my);
        let local = [
            [x0 - h, y0 - h, x1 + h, y0 + h],
            [x0 - h, y1 - h, x1 + h, y1 + h],
            [x0 - h, y0 - h, x0 + h, y1 + h],
            [x1 - h, y0 - h, x1 + h, y1 + h],
            [x0 - h, y0 - h, x0 + c, y0 + c],
            [x1 - c, y0 - h, x1 + h, y0 + c],
            [x0 - h, y1 - c, x0 + c, y1 + h],
            [x1 - c, y1 - c, x1 + h, y1 + h],
        ];
        let area = |b: &[f32; 4]| (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
        if local.iter().map(area).sum::<f32>() > 0.6 * (x1 - x0 + 2.0 * h) * (y1 - y0 + 2.0 * h) {
            return None;
        }
        Some(local.map(|b| self.affine.bounds(b)))
    }

    pub fn encode(&self, blend: f32, s: &mut [f32]) {
        let (c2, c3) = if self.kind == 2 { (self.ex, 0.0) } else { (self.ex, self.ey) };
        s[..12].copy_from_slice(&[
            self.kind as f32, blend, self.stroke, 0.0,
            self.cx, self.cy, self.mx, self.my,
            self.radius, self.rotation, c2, c3,
        ]);
        self.affine.encode(&mut s[12..20]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(affine: Affine) -> FlatShape {
        // A rounded outline as thick as a frame, its box the whole of a 1920 x 1080 screen.
        FlatShape { kind: 1, cx: 960.0, cy: 540.0, mx: 940.0, my: 520.0, radius: 60.0, rotation: 0.0, ex: 1.0, ey: 1.0, stroke: 40.0, affine }
    }

    #[test]
    fn an_outlined_box_is_covered_by_its_band() {
        for affine in [Affine::IDENTITY, Affine::new((960.0, 540.0), 0.0, (0.5, 0.75), (13.0, -7.0))] {
            let p = frame(affine);
            let pieces = p.outline_cover().expect("a thin outline of a big box is a band");
            let b = p.bounds().unwrap();
            let area = |r: &[f32; 4]| (r[2] - r[0]) * (r[3] - r[1]);
            assert!(pieces.iter().map(area).sum::<f32>() < 0.3 * area(&b));
            // Every point it paints falls in one of the pieces.
            let mut y = b[1];
            while y <= b[3] {
                let mut x = b[0];
                while x <= b[2] {
                    if p.distance(x, y) <= 0.0 {
                        assert!(pieces.iter().any(|r| x >= r[0] - 0.01 && x <= r[2] + 0.01 && y >= r[1] - 0.01 && y <= r[3] + 0.01), "({x}, {y}) is painted and in no piece");
                    }
                    x += 3.0;
                }
                y += 3.0;
            }
        }
    }

    #[test]
    fn a_filled_or_turned_box_has_no_band() {
        let mut p = frame(Affine::IDENTITY);
        p.stroke = 0.0;
        assert!(p.outline_cover().is_none());
        let mut p = frame(Affine::IDENTITY);
        p.rotation = 0.2;
        assert!(p.outline_cover().is_none());
        // An outline so thick it is most of its box: the box itself.
        let mut p = frame(Affine::IDENTITY);
        p.stroke = 700.0;
        assert!(p.outline_cover().is_none());
    }

    #[test]
    fn decode_gives_back_what_encode_wrote() {
        let p = frame(Affine::new((100.0, 50.0), 0.3, (2.0, 0.5), (4.0, 9.0)));
        let mut s = [0.0; 20];
        p.encode(0.0, &mut s);
        let q = FlatShape::decode(&s);
        for (x, y) in [(10.0, 20.0), (960.0, 540.0), (1500.0, 30.0)] {
            assert!((p.distance(x, y) - q.distance(x, y)).abs() < 1e-2);
        }
    }
}
