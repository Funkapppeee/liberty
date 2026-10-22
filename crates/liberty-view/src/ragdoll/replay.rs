//! S1 replay: IV's own recorded body drives our ragdoll, with no behaviour code at all, and our
//! motion is set beside IV's. Same wanted angles and muscle numbers in; if the bodies were the same,
//! the same motion out. This measures the body (solver, joint drive, masses, joints) on its own.
//!
//! Input: the flat file `re/mg2_episode.py` writes next to its JSON (one line per recorded tick, 30 a
//! second, GTA world axes). Run: `liberty-view --replay out/s1/ep_shot_0760.txt` (no window).
//! Writes `<file>.replay.csv` and prints per-joint errors.
//!
//! Switches: LIBERTY_REPLAY_NO_JS (no IV joint stiffness), LIBERTY_REPLAY_GROUND=<m> (ground below
//! the lowest foot link, default 0.06), LIBERTY_REPLAY_OPEN (no ground: free fall check),
//! LIBERTY_REPLAY_FROM=<s> (start later in the episode).

use super::*;
use std::io::Write;

pub(super) const LINKS: usize = 21;
pub(super) const JOINTS: usize = 20;

#[derive(Clone, Copy, Default)]
pub(super) struct LinkRec {
    pub rows: [Vec3; 3],
    pub pos: Vec3,
    pub mass: f32,
    /// Velocity (+0x260) and spin (+0x270), GTA world axes.
    pub vel: Vec3,
    pub spin: Vec3,
}

#[derive(Clone, Copy, Default)]
pub(super) struct JointRec {
    pub now: [f32; 3],
    pub want: [f32; 3],
    pub strength: f32,
    pub damping: f32,
    pub muscle: f32,
}

#[derive(Clone)]
pub(super) struct Rec {
    pub t: f32,
    pub com: Vec3,
    pub com_vel: Vec3,
    pub links: [LinkRec; LINKS],
    pub joints: [JointRec; JOINTS],
    /// Per IV part: touching something (the part's +0x118 flags, 0x04), when the line has them.
    pub contact: [bool; LINKS],
}

/// One tick in the flat format (`re/mg2_episode.py`), or None for a short / comment line.
pub(super) fn parse_line(line: &str) -> Option<Rec> {
    if line.starts_with('#') {
        return None;
    }
    let v: Vec<f32> = line.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    if v.len() < 1 + 6 + LINKS * 19 + JOINTS * 10 {
        return None;
    }
    let v3 = |o: usize| Vec3::new(v[o], v[o + 1], v[o + 2]);
    let mut links = [LinkRec::default(); LINKS];
    for (k, l) in links.iter_mut().enumerate() {
        let o = 7 + k * 19;
        *l = LinkRec { rows: [v3(o), v3(o + 3), v3(o + 6)], pos: v3(o + 9), mass: v[o + 18], vel: v3(o + 12), spin: v3(o + 15) };
    }
    let mut joints = [JointRec::default(); JOINTS];
    for (k, j) in joints.iter_mut().enumerate() {
        let o = 7 + LINKS * 19 + k * 10;
        *j = JointRec { now: [v[o], v[o + 1], v[o + 2]], want: [v[o + 3], v[o + 4], v[o + 5]], strength: v[o + 6], damping: v[o + 7], muscle: v[o + 8] };
    }
    let c0 = 7 + LINKS * 19 + JOINTS * 10;
    let contact = std::array::from_fn(|k| v.get(c0 + k).is_some_and(|&f| (f as u32) & 4 != 0));
    Some(Rec { t: v[0], com: v3(1), com_vel: v3(4), links, joints, contact })
}

/// The episode's ticks, and the ground height a live episode was run with (its `# ... ground=<z>` line).
pub(super) fn load(path: &std::path::Path) -> anyhow::Result<(Vec<Rec>, Option<f32>)> {
    let text = std::fs::read_to_string(path)?;
    let ground = text
        .lines()
        .take_while(|l| l.starts_with('#'))
        .find_map(|l| l.split_whitespace().find_map(|w| w.strip_prefix("ground=")?.parse().ok()));
    Ok((text.lines().filter_map(parse_line).collect(), ground))
}

