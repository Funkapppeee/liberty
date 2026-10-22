//! An articulated body in joint coordinates.
//!
//! Link 0 is the free root (6 DOF: its COM velocity and angular velocity, world axes). Every other
//! link hangs off a parent by a joint: a ball (3 rotational DOF, velocities measured about the
//! parent-side joint frame's axes) or a hinge (1 DOF about one axis of that frame). Because the
//! state is joint angles, the anchors can never separate.
//!
//! One step: build the joint-space mass matrix M and the generalised forces (gravity, velocity
//! terms, external forces), add the joint springs implicitly (muscle drives, joint limits, locked
//! axes) giving A = M + h·C + h²·K, solve for the new velocities, run the contact impulses on A,
//! then integrate the joint angles.

use glam::{DMat3, DQuat, DVec3};

use crate::linalg::{dot, Chol, Mat};

/// A rigid transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pos: DVec3,
    pub rot: DQuat,
}

impl Pose {
    pub const IDENTITY: Pose = Pose { pos: DVec3::ZERO, rot: DQuat::IDENTITY };

    pub fn new(pos: DVec3, rot: DQuat) -> Pose {
        Pose { pos, rot }
    }
    pub fn mul(&self, o: &Pose) -> Pose {
        Pose { pos: self.pos + self.rot * o.pos, rot: (self.rot * o.rot).normalize() }
    }
    pub fn inverse(&self) -> Pose {
        let r = self.rot.inverse();
        Pose { pos: -(r * self.pos), rot: r }
    }
    pub fn apply(&self, p: DVec3) -> DVec3 {
        self.pos + self.rot * p
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JointKind {
    /// Three rotational DOF.
    Ball,
    /// One rotational DOF about this axis (0, 1, 2) of the joint frame.
    Hinge(usize),
}

#[derive(Clone, Debug)]
pub struct JointDef {
    pub parent: usize,
    /// The joint frame in the parent link's frame, and in this link's frame. At zero rotation the
    /// two coincide.
    pub frame_parent: Pose,
    pub frame_child: Pose,
    pub kind: JointKind,
    /// Per joint-frame axis: [lo, hi] in radians, or None = locked (held by a stiff spring; for a
    /// hinge only its own axis matters).
    pub limits: [Option<[f64; 2]>; 3],
}

#[derive(Clone, Debug)]
pub struct LinkDef {
    pub mass: f64,
    /// Centre of mass and inertia about it, in the link's frame.
    pub com: DVec3,
    pub inertia: DMat3,
    /// None for the root.
    pub joint: Option<JointDef>,
}

/// A joint's muscle: per-axis target angles, pulled with this stiffness and damping, as
/// accelerations (natural frequency squared and 2·ω·ζ: mass-free, like Euphoria's).
#[derive(Clone, Copy, Debug, Default)]
pub struct Drive {
    pub target: [f64; 3],
    pub stiffness: f64,
    pub damping: f64,
}

/// A contact to resolve this step: `normal` is the direction the body is pushed.
#[derive(Clone, Copy, Debug)]
pub struct Contact {
    pub link: usize,
    pub point: DVec3,
    pub normal: DVec3,
    pub depth: f64,
    /// Velocity of the other surface at the point (a moving car); zero for the world.
    pub other_vel: DVec3,
    pub friction: f64,
}

/// Joint-limit and locked-axis springs (natural frequencies, rad/s).
const LIMIT_OMEGA: f64 = 60.0;
const LOCK_OMEGA: f64 = 120.0;
/// Contact position correction.
const BAUMGARTE: f64 = 0.2;
const SLOP: f64 = 0.005;
const MAX_PUSH: f64 = 2.0;
const ITERATIONS: usize = 12;

pub struct Body {
    pub links: Vec<LinkDef>,
    pub gravity: DVec3,
    /// Light velocity damping per link (linear, angular), per second.
    pub damping: (f64, f64),
    // state
    pub root: Pose,
    pub root_vel: DVec3,
    pub root_angvel: DVec3,
    /// Per link: the joint's rotation (child joint frame in the parent joint frame) and its
    /// velocity about the parent joint frame's axes. Unused for the root.
    pub q: Vec<DQuat>,
    pub u: Vec<DVec3>,
    pub drives: Vec<Option<Drive>>,
    /// Per link: the inertia its joint's springs act against, when that is more than the part
    /// below the joint (a planted foot's ankle and knee carry the body above them, not the foot).
    pub carried: Vec<Option<f64>>,
    /// External forces at each link's COM for the next step (cleared by `step`).
    pub force: Vec<DVec3>,
    /// External torques on each link for the next step (cleared by `step`).
    pub torque: Vec<DVec3>,
    /// Per link: its joint's stiffness s in [0, 1), as RAGE's joints have it (IV's Euphoria sets it
    /// from the muscle numbers each update). The joint answers every push (1 - s) times as much
    /// as a free one, and its muscle is sized so it still reaches the acceleration it asks for:
    /// here, the joint's own inertia is divided by (1 - s), for the springs and in the equations.
    pub stiffness: Vec<f64>,
    // derived (world)
    pub pose: Vec<Pose>,
    pub com: Vec<DVec3>,
    pub omega: Vec<DVec3>,
    pub vcom: Vec<DVec3>,
    dof: Vec<usize>,
    ndof: usize,
    /// Per link: itself and its joint ancestors (not the root), child first.
    chain: Vec<Vec<usize>>,
}

/// A joint rotation as per-axis angles: 2·asin of the quaternion's vector part (w ≥ 0).
pub fn joint_angles(q: DQuat) -> [f64; 3] {
    let q = if q.w < 0.0 { -q } else { q };
    [2.0 * q.x.clamp(-1.0, 1.0).asin(), 2.0 * q.y.clamp(-1.0, 1.0).asin(), 2.0 * q.z.clamp(-1.0, 1.0).asin()]
}

/// The rotation with these per-axis angles (as far as they fit in one quaternion).
pub fn from_joint_angles(a: [f64; 3]) -> DQuat {
    let s = a.map(|x| (x * 0.5).sin());
    let w2 = 1.0 - (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]);
    DQuat::from_xyzw(s[0], s[1], s[2], w2.max(0.0).sqrt()).normalize()
}

fn axis(k: usize) -> DVec3 {
    [DVec3::X, DVec3::Y, DVec3::Z][k]
}

impl Body {
    /// `links[0]` is the root; every other link's parent must come before it.
    pub fn new(links: Vec<LinkDef>, gravity: DVec3) -> Body {
        let n = links.len();
        let mut dof = vec![0; n];
        let mut ndof = 6;
        for (i, l) in links.iter().enumerate().skip(1) {
            let j = l.joint.as_ref().expect("non-root link without a joint");
            assert!(j.parent < i, "link {i}: parent {} must come first", j.parent);
            dof[i] = ndof;
            ndof += match j.kind {
                JointKind::Ball => 3,
                JointKind::Hinge(_) => 1,
            };
        }
        let mut chain = vec![Vec::new(); n];
        for i in 1..n {
            let mut c = vec![i];
            let mut p = links[i].joint.as_ref().unwrap().parent;
            while p != 0 {
                c.push(p);
                p = links[p].joint.as_ref().unwrap().parent;
            }
            chain[i] = c;
        }
        let mut b = Body {
            links,
            gravity,
            damping: (0.05, 0.8),
            root: Pose::IDENTITY,
            root_vel: DVec3::ZERO,
            root_angvel: DVec3::ZERO,
            q: vec![DQuat::IDENTITY; n],
            u: vec![DVec3::ZERO; n],
            drives: vec![None; n],
            carried: vec![None; n],
            force: vec![DVec3::ZERO; n],
            torque: vec![DVec3::ZERO; n],
            stiffness: vec![0.0; n],
            pose: vec![Pose::IDENTITY; n],
            com: vec![DVec3::ZERO; n],
            omega: vec![DVec3::ZERO; n],
            vcom: vec![DVec3::ZERO; n],
            dof,
            ndof,
            chain,
        };
        b.update_kinematics();
        b
    }

