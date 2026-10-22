//! IV's balancer foot placement: each update, decide whether a foot must step to keep the body over its
//! support, and move the stepping foot's target. Spec: `specs/euphoria/balancer.md` ("Foot placement").

use crate::math::Vec3;

/// A part's world frame: axes a, b, c and position.
#[derive(Clone, Copy, Debug, Default)]
pub struct Frame {
    pub a: Vec3,
    pub b: Vec3,
    pub c: Vec3,
    pub pos: Vec3,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Foot {
    pub frame: Frame,
    pub vel: Vec3,
    pub ang_vel: Vec3,
}

/// A hip joint as foot placement sees it: its parent-side frame, swing half-ranges and centres (the two
/// lean axes), and its current angles.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hip {
    pub frame: Frame,
    pub half: [f32; 2],
    pub centre: [f32; 2],
    pub current: [f32; 2],
}

/// The body this update (index 0 = left, 1 = right).
#[derive(Clone, Copy, Debug, Default)]
pub struct Body {
    pub com: Vec3,
    pub com_vel: Vec3,
    /// COM velocity with the up part removed.
    pub com_hvel: Vec3,
    /// The body's angular velocity.
    pub spin: Vec3,
    /// The body frame's side and front axes.
    pub side: Vec3,
    pub front: Vec3,
    pub pelvis: Frame,
    pub feet: [Foot; 2],
    pub hips: [Hip; 2],
    /// Each foot touching the ground.
    pub contact: [bool; 2],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Tuning {
    pub up: Vec3,
    /// Pushes a planted foot's step target forward (along this) when the step starts ahead of it.
    pub forward: Vec3,
    /// Where each foot rests, and the ground normal under it.
    pub reference: [Vec3; 2],
    pub normal: [Vec3; 2],
    pub dt: f32,
    /// Feet closer than this are pushed apart (unless both are down).
    pub min_separation: f32,
    /// How far to the side of the led COM each foot aims.
    pub side_offset: f32,
    /// How far ahead (seconds of COM velocity) the COM is led.
    pub lead_time: f32,
    /// The led COM counts as inside the support when further than this inside its edges.
    pub support_margin: f32,
    /// Clamp on the step's closing term.
    pub gap_limit: f32,
    /// Scales the hip reach ellipse.
    pub reach: f32,
    /// Pelvis twist rest centre and range, and the front lean integrator's centre.
    pub twist_centre: f32,
    pub twist_range: f32,
    pub front_base: f32,
}

pub const LEFT_DOWN: u8 = 1;
pub const RIGHT_DOWN: u8 = 2;
/// The stepping foot was off the ground when the step began.
pub const SWING_FREE: u8 = 8;
/// The other foot had further to go than the stepping one.
pub const OTHER_FURTHER: u8 = 0x10;
/// The led COM is inside the support area.
pub const SUPPORTED: u8 = 0x20;

/// Foot placement's own state, carried between updates.
#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    /// Each foot's placement target.
    pub pos: [Vec3; 2],
    /// Midpoint between the feet (horizontally the led COM while stepping).
    pub mid: Vec3,
    /// Where the stepping foot started.
    pub start: Vec3,
    pub prev_target: Vec3,
    pub prev_foot: Vec3,
    /// Height of each reference point along up.
    pub height: [f32; 2],
    /// How far each foot would have to move to catch the led COM.
    pub need: [f32; 2],
    /// Steps still allowed (decremented per step; stepping stops at 0; negative = never counted).
    pub steps_left: i32,
    /// 0 = none, 1 = left, 2 = right.
    pub stepping: u8,
    pub flags: u8,
}

fn horiz(v: Vec3, up: Vec3) -> Vec3 {
    v - up * v.dot(up)
}

/// Unit vector, or zero for zero.
fn unit(v: Vec3) -> Vec3 {
    let l2 = v.dot(v);
    if l2 == 0.0 {
        Vec3::ZERO
    } else {
        v * (1.0 / l2.sqrt())
    }
}