/// How one of Fred.art's 20 joints lands on our ragdoll.
#[derive(Clone, Copy, Debug)]
pub(super) enum Map {
    Ball(usize),
    Knee(usize),
    Elbow(usize),
    /// Welded in our ragdoll (lower neck, clavicles): nothing to drive.
    None,
}

pub(super) const NAMES: [&str; JOINTS] = [
    "hip L", "knee L", "ankle L", "hip R", "knee R", "ankle R", "spine0", "spine1", "spine2", "spine3", "neck lo", "neck up", "clav L",
    "shoulder L", "elbow L", "wrist L", "clav R", "shoulder R", "elbow R", "wrist R",
];

pub(super) fn joint_map(def: &RagdollDef) -> [Map; JOINTS] {
    use Map::*;
    [
        Ball(def.thighs[0]), Knee(def.calves[0]), Ball(def.feet[0]), Ball(def.thighs[1]), Knee(def.calves[1]), Ball(def.feet[1]),
        Ball(def.spine[0]), Ball(def.spine[1]), Ball(def.spine[2]), Ball(def.spine[3]), None, Ball(def.head_part), None,
        Ball(def.upper[0]), Elbow(def.fore[0]), Ball(def.hands[0]), None, Ball(def.upper[1]), Elbow(def.fore[1]), Ball(def.hands[1]),
    ]
}

/// IV's part index (0..21) of one of our parts.
pub(super) fn iv_index(def: &RagdollDef, part: usize) -> Option<usize> {
    let p = def.iv_part[part]?;
    (0..LINKS).find(|&k| euphoria::reach::Part::from_iv_index(k) == Some(p))
}

fn hinge_axis(def: &RagdollDef, part: usize) -> Option<(usize, f32, [f32; 2])> {
    let p = &def.parts[part];
    let k = (0..3).find(|&k| p.limits[k].is_some())?;
    let [lo, hi] = p.limits[k].unwrap();
    let flex = if lo.abs() > hi.abs() { lo.signum() } else { hi.signum() };
    Some((k, flex, [lo, hi]))
}

impl RagdollSim {
    /// Our joint angles for a joint, in Euphoria's terms (the ones IV records as "now").
    pub(super) fn replay_now(&self, def: &RagdollDef, m: Map) -> Option<[f32; 3]> {
        match m {
            Map::Ball(p) => self.nm_frames(def, p).map(|(nm, pr, cr, _)| nm.angles(pr.inverse() * cr)),
            Map::Knee(p) => hinge_axis(def, p).map(|(k, _, _)| [self.body.angles(self.link[p])[k] as f32, 0.0, 0.0]),
            Map::Elbow(p) => hinge_axis(def, p).map(|(k, flex, _)| [flex * self.body.angles(self.link[p])[k] as f32, 0.0, 0.0]),
            Map::None => None,
        }
    }

    /// Put one joint at Euphoria angles (the same conversion `set_nm_targets` uses for targets).
    fn replay_set_joint(&mut self, def: &RagdollDef, m: Map, a: [f32; 3]) {
        let l = match m {
            Map::Ball(p) | Map::Knee(p) | Map::Elbow(p) => self.link[p],
            Map::None => return,
        };
        if l == 0 {
            return;
        }
        let mut t = [[0.0f32; 3]; 1];
        match m {
            Map::Ball(p) => {
                let Some(nm) = def.parts[p].nm else { return };
                let q_nm = nm.q_rest.inverse() * nm.rotation(a);
                let q = nm.k * q_nm * nm.k.inverse();
                let ang = joint_angles(q);
                for i in 0..3 {
                    t[0][i] = def.parts[p].limits[i].map_or(0.0, |[lo, hi]| ang[i].clamp(lo, hi));
                }
                self.body.q[l] = phys::artic::from_joint_angles(t[0].map(|x| x as f64));
            }
            Map::Knee(p) | Map::Elbow(p) => {
                let Some((k, flex, [lo, hi])) = hinge_axis(def, p) else { return };
                let v = if matches!(m, Map::Elbow(_)) { flex * a[0] } else { a[0] };
                let ang = v.clamp(lo, hi) as f64;
                let axis = [DVec3::X, DVec3::Y, DVec3::Z][k];
                self.body.q[l] = DQuat::from_axis_angle(axis, ang);
            }
            Map::None => {}
        }
        self.body.u[l] = DVec3::ZERO;
    }

