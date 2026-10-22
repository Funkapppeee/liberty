//! catchFall: arms out to break a fall, legs crouched. Spec: `specs/euphoria/catchFall.md`.
//! This module makes catchFall's own decisions (speed gauge, fall direction, stiffness schedule,
//! spine/leg/wrist targets, where each hand should go); the arm IK that reaches for the hand target
//! is the caller's (IV's own arm IK is specced separately).

use crate::math::Vec3;
use crate::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub legs_stiffness: f32,
    pub torso_stiffness: f32,
    pub arms_stiffness: f32,
    pub forward_max_arm_offset: f32,
    pub backwards_min_arm_offset: f32,
    pub use_head_look: bool,
}

impl Default for Params {
    fn default() -> Self {
        Params { legs_stiffness: 6.0, torso_stiffness: 9.0, arms_stiffness: 15.0, forward_max_arm_offset: 0.35, backwards_min_arm_offset: -0.25, use_head_look: true }
    }
}

const MAX_REACH: f32 = 0.6;
const MIN_REACH: f32 = 0.4;
const PROBE_LENGTH: f32 = 0.7;
const REACH_DECAY: f32 = 0.5;
const GRAVITY_LEAD: f32 = 4.9;
const SPEED_GAIN: f32 = 1.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Arm {
    /// Start draws: a (unused by the parts specced so far), b, and the elbow cap.
    pub a: f32,
    pub b: f32,
    pub elbow_cap: f32,
    /// Reach amount r.
    pub reach: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct CatchFall {
    pub params: Params,
    /// Speed gauge g.
    pub gauge: f32,
    /// Leg bend per side (left, right).
    pub m: [f32; 2],
    /// Spine lean2 (s).
    pub side_lean: f32,
    pub arms: [Arm; 2],
}

/// What the character tells catchFall each frame (world space, Z up).
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub fwd: Vec3,
    pub face: Vec3,
    pub up: Vec3,
    pub com_vel: Vec3,
    pub chest_vel: Vec3,
    /// Body kinetic energy per kg.
    pub ke: f32,
    pub shoulder: [Vec3; 2],
    pub hand: [Vec3; 2],
    pub touching: [bool; 2],
    /// Velocity of the surface under each hand (0 on the ground).
    pub surface_vel: [Vec3; 2],
    pub dt: f32,
}

/// A body-mask stiffness request (damping ratio `damping`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stiffness {
    pub mask: &'static str,
    pub stiffness: f32,
    pub damping: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Output {
    pub fall_dir: Vec3,
    pub back: f32,
    pub side: f32,
    pub lean: f32,
    /// Spine joints 0..3: (lean1, lean2).
    pub spine: [(f32, f32); 4],
    /// Per side (left, right): hip lean1, knee, ankle lean1.
    pub hip: [f32; 2],
    pub knee: [f32; 2],
    pub ankle: [f32; 2],
    pub stiffness: Vec<Stiffness>,
    /// Wrist (stiffness, damping) and ankle (stiffness, damping).
    pub wrist_stiffness: [(f32, f32); 2],
    pub ankle_stiffness: (f32, f32),
    pub probe: [(Vec3, Vec3); 2],
    pub hand_target: [Vec3; 2],
    pub elbow_cap: [Option<f32>; 2],
    /// Wrist (lean1, lean2, twist) when the probe found nothing; with a hit IV aims the wrist at the
    /// surface instead (its wrist IK, not ported).
    pub wrist: [Option<[f32; 3]>; 2],
    /// The two "catchFall" feedback events IV sends the game this frame (feedback interface slots
    /// 0x0C and 0x10; what the game does with them is OPEN).
    pub feedback_0c: bool,
    pub feedback_10: bool,
}

impl CatchFall {
    /// Start: 9 draws (m left, m right, s, then per arm a, b, elbow cap).
    pub fn start(params: Params, rng: &mut Rng) -> CatchFall {
        let ml = (rng.uniform() + 1.0) * 0.5;
        let mr = (rng.uniform() + 1.0) * 0.5;
        let s = rng.uniform() * 0.2 - 0.1;
        let mut arm = || {
            let a = rng.uniform() * 0.4 - 0.4;
            let b = rng.uniform() * 0.5;
            let elbow_cap = rng.uniform() * 0.6 + 0.7;
            Arm { a, b, elbow_cap, reach: 1.0 }
        };
        let left = arm();
        let right = arm();
        CatchFall { params, gauge: 1.0, m: [ml, mr], side_lean: s, arms: [left, right] }
    }

    /// Leg targets at start: hip m, knee −2m, ankle −m.
    pub fn start_legs(&self) -> ([f32; 2], [f32; 2], [f32; 2]) {
        (self.m, self.m.map(|m| m * -2.0), self.m.map(|m| -m))
    }