    pub fn ndof(&self) -> usize {
        self.ndof
    }

    fn joint(&self, i: usize) -> &JointDef {
        self.links[i].joint.as_ref().unwrap()
    }

    /// The parent-side joint frame of link i's joint, in world.
    pub fn joint_frame_parent(&self, i: usize) -> Pose {
        let j = self.joint(i);
        self.pose[j.parent].mul(&j.frame_parent)
    }

    /// The child-side joint frame of link i's joint, in world.
    pub fn joint_frame_child(&self, i: usize) -> Pose {
        self.pose[i].mul(&self.joint(i).frame_child)
    }

    /// Per-axis joint angles of link i's joint (a hinge reports its one axis, others 0).
    pub fn angles(&self, i: usize) -> [f64; 3] {
        match self.joint(i).kind {
            JointKind::Ball => joint_angles(self.q[i]),
            JointKind::Hinge(k) => {
                let q = self.q[i];
                let v = [q.x, q.y, q.z];
                let mut a = [0.0; 3];
                a[k] = 2.0 * v[k].atan2(q.w);
                a
            }
        }
    }

    /// Forward kinematics: every link's pose, COM, angular and COM velocity from the state.
    pub fn update_kinematics(&mut self) {
        let n = self.links.len();
        self.pose[0] = self.root;
        self.com[0] = self.root.apply(self.links[0].com);
        self.omega[0] = self.root_angvel;
        self.vcom[0] = self.root_vel;
        for i in 1..n {
            let j = self.joint(i).clone();
            let p = j.parent;
            let w1 = self.pose[p].mul(&j.frame_parent);
            let rot = (w1.rot * self.q[i] * j.frame_child.rot.inverse()).normalize();
            let pos = w1.pos - rot * j.frame_child.pos;
            self.pose[i] = Pose { pos, rot };
            self.com[i] = self.pose[i].apply(self.links[i].com);
            let w_rel = w1.rot * self.rel_vel(i);
            self.omega[i] = self.omega[p] + w_rel;
            let va = self.vcom[p] + self.omega[p].cross(w1.pos - self.com[p]);
            self.vcom[i] = va + self.omega[i].cross(self.com[i] - w1.pos);
        }
    }

