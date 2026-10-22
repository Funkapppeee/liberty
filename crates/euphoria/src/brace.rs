//! braceForImpact, part 1: random draws, sensing, the brace decision, arm choice, the leading-hand
//! delay, the look timer and the feedback bits. Spec: `specs/euphoria/braceForImpact.md` (draft 1.5).
//!
//! Not here yet: arm IK, torso/spine lean, balance and the falling branch. This module decides
//! *what* to do each frame; later parts turn that into joint targets.

use crate::math::Vec3;
use crate::rng::Rng;

/// Which rig the character uses; decides the hand-spread range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CharacterKind {
    Other = 0,
    /// Male peds (`Fred.art`).
    #[default]
    Fred = 1,
    /// Female peds (`Wilma.art`): smaller hand spread.
    Wilma = 2,
    Cowboy = 3,
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub body_stiffness: f32,
    pub brace_distance: f32,
    pub target_prediction_time: f32,
    pub reach_absorbtion_time: f32,
    pub time_to_backwards_brace: f32,
}

impl Default for Params {
    /// RDR defaults where the spec gives one.
    fn default() -> Self {
        Params {
            body_stiffness: 12.0,
            brace_distance: 0.65,
            target_prediction_time: 0.45,
            reach_absorbtion_time: 0.15,
            time_to_backwards_brace: 0.0,
        }
    }
}

/// What the character tells the behaviour each on-feet frame. All positions in world space.
#[derive(Clone, Copy, Debug)]
pub struct FrameInput {
    /// The brace target (`pos` already converted to world).
    pub target: Vec3,
    pub com: Vec3,
    pub com_vel: Vec3,
    /// Unit vector against gravity.
    pub up: Vec3,
    /// F, the orientation row the hold-off tests against. IV leaves it equal to `up`
    /// (spec, Conventions), so pass `up` to be faithful, or the ped's forward for the intent.
    pub facing: Vec3,
    pub spine2_pos: Vec3,
    /// spine2's second orientation row (points to his left in the rest pose).
    pub spine2_side: Vec3,
    pub shoulder_l: Vec3,
    pub shoulder_r: Vec3,
    /// The game flags an arm unavailable when that hand holds something.
    pub arm_unavailable: [bool; 2],
    pub dt: f32,
}

/// Values drawn once at Start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StartDraws {
    /// Hip bend.
    pub h: f32,
    /// Side bias.
    pub s: f32,
    /// Torso factor.
    pub w: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Brace {
    pub params: Params,
    pub kind: CharacterKind,
    pub start: StartDraws,
    /// Hand spread, left (A) and right (B).
    pub spread_a: f32,
    pub spread_b: f32,
    prev_target: Vec3,
    /// Smoothed predicted distance S.
    pub smoothed: f32,
    pub bracing: bool,
    pub arm_l: bool,
    pub arm_r: bool,
    /// Set at Start and on not-bracing frames; allows the one draw of D per brace.
    fresh: bool,
    /// Leading-hand delay D.
    pub delay: f32,
    /// Which hand reaches first (nearer the target) once both arms come on together.
    pub left_leads: bool,
    pub look_timer: f32,
    pub look_ahead: bool,
    pub time_since_start: f32,
    /// Stiffness at the last Start / apply, so later changes can be detected.
    pub applied_stiffness: f32,
}

/// Everything a frame decided.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameOutput {
    pub target_vel: Vec3,
    /// Distance from spine2 to the target.
    pub l: f32,
    /// Unit direction from spine2 to the target.
    pub n: Vec3,
    /// Closing speed, positive when the target approaches.
    pub closing_speed: f32,
    /// Predicted distance P.
    pub predicted: f32,
    /// Give distance g; the body's lean-away vector is `-g·n`.
    pub give: f32,
    pub smoothed: f32,
    pub bracing: bool,
    pub arm_l: bool,
    pub arm_r: bool,
    /// Whether each hand reaches this frame (on, and not waiting out D).
    pub reach_l: bool,
    pub reach_r: bool,
    pub delay: f32,
    pub look_ahead: bool,
    pub look_timer: f32,
    pub feedback: Feedback,
}

/// The "brace" feedback bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Feedback {
    pub left_on: bool,
    pub right_on: bool,
    pub left_changed: bool,
    pub right_changed: bool,
}