    /// One physics tick driven by IV's recorded wanted angles and muscle numbers.
    fn replay_tick(&mut self, def: &RagdollDef, map: &[Map; JOINTS], rec: &Rec, joint_stiffness: bool) {
        // IV's gravity opposition (the viewer's port, `nm_leg_drive`: planted legs hold the body above
        // them, other joints what hangs below; limit strength x 2 on a planted ankle); our own foot
        // contacts say which legs are planted. Its leg stiffness settings are overwritten below by
        // the recorded ones. LIBERTY_REPLAY_NO_GRAV leaves it out.
        if std::env::var("LIBERTY_REPLAY_NO_GRAV").is_err() {
            let planted = [0, 1].map(|s| self.touching.get(self.link[def.feet[s]]).copied().unwrap_or(true));
            self.nm_leg_drive(def, planted);
        }
        let mut targets = self.base.clone();
        for (k, m) in map.iter().enumerate() {
            let j = &rec.joints[k];
            let part = match *m {
                Map::Ball(p) | Map::Knee(p) | Map::Elbow(p) => p,
                Map::None => continue,
            };
            match *m {
                Map::Ball(p) => self.set_nm_targets(def, p, j.want, &mut targets),
                Map::Knee(p) | Map::Elbow(p) => {
                    if let Some((ax, flex, [lo, hi])) = hinge_axis(def, p) {
                        let v = if matches!(m, Map::Elbow(_)) { flex * j.want[0] } else { j.want[0] };
                        targets[p][ax] = v.clamp(lo, hi);
                    }
                }
                Map::None => {}
            }
            let l = self.link[part];
            if l == 0 || def.parts[part].body != part {
                continue;
            }
            self.body.drives[l] = (j.strength > 0.0).then(|| Drive {
                target: targets[part].map(|a| a as f64),
                stiffness: j.strength as f64,
                damping: j.damping as f64,
            });
            self.body.stiffness[l] = if joint_stiffness { iv_joint_stiffness(j.strength, j.damping, j.muscle) } else { 0.0 };
        }
        let touches = pc::generate(&self.body, &self.colliders, &self.tris, &self.boxes, DVec3::Y);
        self.touching = vec![false; self.body.links.len()];
        for t in &touches {
            self.touching[t.contact.link] = true;
        }
        let contacts: Vec<_> = touches.iter().map(|t| t.contact).collect();
        self.body.step((1.0 / HZ) as f64, &contacts);
    }

    /// The COM of the link that carries IV part k (Bevy axes), if we have that part.
    pub(super) fn iv_part_com(&self, def: &RagdollDef, k: usize) -> Option<Vec3> {
        let pi = (0..def.parts.len()).find(|&i| iv_index(def, i) == Some(k))?;
        Some(fv(self.body.com[self.link[def.parts[pi].body]]))
    }

    /// The body's frame from its parts: up = feet (mid) to head, right = left thigh to right thigh.
    pub(super) fn body_frame(&self, def: &RagdollDef) -> Option<Mat3> {
        let g = |k| self.iv_part_com(def, k);
        frame_of(g(3)?, g(6)?, g(12)?, g(1)?, g(4)?)
    }

    pub(super) fn whole_com(&self) -> Vec3 {
        let (mut m, mut c) = (0.0f32, Vec3::ZERO);
        for l in 0..self.body.links.len() {
            let lm = self.body.links[l].mass as f32;
            m += lm;
            c += fv(self.body.com[l]) * lm;
        }
        c / m.max(1e-3)
    }
}

/// A body frame (columns: right, up, right x up) from feet, head and thighs, all in one axis set.
fn frame_of(foot_l: Vec3, foot_r: Vec3, head: Vec3, thigh_l: Vec3, thigh_r: Vec3) -> Option<Mat3> {
    let up = (head - (foot_l + foot_r) * 0.5).normalize_or_zero();
    let right = (thigh_r - thigh_l).reject_from(up).normalize_or_zero();
    if up == Vec3::ZERO || right == Vec3::ZERO {
        return None;
    }
    Some(Mat3::from_cols(right, up, right.cross(up)))
}

