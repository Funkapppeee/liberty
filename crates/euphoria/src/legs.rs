//! IV's balancer leg solve: from the foot placements and the pelvis targets to joint targets for both
//! legs (hip lean1/lean2/twist, knee, ankle lean1/lean2), the spine's side lean and the pelvis height
//! target. Includes the two-bone leg IK. Spec: `specs/euphoria/balancer.md` ("Leg solve").

use crate::footplace::{Foot as Part, Frame};
use crate::math::{Quat, Vec3};
use std::f32::consts::{FRAC_PI_2, PI};

/// A hip as the leg solve sees it (angle order: lean1, lean2, twist).
#[derive(Clone, Copy, Debug, Default)]
pub struct Hip {
    pub frame: Frame,
    pub strength: f32,
    pub damping: f32,
    pub twist_limit: f32,
    pub half: [f32; 2],
    pub centre: [f32; 2],
    pub centre_twist: f32,
    pub desired: [f32; 3],
    pub current: [f32; 3],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Knee {
    /// Child-side frame (its c axis is the hinge; its position is the knee).
    pub frame: Frame,
    /// Parent-side frame.
    pub parent: Frame,
    pub desired: f32,
    pub current: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Body {
    pub com_vel: Vec3,
    /// The body frame's front axis.
    pub front: Vec3,
    pub pelvis: Frame,
    pub pelvis_vel: Vec3,
    pub pelvis_spin: Vec3,
    pub hips: [Hip; 2],
    pub knees: [Knee; 2],
    /// Ankle muscle (strength, damping).
    pub ankles: [[f32; 2]; 2],
    pub calves: [Part; 2],
    pub feet: [Part; 2],
    /// Each leg's limb frame: the knee's frame in the hip's; its translation is the thigh.
    pub limbs: [Frame; 2],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Tuning {
    pub up: Vec3,
    pub normal: [Vec3; 2],
    /// Subtracted from a swing foot's velocity when predicting it (the ground's velocity).
    pub ground_vel: Vec3,
    pub hip_height: f32,
    pub hip_spread: f32,
    /// Leg straightness (a cosine; the swing ankle adds its arc cosine).
    pub straightness: f32,
    /// How far ahead a swing foot's target is led.
    pub step_predict: f32,
    pub reach: f32,
}

/// What foot placement hands over.
#[derive(Clone, Copy, Debug, Default)]
pub struct Placement {
    pub pos: [Vec3; 2],
    pub mid: Vec3,
    /// Bit 0 / 1: left / right foot down.
    pub flags: u8,
}

/// What pelvis control hands over.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pelvis {
    pub offset: Vec3,
    pub drift: f32,
    pub twist: [f32; 2],
    /// Pelvis height target over the feet (last update's `Output::height`).
    pub height: f32,
    pub lean_side: f32,
    pub lean_front: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Per leg: hip (lean1, lean2, twist), knee, ankle (lean1, lean2).
    pub hip: [[f32; 3]; 2],
    pub knee: [f32; 2],
    pub ankle: [[f32; 2]; 2],
    pub spine_lean: f32,
    pub height: f32,
}

fn horiz(v: Vec3, up: Vec3) -> Vec3 {
    v - up * v.dot(up)
}

fn apply(f: &Frame, p: Vec3) -> Vec3 {
    f.pos + f.a * p.x + f.b * p.y + f.c * p.z
}

fn turn(f: &Frame, v: Vec3) -> Vec3 {
    f.a * v.x + f.b * v.y + f.c * v.z
}

fn local(f: &Frame, v: Vec3) -> Vec3 {
    Vec3::new(f.a.dot(v), f.b.dot(v), f.c.dot(v))
}

fn inverse(f: &Frame) -> Frame {
    Frame {
        a: Vec3::new(f.a.x, f.b.x, f.c.x),
        b: Vec3::new(f.a.y, f.b.y, f.c.y),
        c: Vec3::new(f.a.z, f.b.z, f.c.z),
        pos: -local(f, f.pos),
    }
}

/// `second(first(p))`.
fn then(first: &Frame, second: &Frame) -> Frame {
    Frame { a: turn(second, first.a), b: turn(second, first.b), c: turn(second, first.c), pos: apply(second, first.pos) }
}

fn asin_clamped(x: f32) -> f32 {
    if x <= -1.0 {
        -FRAC_PI_2
    } else if x < 1.0 {
        x.asin()
    } else {
        FRAC_PI_2
    }
}

/// One leg-IK request.
struct Request {
    target: Vec3,
    predict: f32,
    twist: f32,
    mirror: f32,
    knee_sign: f32,
    blend: f32,
    reach_time: f32,
    straight: f32,
    /// 1: subtract the COM velocity when predicting; 2: no knee overflow; 4: pull the target in (swing).
    flags: u8,
}

/// Lead the target by the pelvis motion over the prediction time.
fn predict(r: &Request, hip: &Hip, body: &Body) -> Vec3 {
    let d = r.target - hip.frame.pos;
    let k = hip.strength / (hip.damping + 0.1) * r.predict;
    let c = d.cross(body.pelvis_spin) * k;
    let d2 = d + c;
    let l2 = d2.length();
    let inv = if l2 != 0.0 { 1.0 / l2 } else { 0.0 };
    let mut vel = body.pelvis_vel;
    if r.flags & 1 != 0 {
        vel = vel - body.com_vel;
    }
    let mut out = r.target + (d2 * ((d.length() - l2) * inv) + c) - vel * k;
    let n = d.normalize_or_zero();
    let e = (out - hip.frame.pos).dot(n);
    if e < 0.1 {
        out = out + n * (0.1 - e);
    }
    out
}

/// Two-bone leg IK: returns (hip lean1/lean2/twist, knee).
fn leg_ik(r: &Request, hip: &Hip, knee: &Knee, foot: &Part, limb: &Frame, body: &Body) -> ([f32; 3], f32) {
    let mut t = if r.predict > 0.0 { predict(r, hip, body) } else { r.target };
    let mut w78 = 0.0;
    if r.flags & 4 != 0 {
        let d = t - foot.frame.pos;
        let len = d.length();
        let m = (hip.strength * r.reach_time / hip.damping).min(len) + 1e-4;
        let inv = if len != 0.0 { 1.0 / len } else { 0.0 };
        let a = (1.0 - 0.1 / m).max(0.0);
        let mut s = r.straight;
        if a < s {
            s = (s + a) * 0.5;
        }
        t = foot.frame.pos + d * (inv * m * (1.0 - s));
        w78 = 1.0 / (1.0001 - s);
    }
    // the shin in the hip-limb frame, and its current bend about the knee hinge
    let dk = foot.frame.pos - knee.parent.pos;
    let low = turn(limb, local(&knee.parent, dk));
    let (upper, lower) = (limb.pos.length(), low.length());
    let cur = asin_clamped(limb.c.dot(low.cross(limb.pos)) / (lower * upper));
    // target in the hip frame, clipped to reach; knee bend from the triangle
    let mut tl = local(&hip.frame, t - hip.frame.pos);
    let dist = tl.length();
    if lower + upper < dist {
        tl = tl * ((lower + upper) / dist);
    }
    let c = (lower * lower + upper * upper - dist * dist) / (upper * 2.0 * lower);
    let inner = if c <= -1.0 { PI } else if c < 1.0 { c.acos() } else { 0.0 };
    let mut knee_ang = (PI - inner) * r.knee_sign + cur;
    let foot_h = Quat::from_scaled_axis(limb.c * knee_ang).rotate(low) + limb.pos;
    let tw = r.twist * r.mirror;
    let aim = Quat::arc(foot_h, tl);
    let (twist, mut s1, mut s2) = aim.mul(Quat::from_scaled_axis(foot_h.normalize_or_zero() * tw)).twist_swing();
    if hip.twist_limit < twist.abs() {
        let tc = twist.clamp(-hip.twist_limit, hip.twist_limit);
        // (the swing is redone with the clamped twist; the twist output keeps the unclamped value)
        let (_, a, b) = aim.mul(Quat::from_scaled_axis(foot_h.normalize_or_zero() * (tw - (twist - tc)))).twist_swing();
        s1 = a;
        s2 = b;
    }
    let e = s1 * s1 / (hip.half[0] * hip.half[0]) + s2 * s2 / (hip.half[1] * hip.half[1]);
    if e > 1.0 && r.flags & 2 == 0 {
        let ex = (1.0 - 1.0 / e.sqrt()) * (s1 * s1 + s2 * s2).sqrt() * r.knee_sign;
        knee_ang += ex.clamp(-0.5, 0.5);
    }
    let (w, iw) = (r.blend, 1.0 - r.blend);
    let mut knee_out = knee.desired * iw + w * knee_ang;
    let mut hip_out = [
        (hip.centre[0] + s1) * w + hip.desired[0] * iw,
        (s2 * r.mirror + hip.centre[1]) * w + hip.desired[1] * iw,
        (twist * r.mirror + hip.centre_twist) * w + hip.desired[2] * iw,
    ];
    if r.flags & 4 != 0 {
        let k = 1.0 - w78;
        for (j, h) in hip_out.iter_mut().enumerate() {
            *h = hip.current[j] * k + *h * w78;
        }
        knee_out = knee.current * k + knee_out * w78;
    }
    (hip_out, knee_out)
}

pub fn solve(t: &Tuning, body: &Body, pl: &Placement, pv: &Pelvis) -> Output {
    let up = t.up;
    let [mut l, mut r] = pl.pos;
    let m = pl.mid;
    let (h, spread) = (t.hip_height, t.hip_spread);
    // how far each foot can sit below the hips, for the height target
    let cap = h * h * 0.5625;
    let drop = |p: Vec3| {
        let a = ((horiz(p - m, up).length() - spread) * 1.3).powi(2).min(cap);
        up.dot(p) - (h - (h * h - a).sqrt())
    };
    let low = drop(l).min(drop(r));
    let flags = pl.flags;
    if flags & 1 != 0 {
        l = l + up * (pv.lean_side * spread);
    }
    if flags & 2 != 0 {
        r = r - up * (pv.lean_side * spread);
    }
    let mut spine = pv.lean_side.clamp(-0.2, 0.2);
    let mut height = h * t.straightness - (up.dot(m) - low);
    if !(flags & 1 != 0 && flags & 2 != 0) {
        height += 0.03;
    }
    let mut normal = t.normal;
    if flags & 3 != 0 {
        // where the pelvis should be: heading from the pelvis, leaned by drift and side lean, turned back
        // by 0.1 s of pelvis spin; then retarget the planted feet by how far the pelvis is from it
        let w = body.pelvis.c.cross(up).normalize_or_zero();
        let fw = up.cross(w);
        let (s, c) = (-pv.lean_side).sin_cos();
        let (sp, cp) = (-pv.drift).sin_cos();
        let n = [u_mix(up, w, fw, c * cp, s * cp, -sp), up * -s + w * c, u_mix(up, w, fw, c * sp, s * sp, cp)];
        let q = Quat::from_scaled_axis(body.pelvis_spin * -0.1);
        let pos = m + up * pv.height + pv.offset;
        let d = Frame { a: q.rotate(n[0]), b: q.rotate(n[1]), c: q.rotate(n[2]), pos };
        for (i, p) in [&mut l, &mut r].into_iter().enumerate() {
            if flags & (1 << i) != 0 {
                let hip = &body.hips[i];
                *p = crate::footplace::clamp_reach(*p, up, &d, hip.half[0] * t.reach, hip.half[1] * t.reach);
            }
        }
        // the foot target as seen from the desired pelvis, put back out through the current pelvis
        // (the order is proven on the real game: a balancing pedestrian's stored targets from the
        // session recording come out to 4 decimals this way, and nowhere near the other way)
        let x = then(&inverse(&d), &body.pelvis);
        if flags & 1 != 0 {
            l = apply(&x, l);
            normal[0] = turn(&x, normal[0]);
        }
        if flags & 2 != 0 {
            r = apply(&x, r);
            normal[1] = turn(&x, normal[1]);
        }
    }
    let mut out = Output { spine_lean: 0.0, height, ..Default::default() };
    let targets = [l, r];
    for i in 0..2 {
        let mirror = if i == 0 { 1.0 } else { -1.0 };
        let target = targets[i];
        let req = if flags & (1 << i) != 0 {
            Request { target, predict: 0.0, twist: pv.twist[i], mirror, knee_sign: -1.0, blend: 1.0, reach_time: 5.0, straight: 0.0, flags: 1 }
        } else {
            // (both legs predict the foot with the left foot's velocity)
            let pf = body.feet[i].frame.pos + (body.feet[0].vel - t.ground_vel) * 0.05;
            let st = (up.dot(target - pf) * 4.0 + 0.1).clamp(0.15, 0.6);
            Request { target, predict: t.step_predict, twist: pv.twist[i], mirror, knee_sign: -1.0, blend: 1.0, reach_time: 5.0, straight: st, flags: 5 }
        };
        let hip = &body.hips[i];
        let (hip_out, knee_out) = leg_ik(&req, hip, &body.knees[i], &body.feet[i], &body.limbs[i], body);
        let ankle = if req.flags & 4 != 0 {
            // swing: keep the foot level with the ground ahead
            let calf = &body.calves[i];
            let [st, dm] = body.ankles[i];
            let mut a = calf.frame.c.dot(normal[i]) - calf.ang_vel.dot(calf.frame.a) * (st / dm);
            let side = body.feet[i].frame.a.cross(up).normalize_or_zero();
            a = side.dot(t.normal[i]) * 0.8 + a * 0.2;
            if body.front.dot(body.feet[i].vel) < 0.0 {
                a -= 0.2;
            }
            a + t.straightness.clamp(1e-4, 0.999).acos()
        } else {
            // stance: level the foot under the shin the new hip angles make
            let knee_at = knee_point(hip, hip_out, body.knees[i].frame.pos, mirror);
            let dir = (knee_at - target).normalize_or_zero();
            asin_clamped(body.knees[i].frame.c.dot(dir.cross(normal[i])))
        };
        out.hip[i] = hip_out;
        out.knee[i] = knee_out;
        out.ankle[i] = [ankle, pv.twist[i] * 0.5];
    }
    // the pelvis leans go onto planted ankles; a hip lean past its limit spills to the other hip and spine
    if flags & 1 != 0 {
        out.ankle[0][0] += pv.lean_front + spine;
    }
    if flags & 2 != 0 {
        out.ankle[1][0] += pv.lean_front - spine;
    }
    let excess = |i: usize| {
        let hip = &body.hips[i];
        let e2 = out.hip[i][1] - hip.centre[1];
        let (a, b) = (e2 / hip.half[1], (out.hip[i][0] - hip.centre[0]) / hip.half[0]);
        let mm = (a * a + b * b).sqrt();
        (mm - 1.0).max(0.0) / (mm + 1e-6) * e2
    };
    let (el, er) = (excess(0), excess(1));
    let (a, b) = ((-er).max(el), er.max(-el));
    if a > 0.0 && b <= 0.0 {
        out.hip[1][1] += a;
        if flags & 1 != 0 {
            out.hip[0][1] -= a;
        }
        spine -= a;
    }
    if b > 0.0 && a <= 0.0 {
        out.hip[0][1] += b;
        if flags & 2 != 0 {
            out.hip[1][1] -= b;
        }
        spine -= b;
    }
    out.spine_lean = spine;
    out
}

fn u_mix(up: Vec3, w: Vec3, fw: Vec3, a: f32, b: f32, c: f32) -> Vec3 {
    up * a + w * b + fw * c
}

/// Where the knee goes with the hip at `desired` (thigh length from the current hip→knee distance).
fn knee_point(hip: &Hip, desired: [f32; 3], knee: Vec3, sign: f32) -> Vec3 {
    let d = (hip.frame.pos - knee).length();
    let q = Quat::from_twist_swing(
        (desired[2] - hip.centre_twist) * sign,
        desired[0] - hip.centre[0],
        (desired[1] - hip.centre[1]) * sign,
    );
    hip.frame.pos + turn(&hip.frame, q.rotate(Vec3::new(0.0, 0.0, d)))
}
