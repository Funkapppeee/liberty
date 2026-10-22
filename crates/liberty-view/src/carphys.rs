//! Vehicle physics driven by GTA IV's handling.dat the way IV uses it: a rigid body (mass, centre
//! of mass, box inertia) on raycast suspensions with IV's spring and damper, IV's tyre rules (a
//! traction curve over slip, a slip state that spins up or locks past the curve, load sensitivity,
//! lateral slip sharper with speed), IV's gearbox, rolling drag and air drag, and impulse contacts
//! for walls. Behaviour and numbers from IV (answer key: ed-slopper/large-steal-car-4's in-game
//! checked recreation, read for behaviour only, and the 1 Oct session's driving); our own code and
//! structure. Bevy space: Y up, car faces −Z.

use bevy::math::{Mat3, Quat, Vec3};

use crate::collision::{self, ChunkCollision};

const G: f32 = 9.81;

#[derive(Clone, Debug)]
pub struct Params {
    pub mass: f32,
    /// Centre of mass relative to the model origin (local).
    pub com: Vec3,
    /// Diagonal of the local inverse inertia tensor.
    pub inv_inertia: Vec3,
    /// Engine force in g, split front/rear by `drive_bias_front` (1 = FWD, 0 = RWD).
    pub drive_force: f32,
    pub drive_bias_front: f32,
    pub top_speed: f32,
    pub brake_force: f32,
    pub brake_bias_front: f32,
    pub steer_lock: f32,
    pub traction_max: f32,
    pub traction_min: f32,
    /// Slip angle (radians) where lateral grip peaks.
    pub traction_peak: f32,
    pub traction_bias_front: f32,
    pub susp_force: f32,
    pub susp_comp_damp: f32,
    pub susp_rebound_damp: f32,
    /// Travel above / below the rest wheel position (upper > 0, lower < 0), metres.
    pub susp_upper: f32,
    pub susp_lower: f32,
    pub susp_bias_front: f32,
    pub drag: f32,
    /// The handbrake's brake on each rear wheel, in the car's weight.
    pub handbrake: f32,
    /// Front / rear shares (IV's split of a 0..1 bias: under 0.1 all rear, over 0.9 all front,
    /// else 2 x bias and 2 x (1 - bias)) of drive, brakes, traction and damping.
    pub drive_share: [f32; 2],
    pub brake_share: [f32; 2],
    pub traction_share: [f32; 2],
    pub susp_share: [f32; 2],
    /// Damping multiplier on the handling's damping columns: 0.1, or 0.3 with handling flag 0x20000000.
    pub damp_mult: f32,
    pub gears: u8,
    pub drive_inertia: f32,
    /// Where the engine hits its limiter in top gear: 1.2 x the handling top speed (m/s).
    pub max_flat: f32,
}

impl Params {
    /// From handling.dat, plus the body box (local min/max) for the inertia.
    pub fn from_handling(h: &rage::vehicles::Handling, lo: Vec3, hi: Vec3) -> Params {
        let [cx, cy, cz] = h.centre_of_mass;
        Params {
            mass: h.mass.max(200.0),
            com: Vec3::new(cx, cz, -cy),
            inv_inertia: box_inv_inertia(h.mass.max(200.0), lo, hi),
            drive_force: h.drive_force,
            drive_bias_front: h.drive_bias_front.clamp(0.0, 1.0),
            top_speed: (h.max_velocity_kmh / 3.6).max(10.0),
            brake_force: h.brake_force,
            brake_bias_front: h.brake_bias_front.clamp(0.0, 1.0),
            steer_lock: h.steering_lock_deg.clamp(10.0, 55.0).to_radians(),
            traction_max: h.traction_curve_max.max(0.3),
            traction_min: h.traction_curve_min.max(0.2),
            traction_peak: h.traction_curve_lateral_deg.clamp(4.0, 40.0).to_radians(),
            traction_bias_front: h.traction_bias_front.clamp(0.1, 0.9),
            susp_force: h.suspension_force.max(0.3),
            susp_comp_damp: h.suspension_comp_damp.max(0.1),
            susp_rebound_damp: h.suspension_rebound_damp.max(0.1),
            susp_upper: h.suspension_upper.clamp(0.02, 0.5),
            susp_lower: h.suspension_lower.clamp(-0.5, -0.02),
            susp_bias_front: h.suspension_bias_front.clamp(0.1, 0.9),
            drag: h.drag_mult,
            handbrake: h.handbrake.max(0.0),
            drive_share: split(h.drive_bias_front),
            brake_share: split(h.brake_bias_front),
            traction_share: split(h.traction_bias_front),
            susp_share: split(h.suspension_bias_front),
            damp_mult: if h.handling_flags & 0x2000_0000 != 0 { 0.3 } else { 0.1 },
            gears: (h.gears as u8).clamp(1, 7),
            drive_inertia: h.drive_inertia.max(0.1),
            max_flat: (h.max_velocity_kmh / 3.6).max(10.0) * 1.2,
        }
    }

