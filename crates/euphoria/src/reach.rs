//! Reach for wound: the shot behaviour's hand that goes to the bullet wound (spec:
//! specs/euphoria/reachForWound.md).
//!
//! Three pieces. On each hit, `choose` decides which hand (or both) goes for the wound. Each step a
//! hand is reaching, `plan` sets its clavicle, the arm IK request (aim point off the wound, twist)
//! and the wrist IK request (palm onto the wound). `hold` is the spring that pulls the hand onto the
//! wound once it's close. Checked against IV's own routines: 6,808 hand choices and 480 random reach
//! steps (wounds on every part, both hands, hand positions, timers, clavicle flags).

use crate::math::Vec3;

/// IV's left / right (its "armL" is the left arm as the character sees it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

/// A body part, in IV's terms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Pelvis,
    Spine0,
    Spine1,
    Spine2,
    Spine3,
    Neck,
    Head,
    Thigh(Side),
    Shin(Side),
    Foot(Side),
    Clavicle(Side),
    UpperArm(Side),
    Forearm(Side),
    Hand(Side),
}

impl Part {
    /// IV's 21-part NM order (as player.wft).
    pub fn from_iv_index(i: usize) -> Option<Part> {
        use Part::*;
        use Side::*;
        Some(match i {
            0 => Pelvis,
            1 => Thigh(Left),
            2 => Shin(Left),
            3 => Foot(Left),
            4 => Thigh(Right),
            5 => Shin(Right),
            6 => Foot(Right),
            7 => Spine0,
            8 => Spine1,
            9 => Spine2,
            10 => Spine3,
            11 => Neck,
            12 => Head,
            13 => Clavicle(Left),
            14 => UpperArm(Left),
            15 => Forearm(Left),
            16 => Hand(Left),
            17 => Clavicle(Right),
            18 => UpperArm(Right),
            19 => Forearm(Right),
            20 => Hand(Right),
            _ => return None,
        })
    }

    fn arm(self) -> Option<Side> {
        match self {
            Part::Clavicle(s) | Part::UpperArm(s) | Part::Forearm(s) | Part::Hand(s) => Some(s),
            _ => None,
        }
    }

    fn leg(self) -> Option<Side> {
        match self {
            Part::Thigh(s) | Part::Shin(s) | Part::Foot(s) => Some(s),
            _ => None,
        }
    }

    /// The trunk and head: hits here measure their side from the part itself, not the body centre.
    pub fn is_axial(self) -> bool {
        matches!(self, Part::Pelvis | Part::Spine0 | Part::Spine1 | Part::Spine2 | Part::Spine3 | Part::Neck | Part::Head)
    }
}

/// What the shot remembers about wounds between hits.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wounds {
    /// The part each hand is currently assigned to (None = no wound for that hand yet).
    pub left: Option<Part>,
    pub right: Option<Part>,
    /// A hit on that arm (below the clavicle) before it had a wound to hold: it won't reach again.
    pub left_arm_hurt: bool,
    pub right_arm_hurt: bool,
    /// A hit on that leg (feeds the wounded-leg collapse).
    pub left_leg_hit: bool,
    pub right_leg_hit: bool,
    /// Shot from behind (and reachForWound on, and not in the neck or head).
    pub from_behind: bool,
}

/// One new hit, as the hand choice sees it.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub part: Part,
    /// How far the hit is to the character's right (+0x10 row) of the reference (the part itself for
    /// trunk/head hits, the body's centre of mass otherwise), clamped to ±2: see `lateral`.
    pub lateral: f32,
    /// Minus (facing · bullet direction): below 0 means the bullet came from behind.
    pub front: f32,
    pub reach_for_wound: bool,
    /// The behaviour's "don't use the left / right hand" flags.
    pub no_left: bool,
    pub no_right: bool,
    /// Each hand's busy state (1 = holding something; IV compares against exactly 1 here).
    pub left_hand: u32,
    pub right_hand: u32,
}

/// The side offset `Hit::lateral` wants: right row · (hit − reference), clamped to [−2, 2].
pub fn lateral(right: Vec3, hit: Vec3, reference: Vec3) -> f32 {
    right.dot(hit - reference).clamp(-2.0, 2.0)
}

