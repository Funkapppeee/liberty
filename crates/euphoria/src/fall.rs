//! braceForImpact, part 2b: the hand-off when the balancer reports a fall.
//! Spec: `specs/euphoria/braceForImpact.md`, "Once the balancer has failed (falling)".

use crate::rng::Rng;

/// Which body parts are touching something at the moment of failure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contacts {
    pub feet: bool,
    /// Anything above the feet: upper body ("ub" mask), pelvis, thighs or shins.
    pub above_feet: bool,
}

/// rollDownStairs settings, in the order the behaviour writes them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RollSettings {
    pub stiffness: f32,
    pub v1: f32,
    pub v2: f32,
    /// 0.2 + 0.6·u.
    pub random_factor: f32,
    pub v4: f32,
    pub v5: f32,
    pub v6: f32,
    pub v7: f32,
}

/// catchFall settings block, in IV's object order. Names from NaturalMotion's own parameter list
/// (RDR behaviours.xml; matched by their unique defaults 6/9/15, 0.35, −0.25).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CatchFallSettings {
    pub legs_stiffness: f32,
    pub torso_stiffness: f32,
    pub arms_stiffness: f32,
    pub forward_max_arm_offset: f32,
    pub backwards_min_arm_offset: f32,
    /// zAxisSpinReduction (by position).
    pub z_axis_spin_reduction: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FallAction {
    CatchFall(CatchFallSettings),
    RollDownStairs(RollSettings),
}

/// Runs once at the moment of failure. Draws: 1 coin always, plus 1 more for a roll.
pub fn on_fail(contacts: Contacts, body_stiffness: f32, rng: &mut Rng) -> FallAction {
    let coin = rng.coin();
    let only_feet = contacts.feet && !contacts.above_feet;
    if !coin || only_feet {
        FallAction::CatchFall(catch_fall_settings(body_stiffness))
    } else {
        let u = rng.uniform();
        FallAction::RollDownStairs(roll_settings(body_stiffness, u))
    }
}

/// Keep the multiply order bodyStiffness·k·(1/12).
pub fn catch_fall_settings(body_stiffness: f32) -> CatchFallSettings {
    let twelfth = 1.0 / 12.0;
    CatchFallSettings {
        legs_stiffness: body_stiffness * 6.0 * twelfth,
        torso_stiffness: body_stiffness * 9.0 * twelfth,
        arms_stiffness: body_stiffness * 15.0 * twelfth,
        forward_max_arm_offset: 0.35,
        backwards_min_arm_offset: -0.25,
        z_axis_spin_reduction: 0.0,
    }
}

pub fn roll_settings(body_stiffness: f32, u: f32) -> RollSettings {
    RollSettings {
        stiffness: (body_stiffness - 3.0).max(1.0).min(15.0),
        v1: 0.4,
        v2: 0.0,
        random_factor: 0.2 + 0.6 * u,
        v4: -0.9,
        v5: 1.4,
        v6: 0.2,
        v7: 0.6,
    }
}
