//! Geometry primitives: points, KiCad rotation, shapes (capsule / rounded polygon), distances, grid stamping.

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct P {
    pub x: f64,
    pub y: f64,
}

pub fn p(x: f64, y: f64) -> P {
    P { x, y }
}

impl std::ops::Add for P {
    type Output = P;
    fn add(self, o: P) -> P {
        p(self.x + o.x, self.y + o.y)
    }
}
impl std::ops::Sub for P {
    type Output = P;
    fn sub(self, o: P) -> P {
        p(self.x - o.x, self.y - o.y)
    }
}
impl std::ops::Mul<f64> for P {
    type Output = P;
    fn mul(self, k: f64) -> P {
        p(self.x * k, self.y * k)
    }
}

impl P {
    pub fn dot(self, o: P) -> f64 {
        self.x * o.x + self.y * o.y
    }
    pub fn cross(self, o: P) -> f64 {
        self.x * o.y - self.y * o.x
    }
    pub fn len(self) -> f64 {
        self.dot(self).sqrt()
    }
    /// KiCad rotation (degrees, y axis down, positive = counter-clockwise on screen)
    pub fn rot(self, deg: f64) -> P {
        if deg == 0.0 {
            return self;
        }
        let (s, c) = deg.to_radians().sin_cos();
        p(self.x * c + self.y * s, -self.x * s + self.y * c)
    }
}

/// distance from point q to segment ab
pub fn dist_seg(q: P, a: P, b: P) -> f64 {
    let ab = b - a;
    let l2 = ab.dot(ab);
    if l2 < 1e-18 {
        return (q - a).len();
    }
    let t = ((q - a).dot(ab) / l2).clamp(0.0, 1.0);
    (q - (a + ab * t)).len()
}

pub fn seg_seg_dist(a: P, b: P, c: P, d: P) -> f64 {
    if segs_intersect(a, b, c, d) {
        return 0.0;
    }
    dist_seg(a, c, d)
        .min(dist_seg(b, c, d))
        .min(dist_seg(c, a, b))
        .min(dist_seg(d, a, b))
}