/// IV's body frame at a recorded tick, in Bevy axes.
pub(super) fn rec_frame(r: &Rec) -> Option<Mat3> {
    let p = |k: usize| from_nm_v(r.links[k].pos);
    frame_of(p(3), p(6), p(12), p(1), p(4))
}

/// How far a body frame's up is from vertical (degrees).
pub(super) fn tilt(f: Mat3) -> f32 {
    f.y_axis.dot(Vec3::Y).clamp(-1.0, 1.0).acos().to_degrees()
}

/// IV's recorded COM-relative link positions (world axes) and its COM.
pub(super) fn rec_rel_com(r: &Rec) -> Vec3 {
    let m: f32 = r.links.iter().map(|l| l.mass).sum();
    r.links.iter().map(|l| l.pos * l.mass).sum::<Vec3>() / m.max(1e-3)
}

/// The ground (GTA world z) under an episode: the lowest foot link of the given ticks, less a gap
/// (LIBERTY_REPLAY_GROUND=<m>, default 0.06). A file replay looks at the whole episode; the live
/// player only has the first tick, and writes the height it used into the file.
pub(super) fn ground_under(recs: &[Rec]) -> f32 {
    let gap: f32 = std::env::var("LIBERTY_REPLAY_GROUND").ok().and_then(|v| v.parse().ok()).unwrap_or(0.06);
    let foot_z = recs
        .iter()
        .flat_map(|r| {
            let rc = rec_rel_com(r);
            [3usize, 6].map(|k| r.com.z + r.links[k].pos.z - rc.z)
        })
        .fold(f32::MAX, f32::min);
    foot_z - gap
}

/// One measurement, at one of IV's ticks: IV's numbers next to ours.
pub(super) struct Measure {
    pub t: f32,
    pub iv_com_z: f32,
    pub our_com_z: f32,
    pub iv_dx: f32,
    pub our_dx: f32,
    pub iv_tilt: f32,
    pub our_tilt: f32,
    /// Per joint: IV's actual angles, ours (None where our ragdoll has no such joint).
    pub joints: [([f32; 3], Option<[f32; 3]>); JOINTS],
}

impl Measure {
    pub fn csv_head() -> String {
        let mut head = vec!["t".to_string(), "iv_com_z".into(), "our_com_z".into(), "iv_com_dx".into(), "our_com_dx".into(), "iv_tilt".into(), "our_tilt".into()];
        for n in NAMES {
            for a in ["l1", "l2", "tw"] {
                head.push(format!("iv {n} {a}"));
                head.push(format!("our {n} {a}"));
            }
        }
        head.join(",")
    }

    pub fn csv_row(&self) -> String {
        let mut row = vec![
            format!("{:.4}", self.t),
            format!("{:.4}", self.iv_com_z),
            format!("{:.4}", self.our_com_z),
            format!("{:.4}", self.iv_dx),
            format!("{:.4}", self.our_dx),
            format!("{:.1}", self.iv_tilt),
            format!("{:.1}", self.our_tilt),
        ];
        for (iv, ours) in &self.joints {
            for i in 0..3 {
                row.push(format!("{:.4}", iv[i]));
                row.push(ours.map_or(String::new(), |a| format!("{:.4}", a[i])));
            }
        }
        row.join(",")
    }

    /// The worst axis's error of joint k (radians), where both have it.
    pub fn err(&self, map: &[Map; JOINTS], k: usize) -> Option<f32> {
        let (iv, ours) = &self.joints[k];
        let n = if matches!(map[k], Map::Knee(_) | Map::Elbow(_)) { 1 } else { 3 };
        ours.map(|a| (0..n).map(|i| (a[i] - iv[i]).abs()).fold(0.0, f32::max))
    }
}

/// Our ragdoll following one of IV's characters, tick by tick: started in the first tick's pose,
/// then driven by each tick's wanted angles and gains. A file replay and the live player both feed
/// it the same way, so the same ticks give the same motion.
pub(super) struct Replay {
    pub sim: RagdollSim,
    pub map: [Map; JOINTS],
    pub ground_gta: f32,
    pub rec0: Rec,
    pub cur: Rec,
    pub last: Option<Measure>,
    com0_ours: Vec3,
    joint_stiffness: bool,
    t: f32,
    err_sum: [[f64; 3]; JOINTS],
    err_n: usize,
}

