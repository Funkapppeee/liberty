//! IV's dynamic balancer. Spec: `specs/euphoria/balancer.md`. So far: the balance check (0x00846dd0),
//! the rule that decides "still balancing" vs "fallen" each update.

use crate::math::Vec3;

/// The balancer's tuning for the check (IV defaults from configureBalance).
#[derive(Clone, Copy, Debug)]
pub struct CheckParams {
    /// balanceAbortThreshold: fail when the COM is further (horizontally) than this × its height from
    /// the support. IV: 0.6.
    pub abort_ratio: f32,
    /// Fail when the COM is below 0.5 m above the lower foot. IV: on.
    pub give_up_low: bool,
}

impl Default for CheckParams {
    fn default() -> Self {
        CheckParams { abort_ratio: 0.6, give_up_low: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Check {
    pub failed: bool,
    /// Smoothed stability (≈ distance to the support over 0.18 × height; below 1 is close to falling).
    /// Unchanged on a failure frame.
    pub stability: f32,
}

/// Horizontal distance (the part of `a − b` across `up`).
fn hdist(a: Vec3, b: Vec3, up: Vec3) -> f32 {
    let d = a - b;
    (d - up * d.dot(up)).length()
}

/// One balance check. `stability` is last update's value (smoothed only on the way down).
#[allow(clippy::too_many_arguments)]
pub fn check(p: &CheckParams, up: Vec3, foot_l: Vec3, foot_r: Vec3, com: Vec3, com_vel: Vec3, foot_l_vel: Vec3, foot_r_vel: Vec3, stability: f32) -> Check {
    let mid = (foot_l + foot_r) * 0.5;
    let near = hdist(com, foot_l, up).min(hdist(com, foot_r, up)).min(hdist(com, mid, up));
    let height = com.dot(up) - foot_l.dot(up).min(foot_r.dot(up));
    if p.abort_ratio * height < near || (height < 0.5 && p.give_up_low) {
        return Check { failed: true, stability };
    }
    // where things will be in 0.3 s
    let (c, l, r) = (com + com_vel * 0.3, foot_l + foot_l_vel * 0.3, foot_r + foot_r_vel * 0.3);
    let near = hdist(c, l, up).min(hdist(c, r, up)).min(hdist(c, mid, up));
    let mut s = near / (height * 0.6 * 0.5 * 0.6);
    if s <= stability {
        s = (s - stability) * 0.25 + stability;
    }
    Check { failed: false, stability: s }
}
