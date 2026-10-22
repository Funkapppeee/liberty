//! IV's balancer pelvis control: the pelvis twist targets (turning the hips toward where the body is
//! going) and the lean corrections from the COM's drift. Spec: `specs/euphoria/balancer.md`
//! ("Pelvis control").

use crate::footplace::{Body, Tuning};
use crate::math::Vec3;
use std::f32::consts::{FRAC_PI_2, PI};

#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    /// Pelvis position relative to the COM, horizontal.
    pub offset: Vec3,
    /// A velocity to use instead of the COM's (when `use_vel`).
    pub vel: Vec3,
    /// Where the COM should sit (the lean integrators pull toward it).
    pub anchor: Vec3,
    /// Backward-drift accumulator (only when not double-stance).
    pub drift: f32,
    /// Front / side lean integrators.
    pub front_i: f32,
    pub side_i: f32,
    /// Twist target per hip.
    pub twist: [f32; 2],
    /// Lean outputs: side, front.
    pub lean_side: f32,
    pub lean_front: f32,
    /// Which legs are in stance (set by the leg side).
    pub stance: [bool; 2],
    pub use_vel: bool,
}

fn horiz(v: Vec3, up: Vec3) -> Vec3 {
    v - up * v.dot(up)
}

/// Rotate `v` by the rotation vector `r`.
fn rotate(v: Vec3, r: Vec3) -> Vec3 {
    let th = r.length();
    let (q, w) = if th > 0.0 { (r * ((th * 0.5).sin() / th), (th * 0.5).cos()) } else { (Vec3::ZERO, 1.0) };
    v * (2.0 * w * w - 1.0) + q * (2.0 * q.dot(v)) + q.cross(v) * (2.0 * w)
}

/// Heading of `v` about the pelvis (from its b and c axes), a quarter turn off. `wrap` folds it toward
/// zero from either side; otherwise angles past ±0.8π read as 0. `scaled` first turns v back by 0.2 s of
/// spin and weights the result by min(|v|, 1)·0.025.
fn heading(body: &Body, v: Vec3, wrap: bool, scaled: bool, sign: f32) -> f32 {
    let vv = if scaled { rotate(v, body.spin * -0.2) } else { v };
    let mut ang = (-vv.dot(body.pelvis.c)).atan2(-vv.dot(body.pelvis.b) * sign);
    if wrap {
        ang = if ang > 0.0 { ang - FRAC_PI_2 } else { ang + FRAC_PI_2 };
    } else {
        ang -= FRAC_PI_2;
        if ang < -PI {
            ang += 2.0 * PI;
        }
        if ang > 0.8 * PI || ang < -0.8 * PI {
            ang = 0.0;
        }
    }
    if scaled {
        v.length().min(1.0) * ang * 0.025
    } else {
        ang
    }
}

pub fn update(st: &mut State, t: &Tuning, body: &Body) {
    let up = t.up;
    st.offset = horiz(body.pelvis.pos - body.com, up);
    let v = if st.use_vel { st.vel } else { body.com_hvel };
    let [a, b] = st.stance;
    let wrap = !st.use_vel;
    // twist: toward the heading on a single stance, split evenly when both/neither (with a velocity),
    // else relax toward the rest range
    let follow = (v.length() * 2.0).min(1.0) * 0.2;
    if a && !b {
        let h = heading(body, v, wrap, false, 1.0).clamp(-0.6, 0.6);
        st.twist[0] += (h - st.twist[0]) * follow;
        st.twist[1] = st.twist[0] * 0.5;
    } else if b && !a {
        let h = heading(body, v, wrap, false, -1.0).clamp(-0.6, 0.6);
        st.twist[1] += (h - st.twist[1]) * follow;
        st.twist[0] = st.twist[1] * 0.5;
    } else if st.use_vel {
        let r = (heading(body, v, wrap, true, 1.0) + (st.twist[0] - st.twist[1]) * 0.5).clamp(-0.6, 0.6);
        st.twist = [r, -r];
    } else {
        let (hi, lo, rate) = (t.twist_range - t.twist_centre, -t.twist_range - t.twist_centre, t.dt * 0.5);
        let toward = |x: f32, goal: f32| if x > goal { (x - rate).max(goal) } else { (x + rate).min(goal) };
        st.twist = [toward(st.twist[0], hi), toward(st.twist[1], lo)];
    }
    // lean from the COM's drift off the anchor and its velocity, in the body's front/side directions
    let lead = t.lead_time * 476.0;
    let front = horiz(body.front, up);
    let side = horiz(-body.side, up);
    let d = body.com - st.anchor;
    let off_f = (-d.dot(front)).clamp(-0.075, 0.15);
    let off_s = -d.dot(side);
    let (vel_f, vel_s) = (-body.com_hvel.dot(front), -body.com_hvel.dot(side));
    let both = a && b;
    if both {
        st.front_i = (t.dt * off_f + st.front_i).clamp(t.front_base - 0.1, t.front_base + 0.1);
        st.side_i = (t.dt * off_s + st.side_i).clamp(-0.1, 0.1);
    }
    st.lean_side = (st.side_i * 104.0 + off_s * 238.0 + lead * 0.5 * vel_s) * (1.0 / 144.0) * 0.5;
    if both {
        st.lean_front = (st.front_i * 208.0 + off_f * 476.0 + vel_f * lead) * -(1.0 / 144.0);
    } else {
        st.lean_front = 0.0;
        st.drift -= (vel_f * -0.2).clamp(0.0, 0.5);
    }
    // feet tipping over: fade the front lean out
    let tip = -t.normal[0].dot(body.feet[0].frame.c) - t.normal[1].dot(body.feet[1].frame.c);
    if tip > 0.05 {
        let m = ((tip - 0.05) * 3.0).min(1.0);
        st.lean_front -= st.lean_front * m;
    }
}
