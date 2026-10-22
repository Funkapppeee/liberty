//! Minimal f32 vector maths. Behaviours run in f32 like the game's stored values.

use std::ops::{Add, Mul, Neg, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    pub const Z: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 1.0 };

    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3 { x, y, z }
    }

    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// Unit vector, or zero when the length is zero.
    pub fn normalize_or_zero(self) -> Vec3 {
        let len = self.length();
        if len > 0.0 { self * (1.0 / len) } else { Vec3::ZERO }
    }

    /// This vector with its component along the unit vector `axis` removed.
    pub fn reject(self, axis: Vec3) -> Vec3 {
        self - axis * self.dot(axis)
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl Mul<f32> for Vec3 {
    type Output = Vec3;
    fn mul(self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}

impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

impl Vec3 {
    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
}

/// A rotation quaternion (vector part, scalar part).
#[derive(Clone, Copy, Debug)]
pub struct Quat {
    pub v: Vec3,
    pub w: f32,
}

impl Quat {
    pub const IDENTITY: Quat = Quat { v: Vec3::ZERO, w: 1.0 };

    /// Rotation by |axis_angle| about its direction.
    pub fn from_scaled_axis(a: Vec3) -> Quat {
        let m2 = a.dot(a);
        if m2 <= 0.0 {
            return Quat::IDENTITY;
        }
        let m = m2.sqrt();
        let h = m * 0.5;
        Quat { v: a * ((1.0 / m) * h.sin()), w: h.cos() }
    }

    /// Shortest arc taking direction u onto direction v.
    pub fn arc(u: Vec3, v: Vec3) -> Quat {
        let (u, v) = (u.normalize_or_zero(), v.normalize_or_zero());
        let q = Quat { v: u.cross(v), w: 1.0 + u.dot(v) };
        let n = (q.v.dot(q.v) + q.w * q.w).sqrt();
        Quat { v: q.v * (1.0 / n), w: q.w / n }
    }

    pub fn mul(self, b: Quat) -> Quat {
        Quat { v: b.v * self.w + self.v * b.w + self.v.cross(b.v), w: self.w * b.w - self.v.dot(b.v) }
    }

    pub fn rotate(self, p: Vec3) -> Vec3 {
        p * (self.w * self.w * 2.0 - 1.0) + self.v.cross(p) * (self.w * 2.0) + self.v * (self.v.dot(p) * 2.0)
    }

    /// Twist about local Z and the swing split over the two lean axes, each as 4·atan of the
    /// quarter-angle tangent: (twist, swing1, swing2).
    pub fn twist_swing(self) -> (f32, f32, f32) {
        let (x, y, z, w) = (self.v.x, self.v.y, self.v.z, self.w);
        if (x * x + y * y + z * z).sqrt() <= 0.0 {
            return (0.0, 0.0, 0.0);
        }
        let r2 = w * w + z * z;
        let (t, s1, s2) = if r2 <= 1.2e-7 {
            let k = 1.0 / (1.0 - r2).sqrt();
            (0.0, k * y, -(k * x))
        } else {
            let r = r2.sqrt();
            let k = 1.0 / ((r + 1.0) * r);
            let t = if w > 0.0 { z / (w + r) } else { z / (w - r) };
            (t, (w * y + x * z) * k, (y * z - w * x) * k)
        };
        let m = (s1 * s1 + s2 * s2).sqrt();
        let swing = m.atan() * 4.0;
        let k = if m > 0.0 { swing / m } else { 0.0 };
        (t.atan() * 4.0, s1 * k, s2 * k)
    }

    /// The inverse of `twist_swing`: (twist, swing1, swing2) back to a rotation, via tan of the quarter
    /// angles.
    pub fn from_twist_swing(twist: f32, s1: f32, s2: f32) -> Quat {
        if (twist * twist + s1 * s1 + s2 * s2).sqrt() <= 0.0 {
            return Quat::IDENTITY;
        }
        let s = (s2 * s2 + s1 * s1).sqrt();
        let (ts, tt) = ((s * 0.25).tan(), (twist * 0.25).tan());
        let (a, b) = if s > 0.0 { (ts * s1 / s, ts * s2 / s) } else { (0.0, 0.0) };
        let f6 = 2.0 / (b * b + 1.0 + a * a);
        let f4 = f6 - 1.0;
        let f2 = 2.0 / (tt * tt + 1.0);
        let f5 = f2 - 1.0;
        Quat { v: Vec3::new((tt * f2 * a - f5 * b) * f6, (tt * f2 * b + f5 * a) * f6, f4 * tt * f2), w: f4 * f5 }
    }
}
