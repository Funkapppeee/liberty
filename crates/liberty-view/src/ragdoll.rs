//! Passive ragdoll. Everything about the body comes from the game: the 21 parts (capsule/box
//! shapes, masses) from the ped's `.wft` (`rage::ragdoll`) and the joint limits from the skeleton's
//! bone records (`Bone::rot_min` / `rot_max`). The solver is Rapier: the game's hinge
//! (`phJoint1Dof`: knees, elbows) and ball (`phJoint3Dof`) joints with their limits, as constraints
//! between the parts so the ragdoll starts exactly from the animated pose (a multibody restarts
//! from the rest pose, and Rapier 0.25's multibody has no 2-axis joint).

use bevy::prelude::*;
use rage::drawable::Bone;
use phys::artic::{Body, Drive, JointDef, JointKind, LinkDef, Pose};
use phys::contact::{self as pc, Collider, MovingBox, Tri};
use phys::glam::{DMat3, DQuat, DVec3};

use crate::assets::to_bevy;
use crate::collision::{self, ChunkCollision};

/// S1: IV's recorded body drives ours, for comparing the bodies alone (`liberty-view --replay`).
mod closed;
mod mini;
mod replay;
pub use closed::run as run_closed;
pub use mini::run as run_mini;
pub use replay::run as run_replay;

/// Physics steps per second (our solver: crates/phys).
const HZ: f32 = 120.0;
/// World collision is copied into Rapier around the body; rebuilt when it drifts this far.
const WORLD_RADIUS: f32 = 14.0;

fn dv(v: Vec3) -> DVec3 {
    DVec3::new(v.x as f64, v.y as f64, v.z as f64)
}

fn fv(v: DVec3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

fn to_pose(t: &Transform) -> Pose {
    let q = t.rotation;
    Pose::new(dv(t.translation), DQuat::from_xyzw(q.x as f64, q.y as f64, q.z as f64, q.w as f64).normalize())
}

fn from_pose(p: &Pose) -> Transform {
    let q = p.rot;
    Transform { translation: fv(p.pos), rotation: Quat::from_xyzw(q.x as f32, q.y as f32, q.z as f32, q.w as f32).normalize(), ..default() }
}

/// A bone's rest transform relative to its parent, in Bevy axes.
fn bone_local(b: &Bone) -> Transform {
    let [qx, qy, qz, qw] = b.local_rot;
    Transform { translation: Vec3::from(to_bevy(b.local_pos)), rotation: Quat::from_xyzw(qx, qz, -qy, qw).normalize(), ..default() }
}

/// GTA model-space matrix (rows = local axes, translation last) as a Bevy transform whose local
/// axes are the GTA ones mapped the same way as positions (X→X, Y→−Z, Z→Y).
fn from_gta_matrix(m: &[[f32; 4]; 4]) -> Transform {
    let v = |r: usize| Vec3::from(to_bevy([m[r][0], m[r][1], m[r][2]]));
    let rot = Mat3::from_cols(v(0), v(2), -v(1));
    Transform { translation: v(3), rotation: Quat::from_mat3(&rot).normalize(), ..default() }
}

/// Which muscle group a part's joint belongs to (Euphoria tunes legs / spine / arms separately).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Region {
    Leg,
    Spine,
    Arm,
}

/// Euphoria's tuning for the behaviours we run. Values from IV's `taskparams.txt` (NMBalance *,
/// NMBrace *), and where IV leaves a parameter to Euphoria's default, the default from RDR's
/// `behaviours.xml` (the same NaturalMotion behaviour set, shipped as data).
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    // bodyBalance (IV NMBalance: its field names are reused — Rockstar's comments say which is which)
    pub arm_stiffness: f32,
    pub arm_damping: f32,
    pub spine_stiffness: f32,
    pub spine_damping: f32,
    pub arms_out_mult: f32,
    pub arms_out_timeout: f32,
    pub ang_vel_mult: Vec3,
    pub ang_vel_threshold: Vec3,
    // configureBalance (RDR defaults; IV doesn't override them)
    pub leg_stiffness: f32,
    /// balanceAbortThreshold: IV's balancer fails when the COM is further (horizontally) from the feet
    /// than this × its height above them (crates/euphoria::balancer).
    pub abort_lean: f32,
    /// Unused since the IV balance check (it hard-codes 0.5 m); kept for the tuning file.
    pub give_up_height: f32,
    // braceForImpact (IV NMBrace)
    pub brace_distance: f32,
    pub brace_prediction: f32,
    pub brace_absorb: f32,
    pub brace_body_stiffness: f32,
    pub brace_leg_stiffness: f32,
    pub stay_upright: f32,
}

impl Tuning {
    pub fn from_taskparams(tp: &rage::taskparams::TaskParams) -> Tuning {
        let f = |task: &str, name: &str, default: f32| tp.f32(task, "*", name).unwrap_or(default);
        let v = |task: &str, name: &str, default: [f32; 3]| Vec3::from(tp.vec3(task, "*", name).unwrap_or(default));
        Tuning {
            arm_stiffness: f("NMBalance", "balanceStiffness", 9.0),
            arm_damping: f("NMBalance", "balanceDamping", 0.7),
            // "balanceHeadLookTimeout 10.0 ; NB. SPINE STIFFNESS", "balanceBodyTurnTimeout ; NB. SPINE DAMPING"
            spine_stiffness: f("NMBalance", "balanceHeadLookTimeout", 10.0),
            spine_damping: f("NMBalance", "balanceBodyTurnTimeout", 1.0),
            arms_out_mult: f("NMBalance", "balanceArmsOutOnPushMultiplier", 1.0),
            arms_out_timeout: f("NMBalance", "balanceArmsOutOnPushTimeout", 1.1),
            ang_vel_mult: v("NMBalance", "balanceAngVelMultiplier", [4.0, 1.0, 4.0]),
            ang_vel_threshold: v("NMBalance", "balanceAngVelThreshold", [1.2, 3.0, 1.2]),
            leg_stiffness: 12.0,
            abort_lean: 0.6,
            give_up_height: 0.5,
            brace_distance: f("NMBrace", "braceDistance", 0.65),
            brace_prediction: f("NMBrace", "braceTargetPredictionTime", 0.45),
            brace_absorb: f("NMBrace", "braceReachAbsorbtionTime", 0.15),
            brace_body_stiffness: 12.0,
            brace_leg_stiffness: f("NMBrace", "braceLegStiffness", 12.0),
            stay_upright: f("NMBrace", "stayUprightStiffness", 3.0),
        }
    }
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning::from_taskparams(&rage::taskparams::TaskParams::default())
    }
}

/// What the ragdoll is doing: a Euphoria behaviour, or just going limp.
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    /// Limp (the old passive ragdoll; Euphoria's bodyRelax).
    Relax,
    /// bodyBalance: hold the pose, stay up, arms out when pushed; gives up past the abort lean.
    Balance,
    /// braceForImpact: hands out toward the front of the car at `car`, then absorb and hand over
    /// to the fall IV's brace picks (crates/euphoria::fall).
    Brace { car: u64 },
    /// catchFall, with the stiffness block IV's brace hands it (arms, legs, back). Driven by IV's own
    /// catchFall logic (crates/euphoria::catchfall) when started from the brace; the arm IK is ours.
    CatchFall { arms: f32, legs: f32, back: f32 },
    /// rollDownStairs, with the stiffness IV's brace hands it: tucked, arms in. APPROXIMATION of
    /// the behaviour itself until it has its own spec.
    Roll { stiffness: f32 },
}

/// A car as the ragdoll sees it: a moving box (kinematic body in its physics world).
#[derive(Clone, Copy, Debug)]
pub struct CarBox {
    pub id: u64,
    pub pos: Vec3,
    pub rot: Quat,
    pub vel: Vec3,
    pub lo: Vec3,
    pub hi: Vec3,
}

/// Euphoria's per-joint muscle damping, from its character rig (RDR's cowboy.art, decoded by
/// age::nmart: the d in each joint's 300 300 d 20 100 muscle block), keyed by the IV bone the
/// joint moves. Its muscle strength is 300 on every joint.
const NM_DAMPING: &[(&str, f32)] = &[
    ("char_spine", 1.0), ("char_spine1", 1.0), ("char_spine2", 1.0), ("char_spine3", 1.0),
    ("char_neck", 0.5), ("char_head", 0.5),
    ("char_l_clavicle", 1.0), ("char_r_clavicle", 1.0), ("char_l_upperarm", 0.5), ("char_r_upperarm", 0.5),
    ("char_l_forearm", 0.5), ("char_r_forearm", 0.5), ("char_l_hand", 10.0), ("char_r_hand", 10.0),
    ("char_l_thigh", 0.5), ("char_r_thigh", 0.5), ("char_l_calf", 0.5), ("char_r_calf", 0.5),
    ("char_l_foot", 5.0), ("char_r_foot", 5.0),
];

/// A part's collision shape, in the part's own frame (Bevy axes; capsules along local Z).
#[derive(Clone, Copy, Debug)]
enum PartShape {
    Capsule { radius: f32, half: f32 },
    Box { half: Vec3 },
    Ball { radius: f32 },
}

impl PartShape {
    fn collider(&self) -> pc::Shape {
        match *self {
            PartShape::Capsule { radius, half } => pc::Shape::Capsule { radius: radius as f64, half_len: half as f64 },
            PartShape::Box { half } => pc::Shape::Box { half: dv(half) },
            PartShape::Ball { radius } => pc::Shape::Sphere { radius: radius as f64 },
        }
    }

    /// Inertia about the shape's centre, in its frame, for mass m.
    fn inertia(&self, m: f64) -> DMat3 {
        match *self {
            PartShape::Capsule { radius, half } => {
                let (r, l) = (radius as f64, 2.0 * half as f64 + radius as f64);
                let ixx = m * (3.0 * r * r + l * l) / 12.0;
                DMat3::from_diagonal(DVec3::new(ixx, ixx, 0.5 * m * r * r))
            }
            PartShape::Box { half } => {
                let h = dv(half);
                DMat3::from_diagonal(DVec3::new(h.y * h.y + h.z * h.z, h.x * h.x + h.z * h.z, h.x * h.x + h.y * h.y) * (m / 3.0))
            }
            PartShape::Ball { radius } => DMat3::from_diagonal(DVec3::splat(0.4 * m * (radius as f64).powi(2))),
        }
    }
}

struct PartDef {
    region: Region,
    /// Euphoria's muscle damping for this joint (NM_DAMPING).
    damping: f32,
    /// For arm capsules: +1 / −1 = which way along the capsule (local Z) points toward the next joint.
    along: f32,
    bone: usize,
    shape: PartShape,
    mass: f32,
    /// The part relative to its bone.
    offset: Transform,
    parent: Option<usize>,
    /// Joint to the parent part: frames in each body, and angular limits (None = locked axis).
    frame1: Transform,
    frame2: Transform,
    limits: [Option<[f32; 2]>; 3],
    /// The part whose rigid body carries this one: itself, or — for a joint the game locks on every
    /// axis (feet, neck, clavicles, spine2) — its parent's body, welded on. Exact for a locked joint,
    /// and keeps the game's 0.08 kg neck/clavicles from being solver links between 6 kg parts.
    body: usize,
    /// This part relative to its body part, at rest.
    rel: Transform,
    /// For joints built from Fred.art: IV's joint-angle convention (specs/euphoria/armIK.md).
    nm: Option<NmJoint>,
}

/// A Fred.art 3-DOF joint as Euphoria measures it. Angle triples are (lean1, lean2, twist).
#[derive(Clone, Copy, Debug)]
struct NmJoint {
    /// Limit centres and half-ranges as the joint's limits give them.
    centre: [f32; 3],
    half: [f32; 3],
    /// Axes the joint mirrors (right shoulders and hips: lean2 and twist).
    mirror: [bool; 3],
    /// Euphoria's child frame (rows: lean-dir, lean axis, twist) in our joint frame's coordinates:
    /// Fred.art's axes turned about the twist axis by -twistCentre.
    k: Quat,
    /// The joint's rotation at rest, in Euphoria's frame: raw angles = -centre.
    q_rest: Quat,
}

impl NmJoint {
    fn new(l: &[f32], right: bool) -> NmJoint {
        let centre = [(l[0] + l[1]) * 0.5, (l[2] + l[3]) * 0.5, (l[4] + l[5]) * 0.5];
        let half = [(l[1] - l[0]) * 0.5, (l[3] - l[2]) * 0.5, (l[5] - l[4]) * 0.5];
        let a = -centre[2];
        let (e0, e1) = (Vec3::new(0.0, a.cos(), a.sin()), Vec3::new(0.0, -a.sin(), a.cos()));
        let k = Quat::from_mat3(&Mat3::from_cols(e0, e1, Vec3::X)).normalize();
        NmJoint { centre, half, mirror: [false, right, right], k, q_rest: nm_from_raw(-centre[2], -centre[0], -centre[1]) }
    }

    /// Joint angles (lean1, lean2, twist) of a rotation given in Euphoria's frame.
    fn angles(&self, q: Quat) -> [f32; 3] {
        let (tw, s1, s2) = nm_raw(q);
        let raw = [s1, s2, tw];
        std::array::from_fn(|i| if self.mirror[i] { -raw[i] - self.centre[i] } else { raw[i] + self.centre[i] })
    }

    /// The rotation in Euphoria's frame with these joint angles.
    fn rotation(&self, a: [f32; 3]) -> Quat {
        let raw: [f32; 3] = std::array::from_fn(|i| if self.mirror[i] { -a[i] - self.centre[i] } else { a[i] - self.centre[i] });
        nm_from_raw(raw[2], raw[0], raw[1])
    }

    /// Centres as the IK adds them (the effector's own, mirrored axes negated).
    fn ik_centre(&self) -> [f32; 3] {
        std::array::from_fn(|i| if self.mirror[i] { -self.centre[i] } else { self.centre[i] })
    }
}

/// Euphoria's raw angles of a rotation: twist about z and the swing over y/x, as 4·atan of the
/// quarter-angle tangent (armIK.md, Joint angles).
fn nm_raw(q: Quat) -> (f32, f32, f32) {
    let (x, y, z, w) = (q.x, q.y, q.z, q.w);
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
        (if w > 0.0 { z / (w + r) } else { z / (w - r) }, (w * y + x * z) * k, (y * z - w * x) * k)
    };
    let m = (s1 * s1 + s2 * s2).sqrt();
    let f = if m > 0.0 { m.atan() * 4.0 / m } else { 0.0 };
    (t.atan() * 4.0, s1 * f, s2 * f)
}

/// The rotation with these raw angles: twist about z first, then the swing.
fn nm_from_raw(tw: f32, s1: f32, s2: f32) -> Quat {
    let sw = (s1 * s1 + s2 * s2).sqrt();
    let qs = if sw > 0.0 {
        let k = (sw * 0.5).sin() / sw;
        Quat::from_xyzw(-s2 * k, s1 * k, 0.0, (sw * 0.5).cos())
    } else {
        Quat::IDENTITY
    };
    qs * Quat::from_xyzw(0.0, 0.0, (tw * 0.5).sin(), (tw * 0.5).cos())
}

/// The ped's ragdoll as the game defines it, ready to instance.
pub struct RagdollDef {
    parts: Vec<PartDef>,
    pub parents: Vec<Option<usize>>,
    pub rest_local: Vec<Transform>,
    pub rest_global: Vec<Transform>,
    head: usize,
    /// Part indices: upper arms and forearms (left, right) and the upper spine (chest).
    upper: [usize; 2],
    fore: [usize; 2],
    chest: usize,
    /// Part indices of the feet (left, right).
    feet: [usize; 2],
    /// Part indices of spine_0..spine_3 (the parts whose joints are Euphoria's spine joints).
    spine: [usize; 4],
    /// Part indices of the thighs (Euphoria's hip joints), calves (knees) and hands (wrists), left/right.
    thighs: [usize; 2],
    calves: [usize; 2],
    hands: [usize; 2],
    /// Part index of the head (`head` is its skeleton bone).
    head_part: usize,
    /// What IV's headLook reads from Fred.art (None without the rig).
    nm_look: Option<NmLook>,
    /// Each part as IV names it (the shot's reach for the wound picks hands by it).
    iv_part: Vec<Option<euphoria::reach::Part>>,
}

/// IV's name for one of our ragdoll's parts, from its bone.
fn iv_part_of(bone: &str) -> Option<euphoria::reach::Part> {
    use euphoria::reach::{Part::*, Side::*};
    Some(match bone {
        "char_pelvis" => Pelvis,
        "char_spine" => Spine0,
        "char_spine1" => Spine1,
        "char_spine2" => Spine2,
        "char_spine3" => Spine3,
        "char_neck" => Neck,
        "char_head" => Head,
        "char_l_thigh" => Thigh(Left),
        "char_l_calf" => Shin(Left),
        "char_l_foot" => Foot(Left),
        "char_r_thigh" => Thigh(Right),
        "char_r_calf" => Shin(Right),
        "char_r_foot" => Foot(Right),
        "char_l_clavicle" => Clavicle(Left),
        "char_l_upperarm" => UpperArm(Left),
        "char_l_forearm" => Forearm(Left),
        "char_l_hand" => Hand(Left),
        "char_r_clavicle" => Clavicle(Right),
        "char_r_upperarm" => UpperArm(Right),
        "char_r_forearm" => Forearm(Right),
        "char_r_hand" => Hand(Right),
        _ => return None,
    })
}

/// Fred.art data headLook needs: spine3's part frame rows in spine3's body (our axes), the
/// spine3-to-head distance, and the joints' limits (lean1 min/max, lean2 min/max, twist min/max).
#[derive(Clone, Copy, Debug)]
struct NmLook {
    rows: [Vec3; 3],
    eye_offset: f32,
    neck: [f32; 6],
    head: [f32; 6],
    spine3: [f32; 6],
    spine2: [f32; 6],
}