/// Which hands take this hit as their wound. Updates `w`; returns (left, right) newly assigned. A
/// newly assigned hand restarts its reach timer: 0 for the left, −0.1 s for the right (the right
/// starts a tenth of a second later).
pub fn choose(h: &Hit, w: &mut Wounds) -> (bool, bool) {
    w.from_behind = h.reach_for_wound && h.front < 0.0 && h.part != Part::Head && h.part != Part::Neck;
    match h.part.leg() {
        Some(Side::Left) => w.left_leg_hit = true,
        Some(Side::Right) => w.right_leg_hit = true,
        None => {}
    }
    let on_left_arm = h.part.arm() == Some(Side::Left);
    let on_right_arm = h.part.arm() == Some(Side::Right);
    let both = matches!(h.part, Part::Thigh(_)) && w.left.is_none() && w.right.is_none();
    if on_left_arm && h.part != Part::Clavicle(Side::Left) && w.left.is_none() {
        w.left_arm_hurt = true;
    }
    if on_right_arm && h.part != Part::Clavicle(Side::Right) && w.right.is_none() {
        w.right_arm_hurt = true;
    }
    // the far hand by default: hits right of centre (or near it) get the left hand, and the other way
    let mut by_left = h.lateral > -0.05 && !on_left_arm;
    let mut by_right = h.lateral <= 0.05 && !on_right_arm;
    if h.left_hand == 1 && !on_left_arm {
        by_right = true;
    }
    if h.right_hand == 1 && !on_right_arm {
        by_left = true;
    }
    if h.no_left {
        by_left = false;
    }
    if h.no_right {
        by_right = false;
    }
    let left = (on_right_arm || both || by_left) && !w.left_arm_hurt;
    let right = (on_left_arm || both || by_right) && !w.right_arm_hurt;
    if left {
        w.left = Some(h.part);
    }
    if right {
        w.right = Some(h.part);
    }
    (left, right)
}

/// Reach timer value a newly assigned hand starts from.
pub fn start_timer(side: Side) -> f32 {
    match side {
        Side::Left => 0.0,
        Side::Right => -0.1,
    }
}

/// Whether a hand that isn't reaching starts now: reachForWound on, not both hands busy, past
/// timeBeforeReachForWound since the hit, a wound newly assigned to it, its reach timer under
/// grabHoldTime, and the hand itself free (0). On start the hand's normal sign resets to +1.
pub fn may_start(reach_for_wound: bool, left_hand: u32, right_hand: u32, side: Side, since_hit: f32, wait: f32, assigned: bool, timer: f32, hold_time: f32) -> bool {
    let own = if side == Side::Left { left_hand } else { right_hand };
    !(left_hand == 1 && right_hand == 1) && reach_for_wound && wait < since_hit && assigned && timer < hold_time && own == 0
}

/// Whether a reaching hand stops: a new wound was assigned to it (it restarts on that one), its reach
/// timer passed grabHoldTime, the hand got busy, or reachForWound went off.
pub fn must_stop(reach_for_wound: bool, own_hand: u32, assigned: bool, timer: f32, hold_time: f32) -> bool {
    assigned || hold_time < timer || own_hand != 0 || !reach_for_wound
}

/// One reaching hand, one step.
#[derive(Clone, Copy, Debug)]
pub struct StepIn {
    pub side: Side,
    pub part: Part,
    /// Clavicle limits (lean1 min, max, lean2 min, max, twist min, max), its wanted angles now
    /// (lean1, lean2, twist), whether it's locked, and the incoming (animated) angles if it has them.
    pub clavicle_limits: [f32; 6],
    pub clavicle: [f32; 3],
    pub clavicle_locked: bool,
    pub clavicle_incoming: Option<[f32; 3]>,
    /// The wound and its outward normal in the world (the stored normal times the hand's sign).
    pub wound: Vec3,
    pub normal: Vec3,
    /// The character's facing row (+0x30).
    pub facing: Vec3,
    pub hand: Vec3,
    pub pelvis: Vec3,
    pub spine3: Vec3,
}

/// What the step asks of the arm.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    /// Clavicle wanted angles (lean1, lean2, twist).
    pub clavicle: [f32; 3],
    /// Arm IK: target, twist, twist scale τ (+1 left, −1 right). Elbow scale 1, no lead, no limiter.
    pub target: Vec3,
    pub twist: f32,
    pub twist_scale: f32,
    /// Wrist IK: palm onto this point facing this normal.
    pub wrist_point: Vec3,
    pub wrist_normal: Vec3,
}

