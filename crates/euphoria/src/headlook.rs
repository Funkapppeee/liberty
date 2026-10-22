//! headLook: turn the neck and head toward a point (spec: specs/euphoria/headLook.md).
//!
//! One step works out three angles between the body's reference frame and the target (a twist
//! about the reference axis, two leans), lets the upper spine take any twist beyond what the neck
//! and head can manage, and splits each angle between the neck and head joints by how far each
//! can bend that way. Checked against IV's own routine on 400 random cases (poses, motion, flags).

use crate::math::{Quat, Vec3};

/// Everything one step reads. Frames are spine3's part frame (rows a, b, c: GTA world, Z up).
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub spine3_rows: [Vec3; 3],
    pub spine3_pos: Vec3,
    /// The head part's first row (used only when the target is straight up or down the world).
    pub head_row_a: Vec3,
    /// Spine3-to-head distance (measured once when the behaviour starts).
    pub eye_offset: f32,
    /// Half-angle of the front region (60 degrees in IV).
    pub cone: f32,
    pub up: Vec3,
    /// Spine3's velocity and spin (the target is led by its motion relative to the body).
    pub body_vel: Vec3,
    pub body_spin: Vec3,
    pub target: Vec3,
    pub target_vel: Vec3,
    /// 0.5 x (neck + head strength) and 0.25 x (neck + head damping) this step: their ratio is how
    /// far ahead the target is led.
    pub strength: f32,
    pub damping: f32,
    /// Neck + head twist maximum (the spine helps beyond 1.2 x this).
    pub twist_max_sum: f32,
    /// Each joint's limits: lean1 min, max, lean2 min, max, twist min, max.
    pub neck_limits: [f32; 6],
    pub head_limits: [f32; 6],
    /// Spine3 and spine2: twist min, max, and the wanted twist they have now.
    pub spine3: [f32; 3],
    pub spine2: [f32; 3],
    /// Look even when the target is outside the front region (flag 0x02).
    pub force: bool,
    /// Measure the up-tilt against the world's up (flag 0x04).
    pub world_up: bool,
}

/// Wanted angles: neck and head (lean1, lean2, twist), and the two upper spine joints' twist.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Angles {
    pub neck: [f32; 3],
    pub head: [f32; 3],
    pub spine3: f32,
    pub spine2: f32,
}

/// The front-region decision, and the angles when it looks (None: nothing is written).
pub fn step(i: &Input) -> (bool, Option<Angles>) {
    let [a, b, c] = i.spine3_rows;
    // reference frame: spine3's rows a and c turned by -0.238196 rad about row b
    let tilt = Quat::from_scaled_axis(b * -0.238196);
    let (ax, fc) = (tilt.rotate(a), tilt.rotate(c));
    let eye = i.spine3_pos + ax * i.eye_offset;
    // the target, led by its motion relative to the body
    let lead = i.damping / i.strength;
    let r = i.target - i.spine3_pos;
    let rel = i.target_vel - i.body_vel - i.body_spin.cross(r);
    let target = i.target + rel * lead;
    let d = (target - i.spine3_pos).normalize_or_zero();
    // front region: a cone that opens from `cone` toward 90 degrees with how far round it is
    let da = d.dot(ax);
    let k = if da > 0.0 { std::f32::consts::FRAC_PI_2 - i.cone } else { i.cone - std::f32::consts::FRAC_PI_2 };
    let in_front = (k * da + i.cone).cos() > d.dot(fc);
    if !(in_front || i.force) {
        return (in_front, None);
    }
    // from the target back to the eyes
    let mut v = (eye - target).normalize_or_zero();
    // twist about the reference axis; beyond 1.2 x the neck + head range the spine takes the rest
    let flat = ax.cross(v).cross(ax).normalize_or_zero();
    let twist_all = acos(flat.dot(fc));
    let limit = i.twist_max_sum * 1.2;
    let mut excess = 0.0;
    if limit < twist_all {
        excess = limit - twist_all;
        if flat.dot(b) < 0.0 {
            excess = -excess;
        }
        let h = excess * -0.5;
        v = Quat { v: ax * h.sin(), w: h.cos() }.rotate(v);
    }
    // the direction's own "up", and the turn that lines it up with the reference axis
    let s = i.up.dot(ax);
    let g = if s * s > 0.25 || i.world_up {
        if i.up.dot(v).abs() < 0.9 { i.up.cross(v) } else { Vec3::new((1.0 - i.up.x) * i.head_row_a.x, (1.0 - i.up.y) * i.head_row_a.y, (1.0 - i.up.z) * i.head_row_a.z) }
    } else {
        ax.cross(v)
    }
    .normalize_or_zero();
    let up = v.cross(g).normalize_or_zero();
    let t1 = acos(up.dot(ax));
    let v2 = Quat::from_scaled_axis(up.cross(ax).normalize_or_zero() * t1).rotate(v).normalize_or_zero();
    let twist = acos(v2.dot(fc)) * if v2.dot(b) > 0.0 { -1.0 } else { 1.0 };
    let m1 = b.cross(up.cross(b)).normalize_or_zero();
    let lean1 = acos(m1.dot(ax)) * if m1.dot(fc) > 0.0 { -1.0 } else { 1.0 };
    let m2 = fc.cross(up.cross(fc)).normalize_or_zero();
    let lean2 = acos(m2.dot(ax)) * if m2.dot(b) < 0.0 { -1.0 } else { 1.0 };
    let (n, hd) = (i.neck_limits, i.head_limits);
    let tw = split(twist, (n[4], n[5]), (hd[4], hd[5]));
    let l1 = split(lean1, (n[0], n[1]), (hd[0], hd[1]));
    let l2 = split(lean2, (n[2], n[3]), (hd[2], hd[3]));
    let spine = |j: [f32; 3], share: f32| (excess * share + j[2]).clamp(j[0] * 1.35, j[1] * 1.35);
    let out = Angles { neck: [l1.0, l2.0, tw.0], head: [l1.1, l2.1, tw.1], spine3: spine(i.spine3, 0.65), spine2: spine(i.spine2, 0.35) };
    (in_front, Some(out))
}

/// An angle split between neck and head by their limits on its side (the max for positive
/// angles, the min for negative), each part kept within +-9 rad.
fn split(angle: f32, neck: (f32, f32), head: (f32, f32)) -> (f32, f32) {
    let (n, h) = if angle > 0.0 {
        (neck.1 / (neck.1 + head.1), head.1 / (neck.1 + head.1))
    } else if angle < 0.0 {
        (neck.0 / (neck.0 + head.0), head.0 / (neck.0 + head.0))
    } else {
        (0.0, 0.0)
    };
    ((n * angle).clamp(-9.0, 9.0), (h * angle).clamp(-9.0, 9.0))
}

fn acos(x: f32) -> f32 {
    x.clamp(-1.0, 1.0).acos()
}