impl RagdollDef {
    pub fn new(skeleton: &[Bone], body: &rage::ragdoll::Ragdoll, rig: &rage::nmart::Biped) -> RagdollDef {
        let nm = &rig.joints;
        let parents: Vec<Option<usize>> = skeleton.iter().map(|b| b.parent).collect();
        let rest_local: Vec<Transform> = skeleton.iter().map(bone_local).collect();
        let mut rest_global: Vec<Transform> = Vec::with_capacity(skeleton.len());
        for (i, l) in rest_local.iter().enumerate() {
            let g = parents[i].map_or(*l, |p| rest_global[p].mul_transform(*l));
            rest_global.push(g);
        }
        let part_of_bone = |b: usize| body.parts.iter().position(|p| p.bone as usize == b);
        let world: Vec<Transform> = body.parts.iter().map(|p| from_gta_matrix(&p.matrix)).collect();
        let parts = body
            .parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let bone = p.bone as usize;
                // capsules run along the part's GTA local Y = Bevy local −Z
                let shape = match p.shape {
                    rage::ragdoll::Shape::Capsule { radius, half_length } => PartShape::Capsule { radius, half: half_length.max(0.01) },
                    rage::ragdoll::Shape::Box { half } => PartShape::Box { half: Vec3::new(half[0], half[2], half[1]) },
                    rage::ragdoll::Shape::Other(_) => PartShape::Ball { radius: 0.05 },
                };
                let mut up = parents[bone];
                let mut parent = None;
                while let Some(b) = up {
                    if let Some(pp) = part_of_bone(b) {
                        parent = Some(pp);
                        break;
                    }
                    up = parents[b];
                }
                // the spine hangs off the skeleton root, not the pelvis bone: every part but the
                // pelvis itself must still join the body (or the upper half flies off on its own)
                if parent.is_none() && i != 0 {
                    parent = Some(0);
                }
                // joint at the child bone; its axes are the MODEL's axes at rest (X right, Y forward,
                // Z up): the game's limits line up with those (knees bend about X, where their -140..0
                // limit is), not with the bone's own local axes
                let mut joint = Transform::from_translation(rest_global[bone].translation);
                let b = &skeleton[bone];
                let lim = |lo: f32, hi: f32| if hi - lo > 1e-3 { Some([lo, hi]) } else { None };
                // GTA X → Bevy X, GTA Z → Bevy Y, GTA Y → Bevy −Z (range flips)
                let mut limits = [lim(b.rot_min[0], b.rot_max[0]), lim(b.rot_min[2], b.rot_max[2]), lim(-b.rot_max[1], -b.rot_min[1])];
                let name = skeleton[bone].name.as_str();
                // Shoulders, spine and hips: Euphoria's own joints (Fred.art), as IV builds them when
                // Euphoria drives the ped (the skeleton's limits lock the forward raise a brace needs).
                // Fred.art gives the twist axis and a lean1 direction. lean1 swings the twist axis
                // toward that direction (spine: forward bend to +20°, back to −10°), i.e. a rotation
                // about twist × lean1; lean2 swings it toward twist × lean1, i.e. about −lean1.
                // Frame: X = twist, Y = lean1 direction, Z = X × Y, turned from Fred's rest onto this
                // skeleton's rest bone. Per-axis angles: X = twist, Y = −lean2, Z = lean1.
                let nm_map = [
                    ("char_l_upperarm", "shoulder_left", "char_l_forearm"),
                    ("char_r_upperarm", "shoulder_right", "char_r_forearm"),
                    ("char_spine", "spine_0", "char_spine1"),
                    ("char_spine1", "spine_1", "char_spine2"),
                    ("char_spine2", "spine_2", "char_spine3"),
                    ("char_spine3", "spine_3", "char_neck"),
                    // the head nods and turns on Euphoria's upper neck joint (headLook drives it)
                    ("char_head", "neck_upper", ""),
                    ("char_l_thigh", "hip_left", "char_l_calf"),
                    ("char_r_thigh", "hip_right", "char_r_calf"),
                    // wrists: no child bone named; the hand's axis continues the forearm
                    ("char_l_hand", "wrist_left", ""),
                    ("char_r_hand", "wrist_right", ""),
                    // ankles: the skeleton's own limits lock the feet, but IV's balancer leans the
                    // body through them, so they are Euphoria's joints too
                    ("char_l_foot", "ankle_left", ""),
                    ("char_r_foot", "ankle_right", ""),
                ];
                let mut nm_joint = None;
                if let Some(&(_, nm_name, next)) = nm_map.iter().find(|(b, _, _)| *b == name) {
                    let j = nm.iter().find(|j| j.name == nm_name).filter(|j| !j.hinge && j.vectors.len() >= 3 && j.limits.len() >= 6);
                    // the direction the joint's twist axis is aligned to: toward the next bone, or (no next
                    // bone) along the parent segment into this joint
                    let bone_dir = match skeleton.iter().position(|b| b.name == next) {
                        Some(nb) => Some((rest_global[nb].translation - rest_global[bone].translation).normalize()),
                        None => parents[bone].map(|pb| (rest_global[bone].translation - rest_global[pb].translation).normalize()),
                    };
                    if let (Some(j), Some(bone_dir)) = (j, bone_dir) {
                        let x = Vec3::from(crate::assets::to_bevy(j.vectors[1])).normalize();
                        let l1 = Vec3::from(crate::assets::to_bevy(j.vectors[2]));
                        let y = (l1 - x * l1.dot(x)).normalize();
                        let z = x.cross(y);
                        // (an ankle's twist axis runs forward along the foot, not down a bone:
                        // Fred.art's own axes are used as they are; checked against a real
                        // pedestrian's ankle frame, see the debug line in `start`)
                        let align = if nm_name.starts_with("ankle") { Quat::IDENTITY } else { Quat::from_rotation_arc(x, bone_dir) };
                        joint.rotation = (align * Quat::from_mat3(&Mat3::from_cols(x, y, z))).normalize();
                        let l = &j.limits;
                        limits = [lim(l[4], l[5]), lim(-l[3], -l[2]), lim(l[0], l[1])];
                        nm_joint = Some(NmJoint::new(l, nm_name.ends_with("right")));
                    }
                }
                let in_body = |t: &Transform| Transform::from_matrix(t.compute_matrix().inverse() * joint.compute_matrix());
                let frame1 = parent.map_or(joint, |pp| in_body(&world[pp]));
                let frame2 = in_body(&world[i]);
                let region = if ["thigh", "calf", "foot"].iter().any(|s| name.contains(s)) {
                    Region::Leg
                } else if ["clavicle", "upperarm", "forearm", "hand"].iter().any(|s| name.contains(s)) {
                    Region::Arm
                } else {
                    Region::Spine
                };
                // which way along the capsule the limb continues (upper arm → elbow, forearm → wrist)
                let next = name.replace("upperarm", "forearm").replace("forearm", if name.contains("forearm") { "hand" } else { "forearm" });
                let along = skeleton
                    .iter()
                    .position(|b| b.name == next && next != name)
                    .map_or(1.0, |n| (rest_global[n].translation - rest_global[bone].translation).dot(world[i].rotation * Vec3::Z).signum());
                let damping = NM_DAMPING.iter().find(|(n, _)| *n == name).map_or(1.0, |(_, d)| *d);
                PartDef { region, damping, along, bone, shape, mass: p.mass, offset: Transform::from_matrix(rest_global[bone].compute_matrix().inverse() * world[i].compute_matrix()), parent, frame1, frame2, limits, body: i, rel: Transform::IDENTITY, nm: nm_joint }
            })
            .collect::<Vec<_>>();
        let mut parts = parts;
        for i in 0..parts.len() {
            let Some(pp) = parts[i].parent else { continue };
            let pb = parts[pp].body;
            if parts[i].limits.iter().all(Option::is_none) {
                parts[i].body = pb;
                parts[i].rel = Transform::from_matrix(world[pb].compute_matrix().inverse() * world[i].compute_matrix());
            } else {
                // the joint's parent side is on whatever body carries the parent part
                parts[i].frame1 = Transform::from_matrix(world[pb].compute_matrix().inverse() * world[pp].compute_matrix() * parts[i].frame1.compute_matrix());
            }
        }
        let head = skeleton.iter().position(|b| b.name == "char_head").unwrap_or(0);
        let part = |n: &str| parts.iter().position(|p| skeleton[p.bone].name == n).unwrap_or(0);
        let (upper, fore, chest) = ([part("char_l_upperarm"), part("char_r_upperarm")], [part("char_l_forearm"), part("char_r_forearm")], part("char_spine3"));
        let feet = [part("char_l_foot"), part("char_r_foot")];
        let spine = [part("char_spine"), part("char_spine1"), part("char_spine2"), part("char_spine3")];
        let (thighs, calves, hands) = ([part("char_l_thigh"), part("char_r_thigh")], [part("char_l_calf"), part("char_r_calf")], [part("char_l_hand"), part("char_r_hand")]);
        let head_part = part("char_head");
        // what IV's headLook reads: spine3's part frame as Fred.art defines it (rows turned from the
        // GTA axes into ours, then into spine3's body at rest), the spine3-to-head distance, and the
        // neck, head and spine joints' limits (lean1 min/max, lean2 min/max, twist min/max)
        let limits = |n: &str| nm.iter().find(|j| j.name == n && j.limits.len() >= 6).map(|j| [j.limits[0], j.limits[1], j.limits[2], j.limits[3], j.limits[4], j.limits[5]]);
        let nm_look = match (rig.part_frames.get(10), limits("neck_lower"), limits("neck_upper"), limits("spine_3"), limits("spine_2")) {
            (Some(f), Some(neck), Some(head_l), Some(spine3), Some(spine2)) => {
                let to_body = world[chest].rotation.inverse();
                let row = |k: usize| to_body * Vec3::from(crate::assets::to_bevy([f[3 * k], f[3 * k + 1], f[3 * k + 2]]));
                let eye_offset = (world[head_part].translation - world[chest].translation).length();
                Some(NmLook { rows: [row(0), row(1), row(2)], eye_offset, neck, head: head_l, spine3, spine2 })
            }
            _ => None,
        };
        let iv_part: Vec<Option<euphoria::reach::Part>> = parts.iter().map(|p| iv_part_of(&skeleton[p.bone].name)).collect();
        // masses: Fred.art's (the physics masses IV's Euphoria body has: 77.7 kg, pelvis 11, thigh
        // 7.6, forearm 1.1 ...; checked against the 1 Oct session's live links). The ped's .wft
        // fragment records give placeholder-like values (6.15 kg for most parts) and were used
        // until 8 Oct. LIBERTY_WFT_MASSES brings those back.
        if rig.part_masses.len() == 21 && std::env::var("LIBERTY_WFT_MASSES").is_err() {
            for (i, p) in parts.iter_mut().enumerate() {
                let k = iv_part[i].and_then(|ip| (0..21).find(|&k| euphoria::reach::Part::from_iv_index(k) == Some(ip)));
                if let Some(k) = k {
                    p.mass = rig.part_masses[k];
                }
            }
        }
        RagdollDef { parts, parents, rest_local, rest_global, head, head_part, upper, fore, chest, feet, spine, thighs, calves, hands, nm_look, iv_part }
    }

    /// Which arm (0 left, 1 right) a part belongs to: the clavicle, upper arm, forearm or hand.
    fn arm_side(&self, i: usize) -> Option<usize> {
        let mut at = Some(i);
        while let Some(p) = at {
            if self.parts[p].region != Region::Arm {
                return None;
            }
            if let Some(s) = (0..2).find(|&s| self.upper[s] == p || self.parts[self.upper[s]].parent == Some(p)) {
                return Some(s);
            }
            at = self.parts[p].parent;
        }
        None
    }

    pub fn pelvis_bone(&self) -> usize {
        self.parts[0].bone
    }

    pub fn head_bone(&self) -> usize {
        self.head
    }

    /// World transform of every joint from a pose of local transforms and the ped's root.
    pub fn globals(&self, root: &Transform, locals: &[Transform]) -> Vec<Transform> {
        let mut g: Vec<Transform> = Vec::with_capacity(locals.len());
        for (i, l) in locals.iter().enumerate() {
            let parent = self.parents[i].map_or(*root, |p| g[p]);
            g.push(parent.mul_transform(*l));
        }
        g
    }
}

/// One arm-IK call's options (armIK.md, Inputs per solve).
#[derive(Clone, Copy, Debug)]
struct ArmCall {
    twist: f32,
    lead: f32,
    reference_vel: Option<Vec3>,
    /// The shoulder's Euphoria stiffness and damping ratio as the behaviour leaves them (the lead and
    /// limiter read the muscle).
    stiffness: f32,
    damping_ratio: f32,
    limiter: Option<(f32, f32)>,
}

/// catchFall's stiffness requests for one frame (Euphoria stiffness, damping ratio 0.5; wrists 1.0).
#[derive(Clone, Copy, Debug)]
struct CatchStiff {
    torso: f32,
    legs: f32,
    arms: [f32; 2],
    wrists: [f32; 2],
}

pub struct RagdollSim {
    /// The whole ragdoll as one articulated body (crates/phys): one link per part that has its own
    /// body; welded parts ride on their parent's link.
    body: Body,
    colliders: Vec<Collider>,
    /// Per part: the link that carries it.
    link: Vec<usize>,
    /// The map collision around the body, and where it was gathered.
    tris: Vec<Tri>,
    world_at: Option<Vec3>,
    /// Cars near the body, as moving boxes (they push; the body can't push them).
    boxes: Vec<MovingBox>,
    /// Per link: touching anything last step.
    touching: Vec<bool>,
    accumulator: f32,
    /// Seconds since it started, and seconds it has been (nearly) still.
    pub age: f32,
    pub still: f32,
    /// The pose it started in, as joint angles: what the muscles hold unless a behaviour says otherwise.
    base: Vec<[f32; 3]>,
    pub mode: Mode,
    tuning: Tuning,
    mode_age: f32,
    /// When a car first touched the body.
    car_hit: Option<f32>,
    /// How far the arms are out (bodyBalance's armsOutOnPush), decaying.
    arms_out: f32,
    /// IV's braceForImpact logic (crates/euphoria), started on the first brace frame.
    nm_brace: Option<(euphoria::brace::Brace, euphoria::rng::Rng)>,
    /// IV's spine bend b, carried between frames (crates/euphoria::torso).
    nm_torso: euphoria::torso::Torso,
    /// IV's catchFall (crates/euphoria::catchfall), started when the brace hands over to it, and the
    /// stiffness it asked for this frame.
    nm_catch: Option<euphoria::catchfall::CatchFall>,
    catch_stiff: Option<CatchStiff>,
    /// IV balancer's smoothed stability (bal+0x9ec), carried between checks.
    bal_stability: f32,
    /// IV's balancer proper (foot placement, pelvis control, leg solve: crates/euphoria), once it
    /// drives the legs (LIBERTY_NM_BALANCE).
    nm_bal: Option<NmBal>,
    /// IV's balancer drives the legs in Balance (LIBERTY_NM_BALANCE, or a shot).
    nm_on: bool,
    /// IV's shot behaviour, while it runs (see `shot_hit`).
    shot: Option<ShotNm>,
    /// IV's bodyBalance (arms, spine and head while the balancer holds him up), once it runs.
    nm_body: Option<NmBody>,
    /// bodyBalance succeeded: he's steady again and can go back to animation.
    pub recovered: bool,
    /// The character's Euphoria orientation rows as IV sets them once when the ragdoll starts (live
    /// memory, 1 Oct session: +0x10 his right, +0x20 up, +0x30 backward, all horizontal but up):
    /// right and backward, in our axes. Fed to brace, catchFall and the shot's reach.
    nm_rows: [Vec3; 2],
}