    /// Joint velocity as a vector in the parent joint frame (a hinge's lives on its axis).
    fn rel_vel(&self, i: usize) -> DVec3 {
        match self.joint(i).kind {
            JointKind::Ball => self.u[i],
            JointKind::Hinge(k) => axis(k) * self.u[i][k],
        }
    }

    /// The degrees of freedom that move link i: (dof index, angular column, linear column at `x`).
    fn jac_point(&self, i: usize, x: DVec3) -> Vec<(usize, DVec3, DVec3)> {
        let mut out = Vec::with_capacity(6 + 3 * self.chain[i].len());
        for k in 0..3 {
            out.push((k, DVec3::ZERO, axis(k)));
        }
        let r0 = x - self.com[0];
        for k in 0..3 {
            let e = axis(k);
            out.push((3 + k, e, e.cross(r0)));
        }
        for &c in &self.chain[i] {
            let w1 = self.joint_frame_parent(c);
            let d = self.dof[c];
            match self.joint(c).kind {
                JointKind::Ball => {
                    for k in 0..3 {
                        let e = w1.rot * axis(k);
                        out.push((d + k, e, e.cross(x - w1.pos)));
                    }
                }
                JointKind::Hinge(k) => {
                    let e = w1.rot * axis(k);
                    out.push((d, e, e.cross(x - w1.pos)));
                }
            }
        }
        out
    }