    /// An Admiral-like saloon for models without handling data.
    pub fn fallback(lo: Vec3, hi: Vec3) -> Params {
        Params {
            mass: 1700.0,
            com: Vec3::new(0.0, -0.2, 0.0),
            inv_inertia: box_inv_inertia(1700.0, lo, hi),
            drive_force: 0.17,
            drive_bias_front: 0.0,
            top_speed: 140.0 / 3.6,
            brake_force: 0.22,
            brake_bias_front: 0.65,
            steer_lock: 35f32.to_radians(),
            traction_max: 1.2,
            traction_min: 0.95,
            traction_peak: 14f32.to_radians(),
            traction_bias_front: 0.47,
            susp_force: 1.6,
            susp_comp_damp: 1.0,
            susp_rebound_damp: 1.0,
            susp_upper: 0.15,
            susp_lower: -0.16,
            susp_bias_front: 0.5,
            drag: 6.0,
            handbrake: 0.7,
            drive_share: split(0.0),
            brake_share: split(0.65),
            traction_share: split(0.47),
            susp_share: split(0.5),
            damp_mult: 0.1,
            gears: 5,
            drive_inertia: 1.0,
            max_flat: 140.0 / 3.6 * 1.2,
        }
    }
}

/// IV's split of a 0..1 front bias into front / rear shares.
fn split(bias: f32) -> [f32; 2] {
    if bias < 0.1 {
        [0.0, 1.0]
    } else if bias > 0.9 {
        [1.0, 0.0]
    } else {
        [bias * 2.0, (1.0 - bias) * 2.0]
    }
}

fn axle(w: &Wheel) -> usize {
    if w.front { 0 } else { 1 }
}

/// Where the tyre feels the ground, as fractions of its radius ahead of / behind the axle.
const WHEEL_PROBES: [f32; 5] = [-0.8, -0.4, 0.0, 0.4, 0.8];

/// Where a wheel's suspension ray starts, above its rest (model) position: the top of its travel
/// (handling's upper limit). At the modelled wheel position the suspension is compressed by
/// -lower, where IV's spring carries exactly the wheel's static share of the weight.
pub fn mount(p: &Params, _front: bool) -> f32 {
    p.susp_upper
}

fn box_inv_inertia(mass: f32, lo: Vec3, hi: Vec3) -> Vec3 {
    let d = (hi - lo).abs().max(Vec3::splat(0.5));
    // a solid box, fattened a bit: real cars carry their mass low and outboard
    let i = Vec3::new(d.y * d.y + d.z * d.z, d.x * d.x + d.z * d.z, d.x * d.x + d.y * d.y) * (mass / 12.0 * 1.1);
    Vec3::ONE / i
}

#[derive(Clone, Debug)]
pub struct Wheel {
    /// Wheel centre at rest, local.
    pub rest: Vec3,
    pub front: bool,
    pub right: bool,
    pub radius: f32,
    /// Suspension compression, metres (0 = fully extended).
    pub compression: f32,
    pub contact: bool,
    /// Rolling angle for the visuals.
    pub spin: f32,
    /// The tyre's slip state along the wheel (IV's: 0.4 = the curve's peak; driving negative).
    pub slip: f32,
}

#[derive(Clone, Debug)]
pub struct Body {
    pub pos: Vec3,
    pub rot: Quat,
    pub vel: Vec3,
    pub ang: Vec3,
    pub gearbox: Gearbox,
}