impl RagdollSim {
    /// Instance the ragdoll from the joints' current world transforms, all parts moving at `vel`.
    pub fn start<'a>(
        def: &RagdollDef,
        joints: &[Transform],
        vel: Vec3,
        chunks: impl IntoIterator<Item = &'a ChunkCollision> + Clone,
        mode: Mode,
        tuning: Tuning,
    ) -> RagdollSim {
        let np = def.parts.len();
        // links: the parts with a body of their own, parents first (breadth-first from the pelvis)
        let mut order = vec![0usize];
        let mut k = 0;
        while k < order.len() {
            let cur = order[k];
            for (i, p) in def.parts.iter().enumerate() {
                if p.body == i && i != 0 && p.parent.map(|pp| def.parts[pp].body) == Some(cur) && !order.contains(&i) {
                    order.push(i);
                }
            }
            k += 1;
        }
        let mut link_of_body = vec![0usize; np];
        for (l, &pi) in order.iter().enumerate() {
            link_of_body[pi] = l;
        }
        let link: Vec<usize> = def.parts.iter().map(|p| link_of_body[p.body]).collect();
        // mass properties per link, from every part it carries
        let mut links: Vec<LinkDef> = Vec::with_capacity(order.len());
        for (l, &pi) in order.iter().enumerate() {
            let carried: Vec<usize> = (0..np).filter(|&i| link[i] == l).collect();
            let pm = |i: usize| def.parts[i].mass.max(0.05) as f64;
            let mass: f64 = carried.iter().map(|&i| pm(i)).sum();
            let com = carried.iter().map(|&i| dv(def.parts[i].rel.translation) * pm(i)).sum::<DVec3>() / mass;
            let mut inertia = DMat3::ZERO;
            for &i in &carried {
                let p = &def.parts[i];
                let r = DMat3::from_quat(to_pose(&p.rel).rot);
                let d = dv(p.rel.translation) - com;
                inertia += r * p.shape.inertia(pm(i)) * r.transpose() + (DMat3::IDENTITY * d.length_squared() - outer(d, d)) * pm(i);
            }
            let joint = (l > 0).then(|| {
                let p = &def.parts[pi];
                let free: Vec<usize> = (0..3).filter(|&k| p.limits[k].is_some()).collect();
                JointDef {
                    parent: link[p.parent.unwrap_or(0)],
                    frame_parent: to_pose(&p.frame1),
                    frame_child: to_pose(&p.frame2),
                    kind: if free.len() == 1 { JointKind::Hinge(free[0]) } else { JointKind::Ball },
                    limits: p.limits.map(|l| l.map(|[a, b]| [a as f64, b as f64])),
                }
            });
            links.push(LinkDef { mass, com, inertia, joint });
        }
        let mut body = Body::new(links, DVec3::new(0.0, -9.81, 0.0));
        let colliders: Vec<Collider> = def
            .parts
            .iter()
            .enumerate()
            .map(|(i, p)| Collider { link: link[i], local: to_pose(&p.rel), shape: p.shape.collider(), friction: std::env::var("LIBERTY_RAG_FRICTION").ok().and_then(|v| v.parse().ok()).unwrap_or(0.9) })
            .collect();
        // start from the animated pose, inside the game's limits (a joint that starts outside its
        // limits snaps back violently)
        let poses: Vec<Pose> = order.iter().map(|&pi| to_pose(&joints[def.parts[pi].bone].mul_transform(def.parts[pi].offset))).collect();
        body.set_from_poses(&poses, dv(vel));
        body.clamp_to_limits();
        let mut base = vec![[0.0f32; 3]; np];
        for (l, &pi) in order.iter().enumerate().skip(1) {
            base[pi] = body.angles(l).map(|a| a as f32);
        }
        let mut sim = RagdollSim {
            body,
            colliders,
            link,
            tris: Vec::new(),
            world_at: None,
            boxes: Vec::new(),
            touching: Vec::new(),
            accumulator: 0.0,
            age: 0.0,
            still: 0.0,
            base,
            mode,
            tuning,
            mode_age: 0.0,
            car_hit: None,
            arms_out: 0.0,
            nm_brace: None,
            nm_torso: euphoria::torso::Torso::default(),
            nm_catch: None,
            bal_stability: 0.0,
            nm_bal: None,
            nm_on: std::env::var("LIBERTY_NM_BALANCE").is_ok(),
            shot: None,
            nm_body: None,
            catch_stiff: None,
            recovered: false,
            nm_rows: [Vec3::X, Vec3::Z],
        };
        // his right from the shoulders as he stands now, backward = right x up
        let d = sim.body(def.upper[1]).translation - sim.body(def.upper[0]).translation;
        let right = Vec3::new(d.x, 0.0, d.z).normalize_or(Vec3::X);
        sim.nm_rows = [right, right.cross(Vec3::Y)];
        sim.muscles(def, &sim.base.clone());
        sim.refresh_world(chunks.clone());
        // The animated pose stands a few centimetres above where our foot boxes would rest (the boxes
        // are the game's, but RAGE also gives each bound a margin, which we don't read), so a ragdoll
        // used to start with a short free fall. STAND-IN for that margin: each foot's box is moved
        // down to the ground it stands over, if the gap is small.
        for side in 0..2 {
            let part = def.feet[side];
            if def.parts[part].body != part {
                continue;
            }
            let pc::Shape::Box { half } = sim.colliders[part].shape else { continue };
            let t = sim.part(def, part);
            let low = (0..8)
                .map(|c| {
                    let corner = Vec3::new(if c & 1 == 0 { -1.0 } else { 1.0 }, if c & 2 == 0 { -1.0 } else { 1.0 }, if c & 4 == 0 { -1.0 } else { 1.0 }) * fv(half);
                    (t.translation + t.rotation * (fv(sim.colliders[part].local.pos) + corner)).y
                })
                .fold(f32::MAX, f32::min);
            if let Some(g) = collision::ground_height(chunks.clone(), t.translation.x, t.translation.z, t.translation.y + 0.5) {
                let gap = low - g;
                if gap > 0.0 && gap < 0.09 {
                    sim.colliders[part].local.pos += dv(t.rotation.inverse() * Vec3::new(0.0, -gap, 0.0));
                }
                if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                    info!("  foot {side}: box bottom was {gap:.3} above the ground");
                }
            }
        }
        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
            info!("ragdoll start: {} links, {} DOF, {:.1} kg, {} world tris, vel {vel:?}", order.len(), sim.body.ndof(), sim.body.mass(), sim.tris.len());
            info!("  part masses: {}", def.parts.iter().enumerate().map(|(i, p)| format!("{}:{:.1}", i, p.mass)).collect::<Vec<_>>().join(" "));
            // which way each leg part's own axes point at the start (for feeding IV's balancer)
            let r = |v: Vec3| format!("({:5.2} {:5.2} {:5.2})", v.x, v.y, v.z);
            for (name, part) in [("pelvis", 0), ("thigh L", def.thighs[0]), ("thigh R", def.thighs[1]), ("calf L", def.calves[0]), ("calf R", def.calves[1]), ("foot L", def.feet[0]), ("foot R", def.feet[1]), ("chest", def.chest)] {
                let t = sim.part(def, part);
                info!("  axes {name:8} at {} x {} y {} z {}  (own body: {}, limits {:?})", r(t.translation), r(t.rotation * Vec3::X), r(t.rotation * Vec3::Y), r(t.rotation * Vec3::Z), def.parts[part].body == part, def.parts[part].limits);
            }
            for side in 0..2 {
                let f = sim.part(def, def.feet[side]).translation;
                let g = collision::ground_height(chunks.clone(), f.x, f.z, f.y + 0.5);
                info!("  foot {side}: part origin {:.3} above the collision ground ({:?}); a real standing pedestrian's is 0.012", f.y - g.unwrap_or(f32::NAN), g);
            }
            // an ankle's parent-side frame in the calf part's own coordinates (a, b, c = part x, -z, y).
            // The real game's (session recording, both legs): a (0 1 0), b (1 0 0), c (0 0 -1), at (0 -0.161 0)
            for side in 0..2 {
                if let Some((nm, p_rot, c_rot, pos)) = sim.nm_frames(def, def.feet[side]) {
                    let c = sim.part(def, def.calves[side]);
                    let loc = |v: Vec3| { let q = c.rotation.inverse() * v; Vec3::new(q.x, -q.z, q.y) };
                    // the real pedestrian's foot-part rows in its ankle-parent frame, and the ankle
                    // angles the game had for them: what do OUR conventions read from the same pose?
                    let real = [
                        ([[0.156f32, 0.976, -0.152], [0.838, -0.212, -0.503], [-0.523, -0.048, -0.851]], [0.3964f32, 0.1059, -0.1976]),
                        ([[-0.01, 0.975, 0.221], [0.958, 0.073, -0.278], [-0.288, 0.209, -0.935]], [0.1339, 0.2419, -0.0426]),
                    ][side];
                    let k = c_rot.inverse() * sim.part(def, def.feet[side]).rotation;
                    let (mx, mb, my) = (Vec3::from(real.0[0]), Vec3::from(real.0[1]), Vec3::from(real.0[2]));
                    let part_in_p = Quat::from_mat3(&Mat3::from_cols(mx, my, -mb));
                    info!("  ankle {side}: the real pose read our way {:?}, the game's own reading {:?}", nm.angles(part_in_p * k.inverse()), real.1);
                    info!("  ankle {side}: parent frame in calf coords a {} b {} c {} at {} | angles now {:?}", r(loc(p_rot * Vec3::X)), r(loc(p_rot * Vec3::Y)), r(loc(p_rot * Vec3::Z)), r(loc(pos - c.translation)), nm.angles(p_rot.inverse() * c_rot));
                }
            }
            for side in 0..2 {
                if let Some((nm, p_rot, c_rot, pos)) = sim.nm_frames(def, def.thighs[side]) {
                    info!("  hip {side}: at {} parent frame x {} y {} z {} | angles now {:?} centre {:?} half {:?}", r(pos), r(p_rot * Vec3::X), r(p_rot * Vec3::Y), r(p_rot * Vec3::Z), nm.angles(p_rot.inverse() * c_rot), nm.ik_centre(), nm.half);
                }
            }
        }
        sim
    }

    fn pelvis(&self) -> Vec3 {
        fv(self.body.pose[0].pos)
    }

    /// Copy the map collision around the body (once, and again if it moves away).
    fn refresh_world<'a>(&mut self, chunks: impl IntoIterator<Item = &'a ChunkCollision> + Clone) {
        let c = self.pelvis();
        if self.world_at.is_some_and(|at| (at - c).length() < WORLD_RADIUS * 0.5) {
            return;
        }
        let tris = collision::triangles_near(chunks, c, WORLD_RADIUS);
        self.tris = tris.iter().map(|t| Tri::new(dv(t[0]), dv(t[1]), dv(t[2]))).collect();
        self.world_at = Some(c);
    }

    pub fn step<'a>(&mut self, def: &RagdollDef, dt: f32, chunks: impl IntoIterator<Item = &'a ChunkCollision> + Clone, cars: &[CarBox]) {
        self.refresh_world(chunks);
        self.age += dt;
        self.accumulator = (self.accumulator + dt).min(4.0 / HZ);
        let mut touching = 0;
        while self.accumulator >= 1.0 / HZ {
            self.accumulator -= 1.0 / HZ;
            self.drive(def, 1.0 / HZ, cars);
            let touches = pc::generate(&self.body, &self.colliders, &self.tris, &self.boxes, DVec3::Y);
            if self.car_hit.is_none() && touches.iter().any(|t| t.other.is_some()) {
                self.car_hit = Some(self.age);
            }
            touching = touches.len();
            self.touching = vec![false; self.body.links.len()];
            for t in &touches {
                self.touching[t.contact.link] = true;
            }
            let contacts: Vec<_> = touches.iter().map(|t| t.contact).collect();
            self.body.step((1.0 / HZ) as f64, &contacts);
        }
        // settled = the pelvis has (nearly) stopped; hands and feet twitching on the ground don't count
        let pelvis_speed = self.body.vcom[0].length() as f32;
        self.still = if pelvis_speed < 0.4 { self.still + dt } else { 0.0 };
        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() && (self.age % 0.5) < dt {
            let p = self.pelvis();
            let fastest = self.body.vcom.iter().map(|v| v.length()).fold(0.0, f64::max);
            info!("ragdoll t {:.1}: pelvis ({:.2} {:.2} {:.2}) fastest {fastest:.2} contacts {touching} mode {:?}", self.age, p.x, p.y, p.z, self.mode);
        }
    }

    /// World transform of every joint: ragdoll bones from their parts, the rest follow their
    /// parents with the given local pose.
    pub fn joint_globals(&self, def: &RagdollDef, root: &Transform, locals: &[Transform]) -> Vec<Transform> {
        let mut from_part: Vec<Option<Transform>> = vec![None; locals.len()];
        for (i, p) in def.parts.iter().enumerate() {
            let body = self.body(i).mul_transform(p.rel);
            from_part[p.bone] = Some(Transform::from_matrix(body.compute_matrix() * p.offset.compute_matrix().inverse()));
        }
        let mut g: Vec<Transform> = Vec::with_capacity(locals.len());
        for (i, l) in locals.iter().enumerate() {
            let t = from_part[i].unwrap_or_else(|| def.parents[i].map_or(*root, |p| g[p]).mul_transform(*l));
            g.push(t);
        }
        g
    }

    /// Has he lost his balance? IV's balance check (crates/euphoria::balancer, matched 400/400): the
    /// whole-body COM too far across from both feet and their midpoint for its height, or under 0.5 m
    /// above the lower foot. Also updates the smoothed stability. Returns (failed, lean angle).
    fn balance_failed(&mut self, def: &RagdollDef) -> (bool, f32) {
        let (pel, chest) = (self.body(0).translation, self.body(def.chest).translation);
        let tilt = (chest - pel).normalize_or(Vec3::Y).angle_between(Vec3::Y);
        let (mut m, mut com) = (0.0f32, Vec3::ZERO);
        for l in 0..self.body.links.len() {
            let lm = self.body.links[l].mass as f32;
            m += lm;
            com += fv(self.body.com[l]) * lm;
        }
        let com = com / m.max(1e-3);
        let foot = |i: usize| (to_nm(self.body(def.feet[i]).translation), to_nm(fv(self.body.vcom[self.link[def.feet[i]]])));
        let ((l, lv), (r, rv)) = (foot(0), foot(1));
        let p = euphoria::balancer::CheckParams { abort_ratio: self.tuning.abort_lean, give_up_low: true };
        let c = euphoria::balancer::check(&p, to_nm(Vec3::Y), l, r, to_nm(com), to_nm(self.com_vel()), lv, rv, self.bal_stability);
        self.bal_stability = c.stability;
        (c.failed, tilt)
    }

    /// The body's centre-of-mass velocity.
    fn com_vel(&self) -> Vec3 {
        let (mut m, mut v) = (0.0f32, Vec3::ZERO);
        for l in 0..self.body.links.len() {
            let lm = self.body.links[l].mass as f32;
            m += lm;
            v += fv(self.body.vcom[l]) * lm;
        }
        v / m.max(1e-3)
    }

    /// The pelvis part's world transform.
    pub fn pelvis_transform(&self) -> Transform {
        from_pose(&self.body.pose[0])
    }
}

fn outer(a: DVec3, b: DVec3) -> DMat3 {
    DMat3::from_cols(a * b.x, a * b.y, a * b.z)
}

impl RagdollSim {
    /// The world pose of the body (link) that carries this part.
    fn body(&self, part: usize) -> Transform {
        from_pose(&self.body.pose[self.link[part]])
    }

    /// The world pose of a part itself (a welded part rides on its carrier's body).
    fn part(&self, def: &RagdollDef, part: usize) -> Transform {
        let p = &def.parts[part];
        if p.body == part { self.body(part) } else { self.body(p.body).mul_transform(p.rel) }
    }

    /// For the test lab (testlab.rs): the whole body's COM, its tilt from upright (degrees), the two
    /// feet and every part's position (Bevy axes).
    pub fn lab_probe(&self, def: &RagdollDef) -> (Vec3, f32, [Vec3; 2], Vec<Vec3>) {
        let tilt = self.body_frame(def).map_or(0.0, replay::tilt);
        let feet = [self.part(def, def.feet[0]).translation, self.part(def, def.feet[1]).translation];
        (self.whole_com(), tilt, feet, (0..def.parts.len()).map(|i| self.part(def, i).translation).collect())
    }

    /// Kick the upper body (a shove): adds `vel` to the spine, head and arms.
    pub fn shove(&mut self, def: &RagdollDef, vel: Vec3) {
        for (i, p) in def.parts.iter().enumerate() {
            if p.body == i && p.region != Region::Leg && i != 0 {
                let l = self.link[i];
                let (com, m) = (self.body.com[l], self.body.links[l].mass);
                self.body.apply_impulse(l, com, dv(vel) * m);
            }
        }
    }

    /// A bullet: IV's shot behaviour. A standing body goes onto IV's balancer with the shot's
    /// muscle settings (`shot_muscle`) and staggers; every hit restarts its 0.4 s loosening of the
    /// upper body; a body already falling or down just takes the push. `vel` is the push, as for
    /// `shove` (STAND-IN for IV's bullet impulse on the hit part, whose size isn't read yet).
    pub fn shot_hit(&mut self, def: &RagdollDef, vel: Vec3, wound: Vec3, shooter_head: Vec3) {
        if matches!(self.mode, Mode::Balance) {
            self.nm_on = true;
            let steps = self.shot.as_ref().map_or(0, |s| s.steps);
            // one generator per character, as IV has (seeded from this body: any fixed seed would do)
            let mut rng = self.shot.as_ref().map_or_else(|| euphoria::rng::Rng::new(self.age.to_bits() ^ 0x9E37_79B9, self.body.links.len() as u32), |s| s.rng);
            // STAND-IN: the game's task sets useCatchFallOnFall in 16 of the 33 real shot messages;
            // the rule isn't read, so it's a coin toss per hit (LIBERTY_SHOT_CATCH=0/1 forces it)
            let catch_on_fall = match std::env::var("LIBERTY_SHOT_CATCH").as_deref() {
                Ok("1") => true,
                Ok("0") => false,
                _ => rng.coin(),
            };
            // the wound, kept on the part nearest the hit so it moves with the body
            let part = (0..def.parts.len()).filter(|&i| def.parts[i].body == i).min_by(|&a, &b| {
                let d = |i: usize| self.body(i).translation.distance_squared(wound);
                d(a).total_cmp(&d(b))
            });
            let reach = self.shot_reach_hit(def, part, wound, shooter_head, &mut rng);
            let wound = part.map_or((0, Vec3::ZERO), |p| (p, self.body(p).rotation.inverse() * (wound - self.body(p).translation)));
            // the head-look timer starts at headLookAtWoundMaxTimer when the behaviour starts (0.75 s
            // in every real message); a further hit keeps the running schedule
            let look_t = self.shot.as_ref().map_or(0.75, |s| s.look_t);
            self.shot = Some(ShotNm { since_hit: 0.0, steps, stepping: self.nm_bal.as_ref().map_or(0, |b| b.place.stepping), catch_on_fall, rng, look_t, wound, shooter: shooter_head, reach });
            self.recovered = false;
        }
        self.shove(def, vel);
    }

    /// The shot's head look: IV's schedule (shot.md: the wound first for headLookAtWoundMaxTimer,
    /// then the shooter's head for a random 0.75-1.5 s, the wound for a random 0.1-0.75 s, and so
    /// on, from the real messages' timers) choosing the target, and IV's headLook
    /// (crates/euphoria::headlook, matches the game) turning neck and head toward it.
    /// STAND-INS: our neck is welded to spine3 (as the physics ragdoll has it), so the neck's share
    /// of each angle goes onto the head joint; spine2 is welded too, so its share of a far twist is
    /// lost; the random draw's exact form (helper 0x617ba0) isn't read (uniform between min and max).
    fn shot_head_look(&mut self, def: &RagdollDef, dt: f32, targets: &mut [[f32; 3]]) {
        let Some(look) = def.nm_look else { return };
        let Some(s) = self.shot.as_mut() else { return };
        let looking_at_wound = s.look_t >= 0.0;
        if looking_at_wound {
            s.look_t -= dt;
            if s.look_t < 0.0 {
                s.look_t = -(0.75 + 0.75 * s.rng.uniform());
            }
        } else {
            s.look_t += dt;
            if s.look_t > 0.0 {
                s.look_t = 0.1 + 0.65 * s.rng.uniform();
            }
        }
        let (wpart, woff) = s.wound;
        let target = if looking_at_wound { self.body(wpart).transform_point(woff) } else { s.shooter };
        let ev = |v: Vec3| euphoria::math::Vec3::new(v.x, v.y, v.z);
        let chest = self.body(def.chest);
        let link = self.link[def.chest];
        let (head_lim, neck_lim) = (look.head, look.neck);
        let input = euphoria::headlook::Input {
            spine3_rows: look.rows.map(|r| ev(chest.rotation * r)),
            spine3_pos: ev(chest.translation),
            head_row_a: ev(self.part(def, def.head_part).rotation * Vec3::X),
            eye_offset: look.eye_offset,
            cone: 1.047_197_6,
            up: ev(Vec3::Y),
            body_vel: ev(fv(self.body.vcom[link])),
            body_spin: ev(fv(self.body.omega[link])),
            target: ev(target),
            target_vel: euphoria::math::Vec3::ZERO,
            // the shot runs headLook at stiffness 14.5, damping ratio 1.5 (real necks: 210 / 43.5)
            strength: 0.5 * (210.25 + 210.25),
            damping: 0.25 * (43.5 + 43.5),
            twist_max_sum: neck_lim[5] + head_lim[5],
            neck_limits: neck_lim,
            head_limits: head_lim,
            spine3: [look.spine3[4], look.spine3[5], targets[def.chest][0]],
            spine2: [look.spine2[4], look.spine2[5], 0.0],
            force: false,
            world_up: false,
        };
        let (_, out) = euphoria::headlook::step(&input);
        if let Some(a) = out {
            let sum = [a.neck[0] + a.head[0], a.neck[1] + a.head[1], a.neck[2] + a.head[2]];
            self.set_nm_targets(def, def.head_part, sum, targets);
            if let Some([lo, hi]) = def.parts[def.chest].limits[0] {
                targets[def.chest][0] = a.spine3.clamp(lo, hi);
            }
        }
    }