    fn inertia_world(&self, i: usize) -> DMat3 {
        let r = DMat3::from_quat(self.pose[i].rot);
        r * self.links[i].inertia * r.transpose()
    }

    /// The generalised velocity vector.
    fn gen_vel(&self) -> Vec<f64> {
        let mut v = vec![0.0; self.ndof];
        v[0..3].copy_from_slice(&self.root_vel.to_array());
        v[3..6].copy_from_slice(&self.root_angvel.to_array());
        for i in 1..self.links.len() {
            let d = self.dof[i];
            match self.joint(i).kind {
                JointKind::Ball => v[d..d + 3].copy_from_slice(&self.u[i].to_array()),
                JointKind::Hinge(k) => v[d] = self.u[i][k],
            }
        }
        v
    }

    fn set_gen_vel(&mut self, v: &[f64]) {
        self.root_vel = DVec3::from_slice(&v[0..3]);
        self.root_angvel = DVec3::from_slice(&v[3..6]);
        for i in 1..self.links.len() {
            let d = self.dof[i];
            match self.joint(i).kind {
                JointKind::Ball => self.u[i] = DVec3::from_slice(&v[d..d + 3]),
                JointKind::Hinge(k) => {
                    self.u[i] = DVec3::ZERO;
                    self.u[i][k] = v[d];
                }
            }
        }
    }

    /// Joint-space mass matrix.
    fn mass_matrix(&self) -> Mat {
        let mut m = Mat::zeros(self.ndof);
        for i in 0..self.links.len() {
            let mass = self.links[i].mass;
            let iw = self.inertia_world(i);
            let cols = if i == 0 { self.jac_root(self.com[0]) } else { self.jac_point(i, self.com[i]) };
            for &(da, aa, la) in &cols {
                let ia = iw * aa;
                for &(db, ab, lb) in &cols {
                    m.add(da, db, mass * la.dot(lb) + ab.dot(ia));
                }
            }
        }
        m
    }

    fn jac_root(&self, x: DVec3) -> Vec<(usize, DVec3, DVec3)> {
        let r0 = x - self.com[0];
        (0..3).map(|k| (k, DVec3::ZERO, axis(k))).chain((0..3).map(|k| (3 + k, axis(k), axis(k).cross(r0)))).collect()
    }

    fn cols(&self, i: usize, x: DVec3) -> Vec<(usize, DVec3, DVec3)> {
        if i == 0 { self.jac_root(x) } else { self.jac_point(i, x) }
    }

    /// Adds a force `f` at the COM and a torque `t` on link i to the generalised forces.
    fn add_wrench(&self, q: &mut [f64], i: usize, f: DVec3, t: DVec3) {
        for (d, a, l) in self.cols(i, self.com[i]) {
            q[d] += l.dot(f) + a.dot(t);
        }
    }