/// IV's gearbox, in behaviour: `gears` forward ratios from 3.33 down to 1.0 (geometric), reverse
/// -3.33; the engine speed is ratio x speed / (1.2 x the handling top speed), 1 = the limiter.
/// Up a gear past 0.93 of a load factor (0.7 + 0.3 x pedal) with the throttle on and the driven
/// wheels down; down when the gear below would turn under 0.75 of it; no change back within 2 s
/// (down after up: 3 s) while the throttle stays past 0.7. A shift drops the clutch to 0.1, which
/// comes back at 1.3 a second (0.6 for a drive inertia under 1); above first gear nothing drives
/// while it's under 0.6. Force = throttle x ratio x drive force (in the car's weight), x the clutch
/// (doubled, at most 1), cut at the limiter. The handbrake holds the clutch out.
#[derive(Clone, Debug)]
pub struct Gearbox {
    /// 0 reverse, 1.. forward.
    pub gear: i8,
    /// Engine speed, 0.2 idle .. 1 limiter.
    pub revs: f32,
    clutch: f32,
    /// Seconds since the last change, whether it was up, and whether the throttle has stayed on since.
    since: f32,
    up: bool,
    held: bool,
}

impl Default for Gearbox {
    fn default() -> Self {
        Gearbox { gear: 1, revs: 0.2, clutch: 1.0, since: 10.0, up: false, held: false }
    }
}

impl Gearbox {
    fn ratio(p: &Params, gear: i8) -> f32 {
        let n = p.gears.clamp(1, 7) as i32;
        match gear {
            g if g <= 0 => -10.0 / 3.0,
            _ if n == 1 => 1.0,
            g => (10.0f32 / 3.0) * (3.0f32 / 10.0).powf((g as i32 - 1).min(n - 1) as f32 / (n - 1) as f32),
        }
    }

    /// One step: `throttle` -1..1 (negative = reverse), `brake` 0..1, `speed` along the car (m/s).
    /// Returns the total drive force, in the car's weight.
    #[allow(clippy::too_many_arguments)]
    pub fn drive(&mut self, p: &Params, throttle: f32, brake: f32, handbrake: bool, speed: f32, grounded: bool, dt: f32) -> f32 {
        let per = 1.0 / p.max_flat;
        self.since += dt;
        if throttle.abs() < 0.7 {
            self.held = false;
        }
        if self.clutch < 1.0 {
            self.clutch = (self.clutch + if p.drive_inertia < 1.0 { 0.6 } else { 1.3 } * dt).min(1.0);
        }
        let shift = |g: &mut Self, to: i8, up: bool| {
            (g.gear, g.clutch, g.since, g.up, g.held) = (to, 0.1, 0.0, up, true);
        };
        let load = (brake.abs() * 0.9 + throttle.abs()).min(1.0) * 0.3 + 0.7;
        if throttle.abs() < 0.01 && Self::ratio(p, 1) * per * speed < 0.2 {
            self.clutch = 0.1;
        } else if throttle < 0.0 && Self::ratio(p, 0) * per * speed > -0.5 {
            if self.gear != 0 {
                shift(self, 0, false);
            }
        } else if throttle > 0.0 && self.gear == 0 && Self::ratio(p, 1) * per * speed < 0.5 {
            shift(self, 1, true);
        } else if self.gear > 0 {
            let g = self.gear;
            let can_up = !(!self.up && self.held && self.since < 2.0);
            let can_down = !(self.up && self.held && self.since < 3.0);
            if Self::ratio(p, g) * per * speed > load * 0.93 {
                if (g as u8) < p.gears && throttle > 0.0 && grounded && can_up {
                    shift(self, g + 1, true);
                }
            } else if g > 1 && Self::ratio(p, g - 1) * per * speed < load * 0.75 && can_down {
                shift(self, g - 1, false);
            }
        }
        // force
        let mut thr = throttle;
        if (thr < 0.0 && self.gear > 0) || (thr > 0.0 && self.gear == 0) {
            return 0.0;
        }
        if !handbrake && self.clutch < 0.6 && self.gear > 1 {
            thr = 0.0;
        }
        let clutch = if handbrake { 0.0 } else { self.clutch };
        let ratio = Self::ratio(p, self.gear);
        let target = (thr.abs() * (1.0 - clutch) + clutch * ratio * per * speed).max(0.2);
        let step = p.drive_inertia * dt;
        self.revs = if self.revs < target { (self.revs + step).min(target) } else { (self.revs - step).max(target) };
        if self.revs > 1.0 {
            self.revs = 0.9;
            thr *= 0.99;
        } else if ratio * per * speed > 1.0 {
            thr = 0.0;
        }
        if self.gear < 2 && self.revs < 0.5 {
            self.clutch = self.clutch.min((self.revs * 2.0).max(0.2));
        }
        thr.abs() * ratio * (clutch * 2.0).min(1.0) * p.drive_force * (self.revs / target)
    }
}