    /// A new hit for the shot's reach: carry the state over (or start it, taking the character's rows
    /// from how he stands now), draw this message's wait (timeBeforeReachForWound, 0.1-0.2 s in the
    /// real messages), and let IV's hand choice (crates/euphoria::reach::choose, matches the game)
    /// assign the wound.
    /// STAND-INS: the wound's normal is outward from the hit part's centre (IV's comes from the
    /// bullet's collision); for a bullet from behind IV probes through the body to where it would
    /// leave and uses that point, here the point opposite the entry through the part's centre.
    fn shot_reach_hit(&self, def: &RagdollDef, part: Option<usize>, hit: Vec3, shooter: Vec3, rng: &mut euphoria::rng::Rng) -> ShotReach {
        use euphoria::reach::{choose, lateral, start_timer, Hit, Side};
        let ev = |v: Vec3| euphoria::math::Vec3::new(v.x, v.y, v.z);
        let mut r = self.shot.as_ref().map_or_else(
            || ShotReach { right: self.nm_rows[0], back: self.nm_rows[1], ..Default::default() },
            |s| s.reach,
        );
        r.wait = 0.1 + 0.1 * rng.uniform();
        let Some(p) = part else { return r };
        let Some(ivp) = def.iv_part[p] else { return r };
        let dir = (hit - shooter).normalize_or_zero();
        let centre = fv(self.body.com[self.link[p]]);
        let mut at = hit;
        if dir.dot(-r.back) >= 0.0 {
            at = hit + dir * (2.0 * (centre - hit).dot(dir)).max(0.0);
        }
        let normal = (at - centre).normalize_or(-dir);
        let reference = if ivp.is_axial() {
            centre
        } else {
            let (mut m, mut c) = (0.0f32, Vec3::ZERO);
            for l in 0..self.body.links.len() {
                let lm = self.body.links[l].mass as f32;
                m += lm;
                c += fv(self.body.com[l]) * lm;
            }
            c / m.max(1e-3)
        };
        let h = Hit {
            part: ivp,
            lateral: lateral(ev(r.right), ev(at), ev(reference)),
            front: -r.back.dot(dir),
            reach_for_wound: true,
            no_left: false,
            no_right: false,
            left_hand: 0,
            right_hand: 0,
        };
        let (l, rr) = choose(&h, &mut r.wounds);
        let bt = self.body(p);
        let rec = (p, bt.rotation.inverse() * (at - bt.translation), bt.rotation.inverse() * normal);
        for (side, on) in [(Side::Left, l), (Side::Right, rr)] {
            if on {
                let hand = &mut r.hands[side as usize];
                hand.assigned = Some(rec);
                hand.timer = start_timer(side);
            }
        }
        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
            info!("shot: wound on {ivp:?} (side offset {:.2}) -> left hand {l}, right hand {rr}", h.lateral);
        }
        if std::env::var("LIBERTY_REACH_DEBUG").is_ok() {
            info!("reach hit: at {at:?} (hit {hit:?}) part centre {centre:?} pelvis {:?} chest {:?} hands {:?} {:?} right {:?} back {:?} dir {dir:?}",
                self.body(0).translation, self.body(def.chest).translation, self.body(def.hands[0]).translation, self.body(def.hands[1]).translation, r.right, r.back);
        }
        r
    }

    /// The shot's reach for the wound, one step (crates/euphoria::reach, matches the game): a hand
    /// given a wound starts once timeBeforeReachForWound has passed since the hit and, for up to
    /// grabHoldTime (3 s), gets IV's arm IK request (an aim point off the wound, twist), its wrist IK
    /// request (palm onto the wound) and the hold spring. Returns which hands it drove.
    /// STAND-INS: our clavicles are welded to the chest, so the step's clavicle shrug can't show;
    /// the palm's 2 cm offset along the hand's first Fred.art row is left out; no hand is ever busy.
    fn shot_reach(&mut self, def: &RagdollDef, dt: f32, targets: &mut [[f32; 3]]) -> [bool; 2] {
        use euphoria::reach::{self as r, HoldIn, Side, StepIn};
        let mut driven = [false; 2];
        if std::env::var("LIBERTY_NO_REACH").is_ok() {
            return driven;
        }
        let Some(s) = self.shot.as_ref() else { return driven };
        let (mut st, since) = (s.reach, s.since_hit);
        let ev = |v: Vec3| euphoria::math::Vec3::new(v.x, v.y, v.z);
        let bv = |v: euphoria::math::Vec3| Vec3::new(v.x, v.y, v.z);
        for side in 0..2 {
            let sd = if side == 0 { Side::Left } else { Side::Right };
            let h = &mut st.hands[side];
            h.timer += dt;
            let fresh = h.assigned.is_some();
            if h.on && r::must_stop(true, 0, fresh, h.timer, 3.0) {
                h.on = false;
            }
            if !h.on && r::may_start(true, 0, 0, sd, since, st.wait, fresh, h.timer, 3.0) {
                h.on = true;
                h.record = h.assigned.take();
                h.sign = 1.0;
                if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                    info!("shot: {sd:?} hand reaches for the wound ({:.2} s after the hit)", since);
                }
            }
            let (true, Some((pi, lp, ln))) = (h.on, h.record) else { continue };
            let part = def.iv_part[pi].unwrap_or(r::Part::Spine2);
            let bt = self.body(pi);
            let wound = bt.transform_point(lp);
            let normal = bt.rotation * ln * h.sign;
            let plan = r::plan(&StepIn {
                side: sd,
                part,
                clavicle_limits: [0.0; 6],
                clavicle: [0.0; 3],
                clavicle_locked: true,
                clavicle_incoming: None,
                wound: ev(wound),
                normal: ev(normal),
                facing: ev(st.back),
                hand: ev(self.body(def.hands[side]).translation),
                pelvis: ev(self.body(0).translation),
                spine3: ev(self.body(def.chest).translation),
            });
            // the shot's shoulders run at 12.27 with damping ratio 1 (strength 151, damping 24.5)
            let call = ArmCall { twist: plan.twist, lead: 0.0, reference_vel: None, stiffness: 12.27, damping_ratio: 1.0, limiter: None };
            let Some(shoulder) = self.nm_arm_ik(def, side, bv(plan.target), call, targets) else { continue };
            if let Some(aim) = self.nm_wrist_ik(def, side, shoulder, wound, normal, false, 2.4, targets) {
                if r::flips(aim.ret, part) {
                    h.sign = -h.sign;
                }
            }
            driven[side] = true;
            if std::env::var("LIBERTY_REACH_DEBUG").is_ok() {
                let hand = self.body(def.hands[side]).translation;
                info!("reach {sd:?}: hand to wound {:.3} m, to aim point {:.3} m, twist {:.2}, timer {:.2}", hand.distance(wound), hand.distance(bv(plan.target)), plan.twist, h.timer);
            }
            // the hold spring: on the hand at the wrist, and back on the wounded part at the wound
            let hi = def.hands[side];
            let (hl, wl) = (self.link[hi], self.link[pi]);
            let hold = HoldIn {
                wound: ev(wound),
                wound_vel: ev(fv(self.body.vcom[wl])),
                hand: ev(self.body(hi).translation),
                hand_row_a: euphoria::math::Vec3::ZERO,
                hand_vel: ev(fv(self.body.vcom[hl])),
                sign: h.sign,
                strength: r::hold_strength(0.0, 0.0),
                dt,
            };
            if let Some(f) = r::hold(&hold).filter(|_| std::env::var("LIBERTY_NO_HOLD").is_err()) {
                let f = bv(f) * dt;
                let wrist = def.parts[hi].parent.map_or(self.body(hi).translation, |pp| self.body(pp).mul_transform(def.parts[hi].frame1).translation);
                self.body.apply_impulse(hl, dv(wrist), dv(f));
                self.body.apply_impulse(wl, dv(wound), dv(-f));
            }
        }
        if let Some(s) = self.shot.as_mut() {
            s.reach = st;
        }
        driven
    }

    /// When the balancer gives up under a shot: catchFall (IV's own, already ported) if the shot
    /// asked for it, with its stiffnesses from the shot's bodyStiffness by the same rule the brace
    /// uses (legs 6/12, torso 9/12, arms 15/12; STAND-IN for the shot's own call, not read);
    /// otherwise limp.
    fn shot_fall(&mut self) -> Mode {
        let Some(shot) = self.shot.as_mut() else { return Mode::Relax };
        if !shot.catch_on_fall {
            return Mode::Relax;
        }
        let c = euphoria::fall::catch_fall_settings(13.5);
        let params = euphoria::catchfall::Params {
            legs_stiffness: c.legs_stiffness,
            torso_stiffness: c.torso_stiffness,
            arms_stiffness: c.arms_stiffness,
            forward_max_arm_offset: c.forward_max_arm_offset,
            backwards_min_arm_offset: c.backwards_min_arm_offset,
            use_head_look: true,
        };
        self.nm_catch = Some(euphoria::catchfall::CatchFall::start(params, &mut shot.rng));
        self.shot = None;
        Mode::CatchFall { arms: c.arms_stiffness, legs: c.legs_stiffness, back: c.torso_stiffness }
    }

    /// The shot's muscles for one joint (strength as Euphoria's stiffness, damping ratio), and the
    /// muscle stiffness its joint stiffness is built from (`nm_upper_stiffness`). Read from real
    /// pedestrians being shot (session of 1 Oct, re/mg2_stagger.py and the effector dumps): with
    /// bodyStiffness 13.5 the spine and clavicles run at 13.5 (strength 182, damping 27) and muscle
    /// stiffness 1; neck and head 14.5 (210, damping 43.5) and 0.5; shoulders and elbows 12.27
    /// (bodyStiffness x 10/11: 151, 24.5) and 0.5; wrists 24.55 (x 20/11: 602, damping about 8.9)
    /// and 0.5; the legs stay on the balancer's 12 (144, 24). Each hit makes the upper body's
    /// damping and muscle stiffness start from nothing and rise in a straight line to these over
    /// 0.4 s (the same in all 20 hits of the session; each new hit restarts it).
    fn shot_muscle(&self, def: &RagdollDef, i: usize, region: Region) -> Option<(f32, f32, f32)> {
        let s = self.shot.as_ref()?;
        if region == Region::Leg {
            return None;
        }
        let r = (s.since_hit / 0.4).clamp(0.02, 1.0);
        let (k, d, ms) = if def.hands.contains(&i) {
            (24.55, 0.18, 0.5)
        } else if region == Region::Arm {
            (12.27, 1.0, 0.5)
        } else if i == def.head_part {
            (14.5, 1.5, 0.5)
        } else {
            (13.5, 1.0, 1.0)
        };
        Some((k, d * r, ms * r))
    }

    /// Muscles: every free joint axis is driven toward its target angle. Euphoria's stiffness is a
    /// natural frequency (6..16), so spring = s², damping = 2·s·d, as accelerations (mass-free). Our
    /// solver integrates them implicitly, so they hold at the game's own values.
    fn muscles(&mut self, def: &RagdollDef, targets: &[[f32; 3]]) {
        let t = self.tuning;
        for (i, p) in def.parts.iter().enumerate() {
            if p.body != i || p.parent.is_none() {
                continue;
            }
            // stiffness from the behaviour (IV's taskparams); damping from Euphoria's rig, unless IV's
            // taskparams sets it for this group (balance: arms 0.7, spine 1.0); wrists/ankles keep the rig's
            let own = p.damping >= 5.0;
            let shot = if matches!(self.mode, Mode::Balance) { self.shot_muscle(def, i, p.region) } else { None };
            let (s, d) = match (self.mode, p.region) {
                (Mode::Relax, _) => (0.0, 0.0),
                (Mode::Balance, _) if shot.is_some() => shot.map(|(k, d, _)| (k, d)).unwrap(),
                // on IV's balancer the legs run at strength 144, damping 24 on every joint (read from a
                // real pedestrian's hip, knee and ankle): stiffness 12, damping ratio 1
                (Mode::Balance, Region::Leg) if self.nm_bal.is_some() => (t.leg_stiffness, 1.0),
                (Mode::Balance, Region::Leg) => (t.leg_stiffness, p.damping),
                // bodyBalance: neck and head at the arms' stiffness, damping ratio 1
                (Mode::Balance, Region::Spine) if self.nm_body.is_some() && i == def.head_part => (t.arm_stiffness, 1.0),
                (Mode::Balance, Region::Spine) => (t.spine_stiffness, t.spine_damping),
                // bodyBalance: elbows at 0.9 x the arms' stiffness and 0.8 x their damping ratio
                (Mode::Balance, Region::Arm) if self.nm_body.is_some() && def.fore.contains(&i) => (0.9 * t.arm_stiffness, 0.8 * t.arm_damping),
                (Mode::Balance, Region::Arm) => (t.arm_stiffness, if own { p.damping } else { t.arm_damping }),
                (Mode::Brace { .. }, Region::Leg) => (t.brace_leg_stiffness, p.damping),
                (Mode::Brace { .. }, Region::Arm) if def.hands.contains(&i) => (t.brace_body_stiffness - 1.0, 1.75),
                (Mode::Brace { .. }, Region::Arm) if def.fore.contains(&i) => (t.brace_body_stiffness, 0.75),
                (Mode::Brace { .. }, _) => (t.brace_body_stiffness, p.damping),
                (Mode::CatchFall { .. }, r) if self.catch_stiff.is_some() => {
                    let c = self.catch_stiff.unwrap();
                    match (r, def.arm_side(i)) {
                        (Region::Leg, _) => (c.legs, 0.5),
                        (Region::Arm, Some(side)) if def.hands[side] == i => (c.wrists[side], 1.0),
                        (Region::Arm, Some(side)) => (c.arms[side], 0.5),
                        _ => (c.torso, 0.5),
                    }
                }
                (Mode::CatchFall { legs, .. }, Region::Leg) => (legs, p.damping),
                (Mode::CatchFall { back, .. }, Region::Spine) => (back, p.damping),
                (Mode::CatchFall { arms, .. }, Region::Arm) => (arms, p.damping),
                (Mode::Roll { stiffness }, _) => (stiffness, p.damping),
            };
            let l = self.link[i];
            self.body.drives[l] = (s > 0.0).then(|| Drive {
                target: targets[i].map(|a| a as f64),
                stiffness: (s * s) as f64,
                damping: (2.0 * s * d) as f64,
            });
        }
    }

    /// Euphoria's stayUpright in its force mode (useForces; forceStrength default 3): every part above
    /// the legs is pulled horizontally back over the feet, by its own mass, so the body rights itself
    /// as a whole. (A torque on one light part — the first try — just spins that part.)
    fn stay_upright(&mut self, def: &RagdollDef, s: f32, d: f32) {
        let feet: Vec<Vec3> = def
            .parts
            .iter()
            .enumerate()
            .filter(|(i, p)| p.region == Region::Leg && p.body == *i && def.parts[p.parent.unwrap_or(0)].region == Region::Leg)
            .map(|(i, _)| self.body(i).translation)
            .collect();
        if feet.is_empty() {
            return;
        }
        let support = feet.iter().copied().sum::<Vec3>() / feet.len() as f32;
        // act on the core (pelvis + spine) only, scaled up to the whole upper body's mass: pulling arms
        // and head in too drags the arms down and tips the head back against the behaviour
        let mass = |r: Region| {
            def.parts.iter().enumerate().filter(|(i, p)| p.body == *i && p.region == r).map(|(i, _)| self.body.links[self.link[i]].mass as f32).sum::<f32>()
        };
        let scale = (mass(Region::Spine) + mass(Region::Arm)) / mass(Region::Spine).max(0.1);
        for (i, p) in def.parts.iter().enumerate() {
            if p.body != i || p.region != Region::Spine {
                continue;
            }
            let l = self.link[i];
            let (pos, v, m) = (fv(self.body.com[l]), fv(self.body.vcom[l]), self.body.links[l].mass as f32);
            let dx = Vec3::new(support.x - pos.x, 0.0, support.z - pos.z);
            let dvel = Vec3::new(-v.x, 0.0, -v.z);
            let f = (dx * s * s + dvel * 2.0 * s * d) * m * scale;
            self.body.force[l] += dv(f);
        }
    }

    /// Point an arm at `target` (world): the shoulder's rotation that aims the upper arm there, kept
    /// inside the game's limits, blended into `targets` by `weight`; elbow nearly straight.
    fn reach(&self, def: &RagdollDef, side: usize, target: Vec3, weight: f32, targets: &mut [[f32; 3]]) {
        let ui = def.upper[side];
        let p = &def.parts[ui];
        let Some(pp) = p.parent else { return };
        let w1 = self.body(pp).mul_transform(p.frame1);
        let ub = self.body(ui);
        let dir = (target - w1.translation).normalize_or_zero();
        if dir == Vec3::ZERO {
            return;
        }
        let now = ub.rotation * (Vec3::Z * p.along);
        let desired = Quat::from_rotation_arc(now, dir) * ub.rotation;
        let mut q = w1.rotation.inverse() * (desired * p.frame2.rotation);
        if q.w < 0.0 {
            q = -q;
        }
        let v = joint_angles(q);
        // The skeleton's limits lock or halve axes the free aim needs, so clamping `v` can lose the
        // aim entirely. Instead search the joint's allowed range for the pose whose upper arm points
        // closest to `dir` (coarse grid, then two finer passes around the best).
        let aim_of = |a: [f32; 3]| (w1.rotation * from_joint_angles(a) * p.frame2.rotation.inverse()) * (Vec3::Z * p.along);
        let range = |k: usize| p.limits[k].unwrap_or([0.0, 0.0]);
        let mut best = [0.0f32; 3];
        for k in 0..3 {
            best[k] = v[k].clamp(range(k)[0], range(k)[1]);
        }
        let mut best_dot = aim_of(best).dot(dir);
        let mut span = [0usize, 1, 2].map(|k| (range(k)[1] - range(k)[0]) * 0.5);
        let mut centre = [0usize, 1, 2].map(|k| (range(k)[0] + range(k)[1]) * 0.5);
        for _pass in 0..3 {
            const N: i32 = 6;
            let n = |k: usize| if span[k] > 0.0 { N } else { 0 };
            for i in -n(0)..=n(0) {
                for j in -n(1)..=n(1) {
                    for l in -n(2)..=n(2) {
                        let mut a = [0.0f32; 3];
                        for (k, s) in [i, j, l].into_iter().enumerate() {
                            let [lo, hi] = range(k);
                            a[k] = (centre[k] + span[k] * s as f32 / N as f32).clamp(lo, hi);
                        }
                        let d = aim_of(a).dot(dir);
                        if d > best_dot {
                            best_dot = d;
                            best = a;
                        }
                    }
                }
            }
            centre = best;
            span = span.map(|s| s / 4.0);
        }
        let v = best;
        for k in 0..3 {
            if let Some([lo, hi]) = p.limits[k] {
                targets[ui][k] += (v[k].clamp(lo, hi) - targets[ui][k]) * weight;
            }
        }
        if std::env::var("LIBERTY_ARM_DEBUG").is_ok() {
            let mut cur = w1.rotation.inverse() * (ub.rotation * p.frame2.rotation);
            if cur.w < 0.0 {
                cur = -cur;
            }
            let err = now.angle_between(dir).to_degrees();
            let deg = |a: [f32; 3]| a.map(|x| (x.to_degrees() * 10.0).round() / 10.0);
            let lim = p.limits.map(|l| l.map(|[a, b]| [(a.to_degrees()) as i32, (b.to_degrees()) as i32]));
            let best_err = aim_of(v).angle_between(dir).to_degrees();
            info!("arm {side} t {:.2}: aim error {err:.1} deg, best reachable {best_err:.1} | want {:?} target {:?} now {:?} | limits {:?}", self.mode_age, deg(v), deg(targets[ui]), deg(joint_angles(cur)), lim);
        }
        let fi = def.fore[side];
        for k in 0..3 {
            if let Some([lo, hi]) = def.parts[fi].limits[k] {
                // Euphoria's reach keeps the arm 75-95% straight: a small bend toward flexion
                let bend = if lo.abs() > hi.abs() { lo * 0.12 } else { hi * 0.12 };
                targets[fi][k] += (bend - targets[fi][k]) * weight;
            }
        }
    }

    /// Cars near the body become moving boxes it collides with (they push, it can't push back).
    fn sync_cars(&mut self, cars: &[CarBox]) {
        let pel = self.pelvis();
        self.boxes = cars
            .iter()
            .filter(|c| (c.pos - pel).length() < 25.0)
            .map(|c| MovingBox {
                id: c.id,
                pose: to_pose(&Transform { translation: c.pos, rotation: c.rot, ..default() }),
                lo: dv(c.lo),
                hi: dv(c.hi),
                vel: dv(c.vel),
                friction: 0.5,
            })
            .collect();
    }

    /// One physics step's worth of behaviour: set the muscles' targets and the helper forces.
    fn drive(&mut self, def: &RagdollDef, dt: f32, cars: &[CarBox]) {
        self.sync_cars(cars);
        self.mode_age += dt;
        self.body.carried.iter_mut().for_each(|c| *c = None);
        self.body.stiffness.iter_mut().for_each(|s| *s = 0.0);
        // a foot on the ground: its ankle carries the body (see `Body::carried`). Before the feet
        // had ankle joints they were welded to the shins, i.e. rigid; this keeps a planted ankle
        // about that firm in every mode, so the older behaviours stand as they did
        if !matches!(self.mode, Mode::Relax) {
            let (mut m, mut com) = (0.0f32, Vec3::ZERO);
            for l in 0..self.body.links.len() {
                let lm = self.body.links[l].mass as f32;
                m += lm;
                com += fv(self.body.com[l]) * lm;
            }
            let com = com / m.max(1e-3);
            for side in 0..2 {
                let l = self.link[def.feet[side]];
                if def.parts[def.feet[side]].body == def.feet[side] && self.touching.get(l).copied().unwrap_or(true) {
                    let at = fv(self.body.joint_frame_parent(l).pos);
                    self.body.carried[l] = Some((m * (com - at).length_squared()) as f64);
                }
            }
        }
        let t = self.tuning;
        let mut targets = self.base.clone();
        let (pel, chest) = (self.body(0).translation, self.body(def.chest).translation);
        match self.mode {
            Mode::Relax => {}
            Mode::Balance => {
                if self.nm_on {
                    // a shot runs the balancer with its maxSteps (6, from the game's shot message)
                    self.nm_balance_step(def, dt, 100, &mut targets);
                    let stepping = self.nm_bal.as_ref().map_or(0, |b| b.place.stepping);
                    if let Some(s) = self.shot.as_mut() {
                        s.since_hit += dt;
                        if stepping != 0 && stepping != s.stepping {
                            s.steps += 1;
                        }
                        s.stepping = stepping;
                    }
                    if self.shot.is_some() && std::env::var("LIBERTY_NO_HEADLOOK").is_err() {
                        self.shot_head_look(def, dt, &mut targets);
                    }
                    if self.shot.as_ref().is_some_and(|s| s.steps > 6) {
                        // out of steps: the balancer gives up
                        self.mode = self.shot_fall();
                        self.mode_age = 0.0;
                        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                            info!("shot: out of steps -> {:?}", self.mode);
                        }
                        self.muscles(def, &targets);
                        return;
                    }
                    // Steady again: hand back to animation. In the session's recording, the three
                    // staggers that ended on their feet were released after 1.5, 1.6 and 2.1 s,
                    // with both feet level on the ground, the pelvis moving at 0.04 to 0.17 m/s
                    // and turning at 0.2 to 0.5 rad/s (re/mg2_stagger.py). The rule itself is the
                    // game's task code, not read: this is fitted to those three.
                    if let Some(st) = &self.nm_bal {
                        let spin = fv(self.body.omega[0]).length();
                        if self.mode_age > 0.5 && st.place.stepping == 0 && st.planted == [true, true] && self.com_vel().length() < 0.15 && spin < 0.6 {
                            self.recovered = true;
                        }
                    }
                } else {
                    self.stay_upright(def, 3.0, 1.0);
                }
                // IV's bodyBalance: on the balancer without a shot (shoves, barges, car hits). In the
                // 1 Oct session only 4 of 32 shots came with a bodyBalance message, and the three real
                // shot staggers that stayed up ran the arms on the shot's own settings (151, twist
                // +1.06 / -0.23, elbows held bent), not bodyBalance's: the shot drives the upper body
                // itself (its arm parts aren't ported; the stand-in below). LIBERTY_SHOT_BODYBAL forces it.
                let body_bal = self.shot.is_none() || std::env::var("LIBERTY_SHOT_BODYBAL").is_ok();
                if self.nm_on && body_bal && std::env::var("LIBERTY_NO_BODYBAL").is_err() {
                    let reaching = if self.shot.is_some() { self.shot_reach(def, dt, &mut targets) } else { [false, false] };
                    let neck_held = self.shot.is_some() && std::env::var("LIBERTY_NO_HEADLOOK").is_err();
                    self.nm_body_balance_step(def, reaching, neck_held, &mut targets);
                }
                // armsOutOnPush: arms go out with the torso's tipping speed beyond the thresholds
                // (STAND-IN, only when bodyBalance doesn't run)
                let w = fv(self.body.omega[self.link[def.chest]]);
                let over = |a: f32, th: f32, m: f32| (a.abs() - th).max(0.0) * m;
                let push = over(w.x, t.ang_vel_threshold.x, t.ang_vel_mult.x) + over(w.z, t.ang_vel_threshold.z, t.ang_vel_mult.z);
                let out = if self.mode_age < t.arms_out_timeout { (push * t.arms_out_mult * 0.3).min(1.0) } else { 0.0 };
                self.arms_out = if self.nm_body.is_some() { 0.0 } else { (self.arms_out - dt * 1.5).max(out) };
                if self.nm_body.is_none() && self.shot.is_some() && std::env::var("LIBERTY_SHOT_ARMS_OUT").is_err() {
                    // STAND-IN for the shot's arms (IV drives them from bodyBalance and the shot's
                    // reach-for-wound, neither ported yet). In the 1 Oct session real shot victims'
                    // shoulders settle within ~0.4 s, partway out (lean2 about 0.3-0.6 rad, lean1 0-0.3,
                    // little twist) with the elbows a little bent, and never into a T-pose: so the
                    // arms are held there, loose (the shot's arm muscles), instead of in the pose they
                    // had when hit or thrown out sideways.
                    self.arms_out = 0.0;
                    let reaching = self.shot_reach(def, dt, &mut targets);
                    for side in 0..2 {
                        if reaching[side] {
                            continue;
                        }
                        // LIBERTY_SHOT_ARM=lean1,lean2,twist overrides (to check the direction)
                        let pose: [f32; 3] = std::env::var("LIBERTY_SHOT_ARM").ok().and_then(|v| {
                            let p: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                            (p.len() == 3).then(|| [p[0], p[1], p[2]])
                        }).unwrap_or([0.15, 0.45, 0.0]);
                        self.set_nm_targets(def, def.upper[side], pose, &mut targets);
                        let fi = def.fore[side];
                        if let Some(k) = (0..3).find(|&k| def.parts[fi].limits[k].is_some()) {
                            let [lo, hi] = def.parts[fi].limits[k].unwrap();
                            let flex = if lo.abs() > hi.abs() { lo } else { hi };
                            targets[fi][k] = flex * 0.25;
                        }
                    }
                }
                if self.arms_out > 0.01 {
                    let side_dir = (self.body(def.upper[1]).translation - self.body(def.upper[0]).translation).normalize_or(Vec3::X);
                    for (s, sign) in [(0usize, -1.0f32), (1, 1.0)] {
                        let shoulder = self.body(def.upper[s]).translation;
                        self.reach(def, s, shoulder + side_dir * sign * 0.6 + Vec3::Y * 0.1, self.arms_out, &mut targets);
                    }
                }
                // give up when leaning too far or sunk too low; succeed once steady again. After a
                // brace, a failure hands over to the fall IV's brace chooses (catchFall / roll)
                let (failed, tilt) = self.balance_failed(def);
                if failed {
                    self.mode = if self.nm_brace.is_some() {
                        self.fall_mode(def)
                    } else if self.shot.is_some() {
                        self.shot_fall()
                    } else {
                        Mode::Relax
                    };
                    self.mode_age = 0.0;
                    if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                        info!("balance failed (tilt {:.2}) -> {:?}", tilt, self.mode);
                    }
                } else if self.mode_age > 1.2 && tilt < 0.2 && self.body.vcom[0].length() < 0.25 && std::env::var("LIBERTY_NO_RECOVER").is_err() {
                    self.recovered = true;
                }
            }
            Mode::Brace { car } => {
                self.stay_upright(def, t.stay_upright, 1.0);
                if let Some(c) = cars.iter().find(|c| c.id == car) {
                    // brace against the point of the car nearest his chest
                    let local = c.rot.inverse() * (chest - c.pos);
                    let on_car = c.pos + c.rot * local.clamp(c.lo, c.hi);
                    let (hands, lean) = self.nm_brace_step(def, dt, on_car, car);
                    // IV's arm IK as the brace calls it: lead 1, twist 1, the body's velocity as the
                    // reference, the shoulder at bodyStiffness with damping ratio 1, no limiter
                    let body_vel = self.com_vel();
                    for (s, hand) in hands.into_iter().enumerate() {
                        if let Some(h) = hand {
                            let call = ArmCall {
                                twist: 1.0,
                                lead: 1.0,
                                reference_vel: Some(body_vel),
                                stiffness: t.brace_body_stiffness,
                                damping_ratio: 1.0,
                                limiter: None,
                            };
                            match self.nm_arm_ik(def, s, h, call, &mut targets) {
                                Some(shoulder) => {
                                    self.set_nm_targets(def, def.hands[s], [0.4, -0.4, 1.0], &mut targets);
                                    if std::env::var("LIBERTY_ARM_DEBUG").is_ok() {
                                        let hp = def.hands[s];
                                        info!("brace wrist {s}: fixed -> ours {:?} now {:?} limits {:?}", targets[hp], self.body.angles(self.link[hp]), def.parts[hp].limits);
                                    }
                                    // within 0.5 m of the brace point: palm toward it (normal = hand -
                                    // point; IV passes the wrist-frame flag and weight 1)
                                    let hand = self.body(def.hands[s]).translation;
                                    let d = hand - on_car;
                                    if d.length() < 0.5 {
                                        self.nm_wrist_ik(def, s, shoulder, h, d.normalize_or_zero(), true, 1.0, &mut targets);
                                    }
                                }
                                None => self.reach(def, s, h, 1.0, &mut targets),
                            }
                        }
                    }
                    // IV's lean step: spine_0..3 lean1 (the Z axis of our Euphoria spine frames)
                    for (k, a) in lean.into_iter().enumerate() {
                        targets[def.spine[k]][2] = a;
                    }
                    if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                        let now: Vec<f32> = def.spine.iter().map(|&p| if def.parts[p].body == p { self.body.angles(self.link[p])[2] as f32 } else { f32::NAN }).collect();
                        info!("nm lean target {:?} now {:?}", lean.map(|a| (a * 1000.0).round() / 1000.0), now.iter().map(|a| (a * 1000.0).round() / 1000.0).collect::<Vec<_>>());
                    }
                }
                // IV falls only when the balancer reports failure (brace spec, "Once the balancer has
                // failed"); otherwise the brace absorbs the hit and he stays on his feet
                let (failed, _) = self.balance_failed(def);
                match self.car_hit {
                    Some(_) if failed => {
                        self.mode = self.fall_mode(def);
                        self.mode_age = 0.0;
                        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                            info!("brace: balance failed, hands over to {:?}", self.mode);
                        }
                    }
                    // absorbed and still up: keep balancing (recovers to animation once steady)
                    Some(hit) if self.age - hit > t.brace_absorb => {
                        self.mode = Mode::Balance;
                        self.mode_age = 0.0;
                        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                            info!("brace absorbed the hit, still standing: balance");
                        }
                    }
                    // the car missed: stay on his feet
                    None if self.mode_age > 1.5 => {
                        self.mode = Mode::Balance;
                        self.mode_age = 0.0;
                    }
                    _ => {}
                }
            }
            Mode::CatchFall { .. } => {
                if self.nm_catch.is_some() {
                    self.nm_catch_step(def, dt, &mut targets);
                } else {
                    // no brace to start it from: hands out toward the ground ahead of the fall
                    let v = fv(self.body.vcom[0]).with_y(0.0);
                    let ahead = if v.length() > 0.5 { v.normalize() } else { (chest - pel).with_y(0.0).normalize_or(Vec3::Z) };
                    let ground = pel + ahead * 0.7 - Vec3::Y * 0.9;
                    for s in 0..2 {
                        self.reach(def, s, ground, 1.0, &mut targets);
                    }
                }
                if self.mode_age > 2.5 {
                    self.mode = Mode::Relax;
                    self.mode_age = 0.0;
                }
            }
            Mode::Roll { .. } => {
                // tucked: arms in toward the chest
                let fwd = (chest - pel).normalize_or(Vec3::Y);
                for s in 0..2 {
                    self.reach(def, s, chest + fwd * 0.1, 1.0, &mut targets);
                }
                if self.mode_age > 2.0 {
                    self.mode = Mode::Relax;
                    self.mode_age = 0.0;
                }
            }
        }
        self.muscles(def, &targets);
        if self.nm_on && self.nm_bal.is_some() && matches!(self.mode, Mode::Balance) && std::env::var("LIBERTY_NM_NO_UPPER").is_err() {
            self.nm_upper_stiffness(def);
        }
    }

    /// The joint stiffness IV gives the upper body while its balancer runs, from each joint's
    /// strength and damping (the drives `muscles` just set) and its muscle stiffness, read from
    /// real pedestrians on the balancer: spine 1, neck and head 0.5, shoulders, elbows and wrists
    /// 0.5. (The legs get theirs in `nm_leg_drive`.)
    fn nm_upper_stiffness(&mut self, def: &RagdollDef) {
        for (i, p) in def.parts.iter().enumerate() {
            if p.body != i || p.parent.is_none() || p.region == Region::Leg {
                continue;
            }
            let ms = match self.shot_muscle(def, i, p.region) {
                Some((_, _, ms)) => ms,
                None if p.region == Region::Arm || i == def.head_part => 0.5,
                None => 1.0,
            };
            let l = self.link[i];
            if let Some(d) = &self.body.drives[l] {
                self.body.stiffness[l] = iv_joint_stiffness(d.stiffness as f32, d.damping as f32, ms);
            }
        }
    }
}