/// Smallest signed distance from `p` to the edges of the support quad made from the two foot frames.
fn support_margin(p: Vec3, l: &Frame, r: &Frame, up: Vec3) -> f32 {
    let l1 = l.pos + l.a * -0.05 + l.c * 0.15;
    let l2 = l.pos + l.a * -0.05 - l.c * 0.15;
    let r1 = r.pos + r.a * 0.05 - r.c * 0.15;
    let r2 = r.pos + r.a * 0.05 + r.c * 0.15;
    [(l2, r1), (r1, r2), (r2, l1), (l1, l2)]
        .iter()
        .map(|&(a, b)| (p - a).dot(unit((b - a).cross(up))))
        .fold(f32::MAX, f32::min)
}

/// Keep a foot target inside the hip's reach: an ellipse in the ground plane around a point just in
/// front of / beside the pelvis, radii 1.5·`ra` (front) and 2.5·`rb` (side).
pub(crate) fn clamp_reach(x: Vec3, up: Vec3, pelvis: &Frame, ra: f32, rb: f32) -> Vec3 {
    let n = unit(pelvis.c - up * up.dot(pelvis.c));
    let cr = n.cross(up);
    let mut centre = pelvis.pos + cr * (up.dot(pelvis.b) * 0.4) + n * (up.dot(pelvis.c) * 0.4);
    centre = centre + up * (up.dot(x) - up.dot(centre));
    let d = x - centre;
    let (px, py) = (d.dot(cr), d.dot(n));
    let (ex, ey) = (px / (rb * 2.5), py / (ra * 1.5));
    let s = (1.0 / ((ex * ex + ey * ey).sqrt() + 1e-6)).min(1.0);
    centre + n * (s * py) + cr * (s * px)
}

/// The two hip swing angles that aim the leg at `p`.
fn hip_angles(p: Vec3, hip: &Hip, side: f32) -> [f32; 2] {
    let d = p - hip.frame.pos;
    let v = unit(Vec3::new(hip.frame.a.dot(d), hip.frame.b.dot(d), hip.frame.c.dot(d)));
    let ang = if v.z <= -1.0 {
        std::f32::consts::PI
    } else if v.z < 1.0 {
        v.z.acos()
    } else {
        0.0
    };
    let k = 1.0 / ((v.x * v.x + v.y * v.y).sqrt() + 1e-6);
    [v.x * k * ang + hip.centre[0], v.y * k * ang * side + hip.centre[1]]
}

/// Angular room left along the swing from `from` to `to`: extend the swing line through the midpoint of
/// the current and start angles out to the hip's limit ellipse (1.5 × half-range), and measure how far
/// that exit is past the current angles along the swing direction.
fn swing_room(hip: &Hip, to: [f32; 2], from: [f32; 2]) -> f32 {
    let (ia, ib) = (1.0 / (hip.half[0] * 1.5), 1.0 / (hip.half[1] * 1.5));
    let da = (to[0] - hip.centre[0]) * ia - (from[0] - hip.centre[0]) * ia;
    let db = (to[1] - hip.centre[1]) * ib - (from[1] - hip.centre[1]) * ib;
    let ca = ((hip.current[0] + from[0]) * 0.5 - hip.centre[0]) * ia;
    let cb = ((hip.current[1] + from[1]) * 0.5 - hip.centre[1]) * ib;
    let num = db * cb + da * ca;
    let den = num - ((da + ca) * da + (db + cb) * db);
    // IV only takes the line parameter when the denominator is (nearly) zero; otherwise it stays 0
    let t = if den.abs() <= 0.001 { num / den } else { 0.0 };
    let (pa, pb) = (t * da + ca, t * db + cb);
    let inv = 1.0 / ((db * db + da * da).sqrt() + 1e-6);
    let (ua, ub) = (inv * da, inv * db);
    let cross = (pa * ub - pb * ua).abs();
    let (oa, ob) = if cross >= 1.0 {
        (hip.current[0], hip.current[1])
    } else {
        let s = (1.0 - cross * cross).sqrt();
        ((s * ua + pa) * hip.half[0] * 1.5 + hip.centre[0], (s * ub + pb) * hip.half[1] * 1.5 + hip.centre[1])
    };
    let (ea, eb) = (to[0] - from[0], to[1] - from[1]);
    let inv2 = 1.0 / ((eb * eb + ea * ea).sqrt() + 1e-6);
    inv2 * eb * (ob - hip.current[1]) + inv2 * ea * (oa - hip.current[0])
}