pub fn segs_intersect(a: P, b: P, c: P, d: P) -> bool {
    let d1 = (b - a).cross(c - a);
    let d2 = (b - a).cross(d - a);
    let d3 = (d - c).cross(a - c);
    let d4 = (d - c).cross(b - c);
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

pub fn point_in_poly(q: P, pts: &[P]) -> bool {
    let mut inside = false;
    let n = pts.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b) = (pts[i], pts[j]);
        if (a.y > q.y) != (b.y > q.y) && q.x < (b.x - a.x) * (q.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

pub fn poly_area(pts: &[P]) -> f64 {
    let n = pts.len();
    let mut s = 0.0;
    for i in 0..n {
        s += pts[i].cross(pts[(i + 1) % n]);
    }
    s.abs() / 2.0
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Default for BBox {
    fn default() -> BBox {
        BBox::empty()
    }
}

impl BBox {
    pub fn empty() -> BBox {
        BBox {
            x0: f64::INFINITY,
            y0: f64::INFINITY,
            x1: f64::NEG_INFINITY,
            y1: f64::NEG_INFINITY,
        }
    }
    pub fn add(&mut self, q: P) {
        self.x0 = self.x0.min(q.x);
        self.y0 = self.y0.min(q.y);
        self.x1 = self.x1.max(q.x);
        self.y1 = self.y1.max(q.y);
    }
    pub fn inflate(&self, d: f64) -> BBox {
        BBox {
            x0: self.x0 - d,
            y0: self.y0 - d,
            x1: self.x1 + d,
            y1: self.y1 + d,
        }
    }
    pub fn overlaps(&self, o: &BBox) -> bool {
        self.x0 <= o.x1 && o.x0 <= self.x1 && self.y0 <= o.y1 && o.y0 <= self.y1
    }
    pub fn contains(&self, q: P) -> bool {
        q.x >= self.x0 && q.x <= self.x1 && q.y >= self.y0 && q.y <= self.y1
    }
    pub fn center(&self) -> P {
        p((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }
    pub fn valid(&self) -> bool {
        self.x0 <= self.x1
    }
}

/// Copper / keepout shape. Poly is the Minkowski sum of a polygon with a disk of radius r
/// (r = 0: plain polygon; roundrect = inner rect + corner radius). Capsule: segment ab with radius r.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Shape {
    Capsule { a: P, b: P, r: f64 },
    Poly { pts: Vec<P>, r: f64 },
}

impl Shape {
    pub fn circle(c: P, r: f64) -> Shape {
        Shape::Capsule { a: c, b: c, r }
    }
    pub fn bbox(&self) -> BBox {
        let mut b = BBox::empty();
        match self {
            Shape::Capsule { a, b: e, r } => {
                b.add(*a);
                b.add(*e);
                b.inflate(*r)
            }
            Shape::Poly { pts, r } => {
                for q in pts {
                    b.add(*q);
                }
                b.inflate(*r)
            }
        }
    }
    /// distance from point to the shape (0 inside)
    pub fn dist(&self, q: P) -> f64 {
        match self {
            Shape::Capsule { a, b, r } => (dist_seg(q, *a, *b) - r).max(0.0),
            Shape::Poly { pts, r } => {
                if pts.len() >= 3 && point_in_poly(q, pts) {
                    return 0.0;
                }
                let n = pts.len();
                let mut d = f64::INFINITY;
                for i in 0..n {
                    d = d.min(dist_seg(q, pts[i], pts[(i + 1) % n]));
                }
                (d - r).max(0.0)
            }
        }
    }
    /// transform local -> world: rotate by deg (KiCad sense) then translate
    pub fn xform(&self, at: P, deg: f64) -> Shape {
        match self {
            Shape::Capsule { a, b, r } => Shape::Capsule {
                a: at + a.rot(deg),
                b: at + b.rot(deg),
                r: *r,
            },
            Shape::Poly { pts, r } => Shape::Poly {
                pts: pts.iter().map(|q| at + q.rot(deg)).collect(),
                r: *r,
            },
        }
    }
    /// the shape grown by d on every side
    pub fn inflated(&self, d: f64) -> Shape {
        match self {
            Shape::Capsule { a, b, r } => Shape::Capsule {
                a: *a,
                b: *b,
                r: r + d,
            },
            Shape::Poly { pts, r } => Shape::Poly {
                pts: pts.clone(),
                r: r + d,
            },
        }
    }
    /// mirror local y (flip to the other side)
    pub fn mirror_y(&self) -> Shape {
        match self {
            Shape::Capsule { a, b, r } => Shape::Capsule {
                a: p(a.x, -a.y),
                b: p(b.x, -b.y),
                r: *r,
            },
            Shape::Poly { pts, r } => Shape::Poly {
                pts: pts.iter().rev().map(|q| p(q.x, -q.y)).collect(),
                r: *r,
            },
        }
    }
}

/// minimum distance between two shapes (0 if they touch/overlap)
pub fn shape_dist(s: &Shape, t: &Shape) -> f64 {
    fn edges(s: &Shape) -> (Vec<(P, P)>, f64) {
        match s {
            Shape::Capsule { a, b, r } => (vec![(*a, *b)], *r),
            Shape::Poly { pts, r } => {
                let n = pts.len();
                ((0..n).map(|i| (pts[i], pts[(i + 1) % n])).collect(), *r)
            }
        }
    }
    let (es, rs) = edges(s);
    let (et, rt) = edges(t);
    // containment
    if let Shape::Poly { pts, .. } = s {
        if pts.len() >= 3 && point_in_poly(et[0].0, pts) {
            return 0.0;
        }
    }
    if let Shape::Poly { pts, .. } = t {
        if pts.len() >= 3 && point_in_poly(es[0].0, pts) {
            return 0.0;
        }
    }
    let mut d = f64::INFINITY;
    for (a, b) in &es {
        for (c, e) in &et {
            d = d.min(seg_seg_dist(*a, *b, *c, *e));
        }
    }
    (d - rs - rt).max(0.0)
}

/// rectangle (centre, size, rotation) as polygon, optionally with corner radius `rr` (returns inner rect + r)
pub fn rect_shape(c: P, w: f64, h: f64, deg: f64, rr: f64) -> Shape {
    let rr = rr.min(w / 2.0).min(h / 2.0).max(0.0);
    let (hw, hh) = (w / 2.0 - rr, h / 2.0 - rr);
    let pts = vec![p(-hw, -hh), p(hw, -hh), p(hw, hh), p(-hw, hh)];
    Shape::Poly { pts, r: rr }.xform(c, deg)
}

/// oval pad: capsule along the long axis
pub fn oval_shape(c: P, w: f64, h: f64, deg: f64) -> Shape {
    if (w - h).abs() < 1e-9 {
        return Shape::circle(c, w / 2.0);
    }
    let (a, b, r) = if w > h {
        (p(-(w - h) / 2.0, 0.0), p((w - h) / 2.0, 0.0), h / 2.0)
    } else {
        (p(0.0, -(h - w) / 2.0), p(0.0, (h - w) / 2.0), w / 2.0)
    };
    Shape::Capsule { a, b, r }.xform(c, deg)
}

/// Regular raster over a rectangle. Cell (ix, iy) has its centre at (x0 + (ix+0.5) res, y0 + (iy+0.5) res).
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct GridSpec {
    pub x0: f64,
    pub y0: f64,
    pub res: f64,
    pub nx: usize,
    pub ny: usize,
}

impl GridSpec {
    pub fn new(b: BBox, res: f64) -> GridSpec {
        let nx = ((b.x1 - b.x0) / res).ceil().max(1.0) as usize;
        let ny = ((b.y1 - b.y0) / res).ceil().max(1.0) as usize;
        GridSpec {
            x0: b.x0,
            y0: b.y0,
            res,
            nx,
            ny,
        }
    }
    pub fn n(&self) -> usize {
        self.nx * self.ny
    }
    pub fn center(&self, ix: usize, iy: usize) -> P {
        p(
            self.x0 + (ix as f64 + 0.5) * self.res,
            self.y0 + (iy as f64 + 0.5) * self.res,
        )
    }
    pub fn idx(&self, ix: usize, iy: usize) -> usize {
        iy * self.nx + ix
    }
    pub fn cell_of(&self, q: P) -> Option<(usize, usize)> {
        let fx = ((q.x - self.x0) / self.res).floor();
        let fy = ((q.y - self.y0) / self.res).floor();
        if fx < 0.0 || fy < 0.0 || fx >= self.nx as f64 || fy >= self.ny as f64 {
            return None;
        }
        Some((fx as usize, fy as usize))
    }
    pub fn bbox(&self) -> BBox {
        BBox {
            x0: self.x0,
            y0: self.y0,
            x1: self.x0 + self.nx as f64 * self.res,
            y1: self.y0 + self.ny as f64 * self.res,
        }
    }
    /// cell index range whose centres can lie inside bbox b
    pub fn range(&self, b: &BBox) -> Option<(usize, usize, usize, usize)> {
        let ix0 = ((b.x0 - self.x0) / self.res - 0.5).ceil().max(0.0);
        let iy0 = ((b.y0 - self.y0) / self.res - 0.5).ceil().max(0.0);
        let ix1 = ((b.x1 - self.x0) / self.res - 0.5)
            .floor()
            .min(self.nx as f64 - 1.0);
        let iy1 = ((b.y1 - self.y0) / self.res - 0.5)
            .floor()
            .min(self.ny as f64 - 1.0);
        if ix1 < ix0 || iy1 < iy0 {
            return None;
        }
        Some((ix0 as usize, iy0 as usize, ix1 as usize, iy1 as usize))
    }

    /// call f(ix, iy) for every cell whose centre lies within distance d of the shape (exact at cell centres)
    pub fn stamp<F: FnMut(usize, usize)>(&self, s: &Shape, d: f64, mut f: F) {
        match s {
            Shape::Capsule { a, b, r } => self.stamp_capsule(*a, *b, r + d, &mut f),
            Shape::Poly { pts, r } => {
                let rr = r + d;
                if pts.len() >= 3 {
                    self.fill_poly(pts, &mut f);
                }
                if rr > 0.0 || pts.len() < 3 {
                    let n = pts.len();
                    for i in 0..n {
                        self.stamp_capsule(pts[i], pts[(i + 1) % n], rr.max(1e-9), &mut f);
                    }
                }
            }
        }
    }

    fn stamp_capsule<F: FnMut(usize, usize)>(&self, a: P, b: P, r: f64, f: &mut F) {
        let mut bb = BBox::empty();
        bb.add(a);
        bb.add(b);
        let bb = bb.inflate(r);
        let Some((ix0, iy0, ix1, iy1)) = self.range(&bb) else {
            return;
        };
        let ab = b - a;
        let l2 = ab.dot(ab);
        let r2 = r * r;
        for iy in iy0..=iy1 {
            let cy = self.y0 + (iy as f64 + 0.5) * self.res;
            for ix in ix0..=ix1 {
                let q = p(self.x0 + (ix as f64 + 0.5) * self.res, cy);
                let t = if l2 < 1e-18 {
                    0.0
                } else {
                    ((q - a).dot(ab) / l2).clamp(0.0, 1.0)
                };
                let e = q - (a + ab * t);
                if e.dot(e) <= r2 {
                    f(ix, iy);
                }
            }
        }
    }

    /// even-odd scanline fill at cell centres
    pub fn fill_poly<F: FnMut(usize, usize)>(&self, pts: &[P], f: &mut F) {
        let mut bb = BBox::empty();
        for q in pts {
            bb.add(*q);
        }
        let Some((_, iy0, _, iy1)) = self.range(&bb) else {
            return;
        };
        let n = pts.len();
        let mut xs: Vec<f64> = Vec::new();
        for iy in iy0..=iy1 {
            let cy = self.y0 + (iy as f64 + 0.5) * self.res;
            xs.clear();
            for i in 0..n {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                if (a.y > cy) != (b.y > cy) {
                    xs.push(a.x + (cy - a.y) * (b.x - a.x) / (b.y - a.y));
                }
            }
            xs.sort_by(|u, v| u.partial_cmp(v).unwrap());
            for k in (0..xs.len() / 2).map(|k| 2 * k) {
                let (xa, xb) = (xs[k], xs[k + 1]);
                let i0 = ((xa - self.x0) / self.res - 0.5).ceil().max(0.0) as i64;
                let i1 = (((xb - self.x0) / self.res - 0.5).floor() as i64).min(self.nx as i64 - 1);
                for ix in i0..=i1 {
                    f(ix as usize, iy);
                }
            }
        }
    }
}

/// convex hull (monotone chain)
pub fn hull(mut v: Vec<P>) -> Vec<P> {
    v.sort_by(|a, b| {
        a.x.partial_cmp(&b.x)
            .unwrap()
            .then(a.y.partial_cmp(&b.y).unwrap())
    });
    v.dedup_by(|a, b| (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9);
    if v.len() < 3 {
        return v;
    }
    let mut lo: Vec<P> = Vec::new();
    for q in &v {
        while lo.len() >= 2
            && (lo[lo.len() - 1] - lo[lo.len() - 2]).cross(*q - lo[lo.len() - 2]) <= 0.0
        {
            lo.pop();
        }
        lo.push(*q);
    }
    let mut up: Vec<P> = Vec::new();
    for q in v.iter().rev() {
        while up.len() >= 2
            && (up[up.len() - 1] - up[up.len() - 2]).cross(*q - up[up.len() - 2]) <= 0.0
        {
            up.pop();
        }
        up.push(*q);
    }
    lo.pop();
    up.pop();
    lo.extend(up);
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn kicad_rotation_is_counter_clockwise_on_screen() {
        // y points down: +90 deg turns +x into -y (up on screen)
        let q = p(1.0, 0.0).rot(90.0);
        assert!(close(q.x, 0.0) && close(q.y, -1.0));
        let q = p(0.0, 1.0).rot(90.0);
        assert!(close(q.x, 1.0) && close(q.y, 0.0));
    }

    #[test]
    fn point_segment_and_segment_segment_distance() {
        assert!(close(dist_seg(p(0.0, 1.0), p(-1.0, 0.0), p(1.0, 0.0)), 1.0));
        assert!(close(dist_seg(p(3.0, 0.0), p(-1.0, 0.0), p(1.0, 0.0)), 2.0));
        assert!(close(
            seg_seg_dist(p(0.0, 0.0), p(1.0, 0.0), p(0.0, 2.0), p(1.0, 2.0)),
            2.0
        ));
        assert!(segs_intersect(
            p(0.0, 0.0),
            p(2.0, 2.0),
            p(0.0, 2.0),
            p(2.0, 0.0)
        ));
        assert!(!segs_intersect(
            p(0.0, 0.0),
            p(1.0, 0.0),
            p(0.0, 1.0),
            p(1.0, 1.0)
        ));
    }

    #[test]
    fn polygon_containment_and_area() {
        let sq = vec![p(0.0, 0.0), p(2.0, 0.0), p(2.0, 2.0), p(0.0, 2.0)];
        assert!(point_in_poly(p(1.0, 1.0), &sq));
        assert!(!point_in_poly(p(3.0, 1.0), &sq));
        assert!(close(poly_area(&sq).abs(), 4.0));
    }

    #[test]
    fn shape_distances() {
        let pad = rect_shape(p(0.0, 0.0), 2.0, 1.0, 0.0, 0.0);
        let trk = Shape::Capsule {
            a: p(-5.0, 2.0),
            b: p(5.0, 2.0),
            r: 0.25,
        };
        // pad top edge at y = 0.5, track edge at y = 1.75
        assert!(close(shape_dist(&pad, &trk), 1.25));
        assert!(close(pad.dist(p(0.0, 0.0)), 0.0));
        let rot = rect_shape(p(0.0, 0.0), 2.0, 1.0, 90.0, 0.0);
        assert!(close(rot.dist(p(0.0, 3.0)), 2.0));
        let ov = oval_shape(p(0.0, 0.0), 3.0, 1.0, 0.0);
        assert!(close(ov.dist(p(0.0, 2.0)), 1.5));
        assert!(close(ov.dist(p(3.0, 0.0)), 1.5));
    }

    #[test]
    fn grid_cells_and_stamping() {
        let gs = GridSpec::new(
            BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 10.0,
                y1: 5.0,
            },
            0.5,
        );
        assert_eq!((gs.nx, gs.ny), (20, 10));
        let c = gs.center(0, 0);
        assert!(close(c.x, 0.25) && close(c.y, 0.25));
        assert_eq!(gs.cell_of(p(9.9, 4.9)), Some((19, 9)));
        let mut n = 0;
        gs.stamp(&Shape::circle(p(5.0, 2.5), 0.5), 0.0, |_, _| n += 1);
        assert_eq!(n, 4); // the four cell centres at distance sqrt(2)/4 < 0.5
    }
}