/// IV's shot behaviour as far as it runs here: time since the last hit, steps taken.
struct ShotNm {
    since_hit: f32,
    steps: u32,
    stepping: u8,
    /// The shot's useCatchFallOnFall flag: when the balancer gives up, catchFall takes over.
    catch_on_fall: bool,
    rng: euphoria::rng::Rng,
    /// The head-look schedule's timer (>= 0: looking at the wound, counting down; < 0: at the
    /// shooter, counting up), the wound (part, offset in that part) and the shooter's head.
    look_t: f32,
    wound: (usize, Vec3),
    shooter: Vec3,
    reach: ShotReach,
}

/// The shot's reach for the wound (crates/euphoria::reach): each hand's newly assigned wound and the
/// one it's reaching for (part, and point and outward normal in that part's body), its reach timer,
/// whether it's reaching and its normal sign; the shared wound bookkeeping; the character's right
/// and +0x30 rows as IV sets them when the ragdoll starts (in the 1 Oct session's characters +0x10
/// is his right, +0x20 up and +0x30 backward); and the wait before reaching
/// (timeBeforeReachForWound).
#[derive(Clone, Copy, Debug, Default)]
struct ShotReach {
    hands: [HandReach; 2],
    wounds: euphoria::reach::Wounds,
    right: Vec3,
    back: Vec3,
    wait: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct HandReach {
    assigned: Option<(usize, Vec3, Vec3)>,
    record: Option<(usize, Vec3, Vec3)>,
    timer: f32,
    on: bool,
    sign: f32,
}

/// IV's bodyBalance (crates/euphoria::bodybalance) while the balancer drives the legs: its state
/// and settings (the real game's), its random generator (STAND-IN: IV shares one per character),
/// the balancer's step count as bodyBalance sees it, and its last output (re-applied on the
/// physics steps between IV's 60 Hz updates).
struct NmBody {
    state: euphoria::bodybalance::State,
    settings: euphoria::bodybalance::Settings,
    rng: euphoria::rng::Rng,
    steps: u32,
    stepping: u8,
    out: Option<euphoria::bodybalance::Output>,
    held: [bool; 2],
    neck_held: bool,
}

/// IV's balancer state carried between steps (crates/euphoria: foot placement, pelvis control,
/// and the leg solve's fed-back outputs).
struct NmBal {
    place: euphoria::footplace::State,
    pelvis: euphoria::pelvis::State,
    /// The pelvis height target the leg solve gave last step.
    height: f32,
    /// Ground height under each foot: where the foot last stood (the animated pose can float a
    /// few centimetres over the collision ground, so the start height alone is wrong).
    ground: [f32; 2],
    /// Physics steps since each foot last touched anything (a foot counts as down for a few
    /// steps after, so one missed contact doesn't read as a lifted foot).
    since_touch: [u32; 2],
    /// The pelvis's height over the feet and half the distance between the hips when it started
    /// (IV takes both from the pose: the recorded pedestrian had 0.8873 and 0.1289).
    hip_height: f32,
    hip_spread: f32,
    /// Last step's targets: hips (lean1, lean2, twist) and knees.
    hip: [[f32; 3]; 2],
    knee: [f32; 2],
    ankle: [[f32; 2]; 2],
    planted: [bool; 2],
    /// Physics steps since it started: IV's balancer runs 60 times a second, our physics 120.
    tick: u32,
    logged: u32,
}

impl RagdollSim {
    /// Kinetic energy per kg of the whole body (IV's measure: catchFall's and bodyBalance's speed).
    fn ke_per_kg(&self) -> f32 {
        let (mut m, mut ke) = (0.0f32, 0.0f32);
        for l in 0..self.body.links.len() {
            let link = &self.body.links[l];
            let lm = link.mass as f32;
            let (v, w) = (fv(self.body.vcom[l]), fv(self.body.omega[l]));
            let tr = (link.inertia.x_axis.x + link.inertia.y_axis.y + link.inertia.z_axis.z) as f32;
            m += lm;
            ke += 0.5 * lm * v.length_squared() + 0.5 * tr / 3.0 * w.length_squared();
        }
        ke / m.max(1e-3)
    }