    /// One step of length `h` with these contacts. Returns the normal impulse per contact.
    pub fn step(&mut self, h: f64, contacts: &[Contact]) -> Vec<f64> {
        let n = self.links.len();
        self.update_kinematics();
        let mut a = self.mass_matrix();
        let mut rhs = vec![0.0; self.ndof];

        // velocity-product accelerations (the link accelerations if every joint acceleration were 0)
        let mut alpha = vec![DVec3::ZERO; n];
        let mut acc = vec![DVec3::ZERO; n];
        for i in 1..n {
            let p = self.joint(i).parent;
            let w1 = self.joint_frame_parent(i);
            let w_rel = w1.rot * self.rel_vel(i);
            alpha[i] = alpha[p] + self.omega[p].cross(w_rel);
            let rp = w1.pos - self.com[p];
            let aa = acc[p] + alpha[p].cross(rp) + self.omega[p].cross(self.omega[p].cross(rp));
            let rc = self.com[i] - w1.pos;
            acc[i] = aa + alpha[i].cross(rc) + self.omega[i].cross(self.omega[i].cross(rc));
        }
        for i in 0..n {
            let m = self.links[i].mass;
            let iw = self.inertia_world(i);
            let w = self.omega[i];
            let (ld, ad) = self.damping;
            let f = self.gravity * m + self.force[i] - acc[i] * m - self.vcom[i] * (m * ld);
            let t = self.torque[i] - (iw * alpha[i]) - w.cross(iw * w) - iw * w * ad;
            self.add_wrench(&mut rhs, i, f, t);
        }

        // joint springs, implicit: A += h·C + h²·K, rhs += K·(θt − θ) − (C + h·K)·u
        let v = self.gen_vel();
        for i in 1..n {
            let j = self.joint(i).clone();
            let ang = self.angles(i);
            let axes: Vec<usize> = match j.kind {
                JointKind::Ball => vec![0, 1, 2],
                JointKind::Hinge(k) => vec![k],
            };
            for (slot, &k) in axes.iter().enumerate() {
                let d = self.dof[i] + slot;
                let stiff = self.stiffness[i].clamp(0.0, 0.995);
                let own = a.at(d, d);
                if stiff > 0.0 {
                    a.add(d, d, own * stiff / (1.0 - stiff));
                }
                let mdd = (own / (1.0 - stiff)).max(self.carried[i].unwrap_or(0.0));
                let theta = ang[k];
                let mut springs: Vec<(f64, f64, f64)> = Vec::new(); // (k, c, target) as accelerations
                if let Some(dr) = self.drives[i] {
                    if dr.stiffness > 0.0 || dr.damping > 0.0 {
                        springs.push((dr.stiffness, dr.damping, dr.target[k]));
                    }
                }
                match j.limits[k] {
                    None => springs.push((LOCK_OMEGA * LOCK_OMEGA, 2.0 * LOCK_OMEGA, 0.0)),
                    Some([lo, hi]) => {
                        if theta < lo {
                            springs.push((LIMIT_OMEGA * LIMIT_OMEGA, 2.0 * LIMIT_OMEGA, lo));
                        } else if theta > hi {
                            springs.push((LIMIT_OMEGA * LIMIT_OMEGA, 2.0 * LIMIT_OMEGA, hi));
                        }
                    }
                }
                for (ks, cs, target) in springs {
                    let (kk, cc) = (ks * mdd, cs * mdd);
                    a.add(d, d, h * cc + h * h * kk);
                    rhs[d] += kk * (target - theta) - (cc + h * kk) * v[d];
                }
            }
        }

        let chol = Chol::new(&a);
        let udot = chol.solve(&rhs);
        let mut vel: Vec<f64> = v.iter().zip(&udot).map(|(x, y)| x + h * y).collect();

        // contacts: sequential impulses on A
        struct Row {
            j: Vec<f64>,
            w: Vec<f64>,
            eff: f64,
        }
        let row = |this: &Body, c: &Contact, dir: DVec3| {
            let mut jr = vec![0.0; this.ndof];
            for (d, ang, lin) in this.cols(c.link, c.point) {
                let _ = ang;
                jr[d] += lin.dot(dir);
            }
            let w = chol.solve(&jr);
            let e = dot(&jr, &w);
            Row { j: jr, w, eff: if e > 1e-12 { 1.0 / e } else { 0.0 } }
        };
        let mut rows = Vec::with_capacity(contacts.len());
        for c in contacts {
            let t1 = c.normal.any_orthonormal_vector();
            let t2 = c.normal.cross(t1);
            let goal = if c.depth > SLOP { (BAUMGARTE * (c.depth - SLOP) / h).min(MAX_PUSH) } else { 0.0 };
            rows.push((row(self, c, c.normal), row(self, c, t1), row(self, c, t2), goal, t1, t2));
        }
        let mut lam = vec![[0.0f64; 3]; contacts.len()];
        for _ in 0..ITERATIONS {
            for (ci, c) in contacts.iter().enumerate() {
                let (rn, r1, r2, goal, t1, t2) = &rows[ci];
                let vn = dot(&rn.j, &vel) - c.normal.dot(c.other_vel);
                let new = (lam[ci][0] + rn.eff * (goal - vn)).max(0.0);
                let d = new - lam[ci][0];
                lam[ci][0] = new;
                for (x, w) in vel.iter_mut().zip(&rn.w) {
                    *x += w * d;
                }
                let cap = c.friction * lam[ci][0];
                for (slot, r, t) in [(1, r1, t1), (2, r2, t2)] {
                    let vt = dot(&r.j, &vel) - t.dot(c.other_vel);
                    let new = (lam[ci][slot] - r.eff * vt).clamp(-cap, cap);
                    let d = new - lam[ci][slot];
                    lam[ci][slot] = new;
                    for (x, w) in vel.iter_mut().zip(&r.w) {
                        *x += w * d;
                    }
                }
            }
        }

        // integrate
        self.set_gen_vel(&vel);
        let com0 = self.com[0] + self.root_vel * h;
        let rot0 = (DQuat::from_scaled_axis(self.root_angvel * h) * self.root.rot).normalize();
        self.root = Pose { rot: rot0, pos: com0 - rot0 * self.links[0].com };
        for i in 1..n {
            let w = self.rel_vel(i) * h;
            self.q[i] = (DQuat::from_scaled_axis(w) * self.q[i]).normalize();
        }
        for f in &mut self.force {
            *f = DVec3::ZERO;
        }
        for t in &mut self.torque {
            *t = DVec3::ZERO;
        }
        self.update_kinematics();
        lam.iter().map(|l| l[0]).collect()
    }