/// Start a step if one is needed and allowed.
fn decide(st: &mut State, body: &Body) {
    if st.stepping != 0 {
        return;
    }
    let [dl, dr] = st.need;
    let supported = st.flags & SUPPORTED != 0;
    let big_l = dl > dr && !supported;
    let big_r = dr > dl && !supported;
    let [lc, rc] = body.contact;
    let step_l = if !rc { !lc && big_r } else { big_l || !lc };
    let step_r = if !lc { !rc && big_l } else { big_r || !rc };
    if !(step_l || step_r) {
        return;
    }
    if st.steps_left >= 0 {
        st.steps_left -= 1;
    }
    if st.steps_left <= 0 {
        return;
    }
    let (step, free, further) = if step_l { (1, rc && !lc, dr > dl) } else { (2, lc && !rc, dl > dr) };
    st.flags = (((further as u8) << 1 | free as u8) << 3) | (st.flags & 0xE7);
    st.stepping = step;
    st.start = st.pos[step as usize - 1];
}

/// One foot-placement update.
pub fn update(st: &mut State, t: &Tuning, body: &Body) {
    let up = t.up;
    // each foot's target starts where the foot is, at its reference height
    for i in 0..2 {
        st.height[i] = up.dot(t.reference[i]);
        let p = body.feet[i].frame.pos;
        st.pos[i] = p + up * (st.height[i] - up.dot(p));
    }
    if !(body.contact[0] && body.contact[1]) {
        let d = st.pos[1] - st.pos[0];
        let dist = d.length();
        let inv = if dist == 0.0 { 0.0 } else { 1.0 / dist };
        if dist < t.min_separation {
            st.pos[0] = st.pos[0] + d * (inv * (dist - t.min_separation) * 0.5);
            st.pos[1] = st.pos[1] + d * (inv * (t.min_separation - dist) * 0.5);
        }
    }
    st.mid = (st.pos[0] + st.pos[1]) * 0.5;
    // lead the COM, and ask whether that point is over the feet
    let mut lead = body.com + body.com_hvel * t.lead_time;
    lead = lead + up * (up.dot(st.mid) - up.dot(lead));
    let (fl, fr) = (&body.feet[0].frame, &body.feet[1].frame);
    let mut supported = support_margin(lead, fl, fr, up) > t.support_margin;
    if supported && horiz(fr.pos - fl.pos, up).length() > t.min_separation + 0.2 {
        supported = false;
    }
    st.flags = (st.flags & !SUPPORTED) | if supported { SUPPORTED } else { 0 };
    // where each foot would go to catch it, and how far that is
    let aim = |from: Vec3, sign: f32| {
        let n = unit(horiz(lead - from, up));
        lead + (body.front * (body.side.dot(n) * body.front.dot(n)) + body.side) * (sign * t.side_offset)
    };
    let target = [aim(fr.pos, -1.0), aim(fl.pos, 1.0)];
    st.need = [horiz(target[0] - fl.pos, up).length(), horiz(target[1] - fr.pos, up).length()];
    decide(st, body);
    if st.stepping == 0 {
        st.flags = (st.flags & !(LEFT_DOWN | RIGHT_DOWN)) | body.contact[0] as u8 | (body.contact[1] as u8) << 1;
        return;
    }
    step(st, t, body, target);
}