    /// IV's bodyBalance on this body (crates/euphoria::bodybalance, matches the game's routine):
    /// at IV's 60 Hz update it reads the balancer's hip targets and planted feet, the joints'
    /// Euphoria angles, spine2's frame and the body's energy, and sets the shoulders, elbows,
    /// wrists, spine and head; between updates the last output is re-applied. Arms the shot's
    /// reach holds (`held`) and the head while headLook drives it are left alone.
    /// STAND-INS: its own random generator; the clavicles are welded in our ragdoll, so their
    /// angles are dropped; head look and body turn aren't ported.
    fn nm_body_balance_step(&mut self, def: &RagdollDef, held: [bool; 2], neck_held: bool, targets: &mut [[f32; 3]]) {
        use euphoria::bodybalance::{Input, Settings, State};
        let Some(bal) = self.nm_bal.as_ref() else { return };
        let (tick, stepping, planted, hip_want) = (bal.tick, bal.place.stepping, bal.planted, bal.hip);
        // spine2's part frame as IV holds it (a, b, c = the part matrix's x, -z, y in ours), GTA axes
        let s2 = self.part(def, def.spine[2]);
        let rows = [to_nm(s2.rotation * Vec3::X), to_nm(-(s2.rotation * Vec3::Z)), to_nm(s2.rotation * Vec3::Y)];
        let update = tick % 2 == 0 || std::env::var("LIBERTY_NM_120").is_ok();
        if self.nm_body.is_none() {
            let settings = Settings::default();
            let mut rng = euphoria::rng::Rng::new(0x1234_5678 ^ self.age.to_bits(), 0);
            let state = State::start(&settings, &rows, &mut rng);
            if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                info!("bodyBalance starts: body angles (roll, pitch, heading) {:?}", state.angles);
            }
            self.nm_body = Some(NmBody { state, settings, rng, steps: 0, stepping, out: None, held, neck_held });
        }
        if update {
            let now = |part: usize| self.nm_frames(def, part).map(|(nm, p, c, _)| nm.angles(p.inverse() * c));
            let hip_now = [0, 1].map(|s| now(def.thighs[s]).unwrap_or([0.0; 3]));
            let shoulder_now = [0, 1].map(|s| now(def.upper[s]).unwrap_or([0.0; 3]));
            let spine_now = [0, 1, 2, 3].map(|k| now(def.spine[k]).unwrap_or([0.0; 3]));
            let elbow_now = [0, 1].map(|s| {
                let fp = &def.parts[def.fore[s]];
                (0..3).find(|&k| fp.limits[k].is_some()).map_or(0.0, |k| {
                    let [lo, hi] = fp.limits[k].unwrap();
                    let flex = if lo.abs() > hi.abs() { lo.signum() } else { hi.signum() };
                    flex * self.body.angles(self.link[def.fore[s]])[k] as f32
                })
            });
            let hand_touching = (0..2).any(|s| self.touching.get(self.link[def.hands[s]]).copied().unwrap_or(false));
            let ke = self.ke_per_kg();
            let com_vel = to_nm(self.com_vel());
            let age = self.mode_age;
            let nb = self.nm_body.as_mut().unwrap();
            if stepping != 0 && stepping != nb.stepping {
                nb.steps += 1;
            }
            nb.stepping = stepping;
            let (prev_sh, prev_el) = nb.out.map_or((shoulder_now, elbow_now), |o| (o.shoulder, o.elbow));
            let input = Input {
                dt: 1.0 / 60.0,
                spine2_rows: rows,
                com_vel,
                ke,
                hip_want,
                hip_now,
                shoulder_now_lean1: [shoulder_now[0][0], shoulder_now[1][0]],
                elbow_now,
                foot_planted: planted,
                hand_touching,
                arm_held: held,
                step: nb.steps,
                clavicle_want: [[0.0; 3]; 2],
                shoulder_want: prev_sh,
                elbow_want: prev_el,
            };
            let out = nb.state.step(&nb.settings, &input, &mut nb.rng);
            nb.out = Some(out);
            // LIBERTY_NM_TRACE=<file>: one line per IV update with the same joints memgrab2 records
            // for real characters (re/mg2_effectors.py), to set our staggers beside the game's
            if let Ok(path) = std::env::var("LIBERTY_NM_TRACE") {
                use std::io::Write;
                let f3 = |a: [f32; 3]| format!("{:.4},{:.4},{:.4}", a[0], a[1], a[2]);
                let line = format!(
                    "{:.4},{:.3},{:.3},{},{},{},{},{},{},{},{},{},{},{},{},{:.4},{:.4},{:.4},{:.4},{},{},{},{},{},{}\n",
                    age, out.gauge, out.arms_out,
                    f3(out.shoulder[0]), f3(shoulder_now[0]), f3(out.shoulder[1]), f3(shoulder_now[1]),
                    f3(out.spine[0]), f3(out.spine[1]), f3(out.spine[2]), f3(out.spine[3]),
                    f3(spine_now[0]), f3(spine_now[1]), f3(spine_now[2]), f3(spine_now[3]),
                    out.elbow[0], elbow_now[0], out.elbow[1], elbow_now[1],
                    f3(hip_want[0]), f3(hip_now[0]), f3(hip_want[1]), f3(hip_now[1]),
                    f3(nb.state.angles), f3([com_vel.x, com_vel.y, com_vel.z]),
                );
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                    let _ = f.write_all(line.as_bytes());
                }
            }
            nb.held = held;
            nb.neck_held = neck_held;
            if std::env::var("LIBERTY_RAG_DEBUG").is_ok() && tick % 30 == 0 {
                info!("bodyBalance t {:.2}: gauge {:.2} arms-out {:.2} shoulders {:?} elbows {:?} spine0 {:?}", age, out.gauge, out.arms_out, out.shoulder, out.elbow, out.spine[0]);
            }
        }
        let Some(nb) = self.nm_body.as_ref() else { return };
        let Some(out) = nb.out else { return };
        let (held, neck_held) = (nb.held, nb.neck_held);
        // LIBERTY_BODYBAL_NO_SPINE / _NO_ARMS: leave that part on the base pose (for comparisons)
        let (no_spine, no_arms) = (std::env::var("LIBERTY_BODYBAL_NO_SPINE").is_ok(), std::env::var("LIBERTY_BODYBAL_NO_ARMS").is_ok());
        let held = if no_arms { [true, true] } else { held };
        for k in 0..4 {
            if !no_spine {
                self.set_nm_targets(def, def.spine[k], out.spine[k], targets);
            }
        }
        if !neck_held {
            self.set_nm_targets(def, def.head_part, out.head, targets);
        }
        for side in 0..2 {
            if held[side] {
                continue;
            }
            self.set_nm_targets(def, def.upper[side], out.shoulder[side], targets);
            self.set_nm_targets(def, def.hands[side], out.wrist[side], targets);
            let fi = def.fore[side];
            if let Some(k) = (0..3).find(|&k| def.parts[fi].limits[k].is_some()) {
                let [lo, hi] = def.parts[fi].limits[k].unwrap();
                let flex = if lo.abs() > hi.abs() { lo.signum() } else { hi.signum() };
                targets[fi][k] = (flex * out.elbow[side]).clamp(lo, hi);
            }
        }
    }

    /// One step of IV's balancer on this body: build what it reads from our ragdoll, run foot
    /// placement, pelvis control and the leg solve (all three match IV's own code on recorded
    /// cases), and put the hip and knee targets on our joints.
    ///
    /// How the body is described to it was read from a real balancing pedestrian in the session
    /// recording (`re/bal_snapshot.py`, 03 Oct notes in HANDOFF): a part's frame is the GTA part
    /// matrix's own axes (ours x, -z, y), which for the pelvis come out as (up, left, back) and
    /// for a foot as (right, up, back); a hip's frame is Euphoria's parent-side joint frame; a
    /// knee's two frames have b down the bone, c along the hinge toward the character's right
    /// and a = b x c. The tuning numbers are the ones in that pedestrian's balancer.
    /// STAND-INS: the ground is taken as flat at the height the feet started on; the body frame
    /// and spin come from the pelvis (IV uses a whole-body frame); the anchor follows the feet
    /// while both are down; ankles are welded in our ragdoll, so the ankle targets are dropped;
    /// the knee softening and the muscle-stiffness details of IV's apply step are not done.
    fn nm_balance_step(&mut self, def: &RagdollDef, dt: f32, max_steps: i32, targets: &mut [[f32; 3]]) {
        use euphoria::{footplace as fp, legs as lg, pelvis as pv};
        type V = euphoria::math::Vec3;
        // IV updates the balancer at its own 60 Hz tick (several of its rules are "per update");
        // on our in-between physics steps the last targets are held
        if let Some(st) = self.nm_bal.as_mut() {
            st.tick += 1;
            if st.tick % 2 == 1 && std::env::var("LIBERTY_NM_120").is_err() {
                let (hip, knee, ankle, planted) = (st.hip, st.knee, st.ankle, st.planted);
                self.nm_apply(def, hip, knee, ankle, planted, targets);
                return;
            }
        }
        let dt = if std::env::var("LIBERTY_NM_120").is_ok() { dt } else { 1.0 / 60.0 };
        let ev = |v: Vec3| V::new(v.x, v.y, v.z);
        let up = Vec3::Y;
        let part_frame = |t: &Transform| fp::Frame { a: ev(t.rotation * Vec3::X), b: ev(-(t.rotation * Vec3::Z)), c: ev(t.rotation * Vec3::Y), pos: ev(t.translation) };
        let axes = |a: Vec3, b: Vec3, c: Vec3, pos: Vec3| fp::Frame { a: ev(a), b: ev(b), c: ev(c), pos: ev(pos) };
        // a point's velocity on the link that carries a part, and that link's spin
        let motion = |part: usize, at: Vec3| {
            let l = self.link[part];
            let w = fv(self.body.omega[l]);
            (fv(self.body.vcom[l]) + w.cross(at - fv(self.body.com[l])), w)
        };
        let moving = |part: usize| {
            let t = self.part(def, part);
            let (vel, ang_vel) = motion(part, t.translation);
            fp::Foot { frame: part_frame(&t), vel: ev(vel), ang_vel: ev(ang_vel) }
        };
        // whole body
        let (mut m, mut com) = (0.0f32, Vec3::ZERO);
        for l in 0..self.body.links.len() {
            let lm = self.body.links[l].mass as f32;
            m += lm;
            com += fv(self.body.com[l]) * lm;
        }
        let com = com / m.max(1e-3);
        let com_vel = self.com_vel();
        let pelvis_t = self.part(def, 0);
        let (pelvis_vel, pelvis_spin) = motion(0, pelvis_t.translation);
        let pelvis = part_frame(&pelvis_t);
        // the body frame: side = to his right, front = side x up (IV's "front" row points behind him)
        let right = (pelvis_t.rotation * Vec3::Z).reject_from(up).normalize_or(Vec3::X);
        let front = right.cross(up);
        let feet = [moving(def.feet[0]), moving(def.feet[1])];
        let calves = [moving(def.calves[0]), moving(def.calves[1])];
        let foot_pos = [self.part(def, def.feet[0]).translation, self.part(def, def.feet[1]).translation];
        // (before the first physics step nothing has touched yet: he starts standing)
        let contact = [0, 1].map(|i| self.touching.get(self.link[def.feet[i]]).copied().unwrap_or(true));
        // hips and knees
        let leg_s = self.tuning.leg_stiffness;
        let (strength, damping) = (2.0 * leg_s, leg_s * leg_s); // the slots IV keeps them in: 24 and 144
        let mut fp_hips = [fp::Hip::default(); 2];
        let mut hips = [lg::Hip::default(); 2];
        let mut knees = [lg::Knee::default(); 2];
        let mut limbs = [fp::Frame::default(); 2];
        let mut knee_axis = [0usize; 2];
        for side in 0..2 {
            let (thigh, calf, foot) = (def.thighs[side], def.calves[side], def.feet[side]);
            let Some((nm, p_rot, c_rot, hip_pos)) = self.nm_frames(def, thigh) else { return };
            let now = nm.angles(p_rot.inverse() * c_rot);
            let centre = nm.ik_centre();
            let frame = axes(p_rot * Vec3::X, p_rot * Vec3::Y, p_rot * Vec3::Z, hip_pos);
            // knee: the hinge, pointing to his right, seen from the thigh and from the shin
            let cp = &def.parts[calf];
            let Some(k) = (0..3).find(|&k| cp.limits[k].is_some()) else { return };
            knee_axis[side] = k;
            let axis = [Vec3::X, Vec3::Y, Vec3::Z][k];
            let e1 = self.body(thigh).mul_transform(cp.frame1);
            let e2 = self.body(calf).mul_transform(cp.frame2);
            let sign = (e1.rotation * axis).dot(right).signum();
            let (c1, c2) = (e1.rotation * axis * sign, e2.rotation * axis * sign);
            let knee_pos = e1.translation;
            let ankle = self.part(def, calf).mul_transform(def.parts[foot].frame1).translation;
            let down = |dir: Vec3, c: Vec3| dir.reject_from(c).normalize_or(-up);
            let (b1, b2) = (down(knee_pos - hip_pos, c1), down(ankle - knee_pos, c2));
            let (a1, a2) = (b1.cross(c1), b2.cross(c2));
            let bend = c1.dot(b1.cross(b2)).atan2(b1.dot(b2));
            let st = self.nm_bal.as_ref();
            fp_hips[side] = fp::Hip { frame, half: [nm.half[0], nm.half[1]], centre: [centre[0], centre[1]], current: [now[0], now[1]] };
            hips[side] = lg::Hip {
                frame,
                strength,
                damping,
                twist_limit: nm.half[2],
                half: [nm.half[0], nm.half[1]],
                centre: [centre[0], centre[1]],
                centre_twist: centre[2],
                desired: st.map_or(now, |s| s.hip[side]),
                current: now,
            };
            knees[side] = lg::Knee { frame: axes(a1, b1, c1, knee_pos), parent: axes(a2, b2, c2, knee_pos), desired: st.map_or(bend, |s| s.knee[side]), current: bend };
            // the limb frame: the knee's (thigh-side) frame in the hip's child-side frame
            let (cx, cy, cz) = (c_rot * Vec3::X, c_rot * Vec3::Y, c_rot * Vec3::Z);
            let local = |v: Vec3| Vec3::new(cx.dot(v), cy.dot(v), cz.dot(v));
            limbs[side] = axes(local(a1), local(b1), local(c1), local(knee_pos - hip_pos));
        }
        // first step: start the state from where he stands
        if self.nm_bal.is_none() {
            let ground = [0, 1].map(|i| foot_pos[i].y - 0.012);
            let low = ground[0].min(ground[1]);
            let ground = [0, 1].map(|i| if contact[i] { ground[i] } else { low });
            let pos = [0, 1].map(|i| ev(foot_pos[i].with_y(ground[i])));
            let mid = (foot_pos[0].with_y(ground[0]) + foot_pos[1].with_y(ground[1])) * 0.5;
            self.nm_bal = Some(NmBal {
                place: fp::State { pos, mid: ev(mid), height: ground, steps_left: max_steps, flags: (contact[0] as u8) | ((contact[1] as u8) << 1), ..Default::default() },
                pelvis: pv::State { anchor: ev(mid), stance: contact, ..Default::default() },
                height: pelvis_t.translation.y - mid.y,
                ground,
                since_touch: [0; 2],
                // S1b test LIBERTY_NM_HIP_HEIGHT=<m>: a fixed target instead of where the pelvis is now
                hip_height: std::env::var("LIBERTY_NM_HIP_HEIGHT").ok().and_then(|v| v.parse().ok()).unwrap_or(pelvis_t.translation.y - ground[0].min(ground[1])),
                hip_spread: 0.5 * (hips[0].frame.pos - hips[1].frame.pos).length(),
                hip: [hips[0].current, hips[1].current],
                knee: [knees[0].current, knees[1].current],
                ankle: [[0.0; 2]; 2],
                planted: contact,
                tick: 0,
                logged: 0,
            });
        }
        let touch_now = [0, 1].map(|i| self.touching.get(self.link[def.feet[i]]).copied().unwrap_or(true));
        let contact = {
            let st = self.nm_bal.as_mut().unwrap();
            for i in 0..2 {
                st.since_touch[i] = if touch_now[i] { 0 } else { st.since_touch[i] + 1 };
                if touch_now[i] {
                    // (IV probes the ground under each foot; ours: where the standing foot is)
                    st.ground[i] += (foot_pos[i].y - 0.012 - st.ground[i]) * 0.2;
                }
            }
            for i in 0..2 {
                if !touch_now[i] && touch_now[1 - i] {
                    st.ground[i] = st.ground[1 - i];
                }
            }
            // down = touched within the last few steps, or still resting within 1.5 cm of where it
            // stood (a resting foot's contact comes and goes from step to step in our solver, and
            // IV's balancer starts a step the moment a foot reads as lifted)
            let near = |i: usize| foot_pos[i].y - 0.012 - st.ground[i] < 0.015;
            if std::env::var("LIBERTY_NM_RAW_CONTACT").is_ok() { touch_now } else { [st.since_touch[0] < 4 || near(0), st.since_touch[1] < 4 || near(1)] }
        };
        let ground = self.nm_bal.as_ref().unwrap().ground;
        let tuning = fp::Tuning {
            up: ev(up),
            forward: ev(up),
            reference: [0, 1].map(|i| ev(foot_pos[i].with_y(ground[i]))),
            normal: [ev(up); 2],
            dt,
            min_separation: 0.3568,
            side_offset: 0.187,
            lead_time: 0.2,
            support_margin: 0.0,
            gap_limit: 0.1,
            reach: 1.0,
            twist_centre: 0.1895,
            twist_range: 0.0156,
            front_base: 0.0,
        };
        let body = fp::Body {
            com: ev(com),
            com_vel: ev(com_vel),
            com_hvel: ev(com_vel.with_y(0.0)),
            spin: ev(pelvis_spin),
            side: ev(right),
            front: ev(front),
            pelvis,
            feet,
            hips: fp_hips,
            contact,
        };
        let st = self.nm_bal.as_mut().unwrap();
        fp::update(&mut st.place, &tuning, &body);
        if std::env::var("LIBERTY_STEP_DEBUG").is_ok() && st.logged % 6 == 0 {
            info!("step dbg2: pelvis over ground {:.3} (target {:.3}) | knee now {:.2} {:.2} want {:.2} {:.2} | hip lean1 now {:.2} {:.2} want {:.2} {:.2}", pelvis_t.translation.y - st.ground[0].min(st.ground[1]), st.height, knees[0].current, knees[1].current, st.knee[0], st.knee[1], hips[0].current[0], hips[1].current[0], st.hip[0][0], st.hip[1][0]);
            info!("step dbg3: pelvis spin ({:.2} {:.2} {:.2}) vel ({:.2} {:.2} {:.2}) | foot spin L ({:.1} {:.1} {:.1}) R ({:.1} {:.1} {:.1}) | targets L ({:.2} {:.2} {:.2}) R ({:.2} {:.2} {:.2}) mid ({:.2} {:.2} {:.2}) | feet L ({:.2} {:.2} {:.2}) R ({:.2} {:.2} {:.2})", pelvis_spin.x, pelvis_spin.y, pelvis_spin.z, pelvis_vel.x, pelvis_vel.y, pelvis_vel.z, feet[0].ang_vel.x, feet[0].ang_vel.y, feet[0].ang_vel.z, feet[1].ang_vel.x, feet[1].ang_vel.y, feet[1].ang_vel.z, st.place.pos[0].x - com.x, st.place.pos[0].y - st.ground[0], st.place.pos[0].z - com.z, st.place.pos[1].x - com.x, st.place.pos[1].y - st.ground[1], st.place.pos[1].z - com.z, st.place.mid.x - com.x, st.place.mid.y - st.ground[0], st.place.mid.z - com.z, foot_pos[0].x - com.x, foot_pos[0].y - st.ground[0], foot_pos[0].z - com.z, foot_pos[1].x - com.x, foot_pos[1].y - st.ground[1], foot_pos[1].z - com.z);
            info!("step dbg: need {:?} flags {:#x} stepping {} contact {:?} com vel ({:.2} {:.2} {:.2}) com-mid ({:.2} {:.2}) foot heights over ground {:.3} {:.3}", st.place.need, st.place.flags, st.place.stepping, contact, com_vel.x, com_vel.y, com_vel.z, com.x - st.place.mid.x, com.z - st.place.mid.z, foot_pos[0].y - st.ground[0], foot_pos[1].y - st.ground[1]);
        }
        st.pelvis.offset = ev((pelvis_t.translation - com).with_y(0.0));
        st.pelvis.stance = [st.place.flags & fp::LEFT_DOWN != 0, st.place.flags & fp::RIGHT_DOWN != 0];
        if st.place.stepping == 0 && st.pelvis.stance == [true, true] {
            st.pelvis.anchor = st.place.mid;
        }
        pv::update(&mut st.pelvis, &tuning, &body);
        let lt = lg::Tuning { up: ev(up), normal: [ev(up); 2], ground_vel: V::new(0.0, 0.0, 0.0), hip_height: st.hip_height, hip_spread: st.hip_spread, straightness: 0.9518, step_predict: 1.0, reach: 1.0 };
        let lbody = lg::Body { com_vel: ev(com_vel), front: ev(front), pelvis, pelvis_vel: ev(pelvis_vel), pelvis_spin: ev(pelvis_spin), hips, knees, ankles: [[strength, damping]; 2], calves, feet, limbs };
        let placement = lg::Placement { pos: st.place.pos, mid: st.place.mid, flags: st.place.flags & 3 };
        let pin = lg::Pelvis { offset: st.pelvis.offset, drift: st.pelvis.drift, twist: st.pelvis.twist, height: st.height, lean_side: st.pelvis.lean_side, lean_front: st.pelvis.lean_front };
        let out = lg::solve(&lt, &lbody, &placement, &pin);
        let sane = out.hip.iter().flatten().chain(out.knee.iter()).all(|a| a.is_finite()) && out.height.is_finite();
        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() && (st.logged % 30 == 0 || !sane) {
            let r = |v: V| format!("({:.3} {:.3} {:.3})", v.x, v.y, v.z);
            for side in 0..2 {
                let h = &hips[side].frame;
                let loc = |f: &fp::Frame, v: V| V::new(f.a.dot(v), f.b.dot(v), f.c.dot(v));
                info!("  dbg leg {side}: target in hip frame {} | foot now in hip frame {} | shin in shin-side knee frame {} | pelvis a {} b {} c {} | hip pos {} target {} foot {}", r(loc(h, placement.pos[side] - h.pos)), r(loc(h, feet[side].frame.pos - h.pos)), r(loc(&knees[side].parent, feet[side].frame.pos - knees[side].parent.pos)), r(pelvis.a), r(pelvis.b), r(pelvis.c), r(h.pos), r(placement.pos[side]), r(feet[side].frame.pos));
            }
            for side in 0..2 {
                info!("nm balance leg {side}: limb a {} b {} c {} pos {} | hip now {:?} -> {:?} | knee now {:.3} -> {:.3} | contact {}", r(limbs[side].a), r(limbs[side].b), r(limbs[side].c), r(limbs[side].pos), hips[side].current, out.hip[side], knees[side].current, out.knee[side], contact[side]);
            }
            info!("nm balance: height {:.3} -> {:.3}, lean side {:.3} front {:.3}, twist {:?}, flags {:#x}, stepping {}, steps left {}", pin.height, out.height, pin.lean_side, pin.lean_front, pin.twist, st.place.flags, st.place.stepping, st.place.steps_left);
        }
        st.logged += 1;
        if !sane {
            return;
        }
        st.height = out.height;
        st.hip = out.hip;
        st.knee = out.knee;
        st.ankle = out.ankle;
        let down = [st.place.flags & fp::LEFT_DOWN != 0, st.place.flags & fp::RIGHT_DOWN != 0];
        st.planted = [down[0] && contact[0], down[1] && contact[1]];
        let (hip, knee, ankle, planted) = (st.hip, st.knee, st.ankle, st.planted);
        self.nm_apply(def, hip, knee, ankle, planted, targets);
    }

    /// Put the balancer's leg targets on our joints and run IV's leg drive for this step.
    fn nm_apply(&mut self, def: &RagdollDef, hip: [[f32; 3]; 2], knee: [f32; 2], ankle: [[f32; 2]; 2], planted: [bool; 2], targets: &mut [[f32; 3]]) {
        for side in 0..2 {
            // (IV clamps the ankle's lean1 to +-9.5, i.e. not at all in practice)
            if std::env::var("LIBERTY_NM_NO_ANKLE").is_err() {
                self.set_nm_targets(def, def.feet[side], [ankle[side][0], ankle[side][1], 0.0], targets);
            }
            self.set_nm_targets(def, def.thighs[side], hip[side], targets);
            let calf = def.calves[side];
            if let Some(k) = (0..3).find(|&k| def.parts[calf].limits[k].is_some()) {
                let [lo, hi] = def.parts[calf].limits[k].unwrap();
                targets[calf][k] = knee[side].clamp(lo, hi);
            }
        }
        if std::env::var("LIBERTY_NM_NO_GRAV").is_err() {
            self.nm_leg_drive(def, planted);
        }
    }
}

