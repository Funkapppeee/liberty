//! bodyBalance: what the arms, spine and head do while the balancer keeps him on his feet (spec:
//! specs/euphoria/bodyBalance.md). The legs are the balancer's; this never touches them.
//!
//! Each step: the arms swing against the hips (only while a foot is planted), get thrown out by the
//! body's spin, bend at the elbows after a hand touches something, the spine leans a fixed amount
//! and twists against the hips, and everything is eased toward neutral the slower the body moves.
//! Checked against IV's own routine on 400 random cases (re/vec_bodybal.py, re/fit_bodybal.py).
//!
//! Not ported yet: the head look (useHeadLook) and body turn (useBodyTurn) choices, which also
//! take random draws, and the hand-over when the balancer gives up.

use crate::math::Vec3;
use crate::rng::Rng;

/// A 3-DOF joint's angles: lean1, lean2, twist.
pub type Angles = [f32; 3];

/// The message's settings. `Default` is what the real game sent in 13 of the 14 bodyBalance
/// messages of the 1 Oct session.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub body_stiffness: f32,
    pub elbow: f32,
    pub shoulder: f32,
    pub damping: f32,
    pub spine_stiffness: f32,
    pub spine_damping: f32,
    pub somersault_angle: f32,
    pub somersault_angle_threshold: f32,
    pub side_somersault_angle: f32,
    pub side_somersault_angle_threshold: f32,
    pub ang_vel_multiplier: [f32; 3],
    pub ang_vel_threshold: [f32; 3],
    pub arms_out_on_push: bool,
    pub arms_out_on_push_multiplier: f32,
    pub arms_out_on_push_timeout: f32,
    pub returning_to_balance_arms_out: f32,
    pub arms_out_straighten_elbows: f32,
    pub arms_out_min_lean2: f32,
    pub elbow_angle_on_contact: f32,
    pub bend_elbows_time: f32,
    pub bend_elbows_gait: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            body_stiffness: 9.0,
            elbow: 1.75,
            shoulder: 1.0,
            damping: 0.7,
            spine_stiffness: 10.0,
            spine_damping: 1.0,
            somersault_angle: 1.0,
            somersault_angle_threshold: 0.25,
            side_somersault_angle: 1.0,
            side_somersault_angle_threshold: 0.25,
            ang_vel_multiplier: [4.0, 1.0, 4.0],
            ang_vel_threshold: [1.2, 3.0, 1.2],
            arms_out_on_push: true,
            arms_out_on_push_multiplier: 1.0,
            arms_out_on_push_timeout: 1.1,
            returning_to_balance_arms_out: 0.0,
            arms_out_straighten_elbows: 0.0,
            arms_out_min_lean2: -9.9,
            elbow_angle_on_contact: 1.9,
            bend_elbows_time: 0.3,
            bend_elbows_gait: 0.7,
        }
    }
}

/// What it carries between steps.
#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    /// Counts down from armsOutOnPushTimeout; the hips push the arms out while it runs.
    pub arms_out_timer: f32,
    /// The hips' |lean2| below this doesn't push the arms out (object +0x118; 0 as seen so far).
    pub arms_out_floor: f32,
    /// The body's roll, pitch and heading last step (from spine2's rows).
    pub angles: [f32; 3],
    /// Elbows stay bent this long after a hand touches something.
    pub bend_timer: f32,
    /// A random 0..0.8 drawn at each new balancer step: caps the shoulders' lean2 during a bend
    /// that isn't toward the motion.
    pub elbow_value: f32,
    /// The balancer step count last seen.
    pub last_step: u32,
}

/// Everything one step reads. Left / right are IV's (his own).
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub dt: f32,
    /// spine2's part rows as IV holds them (a up, b left, c back on an upright character), Z up.
    pub spine2_rows: [Vec3; 3],
    pub com_vel: Vec3,
    /// Kinetic energy per kg of the whole body (as catchFall's input).
    pub ke: f32,
    /// Each hip's wanted and current angles.
    pub hip_want: [Angles; 2],
    pub hip_now: [Angles; 2],
    /// Each shoulder's current lean1 and each elbow's current angle.
    pub shoulder_now_lean1: [f32; 2],
    pub elbow_now: [f32; 2],
    pub foot_planted: [bool; 2],
    /// Either hand touching something.
    pub hand_touching: bool,
    /// The game holds that arm (character +0x310 / +0x314): it is left alone.
    pub arm_held: [bool; 2],
    /// The balancer's step count.
    pub step: u32,
    /// What each upper-body joint wants now (eased toward neutral where this step doesn't set it).
    pub clavicle_want: [Angles; 2],
    pub shoulder_want: [Angles; 2],
    pub elbow_want: [f32; 2],
}