impl Replay {
    pub fn new(def: &RagdollDef, r0: &Rec, ground_gta: f32, verbose: bool) -> Replay {
        let map = joint_map(def);
        let start = r0.com;
        let c = from_nm_v(Vec3::new(start.x, start.y, ground_gta));
        let open = std::env::var("LIBERTY_REPLAY_OPEN").is_ok();
        let s = 50.0;
        let ground = ChunkCollision::new(
            &[[c.x - s, c.y, c.z - s], [c.x + s, c.y, c.z - s], [c.x + s, c.y, c.z + s], [c.x - s, c.y, c.z + s]],
            &[[0, 1, 2], [0, 2, 3]],
        );
        let grounds: Vec<&ChunkCollision> = if open { vec![] } else { vec![&ground] };

        // start in IV's recorded pose: every joint at IV's recorded actual angles, then the whole body
        // turned so its frame (feet to head, thigh to thigh) is IV's: no assumption about how a link's
        // rows relate to our part frames; the COM where IV's was
        let globals: Vec<Transform> = def.rest_global.iter().map(|t| Transform { translation: t.translation + from_nm_v(start), ..*t }).collect();
        let mut sim = RagdollSim::start(def, &globals, Vec3::ZERO, grounds.iter().copied(), Mode::Relax, Tuning::default());
        for (k, m) in map.iter().enumerate() {
            sim.replay_set_joint(def, *m, r0.joints[k].now);
        }
        sim.body.update_kinematics();
        if let (Some(ours), Some(iv)) = (sim.body_frame(def), rec_frame(r0)) {
            let turn = Quat::from_mat3(&(iv * ours.transpose())).normalize();
            let t = DQuat::from_xyzw(turn.x as f64, turn.y as f64, turn.z as f64, turn.w as f64);
            sim.body.root.rot = (t * sim.body.root.rot).normalize();
        }
        // LIBERTY_REPLAY_STILL: start at rest (every link otherwise gets the COM velocity, planted feet too)
        sim.body.root_vel = if std::env::var("LIBERTY_REPLAY_STILL").is_ok() { DVec3::ZERO } else { dv(from_nm_v(r0.com_vel)) };
        sim.body.root_angvel = DVec3::ZERO;
        sim.body.update_kinematics();
        let shift = from_nm_v(r0.com) - sim.whole_com();
        sim.body.root.pos += dv(shift);
        sim.body.update_kinematics();
        if verbose {
            // our feet at the start: corner heights of each foot box over the ground (IV's are planted flat)
            for (side, name) in [(0usize, "left"), (1, "right")] {
                let f = def.feet[side];
                if let pc::Shape::Box { half } = sim.colliders[f].shape {
                    let w = sim.body.pose[sim.link[f]].mul(&sim.colliders[f].local);
                    let ground_y = from_nm_v(Vec3::new(0.0, 0.0, ground_gta)).y as f64;
                    let mut hs: Vec<f64> = (0..8)
                        .map(|c| {
                            let corner = DVec3::new(if c & 1 == 0 { -half.x } else { half.x }, if c & 2 == 0 { -half.y } else { half.y }, if c & 4 == 0 { -half.z } else { half.z });
                            w.apply(corner).y - ground_y
                        })
                        .collect();
                    hs.sort_by(|a, b| a.total_cmp(b));
                    println!("our {name} foot box (size {:.3} x {:.3} x {:.3} m, {:.2} kg, link inertia {:.4?}) at the start: lowest corner {:.3} m over the ground, highest {:.3}; the four lowest {:.3} {:.3} {:.3} {:.3}", half.x * 2.0, half.y * 2.0, half.z * 2.0, def.parts[f].mass, sim.body.links[sim.link[f]].inertia, hs[0], hs[7], hs[0], hs[1], hs[2], hs[3]);
                }
            }
            // check: our joints now read IV's recorded actual angles back (where our joints can take them)
            println!("start pose check (IV now -> ours, rad):");
            for (k, m) in map.iter().enumerate() {
                if let Some(a) = sim.replay_now(def, *m) {
                    let iv = r0.joints[k].now;
                    let n = if matches!(m, Map::Knee(_) | Map::Elbow(_)) { 1 } else { 3 };
                    let err = (0..n).map(|i| (a[i] - iv[i]).abs()).fold(0.0f32, f32::max);
                    if err > 0.02 {
                        println!("  {:10} IV {:?} ours {:?}", NAMES[k], &iv[..n], &a[..n]);
                    }
                }
            }
        }
        let com0_ours = sim.whole_com();
        Replay {
            sim,
            map,
            ground_gta,
            rec0: r0.clone(),
            cur: r0.clone(),
            last: None,
            com0_ours,
            joint_stiffness: std::env::var("LIBERTY_REPLAY_NO_JS").is_err(),
            t: 0.0,
            err_sum: [[0.0; 3]; JOINTS],
            err_n: 0,
        }
    }

