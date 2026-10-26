//! Length tuning (`tune`): accordion meanders on straight tracks until a net reaches a target
//! length, for buses that need length matching. A meander replaces one track of the net: bumps
//! of amplitude A at a pitch of 2 (width + gap) add 2 A each; the last bump takes the remainder,
//! so the target is met exactly. Every new segment keeps the clearances of [`thermal::legal`]
//! (other nets' copper, holes, rule areas, board edge) and of the meanders already placed.

use crate::board::Track;
use crate::droute::RSeg;
use crate::geom::{shape_dist, Shape, P};
use crate::model::Model;
use crate::thermal;

/// segments that replace track `t` and make it `extra` mm longer, or None when no amplitude up
/// to `amax` fits on either side; `placed`: meanders of other nets placed before (net, segment)
pub fn meander(
    m: &Model,
    t: &Track,
    extra: f64,
    gap: f64,
    amax: f64,
    placed: &[(usize, RSeg)],
) -> Option<Vec<RSeg>> {
    let d = t.b - t.a;
    let len = d.len();
    if len < 1e-6 || extra <= 0.0 {
        return None;
    }
    let u = d * (1.0 / len);
    let nrm = P { x: -u.y, y: u.x };
    let w = t.w;
    let pitch = 2.0 * (w + gap);
    // keep the bends away from the track's ends (its joints and pads)
    let margin = w + gap;
    let usable = len - 2.0 * margin;
    if usable < pitch {
        return None;
    }
    let kmax = (usable / pitch).floor() as usize;
    let amin = (w + gap).min(amax);
    let mut amp = amax;
    while amp >= amin - 1e-9 {
        let k = (extra / (2.0 * amp)).ceil().max(1.0) as usize;
        if k > kmax {
            // smaller bumps need even more room
            return None;
        }
        let last = extra / 2.0 - amp * (k - 1) as f64;
        for side in [1.0, -1.0] {
            let mut pts = vec![t.a];
            let mut cur = t.a + u * (margin + (usable - k as f64 * pitch) / 2.0);
            pts.push(cur);
            for i in 0..k {
                let a = if i + 1 == k { last } else { amp };
                let up = nrm * (side * a);
                let p1 = cur + up;
                let p2 = p1 + u * (pitch / 2.0);
                let p3 = p2 - up;
                let p4 = p3 + u * (pitch / 2.0);
                pts.extend([p1, p2, p3, p4]);
                cur = p4;
            }
            pts.push(t.b);
            let segs: Vec<RSeg> = pts
                .windows(2)
                .filter(|q| (q[1] - q[0]).len() > 1e-6)
                .map(|q| RSeg {
                    a: q[0],
                    b: q[1],
                    l: t.layer,
                    w,
                })
                .collect();
            let ok = segs.iter().all(|s| {
                let sh = Shape::Capsule {
                    a: s.a,
                    b: s.b,
                    r: w / 2.0,
                };
                thermal::legal(m, t.net, t.layer, &sh)
                    && placed.iter().all(|(n, p)| {
                        p.l != s.l
                            || *n == t.net
                            || shape_dist(
                                &Shape::Capsule {
                                    a: p.a,
                                    b: p.b,
                                    r: p.w / 2.0,
                                },
                                &sh,
                            ) >= m.r.clr(t.net, *n, crate::board::is_outer(s.l)) + 0.01
                    })
            });
            if ok {
                return Some(segs);
            }
        }
        amp -= 0.1;
    }
    None
}