/// Strength and damping for one joint group.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gain {
    pub strength: f32,
    pub damping: f32,
}

impl Gain {
    fn of(stiffness: f32, ratio: f32) -> Gain {
        Gain { strength: stiffness * stiffness, damping: 2.0 * stiffness * ratio }
    }
}

/// Muscle stiffness it sets on the spine, neck and head every step (shoulders and elbows keep
/// 0.5, wrists 10, clavicles 1 from elsewhere).
pub const SPINE_MUSCLE_STIFFNESS: f32 = 1.5;
/// Gravity opposition it sets when it starts: spine0..2 and the clavicles 2, neck and head 1.
pub const OPPOSE_GRAVITY_SPINE: f32 = 2.0;
pub const OPPOSE_GRAVITY_CLAVICLE: f32 = 2.0;
pub const OPPOSE_GRAVITY_NECK: f32 = 1.0;

#[derive(Clone, Copy, Debug, Default)]
pub struct Output {
    pub spine: [Angles; 4],
    pub neck: Angles,
    pub head: Angles,
    pub clavicle: [Angles; 2],
    pub shoulder: [Angles; 2],
    pub elbow: [f32; 2],
    pub wrist: [Angles; 2],
    /// Shoulders and wrists, elbows, spine, neck and head.
    pub arm_gain: Gain,
    pub elbow_gain: Gain,
    pub spine_gain: Gain,
    pub neck_gain: Gain,
    /// The arms-out amount from the body's spin (for debugging).
    pub arms_out: f32,
    /// The speed gauge, 0.001..1.
    pub gauge: f32,
}

/// Spine0..3 lean1 while balancing.
const SPINE_LEAN1: [f32; 4] = [-0.07, 0.25, -0.07, 0.07];
/// Each spine joint's share of the hips' swing as twist.
const SPINE_TWIST: [f32; 4] = [0.35, 0.3, 0.2, 0.15];
const SHOULDER_TWIST: f32 = -0.35;
const ELBOW_MAX: f32 = 6.0;
const SPIN_MAX: f32 = 7.0;
const PUSH_MAX: f32 = 10.0;

fn clamp(x: f32, lo: f32, hi: f32) -> f32 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

/// The body's roll, pitch and heading from spine2's rows (IV keeps row b negated, so its roll
/// reads b's z as is).
fn body_angles(rows: &[Vec3; 3]) -> [f32; 3] {
    let (b, c) = (rows[1], rows[2]);
    let pitch = clamp(c.z, -1.0, 1.0).asin();
    let inv = 1.0 / pitch.cos();
    let roll = clamp(b.z * inv, -1.0, 1.0).asin();
    let heading = clamp(-c.y * inv, -1.0, 1.0).asin();
    [roll, pitch, heading]
}

impl State {
    /// Activate: the arms-out timer starts, the body's angles are taken, and three draws are made
    /// (the elbow value, and the head-look and body-turn draws, kept by IV for those parts).
    pub fn start(s: &Settings, spine2_rows: &[Vec3; 3], rng: &mut Rng) -> State {
        let elbow_value = 0.8 * rng.uniform();
        rng.uniform();
        rng.uniform();
        State {
            arms_out_timer: s.arms_out_on_push_timeout,
            arms_out_floor: 0.0,
            angles: body_angles(spine2_rows),
            bend_timer: 0.0,
            elbow_value,
            last_step: 0,
        }
    }