#[derive(Clone, Copy, Default)]
pub struct Input {
    /// 0..1
    pub throttle: f32,
    /// 0..1: brakes when moving forward, reverse when (almost) stopped.
    pub reverse: f32,
    pub handbrake: bool,
    /// Front wheel angle, radians (+ = left); `steer_angle` turns the stick into it IV's way.
    pub steer: f32,
}

/// Points on the body used to find walls (local): a ring at bumper height, high enough that its
/// 0.3 m spheres clear kerbs (those are the suspension's job).
pub fn hull_points(lo: Vec3, hi: Vec3, wheel_y: f32) -> Vec<Vec3> {
    let y = wheel_y + 0.6;
    let (x0, x1, z0, z1) = (lo.x * 0.92, hi.x * 0.92, lo.z * 0.95, hi.z * 0.95);
    let (xm, zm) = ((x0 + x1) / 2.0, (z0 + z1) / 2.0);
    vec![
        Vec3::new(x0, y, z0),
        Vec3::new(x1, y, z0),
        Vec3::new(x0, y, z1),
        Vec3::new(x1, y, z1),
        Vec3::new(x0, y, zm),
        Vec3::new(x1, y, zm),
        Vec3::new(xm, y, z0),
        Vec3::new(xm, y, z1),
        // roof corners, for when it rolls
        Vec3::new(x0 * 0.8, hi.y * 0.9, zm),
        Vec3::new(x1 * 0.8, hi.y * 0.9, zm),
    ]
}

/// IV's steering for a driven car: the stick eases toward the input at 10 a second standing, down
/// to 5 at 30 m/s forward; the wheels turn stick x the handling lock, divided by 1 + 0.1 per m/s
/// over 5 m/s (unless countersteering), plus the body's slide angle (at most 10 degrees) above
/// 1 m/s, within the lock. Returns the new stick and the wheel angle.
pub fn steer_angle(p: &Params, body: &Body, stick: f32, input: f32, dt: f32) -> (f32, f32) {
    let fwd = body.rot * Vec3::NEG_Z;
    let right = body.rot * Vec3::X;
    let (v_fwd, v_side) = (body.vel.dot(fwd), body.vel.dot(right));
    let f = v_fwd / 30.0;
    let rate = if f > 1.0 { 5.0 } else if f > 0.0 { (1.0 - f) * 10.0 + f * 5.0 } else { 10.0 };
    let stick = (stick + (input - stick) * (dt * rate).min(1.0)).clamp(-1.0, 1.0);
    let lock = p.steer_lock;
    let mut angle = stick * lock;
    if v_fwd > 5.0 && angle * v_side >= -0.1 * v_fwd.abs() {
        angle /= (v_fwd - 5.0) * 0.1 + 1.0;
    }
    if v_fwd > 1.0 {
        let slide = (-v_side).atan2(v_fwd).clamp(-10f32.to_radians(), 10f32.to_radians());
        angle = (slide + angle).clamp(-lock, lock);
    }
    (stick, angle)
}

fn inv_inertia_world(p: &Params, rot: Quat) -> Mat3 {
    let r = Mat3::from_quat(rot);
    r * Mat3::from_diagonal(p.inv_inertia) * r.transpose()
}

