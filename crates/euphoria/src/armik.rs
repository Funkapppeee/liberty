//! Two-bone arm IK (shoulder 3-DOF + elbow hinge), as IV's behaviours call it. Spec:
//! `specs/euphoria/armIK.md`. Works in world space (Z up) on the joints' current world frames; its
//! results are joint targets: shoulder (lean1, lean2, twist) and elbow.

use crate::math::{Quat, Vec3};

/// A joint's world frame: three orientation rows and a position.
#[derive(Clone, Copy, Debug, Default)]
pub struct Frame {
    pub rows: [Vec3; 3],
    pub pos: Vec3,
}

impl Frame {
    /// A world vector in this frame's coordinates.
    fn local(&self, v: Vec3) -> Vec3 {
        Vec3::new(self.rows[0].dot(v), self.rows[1].dot(v), self.rows[2].dot(v))
    }
    /// Coordinates in this frame → the parent frame the rows are expressed in.
    fn out_of(&self, v: Vec3) -> Vec3 {
        self.rows[0] * v.x + self.rows[1] * v.y + self.rows[2] * v.z
    }
}

/// The shoulder as the solver sees it. Angle triples are (lean1, lean2, twist).
#[derive(Clone, Copy, Debug, Default)]
pub struct Shoulder {
    pub frame: Frame,
    /// The shoulder muscle, as setStiffness(s, d) leaves it: strength s², damping 2·s·d.
    pub muscle_strength: f32,
    pub muscle_damping: f32,
    pub half_range: [f32; 3],
    pub centre: [f32; 3],
    /// Current targets (what the blend mixes with) and the reach limiter's blend source.
    pub prev: [f32; 3],
    pub limiter_prev: [f32; 3],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Elbow {
    pub frame: Frame,
    pub prev: f32,
    pub limiter_prev: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub target: Vec3,
    /// The limb's base matrix: the elbow's frame in the shoulder's (rows + translation; the
    /// translation's length is the upper arm).
    pub limb: Frame,
    pub hand: Vec3,
    /// Velocity and angular velocity of the part above the shoulder (the clavicle; the lead uses them).
    pub base_vel: Vec3,
    pub base_ang_vel: Vec3,
    /// A velocity to subtract from the upper arm's (e.g. the body's), when given.
    pub reference_vel: Option<Vec3>,
    /// Lead time scale (> 0 turns the target lead on).
    pub lead: f32,
    /// Twist about the shoulder→hand line, and the scale on lean2 and twist.
    pub twist: f32,
    pub twist_scale: f32,
    /// Scale on the elbow bend and the cone overflow.
    pub elbow_scale: f32,
    pub blend: f32,
    /// Reach limiter: (max step, factor).
    pub limiter: Option<(f32, f32)>,
    pub no_overflow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Output {
    /// Shoulder targets (lean1, lean2, twist) and the elbow target.
    pub shoulder: [f32; 3],
    pub elbow: f32,
    /// The target after lead and limiter.
    pub target: Vec3,
}


/// Push the target ahead to make up for the shoulder base's own motion, and keep it at least 0.1 m out
/// along the shoulder→target line.
fn lead(r: &Request, s: &Shoulder) -> Vec3 {
    let t0 = r.target;
    let d = t0 - s.frame.pos;
    let c = s.muscle_damping / (s.muscle_strength + 0.1) * r.lead;
    let spin = d.cross(r.base_ang_vel) * c;
    let d1 = d + spin;
    let (l0, l1) = (d.length(), d1.length());
    let inv1 = if d1.dot(d1) == 0.0 { 0.0 } else { 1.0 / l1 };
    let mut v = r.base_vel;
    if let Some(rv) = r.reference_vel {
        v = v - rv;
    }
    let mut t = t0 + d1 * ((l0 - l1) * inv1) + spin - v * c;
    let inv0 = if d.dot(d) != 0.0 { 1.0 / l0 } else { 0.0 };
    let along = (t - s.frame.pos).dot(d * inv0);
    if along < 0.1 {
        t = t + d * ((0.1 - along) * inv0);
    }
    t
}

pub fn solve(r: &Request, s: &Shoulder, e: &Elbow) -> Output {
    // 1. target: lead, then the reach limiter (a step toward it from the hand)
    let mut target = if r.lead > 0.0 { lead(r, s) } else { r.target };
    let mut limiter_w = 0.0;
    if let Some((max_step, factor)) = r.limiter {
        let d = target - r.hand;
        let len2 = d.dot(d);
        let len = len2.sqrt();
        let step = (s.muscle_damping * max_step / s.muscle_strength).min(len) + 1e-4;
        let inv = if len2 == 0.0 { 0.0 } else { 1.0 / len };
        let near = (1.0 - 0.1 / step).max(0.0);
        let g = if near < factor { (factor + near) * 0.5 } else { factor };
        target = r.hand + d * ((1.0 - g) * inv * step);
        limiter_w = 1.0 / (1.0001 - g);
    }
    // 2. the forearm in the shoulder's frame, and the bend the rest pose already has
    let forearm = r.limb.out_of(e.frame.local(r.hand - e.frame.pos));
    let upper = r.limb.pos.length();
    let lf = forearm.length();
    let s_rest = r.limb.rows[2].dot(forearm.cross(r.limb.pos)) / (lf * upper);
    let rest = if s_rest <= -1.0 {
        -std::f32::consts::FRAC_PI_2
    } else if s_rest >= 1.0 {
        std::f32::consts::FRAC_PI_2
    } else {
        s_rest.asin()
    };
    // 3. the target in the shoulder's frame, pulled in to full reach
    let mut want = s.frame.local(target - s.frame.pos);
    let d = want.length();
    if lf + upper < d {
        want = want * ((lf + upper) / d);
    }
    let c = (lf * lf + upper * upper - d * d) / (upper * 2.0 * lf);
    let inner = if c <= -1.0 { std::f32::consts::PI } else if c >= 1.0 { 0.0 } else { c.acos() };
    let mut elbow = (std::f32::consts::PI - inner) * r.elbow_scale + rest;
    // 4. the hand with that elbow, twisted about the shoulder→hand line, swung onto the target
    let bend = Quat::from_scaled_axis(r.limb.rows[2] * elbow);
    let hand = r.limb.pos + bend.rotate(forearm);
    let hand_dir = hand * (if hand.dot(hand) != 0.0 { 1.0 / hand.length() } else { 0.0 });
    let twist_amount = r.twist * r.twist_scale;
    let q = Quat::arc(hand, want).mul(Quat::from_scaled_axis(hand_dir * twist_amount));
    let (twist, mut s1, mut s2) = q.twist_swing();
    // 5. twist limit: re-solve with the excess taken off the twist (the reported twist stays)
    let tmax = s.half_range[2];
    if tmax < twist.abs() {
        let clamped = twist.clamp(-tmax, tmax);
        let q = Quat::arc(hand, want).mul(Quat::from_scaled_axis(hand_dir * (twist_amount - (twist - clamped))));
        let (_, a, b) = q.twist_swing();
        s1 = a;
        s2 = b;
    }
    // 6. outside the swing cone: the overflow bends the elbow more (±0.5 at most)
    let cone = s1 * s1 / (s.half_range[0] * s.half_range[0]) + s2 * s2 / (s.half_range[1] * s.half_range[1]);
    if cone > 1.0 && !r.no_overflow {
        let x = (1.0 - 1.0 / cone.sqrt()) * (s1 * s1 + s2 * s2).sqrt() * r.elbow_scale;
        elbow += x.clamp(-0.5, 0.5);
    }
    // 7. blend with the current targets
    let (k, a) = (r.blend, 1.0 - r.blend);
    let mut shoulder = [
        (s.centre[0] + s1) * k + s.prev[0] * a,
        (s2 * r.twist_scale + s.centre[1]) * k + s.prev[1] * a,
        (twist * r.twist_scale + s.centre[2]) * k + s.prev[2] * a,
    ];
    let mut elbow_out = e.prev * a + k * elbow;
    if r.limiter.is_some() {
        let b = 1.0 - limiter_w;
        for i in 0..3 {
            shoulder[i] = s.limiter_prev[i] * b + shoulder[i] * limiter_w;
        }
        elbow_out = e.limiter_prev * b + elbow_out * limiter_w;
    }
    Output { shoulder, elbow: elbow_out, target }
}