/// IV's joint stiffness from a joint's muscle numbers (its effector update, specs/euphoria/
/// balancer.md "The joint muscle model"): x = (8 c + k) x 0.5 x muscle stiffness x dt, s = x / (1 + x),
/// at least 6 dt. IV runs it at its own tick (60 a second in the session), not ours.
fn iv_joint_stiffness(strength: f32, damping: f32, muscle_stiffness: f32) -> f64 {
    let dt = 1.0 / 60.0;
    let x = (8.0 * damping + strength) * 0.5 * muscle_stiffness * dt;
    (x / (1.0 + x)).max(6.0 * dt).min(1.0) as f64
}

impl RagdollSim {
    /// What IV does for the legs besides setting targets, while its balancer runs:
    /// - each joint's stiffness from its muscle numbers (strength 144, damping 24; muscle stiffness
    ///   10 on a planted ankle and 5 on a lifted one, 1.5 on a planted knee and 2 on a swinging
    ///   one, 1 on the hips), which is what makes a planted ankle nearly rigid (0.966, the value
    ///   read from a real pedestrian's ankle joint);
    /// - gravity opposition: every joint gets the torque that holds up the weight it carries. For
    ///   a planted leg that is the body above the joint (half of it when both feet are down); for
    ///   the other joints it is what hangs below. The torque is limited to strength x the joint's
    ///   oppose-gravity factor (2 on a flat planted ankle, 1 elsewhere).
    /// From IV's own routines (the effector update and its gravity-compensation pass). STAND-INS:
    /// the routine's extra rules for the ankles (it trims the torque by the foot's contact
    /// shape), its velocity look-ahead, and its scale factor on the limit (taken as 1).
    fn nm_leg_drive(&mut self, def: &RagdollDef, planted: [bool; 2]) {
        let (k, c) = (144.0f32, 24.0f32);
        for side in 0..2 {
            let (hip, knee, ankle) = (self.link[def.thighs[side]], self.link[def.calves[side]], self.link[def.feet[side]]);
            self.body.stiffness[hip] = iv_joint_stiffness(k, c, 1.0);
            self.body.stiffness[knee] = iv_joint_stiffness(k, c, if planted[side] { 1.5 } else { 2.0 });
            self.body.stiffness[ankle] = iv_joint_stiffness(k, c, if planted[side] { 10.0 } else { 5.0 });
            // (the plain "a planted ankle carries the body" stand-in of the other modes is not used here)
            self.body.carried[ankle] = None;
            self.body.carried[knee] = None;
        }
        // the links below each link (itself included), as mass and mass x position
        let n = self.body.links.len();
        let parent: Vec<Option<usize>> = (0..n).map(|l| self.body.links[l].joint.as_ref().map(|j| j.parent)).collect();
        let below = |top: usize, l: usize| {
            let mut cur = Some(l);
            while let Some(c) = cur {
                if c == top {
                    return true;
                }
                cur = parent[c];
            }
            false
        };
        let hips = [self.link[def.thighs[0]], self.link[def.thighs[1]]];
        let gravity = DVec3::new(0.0, -9.81, 0.0);
        let share = if planted[0] && planted[1] { 0.5 } else { 1.0 };
        let mut torques: Vec<(usize, DVec3)> = Vec::new();
        for l in 1..n {
            let at = self.body.joint_frame_parent(l).pos;
            // is this a joint of a planted leg?
            let leg = (0..2).find(|&s| planted[s] && below(hips[s], l));
            let weight = |m: usize| -> f64 {
                match leg {
                    // the body above the joint: this leg's own parts above it in full, the other
                    // planted leg not at all, the rest by this leg's share
                    Some(s) => {
                        if below(l, m) {
                            0.0
                        } else if below(hips[s], m) {
                            1.0
                        } else if planted[1 - s] && below(hips[1 - s], m) {
                            0.0
                        } else {
                            share
                        }
                    }
                    // what hangs below the joint
                    None => {
                        if below(l, m) {
                            1.0
                        } else {
                            0.0
                        }
                    }
                }
            };
            let (mut mass, mut mpos) = (0.0f64, DVec3::ZERO);
            for m in 0..n {
                let w = weight(m) * self.body.links[m].mass;
                mass += w;
                mpos += self.body.com[m] * w;
            }
            if mass <= 0.0 {
                continue;
            }
            // the torque that holds that weight, on the side that carries it
            let hold = -(mpos / mass - at).cross(gravity * mass);
            let on_child = if leg.is_some() { -hold } else { hold };
            let ankle = (0..2).any(|s| l == self.link[def.feet[s]] && planted[s]);
            let scale = std::env::var("LIBERTY_NM_GRAV_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
            let limit = (k * scale * if ankle { 2.0 } else { 1.0 }) as f64;
            let len = on_child.length();
            let t = if len > limit { on_child * (limit / len) } else { on_child };
            torques.push((l, t));
        }
        for (l, t) in torques {
            self.body.torque[l] += t;
            if let Some(p) = parent[l] {
                self.body.torque[p] -= t;
            }
        }
    }
}

/// GTA's Z-up axes <-> Bevy's (to_bevy maps X→X, Y→−Z, Z→Y).
fn to_nm(v: Vec3) -> euphoria::math::Vec3 {
    euphoria::math::Vec3::new(v.x, -v.z, v.y)
}

fn from_nm(v: euphoria::math::Vec3) -> Vec3 {
    Vec3::new(v.x, v.z, -v.y)
}

impl RagdollSim {
    /// IV's brace hand-off at the moment the fall starts (crates/euphoria::fall: 1 coin, then the
    /// contacts decide between catchFall and rollDownStairs, with their stiffness settings).
    fn fall_mode(&mut self, def: &RagdollDef) -> Mode {
        use euphoria::fall::{on_fail, Contacts, FallAction};
        let Some((brace, rng)) = self.nm_brace.as_mut() else { return Mode::Relax };
        let foot_links = [self.link[def.feet[0]], self.link[def.feet[1]]];
        let touching = |l: usize| self.touching.get(l).copied().unwrap_or(false);
        let contacts = Contacts {
            feet: foot_links.iter().any(|&l| touching(l)),
            above_feet: (0..self.body.links.len()).any(|l| !foot_links.contains(&l) && touching(l)),
        };
        let mut action = on_fail(contacts, brace.params.body_stiffness, rng);
        // test setup: LIBERTY_FALL=catch forces the catchFall branch (the draw above still happens)
        if std::env::var("LIBERTY_FALL").is_ok_and(|v| v == "catch") {
            action = FallAction::CatchFall(euphoria::fall::catch_fall_settings(brace.params.body_stiffness));
        }
        match action {
            FallAction::CatchFall(c) => {
                let params = euphoria::catchfall::Params {
                    legs_stiffness: c.legs_stiffness,
                    torso_stiffness: c.torso_stiffness,
                    arms_stiffness: c.arms_stiffness,
                    forward_max_arm_offset: c.forward_max_arm_offset,
                    backwards_min_arm_offset: c.backwards_min_arm_offset,
                    use_head_look: true,
                };
                self.nm_catch = Some(euphoria::catchfall::CatchFall::start(params, rng));
                Mode::CatchFall { arms: c.arms_stiffness, legs: c.legs_stiffness, back: c.torso_stiffness }
            }
            FallAction::RollDownStairs(r) => Mode::Roll { stiffness: r.stiffness },
        }
    }