    /// One frame. `probe(from, to)` returns the ground hit point along the segment, if any.
    pub fn step(&mut self, input: &Input, mut probe: impl FnMut(Vec3, Vec3) -> Option<Vec3>) -> Output {
        let p = self.params;
        let dt = input.dt;
        let mut out = Output::default();
        self.gauge = (self.gauge + ((2.0 * input.ke).sqrt() * 2.0 - self.gauge) * 5.0 * dt).clamp(0.2, 1.0);
        let r = [self.arms[0].reach.max(0.3), self.arms[1].reach.max(0.3)];
        let g = self.gauge;
        let low = r[0].min(r[1]);
        out.feedback_10 = low == 0.3;
        out.feedback_0c = low < 0.9 && g < 1.0;
        out.stiffness.push(Stiffness { mask: "us", stiffness: p.torso_stiffness * g, damping: 0.5 });
        if p.use_head_look {
            out.stiffness.push(Stiffness { mask: "un", stiffness: p.torso_stiffness * g, damping: 0.5 });
        }
        out.stiffness.push(Stiffness { mask: "ul", stiffness: r[0] * p.arms_stiffness, damping: 0.5 });
        out.stiffness.push(Stiffness { mask: "ur", stiffness: r[1] * p.arms_stiffness, damping: 0.5 });
        out.stiffness.push(Stiffness { mask: "lb", stiffness: p.legs_stiffness * g, damping: 0.5 });
        out.wrist_stiffness = r.map(|ri| ((p.arms_stiffness + 13.0) * 0.5 * ri, 1.0));
        out.ankle_stiffness = (10.0, 1.0);

        // fall direction: chest velocity relative to the surfaces under the hands, led by gravity
        let avg = (input.surface_vel[0] + input.surface_vel[1]) * 0.5;
        let v = input.chest_vel - avg;
        let up = input.up;
        let f = (v - up * (GRAVITY_LEAD * SPEED_GAIN)).normalize_or_zero();
        let back = -input.face.dot(f);
        let side = input.fwd.dot(up).clamp(0.0, 1.0);
        let push = (input.face.dot(input.com_vel - avg) * 2.0).clamp(0.0, 1.0);
        let w = (side * push - back * 0.5).clamp(0.0, 1.0);
        out.fall_dir = f;
        out.back = back;
        out.side = side;
        out.lean = w;
        // the speed gauge scales the spine lean, hips and knees (not the ankles): slower falls crouch
        // and lean less
        out.spine = [(0.2 * w * g, self.side_lean); 4];
        for k in 0..2 {
            out.hip[k] = (self.m[k] + w) * g;
            out.knee[k] = self.m[k] * -2.0 * g;
            out.ankle[k] = -self.m[k];
        }

        // arms
        let fwd = input.fwd;
        let mut d = fwd.cross(up);
        if d.dot(d) > 1e-10 {
            d = d.normalize_or_zero();
        }
        let n = if back > 0.0 { (fwd - d.cross(fwd)).normalize_or_zero() * (0.4 + 0.6 * side) } else { fwd };
        let t = back.max(p.backwards_min_arm_offset).min(p.forward_max_arm_offset);
        for k in 0..2 {
            let sh = input.shoulder[k];
            let p0 = sh + n * t;
            let p1 = p0 + f * PROBE_LENGTH;
            out.probe[k] = (p0, p1);
            let hit = probe(p0, p1);
            let arm = &mut self.arms[k];
            arm.reach = if input.touching[k] {
                (arm.reach - REACH_DECAY * dt).max(g)
            } else {
                (g * 3.0).min(1.0).min(arm.reach + 2.0 * dt)
            };
            out.hand_target[k] = if input.touching[k] {
                // a hand already down stays where it is, pulled halfway to the probe's hit if any;
                // the reach limits don't apply
                hit.map_or(input.hand[k], |h| (h + input.hand[k]) * 0.5)
            } else {
                let mut dd = hit.unwrap_or(p1) - sh;
                let len = dd.length();
                if len > MAX_REACH {
                    dd = dd * (MAX_REACH / len);
                } else if len < MIN_REACH {
                    dd = dd * (MIN_REACH / (len + 1e-4));
                }
                sh + dd
            };
            out.elbow_cap[k] = (back < -0.3).then_some(arm.elbow_cap);
            out.wrist[k] = hit.is_none().then_some([0.3, -0.5, 1.0]);
            // a hand down while falling backward: upper spine arches back
            if input.touching[k] && back > 0.0 {
                out.spine[2].0 = -1.0;
                out.spine[3].0 = -1.0;
            }
        }
        out
    }
}