    /// The next of IV's ticks: run our body up to its time on the previous tick's targets, then
    /// measure against it; it drives the ticks after.
    pub fn feed(&mut self, def: &RagdollDef, r: Rec) -> &Measure {
        while self.t < r.t {
            self.sim.replay_tick(def, &self.map, &self.cur, self.joint_stiffness);
            self.t += 1.0 / HZ;
        }
        let sim = &self.sim;
        let mut joints = [([0.0f32; 3], None); JOINTS];
        for (k, m) in self.map.iter().enumerate() {
            let ours = sim.replay_now(def, *m);
            if let Some(a) = ours {
                let n = if matches!(m, Map::Knee(_) | Map::Elbow(_)) { 1 } else { 3 };
                for i in 0..n {
                    self.err_sum[k][i] += ((a[i] - r.joints[k].now[i]) as f64).powi(2);
                }
            }
            joints[k] = (r.joints[k].now, ours);
        }
        self.err_n += 1;
        self.last = Some(Measure {
            t: r.t,
            iv_com_z: r.com.z - self.ground_gta,
            our_com_z: to_nm(sim.whole_com()).z - self.ground_gta,
            iv_dx: (r.com - self.rec0.com).length(),
            our_dx: (sim.whole_com() - self.com0_ours).length(),
            iv_tilt: rec_frame(&r).map_or(f32::NAN, tilt),
            our_tilt: sim.body_frame(def).map_or(f32::NAN, tilt),
            joints,
        });
        self.cur = r;
        self.last.as_ref().unwrap()
    }

    /// RMS error per joint so far (degrees: lean1 / lean2 / twist).
    pub fn rms(&self) -> [[f64; 3]; JOINTS] {
        self.err_sum.map(|e| e.map(|x| (x / self.err_n.max(1) as f64).sqrt().to_degrees()))
    }

    pub fn ticks(&self) -> usize {
        self.err_n
    }
}

