//! Exact geometry for the detailed router's emitter (aesthetics pass).
//!
//! The grid router only knows legality at cell centres. Everything the emitter changes after the
//! search (pad exit stubs to off-grid pad centres, alignment of the first run to the stub axis,
//! octilinear pull-tight) is checked here exactly against the same rules the cell maps use
//! (`sites::track_map` semantics: DRU clearance per net pair and layer kind, pad-local clearance,
//! mounting-hole keepouts, hole clearance to foreign drills, rule areas, board edge, net layers)
//! and against other nets' routed copper. A change is only kept when it is exactly legal, so the
//! cleanup can never add a DRC error.

use crate::board::*;
use crate::geom::*;
use crate::model::*;
use std::collections::HashMap;

/// routed copper of another net (exact shapes)
#[derive(Clone)]
pub struct FItem {
    pub sh: Shape,
    pub bb: BBox,
    pub layers: u8,
    pub net: usize,
    /// drill radius (vias), 0 for tracks
    pub drill_r: f64,
}

pub struct Checker<'a> {
    pub m: &'a Model<'a>,
    /// locked pours of the routed net's own class yield (as in the router's maps)
    pub yield_same_class: bool,
    /// floor (mm) on the via clearance to copper of other nets (0 = rules only)
    pub min_clr: f64,
    objs: &'a [Obj],
    bs: f64,
    x0: f64,
    y0: f64,
    buckets: HashMap<(i32, i32), Vec<u32>>,
    reach: f64,
}