/// The step's clavicle, arm IK and wrist IK requests. Shoulder, clavicle and elbow also get
/// oppose-gravity 1 each step.
pub fn plan(i: &StepIn) -> Plan {
    let mut clav = i.clavicle;
    let lim = i.clavicle_limits;
    if !i.clavicle_locked {
        // shrug: low trunk wounds pull the clavicle all the way forward and halfway up
        if matches!(i.part, Part::Pelvis | Part::Spine0 | Part::Spine1) {
            clav[0] = lim[1];
            clav[1] = lim[3] * 0.5;
        } else {
            clav[0] = lim[1] * 0.5;
            clav[1] = lim[2] * 0.6;
        }
        // then halfway back to the animated pose, when there is one
        if let Some(inc) = i.clavicle_incoming {
            for k in 0..3 {
                clav[k] = inc[k] * 0.5 + clav[k] * 0.5;
            }
        }
    }
    // aim off the wound along its normal tilted away from the facing, by half the hand's distance
    // from that line (less 5 cm, at most 25 cm): the hand comes in from outside and settles on it
    let d = (i.normal - i.facing).normalize_or_zero();
    let rel = i.hand - i.wound;
    let along = rel.dot(d).max(0.0);
    let away = (rel - d * along).length();
    let off = (0.5 * away - 0.05).min(0.25);
    let target = i.wound + d * off;
    // twist: 1 for wounds low on the trunk, falling to −1 by 80% of the way up pelvis→spine3 (and
    // easing back toward 0 while the hand is far off), 1 for thighs and shins
    let ax = i.spine3 - i.pelvis;
    let up = (i.wound - i.pelvis).dot(ax) / ax.dot(ax);
    let mut a = 1.0 - up.clamp(0.0, 0.8) * 2.5;
    if a < 0.0 {
        a *= 1.0 - (away * 2.5).min(1.0);
    }
    if matches!(i.part, Part::Thigh(_) | Part::Shin(_)) {
        a = 1.0;
    }
    let (twist, twist_scale) = match i.side {
        Side::Left => (a + 0.1, 1.0),
        Side::Right => (a - 0.1, -1.0),
    };
    Plan { clavicle: clav, target, twist, twist_scale, wrist_point: i.wound, wrist_normal: i.normal }
}

/// After the wrist IK: a negative result (palm facing away) flips the hand's normal sign for next
/// time, except on head wounds.
pub fn flips(wrist_ret: f32, part: Part) -> bool {
    wrist_ret < 0.0 && part != Part::Head
}

/// Hold strength: 0.5 once the shot's loosening timer has run its course (a duration that isn't 0),
/// else 4.
pub fn hold_strength(elapsed: f32, duration: f32) -> f32 {
    if duration <= elapsed && duration != 0.0 { 0.5 } else { 4.0 }
}

/// The hand-to-wound spring's inputs.
#[derive(Clone, Copy, Debug)]
pub struct HoldIn {
    pub wound: Vec3,
    pub wound_vel: Vec3,
    /// The hand part's position, first orientation row and velocity.
    pub hand: Vec3,
    pub hand_row_a: Vec3,
    pub hand_vel: Vec3,
    /// The hand's normal sign (after this step's flip).
    pub sign: f32,
    pub strength: f32,
    pub dt: f32,
}

/// The force on the hand (applied at the wrist joint; minus it goes on the wounded part at the
/// wound), or None when the palm is 0.3 m or more from the wound.
pub fn hold(h: &HoldIn) -> Option<Vec3> {
    let palm = h.hand - h.hand_row_a * (h.sign * 0.02);
    let gap = h.wound - palm;
    let d = gap.length();
    if d >= 0.3 {
        return None;
    }
    let w = ((0.3 - d) / 0.2).clamp(0.0, 1.0) * h.strength;
    let mut f = gap * (w * w) + (h.wound_vel - h.hand_vel) * (w * 0.2);
    if h.dt < 1.0 / 60.0 {
        f = f * (h.dt * 60.0);
    }
    Some(f)
}
