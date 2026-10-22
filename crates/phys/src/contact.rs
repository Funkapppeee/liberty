//! Contact generation for an articulated body: each collider is sampled as spheres (a capsule as a
//! row of spheres along its axis, a box as its corners) and tested against static triangles and
//! moving boxes (cars).

use glam::DVec3;

use crate::artic::{Body, Contact, Pose};

#[derive(Clone, Copy, Debug)]
pub enum Shape {
    /// Capsule along the collider's local Z.
    Capsule { radius: f64, half_len: f64 },
    Box { half: DVec3 },
    Sphere { radius: f64 },
}

#[derive(Clone, Copy, Debug)]
pub struct Collider {
    pub link: usize,
    /// The collider in its link's frame.
    pub local: Pose,
    pub shape: Shape,
    pub friction: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Tri {
    pub a: DVec3,
    pub b: DVec3,
    pub c: DVec3,
    lo: DVec3,
    hi: DVec3,
    n: DVec3,
}

impl Tri {
    pub fn new(a: DVec3, b: DVec3, c: DVec3) -> Tri {
        Tri { a, b, c, lo: a.min(b).min(c), hi: a.max(b).max(c), n: (b - a).cross(c - a).normalize_or_zero() }
    }
}

/// The first point where the segment `from`→`to` crosses a triangle (either side), with its normal.
pub fn segment_hit(tris: &[Tri], from: DVec3, to: DVec3) -> Option<(DVec3, DVec3)> {
    let d = to - from;
    let (lo, hi) = (from.min(to), from.max(to));
    let mut best: Option<(f64, DVec3)> = None;
    for t in tris {
        if t.hi.cmplt(lo).any() || t.lo.cmpgt(hi).any() {
            continue;
        }
        // Möller–Trumbore
        let (e1, e2) = (t.b - t.a, t.c - t.a);
        let p = d.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-12 {
            continue;
        }
        let s = from - t.a;
        let u = s.dot(p) / det;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = d.dot(q) / det;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let k = e2.dot(q) / det;
        if (0.0..=1.0).contains(&k) && best.is_none_or(|(b, _)| k < b) {
            best = Some((k, t.n));
        }
    }
    best.map(|(k, n)| (from + d * k, if n.dot(d) > 0.0 { -n } else { n }))
}

/// A moving box (a car), kinematic: it pushes the body, the body can't push it.
#[derive(Clone, Copy, Debug)]
pub struct MovingBox {
    pub id: u64,
    pub pose: Pose,
    pub lo: DVec3,
    pub hi: DVec3,
    pub vel: DVec3,
    pub friction: f64,
}

/// A generated contact and what it touched.
#[derive(Clone, Copy, Debug)]
pub struct Touch {
    pub contact: Contact,
    /// Some(id) when it's a moving box.
    pub other: Option<u64>,
}

/// How far below a face a sphere centre can be and still be pushed back out through it.
const SINK_REACH: f64 = 0.25;
/// Box corners get this radius, so resting on a face is a small overlap rather than a razor edge.
const CORNER_RADIUS: f64 = 0.02;

fn spheres(c: &Collider, link_pose: &Pose) -> Vec<(DVec3, f64)> {
    let w = link_pose.mul(&c.local);
    match c.shape {
        Shape::Sphere { radius } => vec![(w.pos, radius)],
        Shape::Capsule { radius, half_len } => {
            let n = ((2.0 * half_len / radius).ceil() as usize + 1).max(2);
            (0..n)
                .map(|k| {
                    let t = -half_len + 2.0 * half_len * k as f64 / (n - 1) as f64;
                    (w.apply(DVec3::new(0.0, 0.0, t)), radius)
                })
                .collect()
        }
        Shape::Box { half } => {
            let mut v = Vec::with_capacity(8);
            for sx in [-1.0, 1.0] {
                for sy in [-1.0, 1.0] {
                    for sz in [-1.0, 1.0] {
                        let r = (half.abs() - DVec3::splat(CORNER_RADIUS)).max(DVec3::ZERO);
                        v.push((w.apply(DVec3::new(sx * r.x, sy * r.y, sz * r.z)), CORNER_RADIUS));
                    }
                }
            }
            v
        }
    }
}

/// Closest point on triangle abc to p, and whether it lies inside the face (not on an edge/vertex).
fn closest_on_tri(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> (DVec3, bool) {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return (a, false);
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return (b, false);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return (a + ab * (d1 / (d1 - d3)), false);
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return (c, false);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return (a + ac * (d2 / (d2 - d6)), false);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return (b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))), false);
    }
    let denom = 1.0 / (va + vb + vc);
    (a + ab * (vb * denom) + ac * (vc * denom), true)
}