/// Target speeds above this are tracked at this speed.
const MAX_TARGET_SPEED: f32 = 10.0;
/// Extra brace distance while already bracing (hysteresis).
const BRACE_HYSTERESIS: f32 = 0.1;
/// The left arm's margin is this minus the angle (π − 0.25 in the spec's rounding).
const LEFT_MARGIN_BASE: f32 = 2.8916;
const RIGHT_MARGIN_BASE: f32 = 0.25;
const ARM_ON: f32 = 0.2;
const ARM_OFF: f32 = -0.2;

fn draw_spread(rng: &mut Rng, kind: CharacterKind) -> f32 {
    let u = rng.uniform();
    match kind {
        CharacterKind::Wilma => 0.075 + 0.155 * u,
        _ => 0.17 + 0.23 * u,
    }
}

impl Brace {
    /// Start: 5 draws (A, B, h, s, w), or 6 when h < 0.05 (w is drawn again).
    /// `target` is `pos` converted to world.
    pub fn start(params: Params, kind: CharacterKind, target: Vec3, rng: &mut Rng) -> Self {
        let spread_a = draw_spread(rng, kind);
        let spread_b = draw_spread(rng, kind);
        let h = 0.02 + 0.08 * rng.uniform();
        let s = rng.uniform() - 0.5;
        let mut w = 0.2 + 0.4 * rng.uniform();
        if h < 0.05 {
            w = 0.2 + 0.15 * rng.uniform();
        }
        Brace {
            params,
            kind,
            start: StartDraws { h, s, w },
            spread_a,
            spread_b,
            prev_target: target,
            smoothed: 100.0,
            bracing: false,
            arm_l: false,
            arm_r: false,
            fresh: true,
            delay: 0.0,
            left_leads: false,
            look_timer: 0.0,
            look_ahead: false,
            time_since_start: 0.0,
            applied_stiffness: params.body_stiffness,
        }
    }

    /// A `start = false` message: new parameters, nothing restarted.
    pub fn update_params(&mut self, params: Params) {
        self.params = params;
    }

    /// One on-feet frame (the balancer has not failed).
    pub fn step(&mut self, input: &FrameInput, rng: &mut Rng) -> FrameOutput {
        let p = self.params;
        let dt = input.dt;
        let up = input.up;
        // Counts every active frame, on feet or falling.
        self.time_since_start += dt;

        // Sensing. The velocity uses the previous target before it is replaced.
        let mut target_vel = (input.target - self.prev_target) * (1.0 / dt);
        let speed = target_vel.length();
        if speed > MAX_TARGET_SPEED {
            target_vel = target_vel * (MAX_TARGET_SPEED / speed);
        }
        self.prev_target = input.target;

        let to_target = input.target - input.spine2_pos;
        let l = to_target.length();
        let n = to_target.normalize_or_zero();
        let closing_speed = (input.com_vel - target_vel).dot(n);
        let predicted = (l - closing_speed * p.target_prediction_time).max(0.1);
        let give = clamp_lo_first(
            closing_speed * p.reach_absorbtion_time,
            l - p.brace_distance,
            l - 0.3,
        );

        // Brace decision: S drops at once and rises slowly.
        let a = 2.0 * dt;
        self.smoothed = ((1.0 - a) * self.smoothed + a * predicted).min(predicted);
        let threshold = p.brace_distance + if self.bracing { BRACE_HYSTERESIS } else { 0.0 };
        let mut bracing = self.smoothed < threshold;
        // Early front hold-off: no brace on a +F-side target during the first window.
        if bracing && self.time_since_start < p.time_to_backwards_brace {
            let side = (input.target - input.com).reject(up);
            if side.dot(input.facing) > 0.0 {
                bracing = false;
            }
        }
        self.bracing = bracing;

        let (was_l, was_r) = (self.arm_l, self.arm_r);
        let mut reach_l = false;
        let mut reach_r = false;
        if bracing {
            self.choose_arms(input);
            let both = self.arm_l && self.arm_r;
            if both && self.fresh {
                self.delay = 0.1 * rng.uniform();
                let dl = (input.target - input.shoulder_l).length();
                let dr = (input.target - input.shoulder_r).length();
                self.left_leads = dl < dr;
            }
            self.fresh = false;

            // The nearer hand reaches at once; the other waits until D < 0.
            let waiting_done = self.delay < 0.0;
            if both {
                reach_l = self.left_leads || waiting_done;
                reach_r = !self.left_leads || waiting_done;
            } else {
                reach_l = self.arm_l;
                reach_r = self.arm_r;
            }
            if self.delay >= 0.0 {
                self.delay -= dt;
            }
        } else {
            self.arm_l = false;
            self.arm_r = false;
            self.fresh = true;
            self.spread_a = draw_spread(rng, self.kind);
            self.spread_b = draw_spread(rng, self.kind);
        }

        // Head: subtract first, then redraw at ≤ 0 (timer, then coin).
        self.look_timer -= dt;
        if self.look_timer <= 0.0 {
            self.look_timer = 0.25 + 1.25 * rng.uniform();
            self.look_ahead = rng.coin();
        }

        FrameOutput {
            target_vel,
            l,
            n,
            closing_speed,
            predicted,
            give,
            smoothed: self.smoothed,
            bracing,
            arm_l: self.arm_l,
            arm_r: self.arm_r,
            reach_l,
            reach_r,
            delay: self.delay,
            look_ahead: self.look_ahead,
            look_timer: self.look_timer,
            feedback: Feedback {
                left_on: self.arm_l,
                right_on: self.arm_r,
                left_changed: self.arm_l != was_l,
                right_changed: self.arm_r != was_r,
            },
        }
    }