    /// IV's two-bone arm IK (crates/euphoria::armik) on this body: builds the solver's view of the
    /// arm from our joints (Euphoria's frames, angle convention and the limb base), solves, and turns
    /// the shoulder and elbow targets back into our joint targets. False when the arm lacks
    /// Fred.art joint data (the caller falls back to `reach`).
    fn nm_arm_ik(&self, def: &RagdollDef, side: usize, target: Vec3, call: ArmCall, targets: &mut [[f32; 3]]) -> Option<[f32; 3]> {
        use euphoria::armik::{solve, Elbow, Frame, Request, Shoulder};
        let (ui, fi) = (def.upper[side], def.fore[side]);
        let (p, fp) = (&def.parts[ui], &def.parts[fi]);
        let (Some(nm), Some(pp)) = (p.nm, p.parent) else { return None };
        let k = (0..3).find(|&k| fp.limits[k].is_some())?;
        let ev = |v: Vec3| euphoria::math::Vec3::new(v.x, v.y, v.z);
        let rows = |q: Quat| [ev(q * Vec3::X), ev(q * Vec3::Y), ev(q * Vec3::Z)];
        // shoulder: Euphoria's parent-side frame P and child-side frame C
        let f1 = self.body(pp).mul_transform(p.frame1);
        let f2 = self.body(ui).mul_transform(p.frame2);
        let c_rot = f2.rotation * nm.k;
        let p_rot = f1.rotation * nm.k * nm.q_rest.inverse();
        let actual = nm.angles(p_rot.inverse() * c_rot);
        // elbow: our hinge; Euphoria's hinge axis is the way the arm flexes
        let [lo, hi] = fp.limits[k].unwrap();
        let flex = if lo.abs() > hi.abs() { lo.signum() } else { hi.signum() };
        let e1 = self.body(ui).mul_transform(fp.frame1);
        let axis = [Vec3::X, Vec3::Y, Vec3::Z][k];
        let e_rot = e1.rotation * Quat::from_rotation_arc(Vec3::Z, axis * flex);
        let elbow_now = flex * self.body.angles(self.link[fi])[k] as f32;
        // the limb base: the elbow's frame in the shoulder's child frame
        let to_c = c_rot.inverse();
        let limb = Frame { rows: rows(to_c * e_rot), pos: ev(to_c * (e1.translation - f1.translation)) };
        // the part above the shoulder (the clavicle, welded to the chest)
        let base_link = self.link[pp];
        let base_pos = self.body(pp).mul_transform(def.parts[pp].rel).translation;
        let (w, vcom) = (fv(self.body.omega[base_link]), fv(self.body.vcom[base_link]));
        let base_vel = vcom + w.cross(base_pos - fv(self.body.com[base_link]));
        let s = call.stiffness;
        let ic = nm.ik_centre();
        let shoulder = Shoulder {
            frame: Frame { rows: rows(p_rot), pos: ev(f1.translation) },
            muscle_strength: s * s,
            muscle_damping: 2.0 * s * call.damping_ratio,
            half_range: nm.half,
            centre: ic,
            prev: actual,
            limiter_prev: actual,
        };
        let elbow = Elbow { frame: Frame { rows: rows(e_rot), pos: ev(e1.translation) }, prev: elbow_now, limiter_prev: elbow_now };
        let twist_scale = if side == 0 { 1.0 } else { -1.0 };
        let req = Request {
            target: ev(target),
            limb,
            hand: ev(self.body(def.hands[side]).translation),
            base_vel: ev(base_vel),
            base_ang_vel: ev(w),
            reference_vel: call.reference_vel.map(ev),
            lead: call.lead,
            twist: call.twist,
            twist_scale,
            elbow_scale: 1.0,
            blend: 1.0,
            limiter: call.limiter,
            no_overflow: false,
        };
        let out = solve(&req, &shoulder, &elbow);
        self.set_nm_targets(def, ui, out.shoulder, targets);
        targets[fi][k] = (flex * out.elbow).clamp(lo, hi);
        if std::env::var("LIBERTY_ARM_DEBUG").is_ok() {
            info!("iv arm ik {side}: target {target:?} hand {:?} shoulder {:?} (now {:?}) elbow {:.2} (now {elbow_now:.2})", self.body(def.hands[side]).translation, out.shoulder, actual, out.elbow);
        }
        Some(out.shoulder)
    }

    /// A Fred.art joint's Euphoria frames right now: the parent-side frame P and the child-side frame
    /// C (world rotations with Euphoria's axis order) and the joint's position.
    fn nm_frames(&self, def: &RagdollDef, part: usize) -> Option<(NmJoint, Quat, Quat, Vec3)> {
        let p = &def.parts[part];
        let (nm, pp) = (p.nm?, p.parent?);
        let f1 = self.body(pp).mul_transform(p.frame1);
        let f2 = self.body(part).mul_transform(p.frame2);
        Some((nm, f1.rotation * nm.k * nm.q_rest.inverse(), f2.rotation * nm.k, f1.translation))
    }

    /// Drive a Fred.art joint to Euphoria joint angles (lean1, lean2, twist): the rotation they mean,
    /// expressed as our joint's per-axis targets, inside its limits.
    fn set_nm_targets(&self, def: &RagdollDef, part: usize, angles: [f32; 3], targets: &mut [[f32; 3]]) {
        let p = &def.parts[part];
        let Some(nm) = p.nm else { return };
        let q_nm = nm.q_rest.inverse() * nm.rotation(angles);
        let a = joint_angles(nm.k * q_nm * nm.k.inverse());
        for i in 0..3 {
            if let Some([lo, hi]) = p.limits[i] {
                targets[part][i] = a[i].clamp(lo, hi);
            }
        }
    }

    /// IV's wrist IK (crates/euphoria::wristik): predict the elbow from the shoulder's new targets,
    /// then turn the hand so its palm meets `normal`. Returns the wrist's Euphoria angles.
    #[allow(clippy::too_many_arguments)]
    fn nm_wrist_ik(&self, def: &RagdollDef, side: usize, shoulder_targets: [f32; 3], point: Vec3, normal: Vec3, use_wrist_frame: bool, weight: f32, targets: &mut [[f32; 3]]) -> Option<euphoria::wristik::Aim> {
        use euphoria::armik::Frame;
        let ev = |v: Vec3| euphoria::math::Vec3::new(v.x, v.y, v.z);
        let rows = |q: Quat| [ev(q * Vec3::X), ev(q * Vec3::Y), ev(q * Vec3::Z)];
        let (nm_s, p_s, _, sh_pos) = self.nm_frames(def, def.upper[side])?;
        let (_, p_w, _, wr_pos) = self.nm_frames(def, def.hands[side])?;
        let fp = &def.parts[def.fore[side]];
        let elbow_pos = self.body(def.upper[side]).mul_transform(fp.frame1).translation;
        let scale = if side == 0 { 1.0 } else { -1.0 };
        let shoulder = Frame { rows: rows(p_s), pos: ev(sh_pos) };
        let elbow = euphoria::wristik::predict_elbow(&shoulder, shoulder_targets, nm_s.ik_centre(), (elbow_pos - sh_pos).length(), scale);
        let wrist = Frame { rows: rows(p_w), pos: ev(wr_pos) };
        let aim = euphoria::wristik::aim(ev(point), ev(normal), elbow, ev(sh_pos), &wrist, scale, use_wrist_frame, weight);
        self.set_nm_targets(def, def.hands[side], aim.wrist, targets);
        if std::env::var("LIBERTY_ARM_DEBUG").is_ok() {
            let h = def.hands[side];
            info!("iv wrist ik {side}: nm {:?} -> ours {:?} now {:?} limits {:?}", aim.wrist, targets[h], self.body.angles(self.link[h]), def.parts[h].limits);
        }
        Some(aim)
    }

    /// One frame of IV's catchFall (crates/euphoria::catchfall) on this body: stiffness, spine lean,
    /// crouched legs, and each hand's target (reached with our arm aim until IV's arm IK is ported).
    /// The character's orientation rows as the live game has them (brace spec, Conventions,
    /// correction of 3 Oct): fwd (+0x20) = up, face (+0x30) = his backward at the start.
    fn nm_catch_step(&mut self, def: &RagdollDef, dt: f32, targets: &mut [[f32; 3]]) {
        use euphoria::catchfall::Input;
        let (mut m, mut vel, mut ke) = (0.0f32, Vec3::ZERO, 0.0f32);
        for l in 0..self.body.links.len() {
            let link = &self.body.links[l];
            let lm = link.mass as f32;
            let (v, w) = (fv(self.body.vcom[l]), fv(self.body.omega[l]));
            let tr = (link.inertia.x_axis.x + link.inertia.y_axis.y + link.inertia.z_axis.z) as f32;
            m += lm;
            vel += v * lm;
            ke += 0.5 * lm * v.length_squared() + 0.5 * tr / 3.0 * w.length_squared();
        }
        let (vel, ke) = (vel / m.max(1e-3), ke / m.max(1e-3));
        let shoulder = |s: usize| {
            let p = &def.parts[def.upper[s]];
            p.parent.map_or(self.body(def.upper[s]).translation, |pp| self.body(pp).mul_transform(p.frame1).translation)
        };
        let arm_touching = |s: usize| (0..def.parts.len()).any(|i| def.arm_side(i) == Some(s) && self.touching.get(self.link[i]).copied().unwrap_or(false));
        let up = euphoria::math::Vec3::Z;
        let input = Input {
            fwd: up,
            face: to_nm(self.nm_rows[1]),
            up,
            com_vel: to_nm(vel),
            chest_vel: to_nm(fv(self.body.vcom[self.link[def.chest]])),
            ke,
            shoulder: [to_nm(shoulder(0)), to_nm(shoulder(1))],
            hand: [to_nm(self.body(def.hands[0]).translation), to_nm(self.body(def.hands[1]).translation)],
            touching: [arm_touching(0), arm_touching(1)],
            surface_vel: [euphoria::math::Vec3::ZERO; 2],
            dt,
        };
        let tris = &self.tris;
        let Some(cf) = self.nm_catch.as_mut() else { return };
        let mut normals: Vec<Option<Vec3>> = Vec::with_capacity(2);
        let out = cf.step(&input, |a, b| {
            let hit = pc::segment_hit(tris, dv(from_nm(a)), dv(from_nm(b)));
            normals.push(hit.map(|(_, n)| fv(n)));
            hit.map(|(p, _)| to_nm(fv(p)))
        });
        let (params, gauge) = (cf.params, cf.gauge);
        let reach = [cf.arms[0].reach.max(0.3), cf.arms[1].reach.max(0.3)];
        self.catch_stiff = Some(CatchStiff {
            torso: params.torso_stiffness * gauge,
            legs: params.legs_stiffness * gauge,
            arms: reach.map(|r| r * params.arms_stiffness),
            wrists: out.wrist_stiffness.map(|w| w.0),
        });
        let clamp = |part: usize, k: usize, a: f32| def.parts[part].limits[k].map_or(a, |[lo, hi]| a.clamp(lo, hi));
        // spine: lean1 (Z) 0.2*w, lean2 s (Y = -lean2); twist decays toward 0
        for (k, &sp) in def.spine.iter().enumerate() {
            let (l1, l2) = out.spine[k];
            targets[sp] = [targets[sp][0] / (1.0 + 12.0 * dt), clamp(sp, 1, -l2), clamp(sp, 2, l1)];
        }
        // legs: hip lean1 m + w (Fred.art frame, Z); knee -2m about its hinge (X, bends negative);
        // ankles are welded in our ragdoll (the game locks them), so their -m has nothing to drive
        for s in 0..2 {
            let (th, ca) = (def.thighs[s], def.calves[s]);
            targets[th][2] = clamp(th, 2, out.hip[s]);
            if def.parts[ca].limits[0].is_some() {
                targets[ca][0] = clamp(ca, 0, out.knee[s]);
            }
        }
        for s in 0..2 {
            let call = ArmCall {
                twist: 1.3987,
                lead: if input.touching[s] { 0.0 } else { 0.5 },
                reference_vel: (!input.touching[s]).then_some(vel),
                stiffness: reach[s] * params.arms_stiffness,
                damping_ratio: 0.5,
                limiter: Some((200.0, 0.4)),
            };
            let hand_target = from_nm(out.hand_target[s]);
            match self.nm_arm_ik(def, s, hand_target, call, targets) {
                Some(shoulder) => match (out.wrist[s], normals.get(s).copied().flatten()) {
                    // the probe found nothing: IV's fixed wrist
                    (Some(w), _) => self.set_nm_targets(def, def.hands[s], w, targets),
                    // a hit: palm to the surface (IV passes no flag and no weight: 0 and 2.4)
                    (None, Some(n)) => {
                        self.nm_wrist_ik(def, s, shoulder, hand_target, n, false, 2.4, targets);
                    }
                    (None, None) => {}
                },
                None => self.reach(def, s, hand_target, 1.0, targets),
            }
        }
        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() && (self.mode_age % 0.25) < dt {
            info!("nm catchFall t {:.2}: g {gauge:.2} F {:?} back {:.2} w {:.2} reach {:.2}/{:.2} touching {:?} hands {:?}", self.mode_age, out.fall_dir, out.back, out.lean, reach[0], reach[1], input.touching, out.hand_target);
        }
    }

    /// One frame of IV's braceForImpact (crates/euphoria, part 1 + hand targets) on this body:
    /// decides whether he braces, which arms, the leading hand's delay, and where each reaching
    /// hand goes. Returns the hand targets (Bevy space) for the arms that reach this frame.
    fn nm_brace_step(&mut self, def: &RagdollDef, dt: f32, target: Vec3, car: u64) -> ([Option<Vec3>; 2], [f32; 4]) {
        use euphoria::brace::{hand_targets, Brace, CharacterKind, FrameInput, Params};
        let t = self.tuning;
        // centre of mass and its velocity, over the parts that have their own body
        let (mut m, mut com, mut vel) = (0.0f32, Vec3::ZERO, Vec3::ZERO);
        for l in 0..self.body.links.len() {
            let lm = self.body.links[l].mass as f32;
            m += lm;
            com += fv(self.body.com[l]) * lm;
            vel += fv(self.body.vcom[l]) * lm;
        }
        let (com, vel) = (com / m.max(1e-3), vel / m.max(1e-3));
        let chest = self.body(def.chest);
        // spine2's "his left" axis (IV: its second orientation row). Our parts come from the ped's .wft,
        // whose axes are laid out differently from Euphoria's: take the chest axis that lines up with
        // the shoulders (left minus right), so it rotates with the chest like IV's row does.
        let shoulders = (self.body(def.upper[0]).translation - self.body(def.upper[1]).translation).normalize_or_zero();
        let side = [Vec3::X, Vec3::Y, Vec3::Z]
            .into_iter()
            .flat_map(|a| [a, -a])
            .map(|a| chest.rotation * a)
            .max_by(|a, b| a.dot(shoulders).total_cmp(&b.dot(shoulders)))
            .unwrap_or(Vec3::X);
        let shoulder = |s: usize| {
            let p = &def.parts[def.upper[s]];
            p.parent.map_or(self.body(def.upper[s]).translation, |pp| self.body(pp).mul_transform(p.frame1).translation)
        };
        let up = euphoria::math::Vec3::Z;
        let input = FrameInput {
            target: to_nm(target),
            com: to_nm(com),
            com_vel: to_nm(vel),
            up,
            facing: to_nm(self.nm_rows[1]), // F = the +0x30 row: his backward (spec, Conventions)
            spine2_pos: to_nm(chest.translation),
            spine2_side: to_nm(side),
            shoulder_l: to_nm(shoulder(0)),
            shoulder_r: to_nm(shoulder(1)),
            arm_unavailable: [false, false],
            dt,
        };
        let pelvis_nm = to_nm(self.body(0).translation);
        let (brace, rng) = self.nm_brace.get_or_insert_with(|| {
            let params = Params {
                body_stiffness: t.brace_body_stiffness,
                brace_distance: t.brace_distance,
                target_prediction_time: t.brace_prediction,
                reach_absorbtion_time: t.brace_absorb,
                time_to_backwards_brace: 0.0,
            };
            let mut rng = euphoria::rng::Rng::new(car as u32 ^ 0x9E37_79B9, (car >> 32) as u32);
            (Brace::start(params, CharacterKind::Fred, input.target, &mut rng), rng)
        });
        let out = brace.step(&input, rng);
        let hands = hand_targets(brace, &input, &out, [false, false], false);
        // balance and torso (required order): the lean step reads last frame's bend and decays it;
        // while bracing the torso step then sets the bend for next frame
        let lean = self.nm_torso.lean_step(brace.start.s, dt);
        if out.bracing {
            self.nm_torso.torso_step(
                input.facing,
                up,
                input.com,
                pelvis_nm,
                input.target,
                out.target_vel,
                brace.start.w,
                out.arm_l && out.arm_r,
                brace.params.body_stiffness,
            );
        }
        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
            let up = input.up;
            let flat = |v: euphoria::math::Vec3| v.reject(up).normalize_or_zero();
            let ang = |sh: euphoria::math::Vec3| flat(input.target - sh).dot(input.spine2_side.reject(up)).clamp(-1.0, 1.0).acos().to_degrees();
            info!("nm arms: side {:?} to-target {:?} angle L {:.0} R {:.0} deg", input.spine2_side, flat(input.target - input.spine2_pos), ang(input.shoulder_l), ang(input.shoulder_r));
        }
        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
            info!("nm brace t {:.2}: S {:.2} bracing {} arms {}{} reach {}{} D {:.3}", self.mode_age, out.smoothed, out.bracing,
                if out.arm_l { 'L' } else { '-' }, if out.arm_r { 'R' } else { '-' },
                if out.reach_l { 'L' } else { '-' }, if out.reach_r { 'R' } else { '-' }, out.delay);
        }
        (hands.map(|h| h.map(from_nm)), lean.spine_lean1)
    }
}

/// A joint's rotation as Rapier's motors and limits measure it: per axis, 2·asin of the relative
/// rotation's imaginary part (w ≥ 0).
fn joint_angles(mut q: Quat) -> [f32; 3] {
    if q.w < 0.0 {
        q = -q;
    }
    [2.0 * q.x.clamp(-1.0, 1.0).asin(), 2.0 * q.y.clamp(-1.0, 1.0).asin(), 2.0 * q.z.clamp(-1.0, 1.0).asin()]
}

/// The rotation whose joint_angles are  (as far as they fit together in one quaternion).
fn from_joint_angles(a: [f32; 3]) -> Quat {
    let s = a.map(|x| (x * 0.5).sin());
    let w2 = 1.0 - (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]);
    Quat::from_xyzw(s[0], s[1], s[2], w2.max(0.0).sqrt()).normalize()
}

/// Local transforms (relative to each parent, the root relative to `root`) from world ones.
pub fn locals_from_globals(def: &RagdollDef, root: &Transform, globals: &[Transform]) -> Vec<(Quat, Vec3)> {
    globals
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let parent = def.parents[i].map_or(*root, |p| globals[p]);
            let l = Transform::from_matrix(parent.compute_matrix().inverse() * g.compute_matrix());
            (l.rotation.normalize(), l.translation)
        })
        .collect()
}