    pub fn step(&mut self, s: &Settings, i: &Input, rng: &mut Rng) -> Output {
        let mut out = Output::default();
        // a new balancer step: a new elbow value
        if i.step != 0 && i.step != self.last_step {
            self.last_step = i.step;
            self.elbow_value = 0.8 * rng.uniform();
        }
        out.arm_gain = Gain::of(s.body_stiffness, s.damping);
        out.elbow_gain = Gain::of(0.9 * s.body_stiffness, 0.8 * s.damping);
        out.spine_gain = Gain::of(s.spine_stiffness, s.spine_damping);
        out.neck_gain = Gain::of(s.body_stiffness, 1.0);
        let gauge = clamp((2.0 * i.ke).sqrt(), 0.001, 1.0);
        out.gauge = gauge;

        // the body's spin. (The somersault tilt terms come from the character's rows, which IV
        // sets once per character, so with its rows they are 0, and the "returning to balance"
        // factors are 1 for the pitch and returningToBalanceArmsOut for the roll.)
        let now = body_angles(&i.spine2_rows);
        let rate = [0, 1, 2].map(|k| (now[k] - self.angles[k]) / i.dt);
        self.angles = now;
        let spin = |r: f32, k: usize| clamp(r.abs(), s.ang_vel_threshold[k], SPIN_MAX) - s.ang_vel_threshold[k];
        let (r_pitch, r_roll) = (1.0, s.returning_to_balance_arms_out);
        let arms_out = -(s.ang_vel_multiplier[0] * spin(rate[1], 0) * r_pitch)
            - s.ang_vel_multiplier[1] * spin(rate[2], 1)
            - s.ang_vel_multiplier[2] * spin(rate[0], 2) * r_roll;
        out.arms_out = arms_out;

        let push_mult = if self.arms_out_timer > 0.0 {
            self.arms_out_timer -= i.dt;
            s.arms_out_on_push_multiplier
        } else {
            0.0
        };
        if self.bend_timer > 0.0 {
            self.bend_timer -= i.dt;
        }
        if i.hand_touching {
            self.bend_timer = s.bend_elbows_time;
        }

        // spine: fixed leans, twist against the hips' swing; neck and head straight
        let swing = (i.hip_want[0][0] - i.hip_want[1][0]) * 0.15 + (i.hip_now[0][0] - i.hip_now[1][0]);
        for k in 0..4 {
            out.spine[k] = [SPINE_LEAN1[k], 0.0, swing * SPINE_TWIST[k]];
        }

        // arms: each swings against the other side's hip
        out.clavicle = i.clavicle_want;
        out.shoulder = i.shoulder_want;
        out.elbow = i.elbow_want;
        let planted = i.foot_planted[0] || i.foot_planted[1];
        let dir = i.com_vel.normalize_or_zero();
        let toward_back = dir.dot(i.spine2_rows[2]);
        let toward_right = -dir.dot(i.spine2_rows[1]);
        let floor = self.arms_out_floor;
        let push_of = |own: usize, other: usize| {
            let p = |x: f32| clamp(x.abs(), floor, PUSH_MAX) - floor;
            -push_mult
                * (0.5 * p(i.hip_want[other][1]) + 0.25 * p(i.hip_want[own][1]) + 0.5 * p(i.hip_now[other][1]) + 0.25 * p(i.hip_now[own][1]))
        };
        let mut push_left = 0.0;
        for side in 0..2 {
            if i.arm_held[side] || !planted {
                continue;
            }
            let other = 1 - side;
            let (o_want, o_now, m_now) = (i.hip_want[other][0], i.hip_now[other][0], i.hip_now[side][0]);
            let lean1 = ((o_want - o_now * 1.2) - (o_now * 1.1 - m_now) * 0.6) * s.shoulder + i.shoulder_now_lean1[side];
            let mut elbow = (o_want - o_now) * s.elbow + i.elbow_now[side];
            let push = if s.arms_out_on_push { push_of(side, other) } else { 0.0 };
            if side == 0 {
                push_left = push;
            }
            if s.arms_out_on_push {
                // (IV adds the left arm's push to the right elbow's straightening too)
                let a = if side == 0 { arms_out } else { arms_out + push_left };
                elbow += a * s.arms_out_straighten_elbows;
            }
            let mut cap = 0.0;
            if self.bend_timer > 0.0 {
                let sideways = if side == 0 { toward_right > 0.0 } else { toward_right < 0.0 };
                if (toward_back > 0.0 && sideways) || toward_back > 0.5 {
                    elbow = elbow.max(s.elbow_angle_on_contact);
                } else {
                    cap = self.elbow_value;
                }
            }
            elbow = clamp(elbow, s.bend_elbows_gait, ELBOW_MAX);
            let base = i.hip_now[side][1] * 0.3 + 0.5;
            let lean2 = if s.arms_out_on_push {
                let v = base + arms_out + push;
                if v < s.arms_out_min_lean2 {
                    s.arms_out_min_lean2
                } else {
                    v.min(base - cap)
                }
            } else {
                base
            };
            out.shoulder[side] = [lean1, lean2, SHOULDER_TWIST];
            out.elbow[side] = elbow;
        }

        // ease everything toward neutral, harder the slower the body moves
        let w = clamp((1.0 - gauge) * 0.8 + 0.2, 0.0, 1.0);
        let ease = |v: f32, n: f32| (1.0 - w) * v + w * n;
        let ease3 = |a: Angles| a.map(|v| ease(v, 0.0));
        for k in 0..4 {
            out.spine[k] = ease3(out.spine[k]);
        }
        for side in 0..2 {
            out.clavicle[side] = ease3(out.clavicle[side]);
            let sh = out.shoulder[side];
            out.shoulder[side] = [ease(sh[0], 0.0), ease(sh[1], 0.5), ease(sh[2], 0.0)];
            out.elbow[side] = ease(out.elbow[side], 0.0);
        }
        out
    }
}
