//! braceForImpact, part 2a: the balancer lean step, hip pitch and the torso step (spine bend b,
//! spine twist). Spec: `specs/euphoria/braceForImpact.md`, "Balance and torso".
//!
//! Required order each on-feet frame: `lean_step` (reads last frame's b, then decays it), then, only
//! while bracing, `torso_step` (sets the b the next frame's lean step will read).

use crate::math::Vec3;

/// Spine bend state carried between frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Torso {
    /// Spine bend amount b.
    pub bend: f32,
}

/// What the lean step asks of the body this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lean {
    /// lean1 targets for spine_0, spine_1, spine_2, spine_3.
    pub spine_lean1: [f32; 4],
    /// The balancer's hip pitch.
    pub hip_pitch: f32,
}

/// Spine twist request from the torso step.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Twist {
    /// Start (true) or stop (false) the spine-twist behaviour.
    pub on: bool,
    pub stiffness: f32,
    /// Aim point and its velocity (world).
    pub target: Vec3,
    pub target_vel: Vec3,
}

/// Bend decays as b / (1 + DECAY·dt).
const DECAY: f32 = 3.18;
const BEND_BIAS: f32 = 0.2;

impl Torso {
    /// Lean step. `side_bias` is the Start draw s.
    pub fn lean_step(&mut self, side_bias: f32, dt: f32) -> Lean {
        let b = self.bend;
        let upper = (1.5 + side_bias) * 0.25 * b;
        let lower = (1.5 - side_bias) * 0.25 * b;
        // Keep the game's form b·(−4)·(1/6); rewriting it changes the last bits.
        let hip_pitch = b * -4.0 * (1.0 / 6.0);
        self.bend = b / (1.0 + DECAY * dt);
        Lean { spine_lean1: [upper, upper, lower, lower], hip_pitch }
    }

    /// Torso step, only while bracing. ĥ is the horizontal unit direction from the target to the
    /// centre of mass; Δz is the pelvis height above the target along `up`.
    #[allow(clippy::too_many_arguments)]
    pub fn torso_step(
        &mut self,
        facing: Vec3,
        up: Vec3,
        com: Vec3,
        pelvis: Vec3,
        target: Vec3,
        target_vel: Vec3,
        torso_factor: f32,
        both_arms: bool,
        body_stiffness: f32,
    ) -> Twist {
        let h_dir = (com - target).reject(up).normalize_or_zero();
        let dz = (pelvis - target).dot(up);
        self.bend = facing.dot(h_dir) * torso_factor * dz + BEND_BIAS;
        Twist { on: both_arms, stiffness: body_stiffness, target, target_vel }
    }
}