/// All contacts between the body's colliders and the world triangles / moving boxes. `up` orients
/// ground faces (a sphere that sank through a floor is pushed up, not down).
pub fn generate(body: &Body, colliders: &[Collider], tris: &[Tri], boxes: &[MovingBox], up: DVec3) -> Vec<Touch> {
    let mut out = Vec::new();
    for c in colliders {
        let lp = body.pose[c.link];
        for (centre, r) in spheres(c, &lp) {
            let mut mine: Vec<Touch> = Vec::new();
            // look a little past the sphere so one that already sank through a face is still found
            let lo = centre - DVec3::splat(r + SINK_REACH);
            let hi = centre + DVec3::splat(r + SINK_REACH);
            for t in tris {
                if t.hi.x < lo.x || t.lo.x > hi.x || t.hi.y < lo.y || t.lo.y > hi.y || t.hi.z < lo.z || t.lo.z > hi.z {
                    continue;
                }
                let (q, inside) = closest_on_tri(centre, t.a, t.b, t.c);
                let d = centre - q;
                let dist = d.length();
                // the face normal, turned to face "out": up for floors, toward the link for walls
                let mut fn_ = t.n;
                if fn_.dot(up).abs() > 0.5 {
                    if fn_.dot(up) < 0.0 {
                        fn_ = -fn_;
                    }
                } else if fn_.dot(body.com[c.link] - q) < 0.0 {
                    fn_ = -fn_;
                }
                let (normal, depth) = if inside && d.dot(fn_) < 0.0 && dist < SINK_REACH {
                    (fn_, r + dist) // centre went through the face
                } else if dist < r && dist > 1e-9 {
                    (d / dist, r - dist)
                } else if dist <= 1e-9 {
                    (fn_, r)
                } else {
                    continue;
                };
                if depth <= 0.0 {
                    continue;
                }
                push_dedup(&mut mine, Touch { contact: Contact { link: c.link, point: q, normal, depth, other_vel: DVec3::ZERO, friction: c.friction }, other: None });
            }
            for b in boxes {
                let inv = b.pose.inverse();
                let lc = inv.apply(centre);
                let cl = lc.clamp(b.lo, b.hi);
                let d = lc - cl;
                let dist = d.length();
                let (ln, depth, lpt) = if dist > 1e-9 {
                    if dist >= r {
                        continue;
                    }
                    (d / dist, r - dist, cl)
                } else {
                    // inside: out through the nearest face
                    let to_lo = lc - b.lo;
                    let to_hi = b.hi - lc;
                    let mut best = (f64::MAX, DVec3::ZERO, lc);
                    for k in 0..3 {
                        if to_lo[k] < best.0 {
                            let mut n = DVec3::ZERO;
                            n[k] = -1.0;
                            let mut p = lc;
                            p[k] = b.lo[k];
                            best = (to_lo[k], n, p);
                        }
                        if to_hi[k] < best.0 {
                            let mut n = DVec3::ZERO;
                            n[k] = 1.0;
                            let mut p = lc;
                            p[k] = b.hi[k];
                            best = (to_hi[k], n, p);
                        }
                    }
                    (best.1, best.0 + r, best.2)
                };
                let normal = b.pose.rot * ln;
                push_dedup(
                    &mut mine,
                    Touch {
                        contact: Contact {
                            link: c.link,
                            point: b.pose.apply(lpt),
                            normal,
                            depth,
                            other_vel: b.vel,
                            friction: 0.5 * (c.friction + b.friction),
                        },
                        other: Some(b.id),
                    },
                );
            }
            out.extend(mine);
        }
    }
    // keep the solve bounded: the deepest contacts win
    const MAX: usize = 96;
    if out.len() > MAX {
        out.sort_by(|a, b| b.contact.depth.total_cmp(&a.contact.depth));
        out.truncate(MAX);
    }
    out
}

/// One contact per sphere per direction: a sphere on flat ground touching three coplanar triangles
/// makes one contact, not three.
fn push_dedup(v: &mut Vec<Touch>, t: Touch) {
    if let Some(e) = v.iter_mut().find(|e| e.contact.normal.dot(t.contact.normal) > 0.95 && e.other == t.other) {
        if t.contact.depth > e.contact.depth {
            *e = t;
        }
    } else {
        v.push(t);
    }
}