/// Advance one sub-step. Returns false (and does nothing) if no collision is loaded under the car.
pub fn step<'a>(
    body: &mut Body,
    p: &Params,
    wheels: &mut [Wheel],
    hull: &[Vec3],
    input: Input,
    chunks: impl IntoIterator<Item = &'a ChunkCollision> + Clone,
    dt: f32,
) -> bool {
    if collision::ground_height(chunks.clone(), body.pos.x, body.pos.z, body.pos.y + 200.0).is_none() {
        return false;
    }
    let up = body.rot * Vec3::Y;
    let com_w = body.pos + body.rot * p.com;
    let inv_i = inv_inertia_world(p, body.rot);
    let weight = p.mass * G;
    let mut force = Vec3::new(0.0, -weight, 0.0);
    let mut torque = Vec3::ZERO;
    let apply = |f: Vec3, at: Vec3, force: &mut Vec3, torque: &mut Vec3| {
        *force += f;
        *torque += (at - com_w).cross(f);
    };

    let car_fwd = body.rot * Vec3::NEG_Z;
    let speed_fwd = body.vel.dot(car_fwd);
    let travel = p.susp_upper - p.susp_lower;
    let per_wheel_mass = p.mass / wheels.len().max(1) as f32;

    // pedals as IV reads them: the brake pedal brakes while rolling forward, and drives backward
    // (reverse gear) once nearly stopped; throttle while rolling backward drives forward
    let (throttle, brake) = if input.reverse > 0.0 && speed_fwd > 0.5 {
        (if input.throttle > 0.6 { input.throttle } else { 0.0 }, input.reverse) // both pedals: a burnout
    } else if input.reverse > 0.0 && input.throttle == 0.0 {
        (-input.reverse, 0.0)
    } else {
        (input.throttle, 0.0)
    };
    // the gearbox's force for each driven wheel, in the car's weight
    let driven_wheels = wheels.iter().filter(|w| p.drive_share[axle(w)] > 0.0).count().max(1);
    let all_driven_down = wheels.iter().filter(|w| p.drive_share[axle(w)] > 0.0).all(|w| w.contact);
    let drive = body.gearbox.drive(p, throttle, brake, input.handbrake, speed_fwd, all_driven_down, dt) / driven_wheels as f32;

    // static load shares: from where the centre of mass sits between the axles (the wheels share
    // each axle's half)
    let axle_z = |front: bool| {
        let z: Vec<f32> = wheels.iter().filter(|w| w.front == front).map(|w| w.rest.z).collect();
        z.iter().sum::<f32>() / z.len().max(1) as f32
    };
    let (zf, zr) = (axle_z(true), axle_z(false));
    let half_base = ((zr - zf) * 0.5).max(0.1);
    let lean = ((((zf + zr) * 0.5) - p.com.z) / half_base).clamp(-0.9, 0.9);
    let on_axle = |front: bool| wheels_on(wheels, front).max(1) as f32;
    let (front_n, rear_n) = (on_axle(true), on_axle(false));

    for w in wheels.iter_mut() {
        let ax = axle(w);
        let share = 0.5 * (1.0 + if w.front { lean } else { -lean }) / if w.front { front_n } else { rear_n };
        let top = body.pos + body.rot * (w.rest + Vec3::Y * mount(p, w.front));
        // the tyre is round: its lowest centre over the ground across its footprint (along the car),
        // so a kerb or bump lifts it as it rolls on, not all at once under the axle (a single ray
        // under it took a 12 cm kerb in one physics step: a ~6 g jolt)
        let along = Vec3::new(car_fwd.x, 0.0, car_fwd.z).normalize_or_zero();
        let centre_y = WHEEL_PROBES
            .iter()
            .filter_map(|&f| {
                let x = f * w.radius;
                let at = top + along * x;
                collision::ground_height(chunks.clone(), at.x, at.z, top.y + 0.05).map(|g| g + (w.radius * w.radius - x * x).sqrt())
            })
            .reduce(f32::max);
        let ext = centre_y.map_or(f32::MAX, |c| (top.y - c).max(0.0));
        let prev = w.compression;
        if ext > travel {
            w.contact = false;
            w.compression = 0.0;
            w.slip = 0.0;
            continue;
        }
        w.contact = true;
        w.compression = (travel - ext).clamp(0.0, travel);

        // IV's spring: the wheel's static share of the weight at the modelled wheel position
        // (compressed by -lower), plus the handling's suspension force x the car's weight per metre
        // from there (x 0.75 at the rear), never pulling and never more than the car's weight. The
        // damper: compression or rebound damping (x 0.1; x 0.3 with handling flag 0x20000000) x the
        // axle's suspension share x the weight x the compression rate, never pulling harder than the
        // spring pushes. Soft and lightly damped: IV's cars lean, pitch and bob. No anti-roll bar.
        let k = p.susp_force * if w.front { 1.0 } else { 0.75 };
        let spring = (k * (w.compression + p.susp_lower) + share).clamp(0.0, 1.0) * weight;
        let rate = ((w.compression - prev) / dt).clamp(-0.5 * travel / dt, 0.5 * travel / dt);
        let c = if rate > 0.0 { p.susp_comp_damp } else { p.susp_rebound_damp } * p.damp_mult;
        let damp = (p.susp_share[ax] * c * weight * rate).max(-spring);
        // bottoming out: a stiff bump stop, rising over the last 3 cm of travel (was a 3 g step at 1 cm)
        let stop = per_wheel_mass * G * 3.0 * ((0.03 - ext) / 0.03).clamp(0.0, 1.0);
        let centre = top - Vec3::Y * ext;
        let contact = centre - Vec3::Y * w.radius;
        apply(up * (spring + damp + stop), contact, &mut force, &mut torque);
        let load = (spring + damp.min(0.0) + stop).min(weight * 4.0);
        if load <= weight * 0.01 {
            continue;
        }

        // tyre: the contact patch's velocity along the wheel and across it
        let steer_q = if w.front { Quat::from_rotation_y(input.steer) } else { Quat::IDENTITY };
        let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).normalize_or_zero();
        let fwd = flat(body.rot * steer_q * Vec3::NEG_Z);
        let side = flat(body.rot * steer_q * Vec3::X);
        let v_p = body.vel + body.ang.cross(contact - com_w);
        let (v_long, v_lat) = (v_p.dot(fwd), v_p.dot(side));
        w.spin += v_long / w.radius * dt;
        let speed = v_long.hypot(v_lat);

        // what the wheel is asked for, in newtons: drive from the gearbox (its axle's share), brake
        // pedal x brake force x its axle's share, the handbrake's force on the rear (and no drive
        // there); a wheel not driven this step drags 0.05 of the car's weight (rolling); brake beats
        // drive and what's left of the larger acts
        let mut d = drive * p.drive_share[ax];
        let mut b = brake * p.brake_force * p.brake_share[ax];
        if input.handbrake && !w.front {
            b += p.handbrake;
            d = 0.0;
        }
        if d == 0.0 {
            b += 0.05;
        }
        let (d, b) = (d * weight, b * weight);
        let (drive_n, brake_n) = if b > d.abs() {
            (0.0, b)
        } else if d > b {
            (d - b, 0.0)
        } else {
            (d + b, 0.0)
        };

        // grip: the load against the wheel's share (an overloaded tyre grips less per newton, so
        // weight transfer costs grip), the axle's traction share
        let n_static = share * weight;
        let mut l = load / n_static;
        l *= 1.0 - 0.1 * (l - 1.0).min(5.0);
        let n = n_static * l;
        let bias = p.traction_share[ax];
        let curve = |x: f32| -> f32 {
            let x = x.abs();
            let mu = if x < 1.0 {
                x * p.traction_max
            } else if x < 2.5 {
                p.traction_max + (p.traction_min - p.traction_max) * (x - 1.0) / 2.5
            } else {
                p.traction_min
            };
            mu * bias
        };
        // sideways slip: the slip angle over the peak angle, sharper with speed (at 20 m/s the peak
        // comes at 1 / 2.4 of the handling angle); 0.4 is the curve's peak
        let speed_k = 1.0 + (0.07 * speed).min(5.0);
        let lat = (-v_lat).atan2(v_long.abs().max(1.0)) / p.traction_peak * 0.4 * speed_k;
        // along the wheel: a slip state. Under the curve's limit the tyre gives exactly what's asked;
        // over it the wheel spins up (driving) or locks (braking) and the grip falls to the minimum
        let s0 = w.slip;
        let linear = |demand: f32, dir: f32| demand / (p.traction_max * bias) * 0.4 * dir;
        let settle = |demand: f32, dir: f32| ((p.traction_max - demand) / (p.traction_max - p.traction_min).max(0.01) * 1.5 + 1.0) * dir * 0.4;
        let slip = if drive_n != 0.0 {
            let dir = if drive_n > 0.0 { -1.0 } else { 1.0 };
            let demand = drive_n.abs() / n;
            let mut spinning = !(demand < p.traction_min * bias || s0.abs() < 0.4 || dir * s0 < 0.0);
            let mut s = linear(demand, dir);
            if !spinning {
                if s == 0.0 {
                    spinning = true;
                } else if s * s0 >= 0.0 && s.abs() < s0.abs() {
                    s = s0 + (s - s0) * (dt * 5.0).min(1.0);
                }
            }
            if spinning {
                let got = curve(s0 * 2.5);
                s = if got < demand { s0 + (demand - got) * dir * dt * 2.0 } else { settle(demand, dir) };
                s = s.clamp(-5.0, 5.0);
            }
            s
        } else {
            let dir = if v_long > 0.0 { 1.0 } else { -1.0 };
            let demand = brake_n.min(v_long.abs() * weight) / n;
            let s = if demand < p.traction_min * bias || s0.abs() < 0.4 || dir * s0 < 0.0 {
                linear(demand, dir)
            } else {
                let got = curve(s0 * 2.5);
                if got < demand { s0 + (demand - got) * if got >= 0.9 { 10.0 } else { 2.0 } * dir * dt } else { settle(demand, dir) }
            };
            s.clamp(-10.0, 10.0)
        };
        w.slip = slip;
        // both slips as one, sized by the curve, along the slip direction; a locked wheel sliding
        // sideways pushes against the patch's motion instead
        let long = -slip;
        let total = lat.hypot(long);
        let (mut fl, mut fs) = (0.0, 0.0);
        if total > 0.0 {
            let f = curve(total * 2.5) * n;
            if brake_n > 0.0 && speed > 1.0 && total > 2.25 && (slip * v_lat).abs() > (lat * v_long).abs() {
                (fl, fs) = (-v_long / speed * f, -v_lat / speed * f);
            } else {
                (fl, fs) = (long / total * f, lat / total * f);
            }
        }
        // our stand-ins for IV's low-speed traction spring: never push harder sideways than it takes
        // to stop the slide this step, and hold still when stopped with no pedal
        fs = fs.clamp(-v_lat.abs() * per_wheel_mass / dt, v_lat.abs() * per_wheel_mass / dt);
        if throttle == 0.0 && brake == 0.0 && v_long.abs() < 1.0 {
            fl -= v_long * per_wheel_mass * 4.0;
        }
        apply(fwd * fl + side * fs, contact, &mut force, &mut torque);
    }

    // air drag, IV's: the handling drag x 1e-4 x the velocity x its size, per unit mass
    force -= body.vel * body.vel.length() * p.drag * 1e-4 * p.mass;

    body.vel += force / p.mass * dt;
    body.ang += inv_i * torque * dt;
    body.ang *= 1.0 - (0.1 * dt).min(0.5);
    body.pos += body.vel * dt;
    let w = body.ang;
    body.rot = (body.rot + Quat::from_xyzw(w.x, w.y, w.z, 0.0) * body.rot * (0.5 * dt)).normalize();

    // walls: push the hull out and bounce the velocity at the contact point
    for &local in hull {
        let pt = body.pos + body.rot * local;
        let out = collision::push_out_of_walls(chunks.clone(), pt, 0.3, &[0.0]);
        let d = out - pt;
        let len = d.length();
        if len < 1e-4 {
            continue;
        }
        let nrm = d / len;
        body.pos += d * 0.8;
        let com_w = body.pos + body.rot * p.com;
        let r = pt - com_w;
        let v_p = body.vel + body.ang.cross(r);
        let vn = v_p.dot(nrm);
        if vn < 0.0 {
            let inv_i = inv_inertia_world(p, body.rot);
            let k = 1.0 / p.mass + nrm.dot((inv_i * r.cross(nrm)).cross(r));
            let j = -(1.0 + 0.25) * vn / k;
            body.vel += nrm * j / p.mass;
            body.ang += inv_i * r.cross(nrm * j);
            // scrape: lose some speed along the wall too
            let vt = v_p - nrm * vn;
            body.vel -= vt * 0.08;
        }
    }
    true
}