pub fn run(def: &RagdollDef, path: &std::path::Path) -> anyhow::Result<()> {
    let (mut recs, live_ground) = load(path)?;
    // LIBERTY_REPLAY_FROM=<s>: start later in the episode (e.g. once IV's body stands quietly)
    if let Some(from) = std::env::var("LIBERTY_REPLAY_FROM").ok().and_then(|v| v.parse::<f32>().ok()) {
        recs.retain(|r| r.t >= from);
        let t0 = recs.first().map_or(0.0, |r| r.t);
        for r in &mut recs {
            r.t -= t0;
        }
    }
    anyhow::ensure!(recs.len() > 2, "no frames in {}", path.display());
    // masses: ours per IV part vs IV's
    println!("masses (IV part: IV kg / ours kg):");
    let mut ours = [0.0f32; LINKS];
    for (i, p) in def.parts.iter().enumerate() {
        if let Some(k) = iv_index(def, i) {
            ours[k] += p.mass;
        }
    }
    let line: Vec<String> = (0..LINKS).map(|k| format!("{k}: {:.2}/{:.2}", recs[0].links[k].mass, ours[k])).collect();
    println!("  {}", line.join("  "));
    println!("  total IV {:.1} kg, ours {:.1} kg", recs[0].links.iter().map(|l| l.mass).sum::<f32>(), def.parts.iter().map(|p| p.mass).sum::<f32>());

    // ground: the live player's, else under the lowest foot link of the whole episode
    // an explicit LIBERTY_REPLAY_GROUND wins over a live file's ground
    let ground_gta = if std::env::var("LIBERTY_REPLAY_GROUND").is_ok() { ground_under(&recs[..1]) } else { live_ground.unwrap_or_else(|| ground_under(&recs)) };
    let mut rp = Replay::new(def, &recs[0], ground_gta, true);

    // run, driven by IV's numbers; measure at each recorded tick
    let out_path = path.with_extension("replay.csv");
    let mut csv = std::fs::File::create(&out_path)?;
    writeln!(csv, "{}", Measure::csv_head())?;
    // LIBERTY_REPLAY_FEET: the feet over the first second, IV's and ours: how far each slid along the
    // ground from its start (cm) and how far its up axis has turned from the start (deg)
    let feet = std::env::var("LIBERTY_REPLAY_FEET").is_ok();
    let iv_foot = |r: &Rec, k: usize| from_nm_v(r.com + r.links[k].pos - rec_rel_com(r));
    let our_foot = |rp: &Replay, s: usize| rp.sim.body.pose[rp.sim.link[def.feet[s]]];
    let iv0 = [iv_foot(&recs[0], 3), iv_foot(&recs[0], 6)];
    let our0 = [our_foot(&rp, 0), our_foot(&rp, 1)];
    let mut next_print = 0.0;
    for r in &recs[1..] {
        let m = rp.feed(def, r.clone());
        writeln!(csv, "{}", m.csv_row())?;
        if feet && r.t >= next_print && r.t <= 1.0 {
            next_print = r.t + 0.1;
            let mut line = format!("t {:.2} tilt IV {:5.1} ours {:5.1} |", r.t, m.iv_tilt, m.our_tilt);
            for (s, name) in [(0usize, "L"), (1, "R")] {
                let iv_slide = (iv_foot(r, [3, 6][s]) - iv0[s]).with_y(0.0).length() * 100.0;
                let p = our_foot(&rp, s);
                let our_slide = fv(p.pos - our0[s].pos).with_y(0.0).length() * 100.0;
                let turn = (p.rot * our0[s].rot.inverse()).to_axis_angle().1.to_degrees();
                // IV's foot: the same angle between its link rotation now and at the start
                let rot = |r: &Rec| Mat3::from_cols(r.links[[3, 6][s]].rows[0], r.links[[3, 6][s]].rows[1], r.links[[3, 6][s]].rows[2]).transpose();
                let rel = rot(r) * rot(&recs[0]).transpose();
                let iv_turn = ((rel.x_axis.x + rel.y_axis.y + rel.z_axis.z - 1.0) * 0.5).clamp(-1.0, 1.0).acos().to_degrees();
                // and how far each foot's sole has tipped from flat: the angle of its up axis from vertical
                let our_up = fv(p.rot * DVec3::Y);
                let our_tip = our_up.dot(Vec3::Y).clamp(-1.0, 1.0).acos().to_degrees();
                line += &format!(" foot {name}: slid IV {iv_slide:4.1} ours {our_slide:4.1} cm, turned IV {iv_turn:5.1} ours {turn:5.1} deg, ours up-axis tip {our_tip:5.1}, touching {} |", rp.sim.touching.get(rp.sim.link[def.feet[s]]).copied().unwrap_or(false));
            }
            println!("{line}");
        }
    }
    println!("RMS error per joint over {} recorded ticks (degrees: lean1 / lean2 / twist):", rp.ticks());
    let rms = rp.rms();
    for (k, m) in rp.map.iter().enumerate() {
        if matches!(m, Map::None) {
            continue;
        }
        println!("  {:10} {:6.1} {:6.1} {:6.1}", NAMES[k], rms[k][0], rms[k][1], rms[k][2]);
    }
    let last = recs.last().unwrap();
    println!(
        "COM height over ground at the end: IV {:.2} m, ours {:.2} m (start {:.2})",
        last.com.z - ground_gta,
        to_nm(rp.sim.whole_com()).z - ground_gta,
        recs[0].com.z - ground_gta
    );
    println!("wrote {}", out_path.display());
    Ok(())
}

pub(super) fn from_nm_v(v: Vec3) -> Vec3 {
    Vec3::new(v.x, v.z, -v.y)
}