impl<'a> Checker<'a> {
    pub fn new(m: &'a Model<'a>, objs: &'a [Obj], maxd: f64) -> Checker<'a> {
        let bs = 1.0;
        let mut bb = BBox::empty();
        for o in objs {
            bb.add(p(o.bbox.x0, o.bbox.y0));
        }
        let (x0, y0) = if bb.valid() {
            (bb.x0 - 10.0, bb.y0 - 10.0)
        } else {
            (0.0, 0.0)
        };
        let mut buckets: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (k, o) in objs.iter().enumerate() {
            let (a0, b0) = (
                ((o.bbox.x0 - x0) / bs).floor() as i32,
                ((o.bbox.y0 - y0) / bs).floor() as i32,
            );
            let (a1, b1) = (
                ((o.bbox.x1 - x0) / bs).floor() as i32,
                ((o.bbox.y1 - y0) / bs).floor() as i32,
            );
            if (a1 - a0 + 1) as i64 * (b1 - b0 + 1) as i64 > 40_000 {
                // huge object (outline-sized zone / keepout): put in a global list
                buckets
                    .entry((i32::MIN, i32::MIN))
                    .or_default()
                    .push(k as u32);
                continue;
            }
            for a in a0..=a1 {
                for b in b0..=b1 {
                    buckets.entry((a, b)).or_default().push(k as u32);
                }
            }
        }
        Checker {
            m,
            yield_same_class: true,
            min_clr: 0.0,
            objs,
            bs,
            x0,
            y0,
            buckets,
            reach: maxd + 1.0,
        }
    }

    fn near(&self, bb: &BBox, out: &mut Vec<u32>) {
        out.clear();
        let (a0, b0) = (
            ((bb.x0 - self.x0) / self.bs).floor() as i32,
            ((bb.y0 - self.y0) / self.bs).floor() as i32,
        );
        let (a1, b1) = (
            ((bb.x1 - self.x0) / self.bs).floor() as i32,
            ((bb.y1 - self.y0) / self.bs).floor() as i32,
        );
        for a in a0..=a1 {
            for b in b0..=b1 {
                if let Some(v) = self.buckets.get(&(a, b)) {
                    out.extend_from_slice(v);
                }
            }
        }
        if let Some(v) = self.buckets.get(&(i32::MIN, i32::MIN)) {
            out.extend_from_slice(v);
        }
        out.sort_unstable();
        out.dedup();
    }

    /// track centreline a-b with half width hw (incl. margin) on layer l legal for `net` against
    /// fixed copper/rules (same semantics as sites::track_map: blocked when distance <= required)
    pub fn seg_ok_fixed(&self, net: usize, a: P, b: P, hw: f64, l: usize) -> bool {
        let m = self.m;
        if m.net_layers[net] & (1 << l) == 0 {
            return false;
        }
        // copper that lies inside an own pad adds nothing: only the part outside is checked (a
        // stub from the centre of a pad that already sits closer to another net than the rule,
        // e.g. LV pins of an isolator next to its HV pins, #53)
        let Some((a, b)) = self.clip_own_pads(net, a, b, hw, l) else {
            return true;
        };
        let cl = Shape::Capsule { a, b, r: 0.0 };
        let mut bb = cl.bbox().inflate(self.reach + hw);
        bb.x0 -= 0.0;
        let mut cand = Vec::new();
        self.near(&bb, &mut cand);
        for &k in &cand {
            let o = &self.objs[k as usize];
            if o.layers & (1 << l) == 0 {
                continue;
            }
            if self.yield_same_class
                && matches!(o.kind, Kind::Zone)
                && o.net != 0 // net-less copper (graphics, texts) never refills
                && m.r.net_class[o.net] == m.r.net_class[net]
            {
                continue;
            }
            let d = match o.kind {
                Kind::Keepout { tracks, .. } => {
                    if tracks {
                        hw
                    } else {
                        -1.0
                    }
                }
                Kind::ViaKeepout => -1.0,
                _ => {
                    let c = m.clr(net, o, l);
                    if c < 0.0 {
                        -1.0
                    } else {
                        let mut c = c;
                        if let Kind::Pad { fp, .. } = o.kind {
                            if m.is_mh(fp) {
                                c = c.max(m.r.hole_keepout(&m.b.fps[fp].refd, net));
                            }
                        }
                        c + hw
                    }
                }
            };
            if d >= 0.0 && !o.bbox.inflate(d).overlaps(&cl.bbox()) {
                // fast reject
            } else if d >= 0.0 && shape_dist(&cl, &o.shape) <= d {
                return false;
            }
            if o.hole_r > 0.0 && (o.net != net || net == 0) {
                let ctr = match &o.shape {
                    Shape::Capsule { a, b, .. } => (*a + *b) * 0.5,
                    Shape::Poly { pts, .. } => {
                        pts.iter().fold(p(0.0, 0.0), |s, q| s + *q) * (1.0 / pts.len() as f64)
                    }
                };
                if dist_seg(ctr, a, b) <= o.hole_r + m.r.min_hole_clearance + hw {
                    return false;
                }
            }
        }
        let e = m.r.edge_clearance(net) + hw;
        for (p0, p1) in &m.b.edge_segs {
            if seg_seg_dist(a, b, *p0, *p1) <= e {
                return false;
            }
        }
        if !m.b.inside(a) || !m.b.inside(b) {
            return false;
        }
        true
    }

    /// the part of a-b that is checked against other copper when an end lies inside a pad of
    /// `net` on layer l (the track's disk there inside the pad): the track from that end until
    /// its centre leaves the pad, plus its half width, is the pad's own copper. None when the
    /// whole track lies inside own pads
    fn clip_own_pads(&self, net: usize, a: P, b: P, hw: f64, l: usize) -> Option<(P, P)> {
        let len = (b - a).len();
        if net == 0 || len < 1e-12 {
            return Some((a, b));
        }
        let mut cand = Vec::new();
        // largest t in [0, 1] up to which the centre stays in an own pad (pads are convex: one
        // interval from the end), plus the half width
        let covered = |from: P, to: P, cand: &mut Vec<u32>| -> f64 {
            self.near(&BBox::empty_at(from), cand);
            let mut best: f64 = 0.0;
            for &k in cand.iter() {
                let o = &self.objs[k as usize];
                if o.net != net || o.layers & (1 << l) == 0 || !matches!(o.kind, Kind::Pad { .. }) {
                    continue;
                }
                if depth(&o.shape, from) < hw {
                    continue;
                }
                let at = |t: f64| from + (to - from) * t;
                if depth(&o.shape, to) >= hw {
                    return 1.0;
                }
                let (mut lo, mut hi) = (0.0, 1.0);
                for _ in 0..30 {
                    let mid = 0.5 * (lo + hi);
                    if depth(&o.shape, at(mid)) >= 0.0 {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                best = best.max(lo + hw / len);
            }
            best.min(1.0)
        };
        let ta = covered(a, b, &mut cand);
        if ta >= 1.0 {
            return None;
        }
        let tb = covered(b, a, &mut cand);
        if ta + tb >= 1.0 {
            return None;
        }
        Some((a + (b - a) * ta, b + (a - b) * tb))
    }

    /// through via (radius vr incl. margin, drill radius dr) legal for `net` against fixed objects
    /// (same semantics as sites::via_map)
    pub fn via_ok_fixed(&self, net: usize, at: P, vr: f64, dr: f64) -> bool {
        self.via_block_fixed(net, at, vr, dr).is_none()
    }

    /// what blocks a through via at `at` (None = legal; see [`Checker::via_ok_fixed`]):
    /// "keepout", "pad", "copper", "mask", "hole", "edge"
    pub fn via_block_fixed(&self, net: usize, at: P, vr: f64, dr: f64) -> Option<&'static str> {
        let m = self.m;
        let via_pad_gap = 0.2;
        let pt = Shape::circle(at, 0.0);
        let bb = pt.bbox().inflate(self.reach + vr + 1.0);
        let mut cand = Vec::new();
        self.near(&bb, &mut cand);
        for &k in &cand {
            let o = &self.objs[k as usize];
            if self.yield_same_class
                && matches!(o.kind, Kind::Zone)
                && o.net != 0 // net-less copper (graphics, texts) never refills
                && m.r.net_class[o.net] == m.r.net_class[net]
            {
                continue;
            }
            let mut d: f64 = -1.0;
            match o.kind {
                Kind::Keepout { vias, .. } => {
                    if vias {
                        d = vr;
                    }
                }
                Kind::ViaKeepout => d = vr,
                _ => {
                    for l in 0..NL {
                        if o.layers & (1 << l) == 0 {
                            continue;
                        }
                        let mut c = m.clr(net, o, l);
                        if c >= 0.0 && o.net != net {
                            c = c.max(self.min_clr);
                        }
                        if matches!(o.kind, Kind::Pad { .. }) {
                            c = c.max(via_pad_gap);
                        }
                        if c < 0.0 {
                            continue;
                        }
                        d = d.max(c + vr);
                    }
                    if let Kind::Pad { fp, .. } = o.kind {
                        if m.is_mh(fp) {
                            d = d.max(m.r.hole_keepout(&m.b.fps[fp].refd, net) + vr);
                        }
                    }
                }
            }
            if d >= 0.0 && o.shape.dist(at) <= d {
                return Some(match o.kind {
                    Kind::Keepout { .. } | Kind::ViaKeepout => "keepout",
                    Kind::Pad { .. } => "pad",
                    Kind::Mask { .. } => "mask",
                    _ => "copper",
                });
            }
            if o.hole_r > 0.0 {
                let ctr = match &o.shape {
                    Shape::Capsule { a, b, .. } => (*a + *b) * 0.5,
                    Shape::Poly { pts, .. } => {
                        pts.iter().fold(p(0.0, 0.0), |s, q| s + *q) * (1.0 / pts.len() as f64)
                    }
                };
                if (ctr - at).len() <= m.r.min_hole_to_hole + o.hole_r + dr {
                    return Some("hole");
                }
            }
        }
        let e = m.r.edge_clearance(net) + vr;
        for (p0, p1) in &m.b.edge_segs {
            if dist_seg(at, *p0, *p1) <= e {
                return Some("edge");
            }
        }
        if !m.b.inside(at) {
            return Some("edge");
        }
        None
    }

    pub fn via_ok_foreign(&self, net: usize, at: P, vr: f64, dr: f64, foreign: &[FItem]) -> bool {
        for f in foreign {
            if f.net == net {
                continue;
            }
            let mut c: f64 = 0.0;
            for l in 0..NL {
                if f.layers & (1 << l) != 0 {
                    c = c.max(self.m.r.clr(net, f.net, is_outer(l)).max(0.0));
                }
            }
            // copper clearance, and the new hole's clearance to foreign copper
            if f.sh.dist(at) <= (c + vr).max(dr + self.m.r.min_hole_clearance) {
                return false;
            }
            if f.drill_r > 0.0 {
                let ctr = match &f.sh {
                    Shape::Capsule { a, b, .. } => (*a + *b) * 0.5,
                    Shape::Poly { pts, .. } => pts[0],
                };
                if (ctr - at).len() <= self.m.r.min_hole_to_hole + f.drill_r + dr {
                    return false;
                }
            }
        }
        true
    }

    /// against routed copper of other nets
    pub fn seg_ok_foreign(
        &self,
        net: usize,
        a: P,
        b: P,
        hw: f64,
        l: usize,
        foreign: &[FItem],
    ) -> bool {
        let cl = Shape::Capsule { a, b, r: 0.0 };
        let bb = cl.bbox();
        for f in foreign {
            if f.layers & (1 << l) == 0 || f.net == net {
                continue;
            }
            let c = self.m.r.clr(net, f.net, is_outer(l)).max(0.0);
            if !f.bb.inflate(c + hw).overlaps(&bb) {
                continue;
            }
            if shape_dist(&cl, &f.sh) <= c + hw {
                return false;
            }
            if f.drill_r > 0.0 {
                // hole clearance to a foreign via's drill
                if let Shape::Capsule { a: ca, .. } = &f.sh {
                    if dist_seg(*ca, a, b) <= f.drill_r + self.m.r.min_hole_clearance + hw {
                        return false;
                    }
                }
            }
        }
        true
    }
}

// ------------------------------------------------------------------------------------------------
// octilinear helpers

pub fn octi(d: P) -> bool {
    let (ax, ay) = (d.x.abs(), d.y.abs());
    let tol = 2e-6 + 1e-6 * (ax + ay);
    ax <= tol || ay <= tol || (ax - ay).abs() <= tol
}

pub fn unit(d: P) -> P {
    let l = d.len();
    if l < 1e-12 {
        p(0.0, 0.0)
    } else {
        d * (1.0 / l)
    }
}

pub fn parallel(u: P, v: P) -> bool {
    unit(u).cross(unit(v)).abs() < 1e-6
}

/// a segment a -> b that is only slightly off its nearest octilinear direction u (perpendicular
/// offset 1e-7 .. 0.06 mm): the two jog points that make it octilinear, `a + u t` (straight from a,
/// jog at b) and `b - u t` (jog at a). None if the segment is octilinear or clearly diagonal.
pub fn octi_jog(a: P, b: P) -> Option<(P, P)> {
    let d = b - a;
    if d.len() < 1e-9 || octi(d) {
        return None;
    }
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let dirs = [
        p(1.0, 0.0),
        p(-1.0, 0.0),
        p(0.0, 1.0),
        p(0.0, -1.0),
        p(s, s),
        p(s, -s),
        p(-s, s),
        p(-s, -s),
    ];
    let u = dirs
        .into_iter()
        .max_by(|x, y| d.dot(*x).total_cmp(&d.dot(*y)))?;
    let (along, off) = (d.dot(u), d.cross(u).abs());
    if off > 0.06 || along <= off {
        return None;
    }
    Some((a + u * along, b - u * along))
}

/// intersection of lines p + s u and q + t v
pub fn intersect(pp: P, u: P, q: P, v: P) -> Option<P> {
    let den = u.cross(v);
    if den.abs() < 1e-12 {
        return None;
    }
    let s = (q - pp).cross(v) / den;
    Some(pp + u * s)
}

/// turning angle in degrees between consecutive directions d1 then d2 (0 = straight)
pub fn turn(d1: P, d2: P) -> f64 {
    let (a, b) = (unit(d1), unit(d2));
    a.dot(b).clamp(-1.0, 1.0).acos().to_degrees()
}

/// octilinear connections a -> b: 1 segment if already octilinear, else the two 2-segment
/// variants and three 3-segment variants (centred jog)
pub fn octi_links(a: P, b: P) -> Vec<Vec<P>> {
    let d = b - a;
    if d.len() < 1e-9 {
        return vec![];
    }
    if octi(d) {
        return vec![vec![a, b]];
    }
    let (sx, sy) = (d.x.signum(), d.y.signum());
    let (ax, ay) = (d.x.abs(), d.y.abs());
    let m = ax.min(ay);
    let diag = p(sx * m, sy * m);
    let mut out = vec![vec![a, a + diag, b], vec![a, b - diag, b]];
    // straight - diagonal - straight with the straight split in halves; diagonal - straight - diagonal
    let st = if ax > ay {
        p(sx * (ax - ay), 0.0)
    } else {
        p(0.0, sy * (ay - ax))
    };
    out.push(vec![a, a + st * 0.5, a + st * 0.5 + diag, b]);
    out.push(vec![a, a + diag * 0.5, a + diag * 0.5 + st, b]);
    // orthogonal L corners
    out.push(vec![a, p(a.x, b.y), b]);
    out.push(vec![a, p(b.x, a.y), b]);
    out
}

// ------------------------------------------------------------------------------------------------
// runs

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum End {
    /// fixed point, any direction (via, fixed junction)
    Fixed,
    /// pad exit stub: pts[0] (pad centre) -> pts[1] fixed along u; pts[1] may slide along u but
    /// stays at least the given distance from the pad centre (straight-exit rule: half extent + 0.5 mm)
    Stub(P, f64),
    /// junction on routed copper of the same net: the end may slide along its own segment onto
    /// the parent centreline a-b
    Junction(P, P),
}

/// a legal exit stub of a stub end's pad on the run's layer (the cleanup may switch the exit to
/// any of them; all are straight along a pad axis)
#[derive(Clone, Copy, Debug)]
pub struct Alt {
    pub u: P,
    /// exit length (half extent + 0.5 mm, or the shortened length of a short stub)
    pub ml: f64,
    /// shortest length in the relaxed pass (half extent + 0.12 mm): a counted straight-exit exception
    pub hard: f64,
    /// short stub (the full one is blocked): only in the relaxed pass
    pub short: bool,
    /// stub index in the router
    pub id: usize,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub l: usize,
    pub pts: Vec<P>,
    pub w: Vec<f64>, // width per segment
    pub head: End,
    pub tail: End,
    /// stub alternatives of the head [0] and tail [1] pad (empty: only the End's own axis)
    pub alts: [Vec<Alt>; 2],
}

impl Run {
    pub fn reversed(&self) -> Run {
        let mut pts = self.pts.clone();
        pts.reverse();
        let mut w = self.w.clone();
        w.reverse();
        Run {
            l: self.l,
            pts,
            w,
            head: self.tail,
            tail: self.head,
            alts: [self.alts[1].clone(), self.alts[0].clone()],
        }
    }
    /// merge collinear consecutive segments of equal width, drop zero-length ones
    pub fn merge(&mut self) {
        let mut pts = vec![self.pts[0]];
        let mut w: Vec<f64> = Vec::new();
        for k in 1..self.pts.len() {
            let q = self.pts[k];
            if (q - *pts.last().unwrap()).len() < 1e-7 {
                continue;
            }
            let wk = self.w[k - 1];
            if pts.len() >= 2 {
                let n = pts.len();
                let (a, b) = (pts[n - 2], pts[n - 1]);
                if (wk - w[n - 2]).abs() < 1e-9
                    && parallel(b - a, q - b)
                    && (b - a).dot(q - b) > 0.0
                {
                    pts[n - 1] = q;
                    continue;
                }
            }
            pts.push(q);
            w.push(wk);
        }
        self.pts = pts;
        self.w = w;
    }
}

pub struct Ctx<'c, 'a> {
    pub ck: &'c Checker<'a>,
    pub net: usize,
    pub hw_opt: f64, // opt half width (no margin)
    pub margin: f64,
    pub foreign: &'c [FItem],
    /// own pads (other than the run's stub pads) the new copper must not enter: (shape, layers)
    pub own_pads: &'c [(Shape, u8)],
    /// narrow width allowed at this point (neck region)
    pub neck: &'c dyn Fn(P) -> bool,
    /// stub-end / junction sliding allowed
    pub slide: bool,
}

impl<'c, 'a> Ctx<'c, 'a> {
    pub fn seg_ok(&self, a: P, b: P, w: f64, l: usize) -> bool {
        self.seg_ok2(a, b, w, l, false)
    }
    pub fn seg_ok2(&self, a: P, b: P, w: f64, l: usize, skip_own: bool) -> bool {
        if (b - a).len() < 1e-9 {
            return true;
        }
        let hw = w / 2.0 + self.margin;
        if w / 2.0 < self.hw_opt - 1e-9 {
            // narrow segment: only in the neck region (sampled)
            let n = ((b - a).len() / 0.05).ceil() as usize + 1;
            for s in 0..=n {
                if !(self.neck)(a + (b - a) * (s as f64 / n as f64)) {
                    return false;
                }
            }
        }
        let cl = Shape::Capsule { a, b, r: 0.0 };
        for (sh, ls) in self.own_pads.iter().filter(|_| !skip_own) {
            if ls & (1 << l) != 0 && shape_dist(&cl, sh) < w / 2.0 - 1e-6 {
                return false;
            }
        }
        self.ck.seg_ok_fixed(self.net, a, b, hw, l)
            && self.ck.seg_ok_foreign(self.net, a, b, hw, l, self.foreign)
    }
}

/// score of a run: length + bend penalties + short-segment penalty + non-octilinear penalty
fn score(pts: &[P]) -> f64 {
    let mut s = 0.0;
    let n = pts.len();
    for k in 0..n.saturating_sub(1) {
        let d = pts[k + 1] - pts[k];
        let l = d.len();
        s += l;
        if !octi(d) {
            s += 3.0;
        }
        if l < 0.4 {
            s += 1.5 * (0.4 - l) / 0.4;
        }
        if k + 2 < n {
            let t = turn(d, pts[k + 2] - pts[k + 1]);
            if t > 1.0 {
                s += if t > 50.0 { 0.6 } else { 0.4 };
            }
            if t > 91.0 {
                s += 5.0;
            }
        }
    }
    s
}

#[derive(Default, Debug, Clone, Copy)]
pub struct CleanStats {
    pub aligned: usize,
    pub align_failed: usize,
    pub pulls: usize,
    pub mitered: usize,
    pub miter_failed: usize,
    pub shifted: usize,
    /// kept 90-degree corners by cause: clearance, L between stubs, shift blocked, Z/U too short, short legs
    pub kept90: [usize; 5],
    /// stub ends kept on the axis with a tiny perpendicular jog (exact alignment was not legal)
    pub jogs: usize,
}

/// align a stub head: the first vertex after the pad centre moves onto the stub axis. When that
/// is not exactly legal, a vertex that is only slightly off the axis (grid rounding, < 0.06 mm)
/// gets a tiny perpendicular jog instead, so the exit stays straight and every segment octilinear.
fn align_stub_head(r: &mut Run, cx: &Ctx, st: &mut CleanStats) {
    let failed = st.align_failed;
    align_stub_head_exact(r, cx, st);
    if st.align_failed > failed && jog_to_axis(r, cx) {
        st.align_failed = failed;
        st.jogs += 1;
    }
}

/// c -> g with g slightly off the stub axis u becomes c -> e -> g, e = projection of g on the axis
fn jog_to_axis(r: &mut Run, cx: &Ctx) -> bool {
    let End::Stub(u, min_len) = r.head else {
        return false;
    };
    if r.pts.len() < 2 {
        return false;
    }
    let (c, g) = (r.pts[0], r.pts[1]);
    let off = (g - c).cross(u).abs();
    let along = (g - c).dot(u);
    if !(1e-7..=0.06).contains(&off) || along < min_len {
        return false;
    }
    let e = c + u * along;
    let w = r.w[0];
    if !cx.seg_ok2(c, e, w, r.l, true) || !cx.seg_ok(e, g, w, r.l) {
        return false;
    }
    r.pts.insert(1, e);
    r.w.insert(0, w);
    true
}

fn align_stub_head_exact(r: &mut Run, cx: &Ctx, st: &mut CleanStats) {
    let End::Stub(u, min_len) = r.head else {
        return;
    };
    if r.pts.len() < 2 {
        return;
    }
    let c = r.pts[0];
    // already on the axis?
    if (r.pts[1] - c).cross(u).abs() < 1e-7 && (r.pts[1] - c).dot(u) > 0.0 {
        return;
    }
    // first segment after the stub end that is not parallel to u
    let mut k = 1;
    while k + 1 < r.pts.len() && parallel(r.pts[k + 1] - r.pts[k], u) {
        k += 1;
    }
    if k + 1 >= r.pts.len() {
        // straight to the far end: a junction end may slide along its parent onto the axis
        if let End::Junction(pa, pb) = r.tail {
            if let Some(x) = intersect(c, u, pa, pb - pa) {
                let last = r.pts[r.pts.len() - 1];
                if dist_seg(x, pa, pb) < 1e-6
                    && (x - last).len() < 0.12
                    && (x - c).dot(u) >= min_len
                    && cx.seg_ok2(c, x, r.w[0], r.l, true)
                {
                    r.pts = vec![c, x];
                    r.w = vec![r.w[0]];
                    st.aligned += 1;
                    return;
                }
            }
        }
        st.align_failed += 1;
        return;
    }
    let (a, b) = (r.pts[k], r.pts[k + 1]);
    let Some(x) = intersect(c, u, a, b - a) else {
        st.align_failed += 1;
        return;
    };
    // x must lie forward on the axis, at least the straight-exit length, and inside the next segment's
    // line ahead of b's projection (no reversal)
    let along = (x - c).dot(u);
    if along < min_len || (b - x).dot(b - a) <= 1e-9 || turn(u, b - x) > 90.5 {
        st.align_failed += 1;
        return;
    }
    let ws = r.w[0];
    let wn = r.w[k];
    if cx.seg_ok2(c, x, ws, r.l, true) && cx.seg_ok(x, b, wn, r.l) {
        let mut pts = vec![c, x];
        pts.extend_from_slice(&r.pts[k + 1..]);
        let mut w = vec![ws, wn];
        w.extend_from_slice(&r.w[k + 1..]);
        r.pts = pts;
        r.w = w;
        st.aligned += 1;
    } else {
        st.align_failed += 1;
    }
}

/// slide a junction head along its first segment onto the parent centreline
fn align_junction_head(r: &mut Run, cx: &Ctx) {
    let End::Junction(pa, pb) = r.head else {
        return;
    };
    if r.pts.len() < 2 {
        return;
    }
    let (j, q) = (r.pts[0], r.pts[1]);
    if dist_seg(j, pa, pb) < 1e-7 {
        return;
    }
    let x = if parallel(q - j, pb - pa) {
        // along the parent: project
        let ab = pb - pa;
        pa + ab * ((j - pa).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0)
    } else {
        match intersect(q, j - q, pa, pb - pa) {
            Some(x) => x,
            None => return,
        }
    };
    if (x - j).len() > 0.12 || dist_seg(x, pa, pb) > 1e-6 || (q - x).dot(q - j) <= 0.0 {
        return;
    }
    if cx.seg_ok(x, q, r.w[0], r.l) {
        r.pts[0] = x;
    }
}

/// stub constraints of a candidate run and their penalty: the first (last) segment of a stub
/// head (tail) must run along one of the pad's legal stub axes away from (into) the pad and be at
/// least the straight-exit length long. None = violated. Penalty: 0.3 for another axis than the
/// routed one; in the relaxed pass (`relax` = penalty) a stub may be shortened down to `hard`.
pub fn end_pen(pts: &[P], r: &Run, relax: Option<f64>) -> Option<f64> {
    let n = pts.len();
    if n < 2 {
        return None;
    }
    let mut pen = 0.0;
    for e in 0..2 {
        let end = if e == 0 { r.head } else { r.tail };
        let End::Stub(u0, ml0) = end else { continue };
        let d = if e == 0 {
            pts[1] - pts[0]
        } else {
            pts[n - 2] - pts[n - 1]
        };
        let own = [Alt {
            u: u0,
            ml: ml0,
            hard: ml0,
            short: false,
            id: usize::MAX,
        }];
        let alts: &[Alt] = if r.alts[e].is_empty() {
            &own
        } else {
            &r.alts[e]
        };
        let mut best: Option<f64> = None;
        for a in alts {
            if !parallel(d, a.u) || d.dot(a.u) <= 0.0 {
                continue;
            }
            let primary = (a.u - u0).len() < 1e-6;
            if a.short && !primary && relax.is_none() {
                continue;
            }
            let len = d.dot(a.u);
            let mut pp = if primary { 0.0 } else { 0.3 };
            if a.short && !primary {
                pp += relax.unwrap_or(0.0);
            }
            if len < a.ml - 1e-6 {
                match relax {
                    Some(rp) if len >= a.hard - 1e-6 => pp += rp,
                    _ => continue,
                }
            }
            best = Some(best.map_or(pp, |b| b.min(pp)));
        }
        pen += best?;
    }
    Some(pen)
}

/// the alternative a stub end's first segment uses (None: no stub end / own axis only)
pub fn used_alt(r: &Run, e: usize) -> Option<Alt> {
    let n = r.pts.len();
    let d = if e == 0 {
        r.pts[1] - r.pts[0]
    } else {
        r.pts[n - 2] - r.pts[n - 1]
    };
    r.alts[e]
        .iter()
        .copied()
        .find(|a| parallel(d, a.u) && d.dot(a.u) > 0.0)
}

fn turns_ok(pts: &[P], i0: usize, i1: usize) -> bool {
    for k in i0.max(1)..=i1.min(pts.len().saturating_sub(2)) {
        if turn(pts[k] - pts[k - 1], pts[k + 1] - pts[k]) > 90.5 {
            return false;
        }
    }
    true
}

/// candidate run after replacing pts[i..=j] by link, with its score (None: stub rule or turn
/// violated, or a zero-length segment)
fn cand(r: &Run, i: usize, j: usize, link: &[P], relax: Option<f64>) -> Option<(Vec<P>, f64)> {
    let mut pts: Vec<P> = r.pts[..i].to_vec();
    pts.extend_from_slice(link);
    pts.extend_from_slice(&r.pts[j + 1..]);
    if pts.windows(2).any(|s| (s[1] - s[0]).len() < 1e-9) {
        return None;
    }
    let pen = end_pen(&pts, r, relax)?;
    if !turns_ok(&pts, i, i + link.len() - 1) {
        return None;
    }
    let sc = score(&pts) + pen;
    Some((pts, sc))
}

fn base_score(r: &Run, relax: Option<f64>) -> f64 {
    score(&r.pts) + end_pen(&r.pts, r, relax).unwrap_or(0.0)
}

/// track widths a replacement may use: the opt width first, then the neck-down width
#[derive(Clone, Copy)]
struct Widths {
    wmax: f64,
    wmin: f64,
}

/// replace pts[i..=j] (`span`) by `link` when it is exactly legal and lowers the score below `base`
fn try_replace(
    r: &mut Run,
    cx: &Ctx,
    span: (usize, usize),
    link: &[P],
    w: Widths,
    base: f64,
    relax: Option<f64>,
) -> bool {
    let (i, j) = span;
    let Widths { wmax, wmin } = w;
    let Some((pts, sc)) = cand(r, i, j, link, relax) else {
        return false;
    };
    if sc >= base - 1e-6 {
        return false;
    }
    let n = pts.len();
    let stub_seg = |k: usize| {
        (k == 0 && matches!(r.head, End::Stub(..)))
            || (k + 2 == n && matches!(r.tail, End::Stub(..)))
    };
    let mut wsel = None;
    for w in [wmax, wmin] {
        if link
            .windows(2)
            .enumerate()
            .all(|(q, s)| cx.seg_ok2(s[0], s[1], w, r.l, stub_seg(i + q)))
        {
            wsel = Some(w);
            break;
        }
    }
    let Some(w) = wsel else { return false };
    let mut ws: Vec<f64> = r.w[..i].to_vec();
    ws.extend(std::iter::repeat_n(w, link.len() - 1));
    ws.extend_from_slice(&r.w[j..]);
    r.pts = pts;
    r.w = ws;
    r.merge();
    true
}

/// octilinear pull-tight: replace vertex chains by 1-3 segment octilinear links when exactly legal
/// and the run score drops. Stub segments may grow or shrink along their axis (>= exit length).
fn pull_tight(r: &mut Run, cx: &Ctx, st: &mut CleanStats, relax: Option<f64>) {
    let mut guard = 0;
    'outer: loop {
        guard += 1;
        if guard > 300 {
            break;
        }
        let n = r.pts.len();
        if n < 3 {
            return;
        }
        let base = base_score(r, relax);
        for i in 0..n - 1 {
            for j in (i + 1..n).rev() {
                if j == i + 1 && octi(r.pts[j] - r.pts[i]) {
                    continue;
                }
                let (a, b) = (r.pts[i], r.pts[j]);
                let wmax = r.w[i..j].iter().cloned().fold(0.0, f64::max);
                let wmin = r.w[i..j].iter().cloned().fold(f64::INFINITY, f64::min);
                for link in octi_links(a, b) {
                    if try_replace(r, cx, (i, j), &link, Widths { wmax, wmin }, base, relax) {
                        st.pulls += 1;
                        continue 'outer;
                    }
                }
            }
        }
        // jog merge: two parallel segments joined by a short connector move onto one common line
        // (anywhere between the two), the neighbours follow along their own directions
        for k in 1..n.saturating_sub(2) {
            let (d0, d2) = (r.pts[k] - r.pts[k - 1], r.pts[k + 2] - r.pts[k + 1]);
            if (r.pts[k + 1] - r.pts[k]).len() > 0.8 || !parallel(d0, d2) || d0.dot(d2) <= 0.0 {
                continue;
            }
            let off = r.pts[k + 1] - r.pts[k];
            let dn = unit(d0);
            let perp = off - dn * off.dot(dn);
            for t in [0.0, 1.0, 0.5, 0.25, 0.75, 0.1, 0.9] {
                let base_pt = r.pts[k] + perp * t;
                // new A (replaces pts[k-1] .. pts[k]) and B (replaces pts[k+1] .. pts[k+2])
                let a = if k - 1 == 0 {
                    if (r.pts[0] - base_pt).cross(dn).abs() > 1e-7 {
                        continue;
                    }
                    r.pts[0]
                } else {
                    match intersect(base_pt, dn, r.pts[k - 2], r.pts[k - 1] - r.pts[k - 2]) {
                        Some(x) => x,
                        None => continue,
                    }
                };
                let b = if k + 2 == n - 1 {
                    if (r.pts[n - 1] - base_pt).cross(dn).abs() > 1e-7 {
                        continue;
                    }
                    r.pts[n - 1]
                } else {
                    match intersect(base_pt, dn, r.pts[k + 3], r.pts[k + 2] - r.pts[k + 3]) {
                        Some(x) => x,
                        None => continue,
                    }
                };
                if (b - a).dot(dn) <= 1e-6 {
                    continue;
                }
                if k >= 2 && (a - r.pts[k - 2]).dot(r.pts[k - 1] - r.pts[k - 2]) <= 1e-9 {
                    continue;
                }
                if k + 3 < n && (b - r.pts[k + 3]).dot(r.pts[k + 2] - r.pts[k + 3]) <= 1e-9 {
                    continue;
                }
                // chain pts[k-2] (or a) .. pts[k+3] (or b) replaced
                let i = k.saturating_sub(2);
                let j = if k + 3 < n { k + 3 } else { n - 1 };
                let mut link = Vec::new();
                if k >= 2 {
                    link.push(r.pts[k - 2]);
                }
                link.push(a);
                link.push(b);
                if k + 3 < n {
                    link.push(r.pts[k + 3]);
                }
                let wmax = r.w[i..j].iter().cloned().fold(0.0, f64::max);
                let wmin = r.w[i..j].iter().cloned().fold(f64::INFINITY, f64::min);
                if try_replace(r, cx, (i, j), &link, Widths { wmax, wmin }, base, relax) {
                    st.pulls += 1;
                    continue 'outer;
                }
            }
        }
        // stub slide / junction slide: best-first over the candidates
        if cx.slide && slide_ends(r, cx, base, relax) {
            st.pulls += 1;
            continue 'outer;
        }
        break;
    }
}

/// points on a stub end's legal axes: the exit length (or the relaxed one), a few steps beyond, and
/// every length from which `toward` is one octilinear segment away
fn stub_points(c: P, alts: &[Alt], toward: P, relax: bool) -> Vec<P> {
    let mut out = Vec::new();
    for a in alts {
        if a.short && !relax {
            continue;
        }
        if (toward - c).dot(a.u) <= 0.0 {
            continue;
        }
        let lo = if relax { a.hard } else { a.ml };
        let mut ts = vec![a.ml, a.ml + 0.05, a.ml + 0.1, a.ml + 0.25];
        if relax {
            ts.push(a.hard);
            ts.push(0.5 * (a.hard + a.ml));
        }
        for (dx, dy) in [(1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, -1.0)] {
            let v = p(dx, dy);
            // c + t u + s v = toward
            let den = a.u.cross(v);
            if den.abs() < 1e-9 {
                continue;
            }
            ts.push((toward - c).cross(v) / den);
        }
        for t in ts {
            if t >= lo - 1e-9 && t < 15.0 {
                out.push(c + a.u * t);
            }
        }
    }
    out
}

/// move stub ends along (or onto other) legal stub axes and junction heads/tails along their
/// parent segment; the best exactly legal candidate that lowers the score is kept
fn slide_ends(r: &mut Run, cx: &Ctx, base: f64, relax: Option<f64>) -> bool {
    let n = r.pts.len();
    if n < 2 {
        return false;
    }
    let own_h;
    let own_t;
    let alts_h: &[Alt] = match r.head {
        End::Stub(u, ml) if r.alts[0].is_empty() => {
            own_h = [Alt {
                u,
                ml,
                hard: ml,
                short: false,
                id: usize::MAX,
            }];
            &own_h
        }
        End::Stub(..) => &r.alts[0],
        _ => &[],
    };
    let alts_t: &[Alt] = match r.tail {
        End::Stub(u, ml) if r.alts[1].is_empty() => {
            own_t = [Alt {
                u,
                ml,
                hard: ml,
                short: false,
                id: usize::MAX,
            }];
            &own_t
        }
        End::Stub(..) => &r.alts[1],
        _ => &[],
    };
    let rl = relax.is_some();
    let mut cands: Vec<(f64, usize, usize, Vec<P>)> = Vec::new();
    let push = |cands: &mut Vec<(f64, usize, usize, Vec<P>)>, i: usize, j: usize, link: Vec<P>| {
        if let Some((_, sc)) = cand(r, i, j, &link, relax) {
            if sc < base - 1e-6 {
                cands.push((sc, i, j, link));
            }
        }
    };
    let (c1, c2) = (r.pts[0], r.pts[n - 1]);
    // head stub to a fixed vertex j (1..n-1; the tail vertex only if the tail is no stub)
    if !alts_h.is_empty() {
        for j in 1..n {
            if j == n - 1 && !alts_t.is_empty() {
                continue;
            }
            let b = r.pts[j];
            for s1 in stub_points(c1, alts_h, b, rl) {
                for lk in octi_links(s1, b) {
                    let mut link = vec![c1];
                    link.extend(lk);
                    push(&mut cands, 0, j, link);
                }
            }
        }
    }
    if !alts_t.is_empty() {
        for i in 0..n - 1 {
            if i == 0 && !alts_h.is_empty() {
                continue;
            }
            let a = r.pts[i];
            for s2 in stub_points(c2, alts_t, a, rl) {
                for lk in octi_links(a, s2) {
                    let mut link = lk;
                    link.push(c2);
                    push(&mut cands, i, n - 1, link);
                }
            }
        }
    }
    // both stub ends (pad to pad)
    if !alts_h.is_empty() && !alts_t.is_empty() {
        for s2 in stub_points(c2, alts_t, c1, rl) {
            for s1 in stub_points(c1, alts_h, s2, rl) {
                for lk in octi_links(s1, s2) {
                    let mut link = vec![c1];
                    link.extend(lk);
                    link.push(c2);
                    push(&mut cands, 0, n - 1, link);
                }
            }
        }
    }
    // junction head: the start slides along the parent onto the line of a later segment
    for e in 0..2 {
        let end = if e == 0 { r.head } else { r.tail };
        let End::Junction(pa, pb) = end else { continue };
        let pts: Vec<P> = if e == 0 {
            r.pts.clone()
        } else {
            r.pts.iter().rev().cloned().collect()
        };
        for k in 1..n - 1 {
            let (q0, q1) = (pts[k], pts[k + 1]);
            let Some(x) = intersect(q0, q1 - q0, pa, pb - pa) else {
                continue;
            };
            if dist_seg(x, pa, pb) > 1e-6 || (q1 - x).dot(q1 - q0) <= 1e-6 {
                continue;
            }
            // never start inside (or touching) one of the net's pads: straight exits only from the centre
            let hw = r.w[0] / 2.0;
            if cx.own_pads.iter().any(|(sh, ls)| {
                ls & (1 << r.l) != 0
                    && shape_dist(&Shape::Capsule { a: x, b: x, r: 0.0 }, sh) < hw + 0.05
            }) {
                continue;
            }
            if e == 0 {
                push(&mut cands, 0, k, vec![x]);
            } else {
                push(&mut cands, n - 1 - k, n - 1, vec![x]);
            }
        }
    }
    cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    for (_, i, j, link) in cands.into_iter().take(400) {
        let wmax = r.w[i..j].iter().cloned().fold(0.0, f64::max);
        let wmin = r.w[i..j].iter().cloned().fold(f64::INFINITY, f64::min);
        if try_replace(r, cx, (i, j), &link, Widths { wmax, wmin }, base, relax) {
            return true;
        }
    }
    false
}

/// a run that still has a short inner segment or a sharp detour after the strict pass
fn needs_relax(r: &Run) -> bool {
    let n = r.pts.len();
    (1..n.saturating_sub(2)).any(|k| (r.pts[k + 1] - r.pts[k]).len() < 0.3)
}

/// miter 90-degree corners to two 45-degree bends (chamfer legs 1.0 / 0.75 / 0.5 mm, at least 2x
/// the track width) wherever the diagonal is exactly legal; stub segments keep their exit length
/// and no leg is left shorter than 0.3 mm
fn miter(r: &mut Run, cx: &Ctx, st: &mut CleanStats) {
    let mut shifted: Vec<usize> = Vec::new();
    let mut k = 1;
    while k + 1 < r.pts.len() {
        let n = r.pts.len();
        let (a, v, b) = (r.pts[k - 1], r.pts[k], r.pts[k + 1]);
        let (d1, d2) = (v - a, b - v);
        let t = turn(d1, d2);
        if (t - 90.0).abs() > 0.5 {
            k += 1;
            continue;
        }
        let (l1, l2) = (d1.len(), d2.len());
        let (u1, u2) = (unit(d1), unit(d2));
        // reserve on each leg: stub length, or room for the neighbour corner's chamfer
        let res1 = if k - 1 == 0 {
            match r.head {
                End::Stub(..) => used_alt(r, 0).map(|x| x.ml).unwrap_or(l1),
                _ => 0.0,
            }
        } else {
            0.0 // the previous corner is already done
        };
        let res2 = if k + 1 == n - 1 {
            match r.tail {
                End::Stub(..) => used_alt(r, 1).map(|x| x.ml).unwrap_or(l2),
                _ => 0.0,
            }
        } else {
            0.0
        };
        let wmax = r.w[k - 1].max(r.w[k]);
        let wmin = r.w[k - 1].min(r.w[k]);
        let mut done = false;
        let mut shift_err = 0u8;
        let mut why = String::new();
        let cmax = (l1 - res1).min(l2 - res2).min(1.5);
        for c in [cmax, 1.0, 0.75, 0.5] {
            if c < 2.0 * wmax - 1e-9 || c > cmax + 1e-9 {
                continue;
            }
            let left1 = l1 - c - res1;
            let left2 = l2 - c - res2;
            if left1 < -1e-9 || left2 < -1e-9 {
                why.push_str(&format!("c{} res; ", c));
                continue;
            }
            let (s1, s2) = (l1 - c, l2 - c);
            let sl = |x: f64, end: bool| x.abs() < 1e-6 || x >= 0.3 || (end && x >= 0.0);
            if !sl(s1, k - 1 == 0 && !matches!(r.head, End::Stub(..)))
                || !sl(s2, k + 1 == n - 1 && !matches!(r.tail, End::Stub(..)))
            {
                why.push_str(&format!("c{} sliver; ", c));
                continue;
            }
            why.push_str(&format!("c{} legal; ", c));
            let (p1, p2) = (v - u1 * c, v + u2 * c);
            for w in [wmax, wmin] {
                if cx.seg_ok(p1, p2, w, r.l) {
                    let mut pts = r.pts[..k].to_vec();
                    pts.push(p1);
                    pts.push(p2);
                    pts.extend_from_slice(&r.pts[k + 1..]);
                    // a fully consumed leg must not leave a sharp turn at its far end
                    let mut dd = pts.clone();
                    dd.dedup_by(|x, y| (*x - *y).len() < 1e-7);
                    let ok = (1..dd.len().saturating_sub(1))
                        .all(|q| turn(dd[q] - dd[q - 1], dd[q + 1] - dd[q]) <= 90.5);
                    if !ok {
                        break;
                    }
                    let mut ws = r.w[..k].to_vec();
                    ws.push(w);
                    ws.extend_from_slice(&r.w[k..]);
                    r.pts = pts;
                    r.w = ws;
                    r.merge();
                    st.mitered += 1;
                    done = true;
                    break;
                }
            }
            if done {
                break;
            }
        }
        // corner right at a stub end (stub at its exit length): shift the next segment outward along
        // the stub so the stub keeps its length and the corner gets room (a U between two pads
        // grows on both sides), then retry this corner
        if !done && k == 1 && matches!(r.head, End::Stub(..)) && !shifted.contains(&k) {
            shifted.push(k);
            match shift_out(r, cx, k, 0.5f64.max(2.0 * wmax)) {
                Ok(()) => {
                    st.shifted += 1;
                    continue;
                }
                Err(e) => shift_err = e,
            }
        }
        if !done {
            st.miter_failed += 1;
            // why the corner stays 90 degrees
            let at_stub = (k == 1 && matches!(r.head, End::Stub(..)))
                || (k + 2 == n && matches!(r.tail, End::Stub(..)));
            let cat = if why.contains("legal") {
                0 // chamfer room ok, diagonal violates clearance
            } else if at_stub && n == 3 {
                1 // L between two pad stubs (corner = crossing of both exit axes)
            } else if at_stub && matches!(shift_err, 5 | 8) {
                2 // stub at exit length, outward shift blocked by clearance
            } else if at_stub {
                3 // stub at exit length, neighbour leg too short for a shift (Z/U between close pads)
            } else {
                4 // interior legs shorter than the 0.5 mm chamfer
            };
            st.kept90[cat] += 1;
            if std::env::var("PR_DEBUG").is_ok() {
                eprintln!("[miter] {} fail at {:?} l1 {:.3} l2 {:.3} res1 {:.3} res2 {:.3} k {} n {} head {:?} tail {:?}", why, v, l1, l2, res1, res2, k, n, r.head, r.tail);
            }
        }
        k += if done { 2 } else { 1 };
    }
}

/// shift segment k (pts[k] -> pts[k+1]) along the incoming direction u1 so that both its corners
/// can be mitered with chamfer c: leg k-1 grows, leg k+1 changes along its own direction (must be
/// parallel to u1: U or Z shape). Exactly checked.
fn shift_out(r: &mut Run, cx: &Ctx, k: usize, c: f64) -> Result<(), u8> {
    let n = r.pts.len();
    if k + 2 > n || k + 1 >= n {
        return Err(1);
    }
    let (a, v, b) = (r.pts[k - 1], r.pts[k], r.pts[k + 1]);
    let u1 = unit(v - a);
    let ml1 = used_alt(r, 0).map(|x| x.ml).unwrap_or((v - a).len());
    let mut delta = c - ((v - a).len() - ml1);
    if delta <= 1e-9 {
        return Err(2);
    }
    let last = k + 1 == n - 1;
    if last {
        return Err(3);
    }
    let q = r.pts[k + 2];
    let d3 = q - b;
    if !parallel(d3, u1) {
        // next leg at an angle: the shifted segment ends where it meets that leg's line
        delta = (delta / 0.025).ceil() * 0.025;
        let v2 = v + u1 * delta;
        let Some(b2) = intersect(v2, b - v, q, d3) else {
            return Err(4);
        };
        if (q - b2).dot(d3) < 0.3 || (b2 - v2).dot(b - v) < 0.3 {
            return Err(4);
        }
        let tail_stub = k + 2 == n - 1 && matches!(r.tail, End::Stub(..));
        if !cx.seg_ok2(a, v2, r.w[k - 1], r.l, k - 1 == 0)
            || !cx.seg_ok(v2, b2, r.w[k], r.l)
            || !cx.seg_ok2(b2, q, r.w[k + 1], r.l, tail_stub)
        {
            return Err(5);
        }
        if turn(v2 - a, b2 - v2) > 90.5 || turn(b2 - v2, q - b2) > 90.5 {
            return Err(6);
        }
        r.pts[k] = v2;
        r.pts[k + 1] = b2;
        return Ok(());
    }
    let s3 = unit(d3).dot(u1); // +1: Z (next leg shrinks), -1: U (next leg grows)
    let tail_stub = k + 2 == n - 1 && matches!(r.tail, End::Stub(..));
    if s3 < 0.0 && tail_stub {
        let ml3 = used_alt(r, 1).map(|x| x.ml).unwrap_or(d3.len());
        delta = delta.max(c - (d3.len() - ml3));
    }
    delta = (delta / 0.025).ceil() * 0.025;
    let l3 = d3.len() - s3 * delta;
    let need3 = if tail_stub {
        used_alt(r, 1).map(|x| x.ml).unwrap_or(d3.len())
    } else {
        0.3
    };
    if l3 < need3 - 1e-9 {
        return Err(7);
    }
    let (v2, b2) = (v + u1 * delta, b + u1 * delta);
    let w = r.w[k];
    if !cx.seg_ok2(a, v2, r.w[k - 1], r.l, k - 1 == 0)
        || !cx.seg_ok(v2, b2, w, r.l)
        || !cx.seg_ok2(b2, q, r.w[k + 1], r.l, tail_stub)
    {
        return Err(8);
    }
    r.pts[k] = v2;
    r.pts[k + 1] = b2;
    Ok(())
}

/// a run whose stub head continues straight (parallel to the stub axis) to its tail, slightly off
/// the axis: the axis point nearest to the tail
pub fn straight_stub_target(r: &Run) -> Option<P> {
    let End::Stub(u, _) = r.head else { return None };
    let n = r.pts.len();
    if n < 2 {
        return None;
    }
    let c = r.pts[0];
    if (1..n - 1).any(|k| !parallel(r.pts[k + 1] - r.pts[k], u)) {
        return None;
    }
    let v = r.pts[n - 1];
    let x = c + u * (v - c).dot(u);
    if (x - v).len() < 1e-7 || (x - v).len() > 0.06 {
        return None;
    }
    Some(x)
}

/// move a via (end of ra, start of rb) to v2: ra becomes a straight stub run, rb's first segment
/// keeps its direction (its next vertex slides along the following segment)
pub fn via_slide(ra: &mut Run, rb: &mut Run, v2: P, cx: &Ctx, via_ok: &dyn Fn(P) -> bool) -> bool {
    if !via_ok(v2) {
        return false;
    }
    let c = ra.pts[0];
    let na = vec![c, v2];
    let wa = vec![ra.w[0]];
    let v = rb.pts[0];
    let mut nb = rb.pts.clone();
    nb[0] = v2;
    if nb.len() >= 2 {
        let d = rb.pts[1] - v;
        if !parallel(d, v2 - v) || (v2 - v).len() < 1e-9 {
            if nb.len() < 3 {
                return false;
            }
            let Some(x) = intersect(v2, d, rb.pts[1], rb.pts[2] - rb.pts[1]) else {
                return false;
            };
            if (x - rb.pts[1]).len() > 0.12
                || (x - v2).dot(d) <= 1e-6
                || (rb.pts[2] - x).dot(rb.pts[2] - rb.pts[1]) <= 1e-6
            {
                return false;
            }
            nb[1] = x;
        }
    }
    if end_pen(&nb, rb, None).is_none() {
        return false;
    }
    if !cx.seg_ok2(c, v2, wa[0], ra.l, true) {
        return false;
    }
    let nseg = if nb.len() >= 3 { 2 } else { nb.len() - 1 };
    for q in 0..nseg {
        let last_stub = q + 2 == nb.len() && matches!(rb.tail, End::Stub(..));
        if !cx.seg_ok2(nb[q], nb[q + 1], rb.w[q], rb.l, last_stub) {
            return false;
        }
    }
    ra.pts = na;
    ra.w = wa;
    rb.pts = nb;
    true
}

pub fn clean_run(r: &mut Run, cx: &Ctx, st: &mut CleanStats) {
    let dbg = std::env::var("PR_DEBUG_AT").ok().and_then(|v| {
        let f: Vec<f64> = v.split(',').filter_map(|x| x.parse().ok()).collect();
        (f.len() == 2).then(|| p(f[0], f[1]))
    });
    let dbg = dbg.is_some_and(|q| r.pts.iter().any(|x| (*x - q).len() < 1.5));
    let show = |tag: &str, r: &Run| {
        if dbg {
            eprintln!(
                "[clean {}] {:?} head {:?} tail {:?}",
                tag,
                r.pts.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>(),
                r.head,
                r.tail
            );
        }
    };
    show("in", r);
    r.merge();
    align_junction_head(r, cx);
    {
        let mut rv = r.reversed();
        align_junction_head(&mut rv, cx);
        *r = rv.reversed();
    }
    align_stub_head(r, cx, st);
    {
        let mut rv = r.reversed();
        align_stub_head(&mut rv, cx, st);
        *r = rv.reversed();
    }
    r.merge();
    show("aligned", r);
    pull_tight(r, cx, st, None);
    r.merge();
    show("pulled", r);
    if needs_relax(r) && (matches!(r.head, End::Stub(..)) || matches!(r.tail, End::Stub(..))) {
        pull_tight(r, cx, st, Some(1.0));
        r.merge();
    }
    if r.pts.len() >= 3 {
        // forward, then reversed (corners at the tail stub), then forward once more
        let f0 = st.miter_failed;
        let kept_before = st.kept90;
        miter(r, cx, st);
        let mut rv = r.reversed();
        let f1 = st.miter_failed;
        miter(&mut rv, cx, st);
        *r = rv.reversed();
        let f2 = st.miter_failed;
        let k0 = st.kept90;
        st.kept90 = kept_before;
        let _ = k0;
        miter(r, cx, st);
        // count each kept 90-degree corner once (the last pass)
        st.miter_failed = f0 + (st.miter_failed - f2);
        let _ = f1;
    }
    show("out", r);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octilinear_directions() {
        assert!(octi(p(1.0, 0.0)));
        assert!(octi(p(0.0, -3.0)));
        assert!(octi(p(2.0, -2.0)));
        assert!(!octi(p(2.0, 1.0)));
        assert!((turn(p(1.0, 0.0), p(1.0, 1.0)) - 45.0).abs() < 1e-9);
        assert!((turn(p(1.0, 0.0), p(0.0, 1.0)) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn links_are_octilinear_connected_and_never_turn_more_than_90() {
        let a = p(0.0, 0.0);
        for b in [
            p(3.0, 1.0),
            p(-2.0, 5.0),
            p(0.4, -0.1),
            p(-7.0, -7.0),
            p(4.0, 0.0),
        ] {
            let links = octi_links(a, b);
            assert!(!links.is_empty());
            for l in links {
                assert!((l[0] - a).len() < 1e-12 && (l[l.len() - 1] - b).len() < 1e-9);
                for w in l.windows(2) {
                    assert!(
                        octi(w[1] - w[0]),
                        "segment {:?} -> {:?} is not a 45-degree multiple",
                        w[0],
                        w[1]
                    );
                }
                for w in l.windows(3) {
                    assert!(turn(w[1] - w[0], w[2] - w[1]) <= 90.0 + 1e-9);
                }
            }
        }
        assert!(octi_links(a, a).is_empty());
    }

    #[test]
    fn jog_makes_near_axis_segments_octilinear() {
        let (a, b) = (p(0.0, 0.0), p(1.6, 0.015));
        let (ea, eb) = octi_jog(a, b).unwrap();
        for (x, y) in [(a, ea), (ea, b), (a, eb), (eb, b)] {
            assert!(octi(y - x));
        }
        assert!((ea - p(1.6, 0.0)).len() < 1e-12);
        assert!(octi_jog(a, p(2.0, 0.0)).is_none());
        assert!(octi_jog(a, p(2.0, 1.0)).is_none()); // clearly diagonal: not a rounding artefact
        let (ea, _) = octi_jog(a, p(1.0, 1.02)).unwrap();
        assert!(octi(ea - a) && octi(p(1.0, 1.02) - ea));
    }

    #[test]
    fn line_intersection_and_parallel() {
        let x = intersect(p(0.0, 0.0), p(1.0, 0.0), p(2.0, -1.0), p(0.0, 1.0)).unwrap();
        assert!((x - p(2.0, 0.0)).len() < 1e-12);
        assert!(intersect(p(0.0, 0.0), p(1.0, 0.0), p(0.0, 1.0), p(2.0, 0.0)).is_none());
        assert!(parallel(p(1.0, 1.0), p(-3.0, -3.0)));
        assert!(!parallel(p(1.0, 0.0), p(1.0, 1.0)));
    }
}