    /// Instantly changes velocities by an impulse `imp` at world point `x` on link i.
    pub fn apply_impulse(&mut self, i: usize, x: DVec3, imp: DVec3) {
        self.update_kinematics();
        let m = self.mass_matrix();
        let chol = Chol::new(&m);
        let mut jr = vec![0.0; self.ndof];
        for (d, _, l) in self.cols(i, x) {
            jr[d] += l.dot(imp);
        }
        let du = chol.solve(&jr);
        let v: Vec<f64> = self.gen_vel().iter().zip(&du).map(|(a, b)| a + b).collect();
        self.set_gen_vel(&v);
        self.update_kinematics();
    }

    /// Puts the body in these link poses (joint rotations from the poses; anchors are re-joined,
    /// so child positions follow their parents), everything moving at `vel`.
    pub fn set_from_poses(&mut self, poses: &[Pose], vel: DVec3) {
        self.root = poses[0];
        for i in 1..self.links.len() {
            let j = self.joint(i);
            let w1 = poses[j.parent].mul(&j.frame_parent);
            let w2 = poses[i].mul(&j.frame_child);
            let mut q = (w1.rot.inverse() * w2.rot).normalize();
            if q.w < 0.0 {
                q = -q;
            }
            self.q[i] = q;
            self.u[i] = DVec3::ZERO;
        }
        self.root_vel = vel;
        self.root_angvel = DVec3::ZERO;
        self.update_kinematics();
    }

    /// Clamps every joint into its limits (locked axes to 0).
    pub fn clamp_to_limits(&mut self) {
        for i in 1..self.links.len() {
            let j = self.joint(i).clone();
            let a = self.angles(i);
            let c = |k: usize| j.limits[k].map_or(0.0, |[lo, hi]| a[k].clamp(lo, hi));
            self.q[i] = match j.kind {
                JointKind::Ball => from_joint_angles([c(0), c(1), c(2)]),
                JointKind::Hinge(k) => DQuat::from_axis_angle(axis(k), c(k)),
            };
        }
        self.update_kinematics();
    }

    /// Total mass.
    pub fn mass(&self) -> f64 {
        self.links.iter().map(|l| l.mass).sum()
    }
}
