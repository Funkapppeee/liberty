//! Wrist IK: turn the hand so its palm faces a surface normal at a point. Spec:
//! `specs/euphoria/wristIK.md`. Two steps: predict where the elbow will be from the shoulder's
//! targets, then aim the wrist from the arm's plane. Results are wrist targets (lean1, lean2, twist).

use crate::armik::Frame;
use crate::math::Vec3;

/// Where the elbow ends up with the shoulder at its current targets: the upper arm (along the
/// shoulder's twist axis, `upper` long) rotated by the targets' raw angles (targets − centres; lean2
/// and twist scaled by `scale`) from the shoulder's parent-side frame.
pub fn predict_elbow(shoulder: &Frame, targets: [f32; 3], centre: [f32; 3], upper: f32, scale: f32) -> Vec3 {
    let (s1, s2, tw) = (targets[0] - centre[0], (targets[1] - centre[1]) * scale, (targets[2] - centre[2]) * scale);
    // rotation with these raw angles: twist about z, then the swing
    let sw = (s1 * s1 + s2 * s2).sqrt();
    let (qs, ws) = if sw > 0.0 {
        let k = (sw * 0.5).sin() / sw;
        (Vec3::new(-s2 * k, s1 * k, 0.0), (sw * 0.5).cos())
    } else {
        (Vec3::ZERO, 1.0)
    };
    let (qt, wt) = (Vec3::new(0.0, 0.0, (tw * 0.5).sin()), (tw * 0.5).cos());
    let (v, w) = (qt * ws + qs * wt + qs.cross(qt), ws * wt - qs.dot(qt));
    let p = Vec3::new(0.0, 0.0, upper);
    let r = p * (w * w * 2.0 - 1.0) + v.cross(p) * (w * 2.0) + v * (v.dot(p) * 2.0);
    shoulder.pos + shoulder.rows[0] * r.x + shoulder.rows[1] * r.y + shoulder.rows[2] * r.z
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aim {
    /// Wrist targets (lean1, lean2, twist).
    pub wrist: [f32; 3],
    /// −(normal · the side-signed plane normal).
    pub ret: f32,
}

/// Aim the wrist so the hand's palm meets `normal` at `point`. `side` is +1 for the left hand and −1
/// for the right; `weight` limits the twist; `use_wrist_frame` forces the fallback frame.
#[allow(clippy::too_many_arguments)]
pub fn aim(point: Vec3, normal: Vec3, elbow: Vec3, shoulder: Vec3, wrist: &Frame, side: f32, use_wrist_frame: bool, weight: f32) -> Aim {
    let a = point - elbow;
    let n = a.cross(shoulder - elbow);
    let ln2 = n.dot(n);
    let ln = ln2.sqrt();
    let n_hat = if ln2 == 0.0 { Vec3::ZERO } else { n * (1.0 / ln) };
    // the arm's frame: plane normal, along the forearm, and their cross; or the wrist's own
    let (nh, m, along) = if ln <= 0.05 || use_wrist_frame {
        (wrist.rows[2].cross(wrist.rows[0]), wrist.rows[0], wrist.rows[2])
    } else {
        let la2 = a.dot(a);
        let ah = if la2 == 0.0 { Vec3::ZERO } else { a * (1.0 / la2.sqrt()) };
        (n_hat, n_hat.cross(ah), ah)
    };
    let nh = nh * side;
    let (x, y) = (-normal.dot(nh), normal.dot(m));
    let h = (x * x + y * y).sqrt() + 1e-6;
    let lim = h * weight;
    let twist = ((y / h) as f64).atan2((x / h) as f64) as f32;
    let twist = twist.clamp(-lim, lim);
    let (s, c) = (twist.sin(), twist.cos());
    let u = m * s - nh * c;
    let swing = (normal.dot(along) as f64).atan2(normal.dot(u) as f64) as f32;
    Aim { wrist: [-(swing * s), swing * c, twist], ret: -normal.dot(nh) }
}