    /// Per-arm on/off with hysteresis, from the angle between the shoulder-to-target direction
    /// and spine2's side axis (both flattened against up). The target direction is normalised;
    /// the side axis is **not**: it is used at its reduced length after removing up. That is what
    /// the 28 Sep pack needs (its spine2 row 2 points mostly down, so a side target gives
    /// |dot| ≈ 0.22 and both arms come on). Provisional; see the spec's Writer questions (28 Sep).
    fn choose_arms(&mut self, input: &FrameInput) {
        let up = input.up;
        let side = input.spine2_side.reject(up);
        let angle = |shoulder: Vec3| {
            let dir = (input.target - shoulder).reject(up).normalize_or_zero();
            dir.dot(side).clamp(-1.0, 1.0).acos()
        };
        let margin_l = LEFT_MARGIN_BASE - angle(input.shoulder_l);
        let margin_r = angle(input.shoulder_r) - RIGHT_MARGIN_BASE;
        self.arm_l = arm_switch(self.arm_l, margin_l, input.arm_unavailable[0]);
        self.arm_r = arm_switch(self.arm_r, margin_r, input.arm_unavailable[1]);
    }
}

fn arm_switch(on: bool, margin: f32, unavailable: bool) -> bool {
    if unavailable {
        false
    } else if margin > ARM_ON {
        true
    } else if margin < ARM_OFF {
        false
    } else {
        on
    }
}

/// The game's clamp: lower bound first, then upper, so the upper wins when lo > hi.
fn clamp_lo_first(x: f32, lo: f32, hi: f32) -> f32 {
    x.max(lo).min(hi)
}

/// Hand targets for the arms that reach this frame (spec "Arm reach"). `n` is the unit direction
/// from spine2 to the target; `in_contact` says whether each hand already touches something;
/// `twist_held_back` is spine twist's own hold flag.
///
/// The sideways push is across the line from him to the target, left hand to his left (A), right
/// hand to his right (B), only while both arms are on. Then each point is pulled in along its
/// shoulder-to-point line to min(distance + 0.05, R_max) (no 0.05 when that hand is in contact);
/// R_max is 0.55, or 0.65 while both arms are on and spine twist is not held back.
/// The sideways axis as up × n is this module's reading of "across the line"; not vector-checked.
#[allow(clippy::too_many_arguments)]
pub fn hand_targets(
    b: &Brace,
    input: &FrameInput,
    out: &FrameOutput,
    in_contact: [bool; 2],
    twist_held_back: bool,
) -> [Option<Vec3>; 2] {
    let both = b.arm_l && b.arm_r;
    let across = input.up.cross(out.n).normalize_or_zero(); // his left
    let r_max = if both && !twist_held_back { 0.65 } else { 0.55 };
    let one = |reach: bool, shoulder: Vec3, push: Vec3, contact: bool| {
        if !reach {
            return None;
        }
        let point = input.target + if both { push } else { Vec3::ZERO };
        let d = point - shoulder;
        let dist = d.length();
        let want = (dist + if contact { 0.0 } else { 0.05 }).min(r_max);
        Some(shoulder + d.normalize_or_zero() * want)
    };
    [
        one(out.reach_l, input.shoulder_l, across * (0.5 * b.spread_a), in_contact[0]),
        one(out.reach_r, input.shoulder_r, across * (-0.5 * b.spread_b), in_contact[1]),
    ]
}