/// Car against car: each body is a box (local `lo..hi`). Corners and edge points of one box that
/// end up inside the other are pushed out along the shallowest side of the box they hit (level
/// axes only — cars meet side-on or nose-on, not stacked), and both bodies get the impulse at the
/// contact point, with their masses and inertias. Returns the largest impulse (N·s), 0 if no touch.
pub fn collide(a: &mut Body, pa: &Params, abox: (Vec3, Vec3), b: &mut Body, pb: &Params, bbox: (Vec3, Vec3)) -> f32 {
    let mut worst = 0.0f32;
    for flip in [false, true] {
        let (s, ps, sbox, o, po, obox) = if flip { (&mut *b, pb, bbox, &mut *a, pa, abox) } else { (&mut *a, pa, abox, &mut *b, pb, bbox) };
        let (lo, hi) = sbox;
        let mid_y = lo.y + (hi.y - lo.y) * 0.35; // bumper height
        let (xm, zm) = ((lo.x + hi.x) / 2.0, (lo.z + hi.z) / 2.0);
        let points = [
            Vec3::new(lo.x, mid_y, lo.z), Vec3::new(hi.x, mid_y, lo.z), Vec3::new(lo.x, mid_y, hi.z), Vec3::new(hi.x, mid_y, hi.z),
            Vec3::new(xm, mid_y, lo.z), Vec3::new(xm, mid_y, hi.z), Vec3::new(lo.x, mid_y, zm), Vec3::new(hi.x, mid_y, zm),
        ];
        let (olo, ohi) = obox;
        for local in points {
            let pt = s.pos + s.rot * local;
            let q = o.rot.inverse() * (pt - o.pos);
            if q.x <= olo.x || q.x >= ohi.x || q.z <= olo.z || q.z >= ohi.z || q.y <= olo.y - 0.3 || q.y >= ohi.y {
                continue;
            }
            // shallowest way out of the other box, in its local x/z
            let outs = [(q.x - olo.x, Vec3::NEG_X), (ohi.x - q.x, Vec3::X), (q.z - olo.z, Vec3::NEG_Z), (ohi.z - q.z, Vec3::Z)];
            let (depth, axis) = outs.into_iter().fold((f32::MAX, Vec3::X), |m, x| if x.0 < m.0 { x } else { m });
            let nrm = (o.rot * axis).with_y(0.0).normalize_or_zero(); // pushes `s` out of `o`
            if nrm == Vec3::ZERO {
                continue;
            }
            // separate by mass
            let (ms, mo) = (ps.mass, po.mass);
            s.pos += nrm * depth * mo / (ms + mo);
            o.pos -= nrm * depth * ms / (ms + mo);
            // impulse at the contact
            let (rs, ro) = (pt - (s.pos + s.rot * ps.com), pt - (o.pos + o.rot * po.com));
            let (is, io) = (inv_inertia_world(ps, s.rot), inv_inertia_world(po, o.rot));
            let v_rel = (s.vel + s.ang.cross(rs)) - (o.vel + o.ang.cross(ro));
            let vn = v_rel.dot(nrm);
            if vn >= 0.0 {
                continue;
            }
            let k = 1.0 / ms + 1.0 / mo + nrm.dot((is * rs.cross(nrm)).cross(rs)) + nrm.dot((io * ro.cross(nrm)).cross(ro));
            let j = -(1.0 + 0.2) * vn / k;
            s.vel += nrm * j / ms;
            s.ang += is * rs.cross(nrm * j);
            o.vel -= nrm * j / mo;
            o.ang -= io * ro.cross(nrm * j);
            // metal on metal: some friction along the contact
            let vt = v_rel - nrm * vn;
            let jt = (vt.length() * 0.5 / k).min(j * 0.4);
            let t = vt.normalize_or_zero();
            s.vel -= t * jt / ms;
            o.vel += t * jt / mo;
            worst = worst.max(j);
        }
    }
    worst
}

fn wheels_on(wheels: &[Wheel], front: bool) -> usize {
    wheels.iter().filter(|w| w.front == front).count()
}