/// Move the stepping foot's target.
fn step(st: &mut State, t: &Tuning, body: &Body, target: [Vec3; 2]) {
    let up = t.up;
    let i = st.stepping as usize - 1;
    let o = 1 - i;
    let (hip, foot) = (&body.hips[i], &body.feet[i]);
    let side = if i == 0 { 1.0 } else { -1.0 };
    if st.need[i] < st.need[o] {
        st.flags |= OTHER_FURTHER;
    }
    if !body.contact[i] {
        st.flags |= SWING_FREE;
    }
    st.mid = body.com + body.com_hvel * 0.15;
    let (ra, rb) = (hip.half[0] * t.reach, hip.half[1] * t.reach);
    let tgt = clamp_reach(target[i], up, &body.pelvis, ra, rb);
    let dir = unit(horiz(tgt - st.start, up));
    let fpos = foot.frame.pos;
    let progress = horiz(tgt - fpos, up).length() / (horiz(st.start - fpos, up).length() + 1e-6);
    let rel_vel = (dir.dot(body.com_vel) - dir.dot(foot.vel)).min(0.0);
    // the foot's back end, and how fast it closes on the target
    let back = fpos - foot.frame.c * 0.1;
    let rem_old = horiz(st.prev_foot - st.prev_target, up).length();
    let rem_new = horiz(back - tgt, up).length();
    let passed = horiz(back - st.prev_target, up).length() > horiz(st.prev_foot - tgt, up).length();
    let mut closing = (rem_old - rem_new) / t.dt;
    st.prev_target = tgt;
    st.prev_foot = back;
    // lift: bounded by the hip's swing room and by how far there is still to go
    let low = tgt + up * (up.dot(st.pos[i]) - up.dot(tgt));
    let to = hip_angles(low, hip, side);
    let from = hip_angles(st.start, hip, side);
    let gap = ((rel_vel * 0.1 + dir.dot(tgt - fpos)) * 0.5).clamp(-t.gap_limit, t.gap_limit);
    let c = foot.frame.c + foot.ang_vel.cross(foot.frame.c) * 0.07;
    let tilt = t.normal[i].dot(c);
    let slope = 1.5 / (t.normal[i].dot(up) + 0.5);
    let mut lift = swing_room(hip, to, from).min(tilt.abs().min(1.0) * 0.1 + gap);
    let mut rest = t.reference[i];
    if st.flags & SWING_FREE == 0 {
        lift += 0.07;
        let k = (st.start - rest).dot(t.forward).max(0.0);
        rest = rest + t.forward * k;
    }
    let h = up.dot(rest) + slope * lift;
    let raised = low + up * (h - up.dot(low));
    let mut d = horiz(raised - st.pos[i], up);
    if d.dot(d) > 0.01 {
        d = unit(d) * 0.1;
    }
    st.pos[i] = clamp_reach(raised + d, up, &body.pelvis, ra, rb);
    let m = (st.pos[o] + st.pos[i]) * 0.5;
    st.mid = st.mid + up * (up.dot(m) - up.dot(st.mid));
    if tilt < 0.0 || passed {
        closing = -1.0;
    }
    st.flags = if i == 0 { (st.flags & !LEFT_DOWN) | RIGHT_DOWN } else { (st.flags & !RIGHT_DOWN) | LEFT_DOWN };
    let other_down = body.contact[o];
    let landed = (progress < 0.5 || st.flags & (OTHER_FURTHER | SWING_FREE) == OTHER_FURTHER | SWING_FREE) && body.contact[i] && closing < 0.0;
    if (body.contact[i] && !other_down) || landed {
        st.flags |= if i == 0 { LEFT_DOWN } else { RIGHT_DOWN };
        st.stepping = 0;
        decide(st, body);
    }
}
