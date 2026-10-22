//! Niko: the player ped from `playerped.rpf` — component parts (head, torso, legs, hands, feet,
//! hair, teeth) skinned to the 90-bone skeleton in `player.wft` — with a third-person controller,
//! an orbit camera, and the game's own idle/walk/run/sprint clips from `move_player.wad`.
//!
//! Bone transforms: stored local position + quaternion compose exactly to the stored absolute
//! positions (checked: 0.0 error), so they're used as-is, moved into Bevy space by the basis
//! change (x, y, z) → (x, z, −y) on positions and on the quaternion's vector part.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::render::mesh::VertexAttributeValues;
use rage::drawable::{self, Bone, Drawable};
use rage::wtd::{self, Format, Texture};
use rage::{Img, Key, Resource as Rsc, Rpf};

use crate::assets::{self, to_bevy};
use crate::collision;
use crate::ragdoll::{CarBox, Mode, RagdollDef, RagdollSim, Tuning};
use crate::pedanim::{self, Clip, Locomotion};
use crate::vehicle::Car;
use crate::stream::City;
use crate::Fly;

/// Default outfit: (part, texture variant letter).
const OUTFIT: &[&str] = &["head_000_r", "uppr_000_u", "lowr_000_u", "hand_000_r", "feet_000_u", "hair_000_u", "teef_000_u"];
/// Feet sit this far below the skeleton origin (pelvis).
pub const HIP_HEIGHT: f32 = 0.99;

pub struct PlayerData {
    anims: Vec<rage::anim::Animation>,
    /// Getting in/out of and sitting in a standard car (veh@std.wad).
    veh_anims: Vec<rage::anim::Animation>,
    /// His ragdoll body from player.wft.
    ragdoll: rage::ragdoll::Ragdoll,
    taskparams: rage::taskparams::TaskParams,
    nm_rig: rage::nmart::Biped,
    skeleton: Vec<Bone>,
    parts: Vec<Drawable>,
    textures: HashMap<String, Texture>,
}

pub fn load_player(root: &Path, key: &Key) -> Result<PlayerData> {
    let rpf = Rpf::open(&root.join("pc/models/cdimages/playerped.rpf"), key)?;
    let load = |name: &str| -> Result<Rsc> {
        let e = rpf.find(name).with_context(|| format!("{name} not in playerped.rpf"))?;
        Rsc::load(&rpf.read(e)?)
    };
    let wft = load("player.wft")?;
    let skeleton = drawable::read_skeleton(&wft, wft.u32(Rsc::ROOT, 0xB4)?)?;
    let mut parts = Vec::new();
    let mut textures = HashMap::new();
    for part in OUTFIT {
        let d = drawable::read(&load(&format!("{part}.wdr"))?)?;
        for s in &d.shaders {
            let Some(name) = &s.diffuse else { continue };
            if textures.contains_key(name) {
                continue;
            }
            // texture packs are named after the texture (…_diff_000_a_uni.wtd)
            if let Ok(res) = load(&format!("{name}.wtd")) {
                for t in wtd::read(&res)? {
                    textures.insert(t.name.to_ascii_lowercase(), t);
                }
            }
        }
        parts.push(d);
    }
    // his movement set: idle / walk / run / sprint and friends
    let anim_img = Img::open(&root.join("pc/anim/anim.img"), key)?;
    let wad = anim_img.find("move_player.wad").context("move_player.wad")?;
    let mut anims = rage::anim::read_dictionary(&Rsc::load(&anim_img.read(wad)?)?)?;
    // jumps (takeoff / in-air / landings) join the one-shot clips
    let jump = anim_img.find("jump_std.wad").context("jump_std.wad")?;
    anims.extend(rage::anim::read_dictionary(&Rsc::load(&anim_img.read(jump)?)?)?);
    // getting up after a ragdoll: get_up.wad (lying face down) and get_up_back.wad (on the back)
    // share clip names, so prefix them
    for (pack, prefix) in [("get_up.wad", "getup_front_"), ("get_up_back.wad", "getup_back_")] {
        let e = anim_img.find(pack).with_context(|| pack.to_string())?;
        for mut a in rage::anim::read_dictionary(&Rsc::load(&anim_img.read(e)?)?)? {
            a.name = format!("{prefix}{}", a.name.trim_start_matches("get_up_"));
            anims.push(a);
        }
    }
    let ragdoll = rage::ragdoll::read_ragdoll(&wft)?;
    // IV's Euphoria tuning: data/taskparams.txt, the only file in pc/data/game.rpf (contents encrypted)
    let game = Rpf::open(&root.join("pc/data/game.rpf"), key)?;
    let mut tp = game.read(game.find("taskparams.txt").context("taskparams.txt")?)?;
    key.decrypt(&mut tp);
    let taskparams = rage::taskparams::TaskParams::parse(&String::from_utf8_lossy(&tp));
    info!("taskparams.txt: {} Euphoria task parameter sets", taskparams.tasks());
    // gun packs (aim, fire, reload per weapon group): clip names become "<pack>/<clip>"
    for pack in ["move_combat_strafe", "gun@aim_idles", "gun@partials", "gun@handgun", "gun@deagle", "gun@shotgun", "gun@baretta", "gun@uzi", "gun@mp5k", "gun@ak47", "gun@rifle", "gun@rocket", "move_crouch_hgun", "move_crouch_rifle", "move_crouch_rpg"] {
        let Some(e) = anim_img.find(&format!("{pack}.wad")) else { continue };
        for mut a in rage::anim::read_dictionary(&Rsc::load(&anim_img.read(e)?)?)? {
            a.name = format!("{pack}/{}", a.name);
            anims.push(a);
        }
    }
    let veh = anim_img.find("veh@std.wad").context("veh@std.wad")?;
    let veh_anims = rage::anim::read_dictionary(&Rsc::load(&anim_img.read(veh)?)?)?;
    info!(
        "Niko: {} bones, {} parts, {} meshes, {} textures",
        skeleton.len(),
        parts.len(),
        parts.iter().map(|p| p.meshes.len()).sum::<usize>(),
        textures.len()
    );
    // Euphoria's own joint frames and limits (male peds): what IV builds the ragdoll's joints from
    // when Euphoria drives a ped
    let nm_rig = std::fs::read(root.join("pc/data/naturalmotion/Fred.art"))
        .ok()
        .and_then(|d| rage::nmart::read_biped_rig(&d).ok())
        .unwrap_or_default();
    info!("Fred.art: {} Euphoria joints, {} part frames", nm_rig.joints.len(), nm_rig.part_frames.len());
    Ok(PlayerData { anims, veh_anims, ragdoll, taskparams, skeleton, parts, textures, nm_rig })
}

impl PlayerData {
    /// Niko's ragdoll as the viewer builds it (S1's replay uses it headless).
    pub fn ragdoll_def(&self) -> RagdollDef {
        RagdollDef::new(&self.skeleton, &self.ragdoll, &self.nm_rig)
    }

    /// IV's Euphoria tuning from taskparams.txt (S1's closed-loop check uses it headless).
    pub fn nm_tuning(&self) -> crate::ragdoll::Tuning {
        crate::ragdoll::Tuning::from_taskparams(&self.taskparams)
    }

    /// skeleton index of a bone by name
    pub fn bone_index(&self, name: &str) -> Option<usize> {
        self.skeleton.iter().position(|b| b.name == name)
    }
}

impl Player {
    /// the entity of skeleton bone `i`
    pub fn joint_entity(&self, i: usize) -> Entity {
        self.joints[i].0
    }

    /// Each skeleton bone's parent index.
    pub fn parents(&self) -> &[Option<usize>] {
        &self.parents
    }

    /// Standing still: the idle clip's first frame with the helper bones placed, as local
    /// transforms per joint (the rest pose if he has no idle clip).
    pub fn standing_pose(&self) -> Vec<(Quat, Vec3)> {
        let rest: Vec<Transform> = self.joints.iter().map(|j| j.1).collect();
        let mut pose = match &self.loco {
            Some(loco) => pedanim::pose(loco, 0.0, 0.0, 0.0, &rest),
            None => rest.iter().map(|t| (t.rotation, t.translation)).collect(),
        };
        place_helpers(&self.helpers, &rest, &mut pose);
        pose
    }

    /// Place the helper bones (the arm twist/roll bones no clip animates) in a pose of local
    /// transforms per joint, by the same rules as Liberty's own poses. For an add-on posing him.
    pub fn with_helpers(&self, pose: &mut [(Quat, Vec3)]) {
        let rest: Vec<Transform> = self.joints.iter().map(|j| j.1).collect();
        place_helpers(&self.helpers, &rest, pose);
    }

    /// For the test lab (testlab.rs): the running ragdoll and its body, the one-shot clip playing,
    /// and where he is with a car ("none", "approach", "enter", "seated", "exit").
    pub fn lab(&self) -> (Option<(&RagdollSim, &RagdollDef)>, Option<&str>, &'static str) {
        let car = match self.car {
            CarState::None => "none",
            CarState::Approach { .. } => "approach",
            CarState::Enter { .. } => "enter",
            CarState::Seated { .. } => "seated",
            CarState::Exit { .. } => "exit",
        };
        (self.rag.as_ref().zip(self.rag_def.as_ref()), self.action.as_ref().map(|a| a.clip.as_str()), car)
    }
}

/// An add-on has the player: while this is true Liberty doesn't move, pose or film him (the add-on
/// does), and when it goes false he carries on on foot from wherever he was left. Insert it to use it.
#[derive(Resource, Default)]
pub struct PlayerTakenOver(pub bool);

fn bone_local(b: &Bone) -> Transform {
    let [qx, qy, qz, qw] = b.local_rot;
    Transform {
        translation: Vec3::from(to_bevy(b.local_pos)),
        rotation: Quat::from_xyzw(qx, qz, -qy, qw).normalize(),
        ..default()
    }
}

#[derive(Resource)]
pub struct PlayerRes(pub PlayerData);

#[derive(Component)]
pub struct Player {
    pub active: bool,
    /// Last frame's time into the recording, for scripted presses (each fires once, on the frame
    /// that crosses its time).
    last_rec_t: Option<f32>,
    yaw: f32,
    vel_y: f32,
    phase: f32,
    idle_phase: f32,
    speed: f32,
    /// Current yaw rate (rad/s, + = left): IV's heading control (see the loop), drives the lean blend.
    turn_rate: f32,
    /// IV's move blend ratio: 0 still, 1 walk, 2 run, 3 sprint. `speed` is the clips' speed at it.
    mbr: f32,
    /// How fast he actually moves (m/s): chases `speed` at IV's capped acceleration.
    move_v: f32,
    /// Seconds of continuous movement (a quick tap shouldn't play a full stop clip).
    moving_for: f32,
    /// How fast he actually moved last frame (loop or clip root motion) — what a ragdoll inherits.
    vel: Vec3,
    /// Horizontal velocity while airborne (a jump or a fall): IV flies him ballistically, the in-air
    /// clip only poses him.
    air_vel: Vec3,
    loco: Option<Locomotion>,
    cam_yaw: f32,
    cam_pitch: f32,
    cam_dist: f32,
    /// How far out the camera actually sits: `cam_dist`, or less when something is in the way.
    cam_clear: f32,
    /// Joint entity + rest local transform + rest global rotation, by skeleton index.
    joints: Vec<(Entity, Transform, Quat)>,
    /// A one-shot clip (start / stop / turn) playing instead of the locomotion loop.
    action: Option<Action>,
    /// Pose we're cross-fading away from, and the time left.
    fade: Option<(Vec<(Quat, Vec3)>, f32)>,
    last_pose: Vec<(Quat, Vec3)>,
    /// Helper bones no clip animates (IV moves them in code): each follows its limb bone.
    helpers: Vec<Helper>,
    /// veh@std clips by name.
    veh: HashMap<String, Clip>,
    car: CarState,
    rag_def: Option<RagdollDef>,
    /// Euphoria behaviour tuning from IV's taskparams.txt.
    nm: Tuning,
    /// Running ragdoll (hit by a car, or K).
    rag: Option<RagdollSim>,
    /// Aiming: skeleton parents, the spine chain that bends to aim up/down, the right hand, how far the aim pose is blended
    /// in (0..1), the current up/down bend, and the measured hold frame of the current aim clip.
    parents: Vec<Option<usize>>,
    /// Left and right thigh bones (which way the hips face, for debug logs).
    thighs: Option<(usize, usize)>,
    spine: Vec<usize>,
    /// Per bone: part of the upper body (char_spine and everything under it).
    upper: Vec<bool>,
    /// The strafe clip his legs are playing while he aims, and how far through it (0..1).
    strafe: (String, f32),
    /// Which way move_combat_strafe/walk walks, as a yaw in his own frame: his legs' "ahead".
    strafe_fwd: Option<f32>,
    hand: usize,
    aim_w: f32,
    aim_bend: f32,
    aim_ref: Option<AimRef>,
}

/// Getting in and out of cars.
#[derive(Clone, Copy, PartialEq)]
enum CarState {
    None,
    /// Running to the driver's door.
    Approach { car: Entity },
    /// 0 open the door, 1 get in, 2 close it.
    Enter { car: Entity, stage: u8, t: f32 },
    Seated { car: Entity },
    /// 0 get out (opening the door), 1 close it.
    Exit { car: Entity, stage: u8, t: f32 },
}

struct Action {
    clip: String,
    t: f32,
    start_pos: Vec3,
    start_yaw: f32,
    /// Speed to carry into the loop when it ends.
    then_speed: f32,
}

const FADE: f32 = 0.15;

pub fn spawn_player(
    mut commands: Commands,
    data: Res<PlayerRes>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    let d = &data.0;
    let parts: Vec<&Drawable> = d.parts.iter().collect();
    let (root, joints) = build_rig(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut images,
        &mut bindposes,
        &d.skeleton,
        &parts,
        &|n: &str| d.textures.get(n),
        "Niko",
    );

    commands.entity(root).insert(Player {
        active: false,
        last_rec_t: None,
        yaw: 0.0,
        vel_y: 0.0,
        phase: 0.0,
        idle_phase: 0.0,
        speed: 0.0,
        turn_rate: 0.0,
        mbr: 0.0,
        move_v: 0.0,
        moving_for: 0.0,
        vel: Vec3::ZERO,
        air_vel: Vec3::ZERO,
        loco: {
            let ids: HashMap<u16, usize> = d.skeleton.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
            if std::env::var("LIBERTY_BONES").is_ok() {
                // debug: the skeleton, and which bones a clip animates (by track bone id)
                let clip = std::env::var("LIBERTY_BONES").unwrap_or_default();
                let tracks: Vec<(u16, String)> = d.anims.iter().find(|a| a.name == clip).map(|a| a.tracks.iter().map(|t| (t.bone_id, format!("{:?}", t.kind))).collect()).unwrap_or_default();
                let anim = d.anims.iter().find(|a| a.name == clip);
                for (i, b) in d.skeleton.iter().enumerate() {
                    let tk: Vec<&str> = tracks.iter().filter(|(id, _)| *id == b.id).map(|(_, k)| k.as_str()).collect();
                    // how far the clip's first rotation key is from the rest local rotation (degrees)
                    let off = anim.and_then(|a| a.tracks.iter().find(|t| t.bone_id == b.id && matches!(t.kind, rage::anim::TrackKind::Rotation))).and_then(|t| t.frames.first()).map(|f| {
                        let q = Quat::from_array(*f).normalize();
                        let r = Quat::from_array(b.local_rot).normalize();
                        q.angle_between(r).to_degrees()
                    });
                    let o = b.orient.map(|x| (x.to_degrees() * 10.0).round() / 10.0);
                    info!("bone {i:3} {:28} id {:5} parent {:?} tracks {:?} orient {o:?} key0-vs-rest {:?} local_rot {:?} local_pos {:?}", b.name, b.id, b.parent, tk, off.map(|x| x.round()), b.local_rot, b.local_pos);
                }
                let unmapped: Vec<u16> = tracks.iter().map(|(id, _)| *id).filter(|id| !ids.contains_key(id)).collect();
                info!("clip {clip}: {} tracks, unmapped bone ids {:?}", tracks.len(), unmapped);
                // the skeleton as data (GTA space) for the Reader-side tools (re/memgrab2.py)
                let rows: Vec<String> = d
                    .skeleton
                    .iter()
                    .map(|b| {
                        format!(
                            "{{\"name\":\"{}\",\"id\":{},\"parent\":{},\"local_pos\":{:?},\"local_rot\":{:?},\"abs_pos\":{:?}}}",
                            b.name,
                            b.id,
                            b.parent.map_or("null".to_string(), |p| p.to_string()),
                            b.local_pos,
                            b.local_rot,
                            b.abs_pos
                        )
                    })
                    .collect();
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/player_skeleton.json");
                if let Err(e) = std::fs::write(&path, format!("[\n{}\n]\n", rows.join(",\n"))) {
                    warn!("player_skeleton.json: {e}");
                }
            }
            Locomotion::from_anims(&d.anims, &ids, d.skeleton.len())
        },
        cam_yaw: 0.0,
        cam_pitch: std::env::var("LIBERTY_PED_CAMPITCH").ok().and_then(|v| v.parse().ok()).unwrap_or(-0.25),
        cam_dist: std::env::var("LIBERTY_PED_CAMDIST").ok().and_then(|v| v.parse().ok()).unwrap_or(4.5),
        cam_clear: 4.5,
        joints,
        action: None,
        fade: None,
        last_pose: Vec::new(),
        helpers: helper_bones(&d.skeleton),
        veh: {
            let ids: HashMap<u16, usize> = d.skeleton.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
            d.veh_anims.iter().map(|a| (a.name.clone(), Clip::new(a, &ids, d.skeleton.len()))).collect()
        },
        car: CarState::None,
        rag_def: Some(RagdollDef::new(&d.skeleton, &d.ragdoll, &d.nm_rig)),
        nm: Tuning::from_taskparams(&d.taskparams),
        rag: None,
        parents: d.skeleton.iter().map(|b| b.parent).collect(),
        thighs: {
            let find = |n: &str| d.skeleton.iter().position(|b| b.name == n);
            find("char_l_thigh").zip(find("char_r_thigh"))
        },
        spine: ["char_spine", "char_spine1", "char_spine2", "char_spine3"].iter().filter_map(|n| d.skeleton.iter().position(|b| b.name == *n)).collect(),
        upper: {
            // the spine hangs off the root, not the pelvis
            let spine = d.skeleton.iter().position(|b| b.name == "char_spine").unwrap_or(usize::MAX);
            (0..d.skeleton.len())
                .map(|i| {
                    let mut j = Some(i);
                    while let Some(k) = j {
                        if k == spine {
                            return true;
                        }
                        j = d.skeleton[k].parent;
                    }
                    false
                })
                .collect()
        },
        strafe: (String::new(), 0.0),
        strafe_fwd: None,
        hand: d.skeleton.iter().position(|b| b.name == "char_r_hand").unwrap_or(0),
        aim_w: 0.0,
        aim_bend: 0.0,
        aim_ref: None,
    });
}

/// Walk Niko around (when the camera mode is "walking"), then pose him and place the camera.
#[allow(clippy::too_many_arguments)]
/// IV's jump launch (GTAIV.exe 0xd40340, untargeted jump): an impulse of mass x (0.3 x his own
/// ground velocity, 2.8 m/s up), plus the velocity of whatever he stands on. So he leaves the
/// ground at 2.8 m/s up (0.40 m rise, 0.57 s in the air) and 1.3x the takeoff clip's speed
/// (4.22 -> 5.5 m/s). The 1 Oct session agrees: pelvis +0.32..0.38 m, 0.45-0.6 s airborne, 5.2-5.6
/// m/s through the air. (Was 4.2 m/s up: 0.9 m, more than twice IV's height.)
const JUMP_UP: f32 = 2.8;
const JUMP_FWD_GAIN: f32 = 1.3;
/// IV's gravity: the same launch code works with 19.6 = 2 x 9.8.
const GRAVITY: f32 = 9.8;

pub fn control_player(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    city: Res<City>,
    started: Option<Res<crate::RecordStart>>,
    mut players: Query<(&mut Transform, &mut Visibility, &mut Player), Without<Fly>>,
    mut cam: Query<(&mut Transform, &mut Fly), Without<Player>>,
    mut bones: Query<&mut Transform, (Without<Player>, Without<Fly>)>,
    mut cars: Query<(Entity, &mut Car)>,
    roads: Option<Res<crate::traffic::Roads>>,
    armed: Res<crate::weapon::Armed>,
    weapons: Res<crate::weapon::Weapons>,
    mut steps: ResMut<crate::sound::Footsteps>,
    taken: Option<Res<PlayerTakenOver>>,
) {
    let _slow = crate::SlowGuard::new("ped::control_player");
    let Ok((mut cam_t, mut fly)) = cam.single_mut() else { return };
    let Ok((mut t, mut vis, mut p)) = players.single_mut() else { return };
    let dt = time.delta_secs().min(0.05);
    let want = (fly.walking && fly.driving.is_none()) || !matches!(p.car, CarState::None);
    let ground_ready = {
        let c = cam_t.translation;
        collision::ground_height(city.collision.values(), c.x, c.z, c.y + 1.0).is_some()
    };
    if want && !p.active && ground_ready {
        // appear on the ground under the camera, facing the way it looks
        let (yaw, _, _) = cam_t.rotation.to_euler(EulerRot::YXZ);
        let cam = cam_t.translation;
        let mut pos = cam;
        // start on the pavement of the nearest road, not in a planter: find the lane, walk right
        // to the kerb (a step up in the ground), then a metre onto the pavement. The lane search
        // starts from the ground under the camera (lanes more than 8 m above or below don't count).
        // No road nearby is fine in the open (a park, an island); under a roof it means he'd land
        // inside a building, which has no floor, so there the search goes out to 400 m.
        let fwd = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
        let under = Vec3::new(cam.x, collision::ground_height(city.collision.values(), cam.x, cam.z, cam.y + 1.0).unwrap_or(cam.y - 1.7), cam.z);
        let roofed = collision::raycast(city.collision.values(), under + Vec3::Y * 2.0, Vec3::Y, 400.0).is_some();
        let lane = roads.as_deref().and_then(|r| {
            crate::traffic::nearest_lane(r, under, fwd, 60.0).or_else(|| if roofed { crate::traffic::nearest_lane(r, under, fwd, 400.0) } else { None })
        });
        if lane.is_none() && roofed {
            warn!("on-foot start: under a roof at {under:?} and no road within 400 m; he may be indoors");
        }
        if let Some((q, d)) = lane {
            let right = Vec3::new(-d.z, 0.0, d.x);
            let chunks = city.collision.values();
            if let Some(g0) = collision::ground_height(chunks.clone(), q.x, q.z, q.y + 2.0) {
                pos = Vec3::new(q.x, g0 + 1.0, q.z);
                let mut s = 0.25;
                while s < 14.0 {
                    let p2 = q + right * s;
                    match collision::ground_height(chunks.clone(), p2.x, p2.z, g0 + 1.0) {
                        Some(g) if g - g0 > 0.05 && g - g0 < 0.4 => {
                            // a metre past the step, if that's out in the open. Where the kerb
                            // is flush the first step is a building's doorstep and a metre past it
                            // is indoors (no floor: interiors aren't loaded), so with a roof
                            // overhead stay a metre short of the step instead.
                            let open = |at: Vec3, y: f32| collision::raycast(chunks.clone(), Vec3::new(at.x, y + 2.0, at.z), Vec3::Y, 150.0).is_none();
                            let (past, short) = (q + right * (s + 1.0), q + right * (s - 1.0).max(0.0));
                            if open(past, g) {
                                pos = Vec3::new(past.x, g + 1.0, past.z);
                            } else if open(short, g0) {
                                pos = Vec3::new(short.x, g0 + 1.0, short.z);
                            }
                            break;
                        }
                        Some(g) if (g - g0).abs() <= 0.05 => s += 0.25,
                        _ => break,
                    }
                }
            }
        }
        let g = collision::ground_height(city.collision.values(), pos.x, pos.z, pos.y + 1.0).unwrap_or(pos.y - 1.7);
        t.translation = Vec3::new(pos.x, g, pos.z);
        p.yaw = yaw;
        p.cam_yaw = yaw + std::env::var("LIBERTY_PED_CAMYAW").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0).to_radians();
        // LIBERTY_PED_TURN=deg: turn him at spawn, camera unchanged (e.g. 180 to face a ram-test car)
        p.yaw += std::env::var("LIBERTY_PED_TURN").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0).to_radians();
        p.active = true;
        p.vel_y = 0.0;
        *vis = Visibility::Inherited;
    } else if !want && p.active {
        p.active = false;
        *vis = Visibility::Hidden;
    }
    if !p.active {
        return;
    }
    if taken.is_some_and(|t| t.0) {
        // follow where the add-on puts him and the camera, standing, so he walks on from there
        p.yaw = yaw_of(t.rotation);
        let back = cam_t.translation - t.translation;
        if back.x != 0.0 || back.z != 0.0 {
            p.cam_yaw = back.x.atan2(back.z);
        }
        p.vel_y = 0.0;
        p.air_vel = Vec3::ZERO;
        p.vel = Vec3::ZERO;
        p.speed = 0.0;
        p.move_v = 0.0;
        p.mbr = 0.0;
        p.moving_for = 0.0;
        p.action = None;
        // the pose the add-on left him in, to fade out of once it lets go
        let now: Vec<(Quat, Vec3)> = p.joints.iter().map(|j| bones.get(j.0).map_or((j.1.rotation, j.1.translation), |b| (b.rotation, b.translation))).collect();
        p.fade = Some((now.clone(), FADE));
        p.last_pose = now;
        return;
    }

    // --- cars: F gets in (runs to the door first) or out, with the game's veh@std clips.
    // LIBERTY_CAR_IN / LIBERTY_CAR_OUT=<secs into the recording>: scripted presses for tests.
    let rec_t = started.as_ref().map(|s| time.elapsed_secs() - s.0);
    let prev_rec_t = std::mem::replace(&mut p.last_rec_t, rec_t);
    let scripted = |var: &str| {
        // one time or a comma list of times (seconds into the recording); a press fires on the frame
        // whose (last frame, this frame] holds its time, so exactly once even when it lands on a frame
        let times: Vec<f32> = std::env::var(var).unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect();
        rec_t.is_some_and(|r| times.iter().any(|&a| prev_rec_t.unwrap_or(r - dt) < a && a <= r))
    };
    let press_f = keys.just_pressed(KeyCode::KeyF) || scripted("LIBERTY_CAR_IN") || scripted("LIBERTY_CAR_OUT");
    if press_f && std::env::var("LIBERTY_CAR_DEBUG").is_ok() {
        info!("car: F at {:?} s into the recording, state {}", rec_t, match p.car { CarState::None => "none", CarState::Approach { .. } => "approach", CarState::Enter { .. } => "enter", CarState::Seated { .. } => "seated", CarState::Exit { .. } => "exit" });
    }
    let mut approach_to: Option<Vec3> = None;
    match car_interaction(&mut p, &mut t, &mut fly, &mut cars, press_f, dt) {
        CarStep::Posed(pose) => {
            apply_pose(&mut p, &mut bones, pose, dt);
            if fly.driving.is_none() {
                orbit_camera(&mut p, &t, &mut cam_t, &buttons, &motion, &scroll, dt);
            }
            return;
        }
        CarStep::RunTo(target) => approach_to = Some(target),
        CarStep::Free => {}
    }

    // --- camera orbit
    if crate::looklock::looking(&buttons) {
        p.cam_yaw -= motion.delta.x * 0.004;
        p.cam_pitch = (p.cam_pitch - motion.delta.y * 0.004).clamp(-1.2, 0.5);
    }
    if scroll.delta.y != 0.0 {
        p.cam_dist = (p.cam_dist * 0.9f32.powf(scroll.delta.y)).clamp(0.8, 12.0);
    }

    // ram test: face the incoming car
    if let Some(d) = crate::traffic::RAM_FACE.lock().ok().and_then(|mut f| f.take()) {
        // LIBERTY_RAM_ANGLE=deg: the car comes from this angle off his facing (0 = in front, -90 = his
        // right, as in IV's recorded hit); default faces it
        let off = std::env::var("LIBERTY_RAM_ANGLE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0).to_radians();
        p.yaw = (-d.x).atan2(-d.z) - off;
        t.rotation = Quat::from_rotation_y(p.yaw);
        info!("ram test: Niko turned to yaw {:.1} deg to face {d:?}", p.yaw.to_degrees());
    }

    // --- ragdoll: the game's body + joint limits under physics, driven by Euphoria behaviours
    // tuned from IV's taskparams.txt. A car about to hit him → braceForImpact; J or a slow nudge →
    // bodyBalance; K → just go limp.
    let solids: Vec<CarBox> = cars.iter().map(|(e, c)| c.solid(e.to_bits())).collect();
    if p.rag.is_none() {
        let hips = t.translation + Vec3::Y * 0.9;
        let cam_fwd = Vec3::new(-p.cam_yaw.sin(), 0.0, -p.cam_yaw.cos());
        let coming = solids
            .iter()
            .filter(|c| c.vel.length() > 1.0)
            .filter_map(|c| time_to_hit(c, hips, 0.35).map(|at| (c, at)))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let start = if keys.just_pressed(KeyCode::KeyK) || scripted("LIBERTY_RAGDOLL_AT") {
            Some((Mode::Relax, p.vel + Vec3::Y * 0.3, None))
        } else if keys.just_pressed(KeyCode::KeyJ) || scripted("LIBERTY_SHOVE_AT") {
            Some((Mode::Balance, p.vel, Some(cam_fwd * std::env::var("LIBERTY_SHOVE_SPEED").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(2.0))))
        } else {
            match coming {
                // fast and arriving within braceTargetPredictionTime: hands up
                Some((c, at)) if c.vel.length() > 4.0 && at > 0.0 && at <= p.nm.brace_prediction => {
                    Some((Mode::Brace { car: c.id }, p.vel, None))
                }
                // already on him and fast (from behind, no time to brace): thrown
                Some((c, at)) if at <= 0.0 && c.vel.length() > 4.0 => Some((Mode::Relax, c.vel * 0.85 + Vec3::Y * 2.5, None)),
                // a slow nudge: try to keep his feet (the car pushes him physically)
                Some((_, at)) if at <= 0.0 => Some((Mode::Balance, p.vel, None)),
                _ => None,
            }
        };
        if let (Some((mode, vel, shove)), Some(def)) = (start, p.rag_def.as_ref()) {
            let locals = current_locals(&p);
            let globals = def.globals(&rig_of(&t), &locals);
            if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                let f = t.rotation * Vec3::NEG_Z;
                let pb = def.pelvis_bone();
                let belly = globals[pb].rotation * def.rest_global[pb].rotation.inverse() * Vec3::NEG_Z;
                let lsh = def.rest_global.len();
                info!("ragdoll from yaw {:.1} deg, facing {f:?}, pose belly {belly:?} ({lsh} bones)", p.yaw.to_degrees());
            }
            let mut sim = RagdollSim::start(def, &globals, vel, city.collision.values(), mode, p.nm);
            if let Some(s) = shove {
                sim.shove(def, s);
            }
            p.rag = Some(sim);
            p.action = None;
            p.speed = 0.0;
            p.mbr = 0.0;
            p.move_v = 0.0;
            p.vel_y = 0.0;
        }
    }    if p.rag.is_some() {
        ragdoll_frame(&mut p, &mut t, &mut bones, &city, &solids, dt);
        let head = t.translation + Vec3::Y * 1.0;
        let back = Vec3::new(p.cam_yaw.sin() * p.cam_pitch.cos(), -p.cam_pitch.sin(), p.cam_yaw.cos() * p.cam_pitch.cos());
        cam_t.translation = cam_t.translation.lerp(head + back * p.cam_dist, (dt * 8.0).min(1.0));
        cam_t.look_at(head, Vec3::Y);
        return;
    }

    // --- move relative to the camera; face the direction of travel
    let cam_fwd = Vec3::new(-p.cam_yaw.sin(), 0.0, -p.cam_yaw.cos());
    let cam_right = Vec3::new(p.cam_yaw.cos(), 0.0, -p.cam_yaw.sin());
    let mut dir = Vec3::ZERO;
    for (k, v) in [(KeyCode::KeyW, cam_fwd), (KeyCode::KeyS, -cam_fwd), (KeyCode::KeyA, -cam_right), (KeyCode::KeyD, cam_right)] {
        if keys.pressed(k) {
            dir += v;
        }
    }
    if std::env::var("LIBERTY_AUTOWALK").ok().and_then(|s| s.parse::<f32>().ok()).is_some_and(|s| time.elapsed_secs() < s) {
        dir += cam_fwd;
    }
    // LIBERTY_PED_SCRIPT="idle:1,run:2.5,idle:2,walk:2,back:2,sprint:2": scripted input for
    // recordings, timed from when --record starts (idle | walk | run | sprint | back).
    let mut script_mode: Option<&str> = None;
    let mut step_t = 0.0f32;
    let script = std::env::var("LIBERTY_PED_SCRIPT").unwrap_or_default();
    if let (false, Some(s)) = (script.is_empty(), started.as_ref()) {
        let mut at = time.elapsed_secs() - s.0;
        for step in script.split(',') {
            let (mode, secs) = step.split_once(':').unwrap_or((step, "1"));
            let secs: f32 = secs.parse().unwrap_or(1.0);
            if at < secs {
                script_mode = Some(match mode.trim() {
                    "walk" => "walk",
                    "run" => "run",
                    "sprint" => "sprint",
                    "back" => "back",
                    "circle" => "circle",
                    // run across the camera's view (side-on footage: jumps, starts)
                    "right" => "right",
                    "left" => "left",
                    _ => "idle",
                });
                step_t = at;
                break;
            }
            at -= secs;
        }
        script_mode = script_mode.or(Some("idle"));
        dir = match script_mode {
            Some("idle") => Vec3::ZERO,
            Some("back") => -cam_fwd,
            Some("right") => cam_right,
            Some("left") => -cam_right,
            // run while the wanted direction swings left at 80 deg/s (tests the turn lean)
            Some("circle") => Quat::from_rotation_y(step_t * 80f32.to_radians()) * cam_fwd,
            _ => cam_fwd,
        };
    }
    if let Some(target) = approach_to {
        dir = (target - t.translation).with_y(0.0);
    }
    // aiming with a gun: the weapon's aim clip, e.g. gun@handgun/fire (raise, hold, lower), on his
    // upper body; his legs strafe the way the keys say (move_combat_strafe) and he keeps facing
    // where the camera looks
    let aim_clip = armed.weapon.filter(|_| armed.aiming).map(|i| format!("{}/fire", weapons.0[i].info.anim_group));
    let reload_clip = armed.weapon.map(|i| format!("{}/reload", weapons.0[i].info.anim_group));
    let strafe_dir = dir.normalize_or_zero();
    let aim_hold = armed.weapon.map(|i| weapons.0[i].hold());
    let aiming = aim_clip.as_ref().is_some_and(|n| p.loco.as_ref().is_some_and(|l| l.extra.contains_key(n)));
    if aiming {
        dir = Vec3::ZERO;
    }
    let dir = dir.normalize_or_zero();
    let old = std::env::var("LIBERTY_OLD_MOVE").is_ok(); // before S5, for side-by-side runs
    // GTA IV on PC, as the player plays it (1 Oct session, 88 min): W alone walks (1.7-1.8 m/s; IV's own
    // move blender held a desired ratio of 1.0), W + Shift held sprints (7.3-7.5 m/s = the sprint
    // clip). No key in his play gave a steady jog; the scripts' "run" (and LIBERTY_PED_JOG) still ask
    // for ratio 2. (Before S5: jog by default, Alt to walk; LIBERTY_OLD_MOVE=1.)
    let walking = keys.pressed(KeyCode::AltLeft) || std::env::var("LIBERTY_PED_WALK").is_ok() || script_mode == Some("walk");
    let sprinting = keys.pressed(KeyCode::ShiftLeft) || std::env::var("LIBERTY_PED_SPRINT").is_ok() || script_mode == Some("sprint");
    let jogging = !old && !walking && (script_mode.is_some_and(|m| m != "walk") || std::env::var("LIBERTY_PED_JOG").is_ok());
    let walking = walking || (!old && !sprinting && !jogging);
    let target_mbr = if dir == Vec3::ZERO {
        0.0
    } else if sprinting {
        3.0
    } else if walking {
        1.0
    } else {
        2.0
    };
    let clip_speed = |i: usize, fallback: f32| p.loco.as_ref().map_or(fallback, |l| l.clips[i].speed);
    let target_speed = if dir == Vec3::ZERO {
        0.0
    } else if sprinting {
        clip_speed(3, 7.2)
    } else if walking {
        clip_speed(1, 1.6)
    } else {
        clip_speed(2, 4.7)
    };
    // signed angle from facing to the wanted direction (+ = left)
    let turn = if dir == Vec3::ZERO {
        0.0
    } else {
        let mut dy = (-dir.x).atan2(-dir.z) - p.yaw;
        while dy > std::f32::consts::PI {
            dy -= std::f32::consts::TAU;
        }
        while dy < -std::f32::consts::PI {
            dy += std::f32::consts::TAU;
        }
        dy
    };
    let has = |n: &str| p.loco.as_ref().is_some_and(|l| l.extra.contains_key(n));

    // --- pick a one-shot clip: start from standing, stop when released, 180° turns at speed
    if p.action.is_none() && p.loco.is_some() {
        let side = |l: &str, r: &str| if turn > 0.0 { l.to_string() } else { r.to_string() };
        let a = turn.abs().to_degrees();
        // IV starts from standing only from a ratio of 0 (exe 0xcc6020); before S5: speed < 0.3
        let standing = if old { p.speed < 0.3 } else { p.mbr <= 0.0 && p.move_v < 0.3 };
        let pick: Option<(String, f32)> = if standing && dir != Vec3::ZERO {
            let (fwd, ninety, back, then) = if walking {
                ("wstart".to_string(), side("wstart_turn_l90", "wstart_turn_r90"), side("wstart_turn_l180", "wstart_turn_r180"), clip_speed(1, 1.6))
            } else {
                ("runstart_fwd".to_string(), side("runstart_l_90", "runstart_r_90"), side("runstart_l_180", "runstart_r_180"), clip_speed(2, 4.7))
            };
            Some((if a < 45.0 { fwd } else if a < 135.0 { ninety } else { back }, then))
        } else if dir == Vec3::ZERO && p.speed > 1.0 && p.moving_for > 0.45 {
            let foot = if p.phase < 0.5 { "l" } else { "r" };
            // IV picks the stop by the ratio: under 1.5 walk, under 2.7 run, else sprint (exe 0xcc6020)
            let kind = if old {
                if p.speed > 6.0 { "sstop" } else if p.speed > 3.0 { "rstop" } else { "wstop" }
            } else if p.mbr < 1.5 {
                "wstop"
            } else if p.mbr < 2.7 {
                "rstop"
            } else {
                "sstop"
            };
            Some((format!("{kind}_{foot}"), 0.0))
        } else if old && p.speed > 3.0 && a > 150.0 {
            let n = if p.speed > 6.0 { side("sprint_turn_180_l", "sprint_turn_180_r") } else { side("run_turn_180_l", "run_turn_180_r") };
            Some((n, p.speed))
        } else if !old && p.mbr >= 0.8 && a > 150.0 {
            // reversing while moving: the gait's own 180 (walking too, which IV has and we lacked).
            // STAND-IN: the 150 deg trigger and the gait split (IV's stop thresholds) are ours.
            let n = if p.mbr >= 2.7 {
                side("sprint_turn_180_l", "sprint_turn_180_r")
            } else if p.mbr >= 1.5 {
                side("run_turn_180_l", "run_turn_180_r")
            } else {
                side("walk_turn_180_l", "walk_turn_180_r")
            };
            Some((n, p.speed))
        } else {
            None
        };
        if let Some((clip, then_speed)) = pick.filter(|(n, _)| has(n)) {
            let rec = started.as_ref().map_or(0.0, |s| time.elapsed_secs() - s.0);
            debug!("t {rec:5.2}s clip {clip} (turn {:.0} deg, speed {:.1})", turn.to_degrees(), p.speed);
            p.fade = Some((p.last_pose.clone(), FADE));
            p.action = Some(Action { clip, t: 0.0, start_pos: t.translation, start_yaw: p.yaw, then_speed });
        }
    }

    let loco_set = p.loco.take();
    let chunks = city.collision.values();
    let feet = t.translation;
    let mut intended: Option<Vec3> = None;
    let rest: Vec<Transform> = p.joints.iter().map(|j| j.1).collect();
    let mut posed: Option<Vec<(Quat, Vec3)>> = None;
    let mut next;
    // IV lets you cut starts and stops short: push again during a stop and he carries on; let go
    // during a start and he settles, instead of finishing a 1.5-2 s clip
    let cancel = p.action.as_ref().is_some_and(|a| {
        a.t > 0.1 && ((a.clip.contains("stop") && dir != Vec3::ZERO) || (a.clip.contains("start") && dir == Vec3::ZERO))
    });
    if cancel {
        let a = p.action.take().unwrap();
        if let Some(clip) = loco_set.as_ref().and_then(|l| l.extra.get(&a.clip)) {
            // keep the speed the clip was moving at, so the loop picks up where it left off
            let dt_s = 0.05f32.min(a.t);
            p.speed = (clip.mover_at(a.t).0 - clip.mover_at(a.t - dt_s).0).with_y(0.0).length() / dt_s.max(1e-3);
            p.move_v = p.speed;
            p.mbr = loco_set.as_ref().map_or(0.0, |l| pedanim::speed_mbr(l, p.speed));
        }
        p.fade = Some((p.last_pose.clone(), FADE));
    }
    // standing jump: jump_on_spot's own root motion carries the rise (0.45 m), not physics
    let mut spot_lift: Option<f32> = None;
    if let Some(mut act) = p.action.take() {
        // one-shot: root motion from the clip moves and turns him
        let clip = &loco_set.as_ref().unwrap().extra[&act.clip];
        act.t += dt;
        let (off, rot) = clip.mover_at(act.t);
        let yaw_q = Quat::from_rotation_y(act.start_yaw);
        let target = act.start_pos + yaw_q * off;
        next = Vec3::new(target.x, feet.y, target.z);
        if act.clip.starts_with("jump_inair") && !old {
            // airborne: the launch velocity carries him, the in-air clip's own root motion doesn't
            next = Vec3::new(feet.x + p.air_vel.x * dt, feet.y, feet.z + p.air_vel.z * dt);
        }
        intended = Some(next);
        p.yaw = act.start_yaw + rot.to_euler(EulerRot::YXZ).0;
        posed = Some(clip.pose_at(act.t, &rest));
        // get-ups carry his height in their root motion (0 lying -> 0.90 m standing) with the pelvis
        // at the root; every other clip hangs the root at HIP_HEIGHT. Ride the clip's own height.
        if act.clip.starts_with("getup") {
            if let Some(root) = posed.as_mut().and_then(|p| p.first_mut()) {
                root.1.y += off.y - HIP_HEIGHT;
            }
        }
        if act.clip == "jump_on_spot" {
            spot_lift = Some(off.y.max(0.0));
        }
        let inair = act.clip.starts_with("jump_inair");
        if act.t >= clip.duration && act.clip.starts_with("jump_takeoff") {
            // takeoff done: the game adds the height with physics, the in-air clip carries on
            let side = if act.clip.ends_with("_l") { "l" } else { "r" };
            p.vel_y = if old { 4.2 } else { JUMP_UP };
            // the takeoff clip's own speed at its end, in his facing, plus IV's 30%
            let d = 0.05f32.min(clip.duration);
            let v = (clip.mover_at(clip.duration).0 - clip.mover_at(clip.duration - d).0).with_y(0.0) / d;
            p.air_vel = yaw_q * v * JUMP_FWD_GAIN;
            p.fade = Some((clip.pose_at(clip.duration, &rest), 0.08));
            p.action = Some(Action { clip: format!("jump_inair_{side}"), t: 0.0, start_pos: next, start_yaw: p.yaw, then_speed: act.then_speed });
        } else if act.t >= clip.duration && !inair {
            p.fade = Some((clip.pose_at(clip.duration, &rest), FADE));
            p.speed = if dir == Vec3::ZERO { 0.0 } else { act.then_speed };
            p.move_v = p.speed;
            p.mbr = loco_set.as_ref().map_or(0.0, |l| pedanim::speed_mbr(l, p.speed));
            p.phase = 0.0;
        } else {
            p.action = Some(act);
        }
    } else if !old {
        // loop: IV's player move blend (CPedMoveBlendOnFoot; the player's own rates)
        let pi = std::f32::consts::PI;
        // turning hard while moving pulls the wanted ratio toward 1.5: past 45 deg off his heading,
        // linearly down to 1.5 at 180 deg (exe 0xcc14a0; AI peds start at 30 deg)
        let mut want_mbr = target_mbr;
        if p.mbr >= 1.0 && want_mbr >= 1.5 {
            let (a, from) = (turn.abs(), 45f32.to_radians());
            if a > from {
                want_mbr = (want_mbr - (a - from) / (pi - from) * (want_mbr - 1.5)).max(1.5);
            }
        }
        // the ratio moves linearly: up 4 a second, down 2 a second (exe 0xcbf320; AI: 2 and 8)
        p.mbr = if p.mbr < want_mbr { (p.mbr + 4.0 * dt).min(want_mbr) } else { (p.mbr - 2.0 * dt).max(want_mbr) };
        p.speed = loco_set.as_ref().map_or(0.0, |l| pedanim::mbr_speed(l, p.mbr));
        // heading: the wanted turn rate is what is left to turn over 0.5 s, capped at the ped's
        // turn speed (22 for Niko: x pi/180 x 50 = 19.2 rad/s); the rate follows at 50 rad/s2,
        // scaled by (|left to turn| / 2pi + 0.5)^2 (exe 0xcbf320)
        let want_rate = (turn / 0.5).clamp(-19.2, 19.2);
        let f = (turn.abs() / std::f32::consts::TAU + 0.5).clamp(0.0, 1.0);
        let acc = 50.0 * f * f * dt;
        p.turn_rate += (want_rate - p.turn_rate).clamp(-acc, acc);
        p.yaw += p.turn_rate * dt;
        // his velocity chases the clips' own speed at up to 30 m/s2 (exe 0xcc3480)
        p.move_v += (p.speed - p.move_v).clamp(-30.0 * dt, 30.0 * dt);
        p.moving_for = if dir != Vec3::ZERO && p.speed > 1.0 { p.moving_for + dt } else if dir == Vec3::ZERO { 0.0 } else { p.moving_for };
        let facing = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
        next = feet + facing * p.move_v * dt;
    } else {
        // loop (before S5): steer toward the wanted direction at the chosen speed
        p.speed += (target_speed - p.speed) * (dt * 6.0).min(1.0);
        // turn no faster than the game's sharpest turn clip for this gait (its turning circle);
        // from (nearly) standing, turn freely
        let max = loco_set.as_ref().map_or(0.0, |l| pedanim::max_turn_rate(l, p.speed));
        let want = if dir == Vec3::ZERO {
            0.0
        } else if p.speed < 1.0 || max < 0.1 {
            turn * 8.0
        } else {
            (turn * 4.0).clamp(-max * 1.15, max * 1.15)
        };
        p.turn_rate += (want - p.turn_rate) * (dt * 8.0).min(1.0);
        p.yaw += p.turn_rate * dt;
        p.moving_for = if dir != Vec3::ZERO && p.speed > 1.0 { p.moving_for + dt } else if dir == Vec3::ZERO { 0.0 } else { p.moving_for };
        let facing = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
        next = feet + facing * p.speed * dt;
    }
    // aiming: his legs play the strafe clip for the way he's moving (eight directions around
    // where he aims, the combat idle when still) and carry him at that clip's own speed
    if aiming && p.action.is_none() {
        let mut clip_name = "move_combat_strafe/idle".to_string();
        if strafe_dir != Vec3::ZERO {
            // 0 = toward the aim, + = to his left
            let a = (-strafe_dir.x).atan2(-strafe_dir.z) - p.cam_yaw;
            let k = ((a.to_degrees() / 45.0).round() as i32).rem_euclid(8) as usize;
            let name = ["walk", "walk_strafe_l45", "walk_strafe_l", "walk_strafe_bl45", "walk_strafe_b", "walk_strafe_br45", "walk_strafe_r", "walk_strafe_r45"][k];
            clip_name = format!("move_combat_strafe/{name}");
        }
        if let Some(clip) = loco_set.as_ref().and_then(|l| l.extra.get(&clip_name)) {
            if strafe_dir != Vec3::ZERO {
                let v = if clip.speed > 0.2 { clip.speed } else { 1.4 };
                next += strafe_dir * v * dt;
            }
            if p.strafe.0 != clip_name {
                p.fade = Some((p.last_pose.clone(), FADE));
                p.strafe.0 = clip_name;
            }
            let before = p.strafe.1;
            p.strafe.1 = (p.strafe.1 + dt / clip.duration).rem_euclid(1.0);
            if strafe_dir != Vec3::ZERO && ((before < 0.5) != (p.strafe.1 < 0.5)) {
                steps.count += 1;
                steps.running = false;
            }
        }
        if p.strafe_fwd.is_none() {
            // which way the forward strafe walk travels, in his own frame
            p.strafe_fwd = loco_set.as_ref().and_then(|l| l.extra.get("move_combat_strafe/walk")).map(|c| {
                let off = c.mover_at(c.duration).0;
                (-off.x).atan2(-off.z)
            });
        }
    }
    next = collision::push_out_of_walls(chunks.clone(), next, 0.3, &[0.5, 1.1, 1.6]);
    if collision::ground_height(chunks.clone(), next.x, next.z, next.y + 500.0).is_some() {
        let ground = collision::ground_height(chunks.clone(), next.x, next.z, feet.y + 0.5);
        let on_ground = ground.is_some_and(|g| feet.y - g < 0.05);
        // Space: a running jump is takeoff -> in-air -> landing (jump_std.wad); standing, jump_on_spot
        if on_ground && (keys.just_pressed(KeyCode::Space) || scripted("LIBERTY_JUMP_AT")) && p.action.is_none() {
            let (clip, then) = if p.speed > 1.0 {
                (format!("jump_takeoff_{}", if p.phase < 0.5 { "l" } else { "r" }), p.speed)
            } else {
                ("jump_on_spot".to_string(), 0.0)
            };
            if loco_set.as_ref().is_some_and(|l| l.extra.contains_key(&clip)) {
                p.fade = Some((p.last_pose.clone(), FADE));
                p.action = Some(Action { clip, t: 0.0, start_pos: next, start_yaw: p.yaw, then_speed: then });
            }
        }
        if let Some(lift) = spot_lift {
            p.vel_y = 0.0;
            next.y = ground.unwrap_or(feet.y) + lift;
        } else {
            let grounded = p.vel_y == 0.0 && on_ground;
            p.vel_y -= GRAVITY * dt;
            next.y = feet.y + p.vel_y * dt;
            let jumping = p.action.as_ref().is_some_and(|a| a.clip.starts_with("jump"));
            if let Some(g) = ground {
                // walking down steps and kerbs: stay on the ground instead of floating off it
                let step_down = grounded && !jumping && feet.y - g < 0.45;
                if next.y <= g || step_down {
                    let impact = -p.vel_y;
                    next.y = g;
                    p.vel_y = 0.0;
                    if p.action.as_ref().is_some_and(|a| a.clip.starts_with("jump_inair")) {
                        // land by how hard: per-foot landing, a squat, or a roll for big drops
                        let a = p.action.take().unwrap();
                        let side = if a.clip.ends_with("_l") { "l" } else { "r" };
                        let land = if impact > 11.0 { "jump_land_roll".to_string() } else if impact > 7.0 { "jump_land_squat".to_string() } else { format!("jump_land_{side}") };
                        p.fade = Some((p.last_pose.clone(), 0.08));
                        p.action = Some(Action { clip: land, t: 0.0, start_pos: next, start_yaw: p.yaw, then_speed: a.then_speed });
                    }
                }
            }
            // walked off a ledge: fall with the in-air clip
            if p.action.is_none() && p.vel_y < -4.0 && loco_set.as_ref().is_some_and(|l| l.extra.contains_key("jump_inair_l")) {
                p.air_vel = p.vel.with_y(0.0);
                p.fade = Some((p.last_pose.clone(), FADE));
                p.action = Some(Action { clip: "jump_inair_l".into(), t: 0.0, start_pos: next, start_yaw: p.yaw, then_speed: p.speed });
            }
        }
    }
    if let (Some(a), Some(want)) = (p.action.as_mut(), intended) {
        // walls stop root motion too: shift the clip's anchor by what the collision took away
        a.start_pos += (next - want).with_y(0.0);
    }
    p.vel = (next - feet) / dt.max(1e-4);
    t.translation = next;
    // LIBERTY_MOVE_LOG: one line a frame (recording time, position, yaw, speeds, the one-shot clip)
    // for comparing his path with IV's recorded one (re/s5_*.py)
    if std::env::var("LIBERTY_MOVE_LOG").is_ok() {
        if let Some(s) = started.as_ref() {
            info!(
                "move t {:.3} pos {:.3} {:.3} {:.3} yaw {:.1} speed {:.2} vy {:.2} clip {}",
                time.elapsed_secs() - s.0,
                next.x,
                next.y,
                next.z,
                p.yaw.to_degrees(),
                p.speed,
                p.vel_y,
                p.action.as_ref().map_or("-", |a| a.clip.as_str())
            );
        }
    }
    // blend the aim pose in / out over 0.15 s
    p.aim_w = (p.aim_w + if aiming { dt / 0.15 } else { -dt / 0.15 }).clamp(0.0, 1.0);
    // where the camera looks: the barrel should point there
    let look_pitch = p.cam_pitch;
    if aiming && p.action.is_none() {
        let name = aim_clip.clone().unwrap_or_default();
        if p.aim_ref.as_ref().is_none_or(|r| r.clip != name) {
            let rest: Vec<Transform> = p.joints.iter().map(|j| j.1).collect();
            let r = loco_set.as_ref().and_then(|l| l.extra.get(&name)).map(|c| aim_ref(c, &name, &rest, &p.parents, &p.spine, p.hand, aim_hold.unwrap_or_default()));
            if let Some(r) = &r {
                info!("aim clip {name}: hold frame at {:.2} of the clip, barrel {:.1} deg up, {:.1} deg off his facing", r.phase, r.pitch.to_degrees(), r.yaw.to_degrees());
            }
            p.aim_ref = r;
        }
        if let Some((r_yaw, r_pitch)) = p.aim_ref.as_ref().map(|r| (r.yaw, r.pitch)) {
            // turn so his legs' "ahead" is where the camera looks; the upper body is then turned
            // onto the aim in the pose (IV's pistol stance is side-on to the hips). Without the
            // strafe set the whole body takes the aim clip and turns by its own angle.
            let mut d = (p.cam_yaw - p.strafe_fwd.unwrap_or(r_yaw)) - p.yaw;
            d = (d + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            p.yaw += d * (dt * 14.0).min(1.0);
            p.turn_rate = 0.0;
            // and bend the spine for the rest of the pitch
            p.aim_bend = (look_pitch - r_pitch + armed.kick * 0.035).clamp(-1.1, 1.1);
        }
    }
    // Skeleton faces GTA +Y = Bevy −Z at yaw 0, same as our convention.
    t.rotation = Quat::from_rotation_y(p.yaw);

    // LIBERTY_PED_CLIPS="run,rstop_l,...": debug — play these clips back to back in place
    // (from the start of recording) so a recording shows each clip's raw pose.
    if let (Ok(list), Some(s), Some(loco)) = (std::env::var("LIBERTY_PED_CLIPS"), started.as_ref(), loco_set.as_ref()) {
        let mut at = time.elapsed_secs() - s.0;
        for name in list.split(',') {
            let clip = loco.extra.get(name).or_else(|| loco.clips.iter().find(|c| c.name == name));
            let Some(clip) = clip else { continue };
            let span = clip.duration.max(1.2);
            if at < span {
                posed = Some(clip.pose_at(at.min(clip.duration), &rest));
                break;
            }
            at -= span;
        }
    }

    // --- pose: one-shot clip, or the blended loop; cross-fade after a switch
    if let Some(loco) = loco_set.as_ref() {
        let mut pose = match posed {
            Some(ps) => ps,
            None => {
                let cycle = pedanim::cycle_duration(loco, p.speed);
                let idle_len = loco.clips[0].duration;
                let before = p.phase;
                p.phase = (p.phase + dt / cycle).rem_euclid(1.0);
                // a foot comes down twice a cycle. Which phase is the exact contact isn't read
                // from the clips yet: the cycle's start and middle are taken as the two contacts.
                if p.speed > 0.5 && ((before < 0.5) != (p.phase < 0.5)) {
                    steps.count += 1;
                    steps.running = p.speed > 3.0;
                }
                // scripted tests on a fixed clock: the idle clip waits for the recording to start,
                // so a shove at "5 s in" always meets the same pose (loading time no longer shifts it)
                if std::env::var("LIBERTY_FIXED_DT").is_ok() && started.is_none() {
                    p.idle_phase = 0.0;
                } else {
                    p.idle_phase = (p.idle_phase + dt / idle_len).rem_euclid(1.0);
                }
                pedanim::pose_lean(loco, p.speed, p.phase, p.idle_phase, &rest, p.turn_rate)
            }
        };
        // aiming: the upper body takes the aim clip's hold frame (or the reload clip while
        // reloading), the legs and root the strafe clip. The spine hangs off the root, and the two
        // clips' roots are turned differently, so the spine's base is re-seated: as the aim clip's
        // own root would carry it, then turned from the measured barrel direction onto the legs'
        // "ahead" (which faces the camera's aim).
        if p.aim_w > 0.0 {
            if let Some(r) = &p.aim_ref {
                if let Some(clip) = loco.extra.get(&r.clip) {
                    let reload = armed.reload_progress().zip(reload_clip.as_ref().and_then(|n| loco.extra.get(n)));
                    let mut up = match reload {
                        Some(((t, _), rc)) => rc.pose_at(t.min(rc.duration), &rest),
                        None => clip.pose_at(r.phase * clip.duration, &rest),
                    };
                    // spread the up/down bend over the spine bones, about the horizontal axis
                    // across the barrel (each bone's parent frame)
                    let share = p.aim_bend / r.axes.len().max(1) as f32;
                    for &(j, axis) in &r.axes {
                        if let Some(b) = up.get_mut(j) {
                            b.0 = Quat::from_axis_angle(axis, share) * b.0;
                        }
                    }
                    if let (Some(legs_clip), Some(fwd), Some(&base)) = (loco.extra.get(&p.strafe.0), p.strafe_fwd, p.spine.first()) {
                        let mut whole = legs_clip.pose_at(p.strafe.1 * legs_clip.duration, &rest);
                        // some of these clips keep the root's height from the ground (about 0.93 to
                        // 0.99 m), the rest from the hips (about -0.04 m) like every loop clip; the rig
                        // already stands HIP_HEIGHT up
                        if whole[0].1.y > 0.5 {
                            whole[0].1.y -= HIP_HEIGHT;
                        }
                        let seat = whole[0].0.inverse() * Quat::from_rotation_y(fwd - r.yaw) * up[0].0;
                        for (j, a) in up.iter().enumerate() {
                            if p.upper.get(j).copied().unwrap_or(false) {
                                whole[j] = *a;
                            }
                        }
                        whole[base] = (seat * up[base].0, seat * up[base].1);
                        up = whole;
                    }
                    let w = p.aim_w;
                    for (cur, a) in pose.iter_mut().zip(&up) {
                        *cur = (cur.0.slerp(a.0, w), cur.1.lerp(a.1, w));
                    }
                }
            }
        }
        // one path for every pose (fade, helper bones, debug overrides)
        apply_pose(&mut p, &mut bones, std::mem::take(&mut pose), dt);
    }
    p.loco = loco_set;
    // --- camera: orbit behind and above; aiming, in over his right shoulder
    let cam_right = Vec3::new(p.cam_yaw.cos(), 0.0, -p.cam_yaw.sin());
    let head = t.translation + Vec3::Y * (1.6 - 0.05 * p.aim_w) + cam_right * (0.45 * p.aim_w);
    let back = Vec3::new(p.cam_yaw.sin() * p.cam_pitch.cos(), -p.cam_pitch.sin(), p.cam_yaw.cos() * p.cam_pitch.cos());
    let want_dist = p.cam_dist + (1.6 - p.cam_dist) * p.aim_w;
    // Something between him and the camera (a wall, a ceiling, the ground when looking up): the
    // camera comes in to just short of it at once, and eases back out when the way is clear.
    // Five rays, the middle one and four 0.2 m around it, stand in for a sphere.
    let side = back.cross(Vec3::Y).normalize_or(Vec3::X);
    let up = side.cross(back).normalize_or(Vec3::Y);
    let mut clear = want_dist;
    for off in [Vec3::ZERO, side * 0.2, -side * 0.2, up * 0.2, -up * 0.2] {
        if let Some((d, _)) = collision::raycast(city.collision.values(), head + off, back, want_dist + 0.3) {
            clear = clear.min((d - 0.3).max(0.35));
        }
    }
    let blocked = clear < want_dist - 0.01;
    p.cam_clear = if clear < p.cam_clear { clear } else { (p.cam_clear + (clear - p.cam_clear) * (dt * 4.0).min(1.0)).min(clear) };
    let want_cam = head + back * p.cam_clear;
    // blocked: go straight there (easing would pass through the wall on the way)
    cam_t.translation = if blocked { want_cam } else { cam_t.translation.lerp(want_cam, (dt * 12.0).min(1.0)) };
    cam_t.look_at(head, Vec3::Y);
}

enum CarStep {
    /// Not busy with a car: normal movement.
    Free,
    /// Run to this spot (the driver's door) with normal movement.
    RunTo(Vec3),
    /// A car clip is posing him; skip normal movement.
    Posed(Vec<(Quat, Vec3)>),
}

fn yaw_of(q: Quat) -> f32 {
    q.to_euler(EulerRot::YXZ).0
}

/// Where get_in_ds must start (car-local position, yaw) so that it ends with him in the seat:
/// the seat, minus the clip's root motion run backwards.
fn entry_anchor(p: &Player, car: &Car) -> Option<(Vec3, f32)> {
    let seat = car.driver_seat()? - Vec3::Y * HIP_HEIGHT;
    let gi = p.veh.get("get_in_ds")?;
    let (m_end, r_end) = gi.mover_at(gi.duration);
    let yaw0 = -yaw_of(r_end);
    Some((seat - Quat::from_rotation_y(yaw0) * m_end, yaw0))
}

/// Which way he sits (car-local yaw) once get_in_ds has finished. The clip ends with its root bone
/// turned ~97 degrees while d_close_in_ds and sit_drive start at a root turn of 0, so those two are
/// played turned by get_in_ds's final root (and mover) turn: the clips then join up and he faces
/// forward. Before (to 8 Oct) they were played at 0 and he sat facing the passenger door.
fn seated_yaw(p: &Player) -> f32 {
    let Some(gi) = p.veh.get("get_in_ds") else { return 0.0 };
    let (_, r_end) = gi.mover_at(gi.duration);
    let root_end = if std::env::var("LIBERTY_OLD_SEAT").is_ok() { Quat::IDENTITY } else { gi.rot(0, 0.9999).unwrap_or(Quat::IDENTITY) };
    yaw_of(r_end) + yaw_of(root_end)
}

fn place_in_car(p: &mut Player, t: &mut Transform, car: &Car, local: Vec3, local_yaw: f32) {
    let (cp, cr) = car.body_pose();
    t.translation = cp + cr * local;
    t.rotation = cr * Quat::from_rotation_y(local_yaw);
    p.yaw = yaw_of(t.rotation);
}

fn car_interaction(
    p: &mut Player,
    t: &mut Transform,
    fly: &mut Fly,
    cars: &mut Query<(Entity, &mut Car)>,
    press_f: bool,
    dt: f32,
) -> CarStep {
    let rest: Vec<Transform> = p.joints.iter().map(|j| j.1).collect();
    match p.car {
        CarState::None => {
            if press_f && fly.driving.is_none() {
                let pos = t.translation;
                let nearest = cars
                    .iter()
                    .filter(|(_, c)| c.driver_seat().is_some())
                    .map(|(e, c)| (e, c.body_pose().0.distance(pos)))
                    .filter(|(_, d)| *d < 9.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((car, _)) = nearest {
                    p.car = CarState::Approach { car };
                    p.action = None;
                }
            }
            CarStep::Free
        }
        CarState::Approach { car } => {
            let Ok((_, c)) = cars.get(car) else {
                p.car = CarState::None;
                return CarStep::Free;
            };
            let Some((start, _)) = entry_anchor(p, &c) else {
                p.car = CarState::None;
                return CarStep::Free;
            };
            let (cp, cr) = c.body_pose();
            let target = cp + cr * start;
            if press_f {
                p.car = CarState::None; // changed his mind
                return CarStep::Free;
            }
            if (target - t.translation).with_y(0.0).length() < 0.4 {
                p.fade = Some((p.last_pose.clone(), FADE));
                p.car = CarState::Enter { car, stage: 0, t: 0.0 };
            }
            CarStep::RunTo(target)
        }
        CarState::Enter { car, stage, t: ct } => {
            let Ok((_, mut c)) = cars.get_mut(car) else {
                p.car = CarState::None;
                return CarStep::Free;
            };
            let (Some((start, yaw0)), Some(seat)) = (entry_anchor(p, &c), c.driver_seat()) else {
                p.car = CarState::None;
                return CarStep::Free;
            };
            let seat = seat - Vec3::Y * HIP_HEIGHT;
            let name = ["d_open_out_ds", "get_in_ds", "d_close_in_ds"][stage as usize];
            let Some(clip) = p.veh.get(name) else {
                p.car = CarState::Seated { car };
                return CarStep::Free;
            };
            let nt = ct + dt;
            let f = (nt / clip.duration).clamp(0.0, 1.0);
            let (local, local_yaw) = match stage {
                0 => (start, yaw0),
                1 => {
                    let (m, r) = clip.mover_at(nt);
                    (start + Quat::from_rotation_y(yaw0) * m, yaw0 + yaw_of(r))
                }
                _ => (seat, seated_yaw(p)),
            };
            c.door_open = match stage {
                0 => f,
                1 => 1.0,
                _ => 1.0 - f,
            };
            let pose = clip.pose_at(nt, &rest);
            let dur = clip.duration;
            if std::env::var("LIBERTY_CAR_DEBUG").is_ok() {
                let (m, r) = clip.mover_at(nt);
                let deg = |q: Quat| yaw_of(q).to_degrees();
                // which way the hips face in the rig's frame (0 = the entity's forward, + = turned left)
                let global = |mut j: usize| {
                    let mut m = Mat4::IDENTITY;
                    loop {
                        m = Mat4::from_rotation_translation(pose[j].0, pose[j].1) * m;
                        match p.parents[j] {
                            Some(q) => j = q,
                            None => break,
                        }
                    }
                    m.w_axis.truncate()
                };
                let hips = p.thighs.map(|(l, r)| {
                    let across = (global(r) - global(l)).with_y(0.0);
                    let fwd = Vec3::Y.cross(across); // facing = up x (left -> right)
                    (-fwd.x).atan2(-fwd.z).to_degrees()
                });
                info!("car enter: {name} t {nt:.2}/{dur:.2} mover ({:.2},{:.2},{:.2}) yaw {:.0}, root yaw {:.0}, hips face {:?}, placed yaw {:.0}", m.x, m.y, m.z, deg(r), deg(pose[0].0), hips.map(|h| h.round()), local_yaw.to_degrees());
            }
            place_in_car(p, t, &c, local, local_yaw);
            if nt >= dur {
                p.fade = Some((pose.clone(), FADE));
                p.car = if stage < 2 {
                    CarState::Enter { car, stage: stage + 1, t: 0.0 }
                } else {
                    fly.driving = Some(car);
                    fly.walking = false;
                    CarState::Seated { car }
                };
            } else {
                p.car = CarState::Enter { car, stage, t: nt };
            }
            CarStep::Posed(pose)
        }
        CarState::Seated { car } => {
            let Ok((_, c)) = cars.get(car) else {
                p.car = CarState::None;
                return CarStep::Free;
            };
            let Some(seat) = c.driver_seat() else {
                p.car = CarState::None;
                return CarStep::Free;
            };
            let yaw = seated_yaw(p);
            place_in_car(p, t, &c, seat - Vec3::Y * HIP_HEIGHT, yaw);
            p.idle_phase += dt;
            let pose = match p.veh.get("sit_drive") {
                Some(sit) => sit.pose_at(p.idle_phase.rem_euclid(sit.duration), &rest),
                None => p.last_pose.clone(),
            };
            if press_f {
                fly.driving = None;
                fly.walking = true;
                p.fade = Some((pose.clone(), FADE));
                p.car = CarState::Exit { car, stage: 0, t: 0.0 };
            }
            CarStep::Posed(pose)
        }
        CarState::Exit { car, stage, t: ct } => {
            let Ok((_, mut c)) = cars.get_mut(car) else {
                p.car = CarState::None;
                return CarStep::Free;
            };
            let Some(seat) = c.driver_seat() else {
                p.car = CarState::None;
                return CarStep::Free;
            };
            let seat = seat - Vec3::Y * HIP_HEIGHT;
            let name = ["get_out_ds", "d_close_out_ds"][stage as usize];
            let (Some(clip), Some(out)) = (p.veh.get(name), p.veh.get("get_out_ds")) else {
                p.car = CarState::None;
                return CarStep::Free;
            };
            let nt = ct + dt;
            let f = (nt / clip.duration).clamp(0.0, 1.0);
            let (local, local_yaw) = if stage == 0 {
                let (m, r) = clip.mover_at(nt);
                (seat + m, yaw_of(r))
            } else {
                let (m_end, r_end) = out.mover_at(out.duration);
                let (m, r) = clip.mover_at(nt);
                let base_yaw = yaw_of(r_end);
                (seat + m_end + Quat::from_rotation_y(base_yaw) * m, base_yaw + yaw_of(r))
            };
            c.door_open = if stage == 0 { (f / 0.35).min(1.0) } else { 1.0 - f };
            let pose = clip.pose_at(nt, &rest);
            let dur = clip.duration;
            place_in_car(p, t, &c, local, local_yaw);
            if nt >= dur {
                p.fade = Some((pose.clone(), FADE));
                if stage == 0 {
                    p.car = CarState::Exit { car, stage: 1, t: 0.0 };
                } else {
                    p.car = CarState::None;
                    p.speed = 0.0;
                    p.mbr = 0.0;
                    p.move_v = 0.0;
                    p.vel_y = 0.0;
                    t.rotation = Quat::from_rotation_y(p.yaw);
                }
            } else {
                p.car = CarState::Exit { car, stage, t: nt };
            }
            CarStep::Posed(pose)
        }
    }
}

/// A skin helper bone parented beside the bone it belongs with (the arm twist/roll bones hang off the
/// upper arm and clavicle, not the forearm and upper arm they sit on). Clips don't animate them; IV
/// moves them in code. Rules measured from IV's own skeleton (re/memgrab2.py, 29 Sep 2026, ~20k frames):
/// - `char_*_foretwist` (most of the forearm skin): the forearm, plus half the hand's twist about the
///   forearm's axis (leftover vs IV: median 0.3 deg).
/// - `*_armroll` (upper-arm skin): the upper arm, keeping 75% of its own twist (median ~5 deg).
/// - `*_upperarmroll`: stays at rest on the clavicle (median 2-5 deg), so it needs no rule.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Helper {
    bone: usize,
    driver: usize,
    twist: HelperTwist,
}

#[derive(Clone, Copy, Debug)]
enum HelperTwist {
    /// Add this fraction of a child bone's twist about the driver's axis (toward that child).
    Child(usize, f32),
    /// Add this fraction of the driver's own twist since rest (negative = keep less of it).
    Own(f32),
}

pub(crate) fn helper_bones(skeleton: &[Bone]) -> Vec<Helper> {
    let find = |n: &str| skeleton.iter().position(|b| b.name == n);
    let mut out = Vec::new();
    for side in ["l", "r"] {
        let rules = [
            (format!("char_{side}_foretwist"), format!("char_{side}_forearm"), find(&format!("char_{side}_hand")).map(|h| HelperTwist::Child(h, 0.5))),
            (format!("{side}_armroll"), format!("char_{side}_upperarm"), Some(HelperTwist::Own(-0.25))),
        ];
        for (bone, driver, twist) in rules {
            if let (Some(b), Some(d), Some(twist)) = (find(&bone), find(&driver), twist) {
                // the rule below needs the helper and its driver to share a parent
                if skeleton[b].parent == skeleton[d].parent {
                    out.push(Helper { bone: b, driver: d, twist });
                }
            }
        }
    }
    out
}

/// The twist part of `q` about `axis` (swing-twist split).
fn twist_about(q: Quat, axis: Vec3) -> Quat {
    let v = Vec3::new(q.x, q.y, q.z).dot(axis);
    let t = Quat::from_xyzw(axis.x * v, axis.y * v, axis.z * v, q.w);
    if t.length_squared() > 1e-12 { t.normalize() } else { Quat::IDENTITY }
}

/// Place the helper bones from their drivers (same parent, so all in that parent's frame).
pub(crate) fn place_helpers(helpers: &[Helper], rest: &[Transform], pose: &mut [(Quat, Vec3)]) {
    for h in helpers {
        if h.bone >= pose.len() || h.driver >= pose.len() {
            continue;
        }
        let local = |j: usize| Transform { translation: pose[j].1, rotation: pose[j].0, ..default() };
        let offset = Transform::from_matrix(rest[h.driver].compute_matrix().inverse() * rest[h.bone].compute_matrix());
        let twist = match h.twist {
            HelperTwist::Child(c, frac) if c < pose.len() => {
                // the child's change since rest, in the driver's frame, twisted about the driver→child axis
                let d = pose[c].0 * rest[c].rotation.inverse();
                Quat::IDENTITY.slerp(twist_about(d, rest[c].translation.normalize_or_zero()), frac)
            }
            HelperTwist::Own(frac) => {
                // the driver's change since rest, in its own frame, twisted about its bone axis (local X)
                let d = rest[h.driver].rotation.inverse() * pose[h.driver].0;
                let t = twist_about(d, Vec3::X);
                let (axis, angle) = t.to_axis_angle();
                Quat::from_axis_angle(axis, angle * frac)
            }
            _ => Quat::IDENTITY,
        };
        let g = local(h.driver).mul_transform(Transform::from_rotation(twist)).mul_transform(offset);
        pose[h.bone] = (g.rotation.normalize(), g.translation);
    }
}

/// Write a pose to the joints, cross-fading from the previous one after a switch.
fn apply_pose(p: &mut Player, bones: &mut Query<&mut Transform, (Without<Player>, Without<Fly>)>, mut pose: Vec<(Quat, Vec3)>, dt: f32) {
    let rest: Vec<Transform> = p.joints.iter().map(|j| j.1).collect();
    if std::env::var("LIBERTY_BINDPOSE").is_ok() {
        // debug: the skeleton's rest pose, untouched (what the mesh is skinned against)
        pose = rest.iter().map(|t| (t.rotation, t.translation)).collect();
    }
    let refpose = std::env::var("LIBERTY_REFPOSE").ok();
    if let Some(path) = &refpose {
        // debug: play IV's own recorded skeleton (re/memgrab2.py export: u32 frames, u32 bones, then
        // per frame per bone quat xyzw + local position, GTA space, 60 Hz) on Niko; root stays ours
        static REF: std::sync::OnceLock<Option<(Vec<f32>, usize, usize)>> = std::sync::OnceLock::new();
        static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let r = REF.get_or_init(|| {
            let d = std::fs::read(path).ok()?;
            let n = u32::from_le_bytes(d[0..4].try_into().ok()?) as usize;
            let nb = u32::from_le_bytes(d[4..8].try_into().ok()?) as usize;
            let f: Vec<f32> = d[8..].chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
            info!("refpose {path}: {n} frames x {nb} bones ({} floats)", f.len());
            (f.len() >= n * nb * 7).then_some((f, n, nb))
        });
        if r.is_none() {
            warn!("refpose {path}: not loaded");
        }
        if let Some((f, n, nb)) = r {
            let start: f32 = std::env::var("LIBERTY_REFPOSE_START").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let hold = std::env::var("LIBERTY_REFPOSE_HOLD").is_ok();
            let t = if hold { start } else { T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f32() + start };
            let k = ((t * 60.0) as usize) % n;
            for j in 1..pose.len().min(*nb) {
                let v = &f[(k * nb + j) * 7..(k * nb + j) * 7 + 7];
                pose[j] = (Quat::from_xyzw(v[0], v[2], -v[1], v[3]).normalize(), Vec3::from(to_bevy([v[4], v[5], v[6]])));
            }
        }
    }
    if std::env::var("LIBERTY_NO_HELPERS").is_err() && (refpose.is_none() || std::env::var("LIBERTY_REFPOSE_OURHELPERS").is_ok()) {
        place_helpers(&p.helpers, &rest, &mut pose);
    }
    if let Some((from, left)) = p.fade.take() {
        let w = (1.0 - left / FADE).clamp(0.0, 1.0);
        if from.len() == pose.len() {
            for (cur, old) in pose.iter_mut().zip(&from) {
                *cur = (old.0.slerp(cur.0, w), old.1.lerp(cur.1, w));
            }
        }
        if left - dt > 0.0 {
            p.fade = Some((from, left - dt));
        }
    }
    for (j, (rot, pos)) in pose.iter().enumerate() {
        if let Ok(mut bt) = bones.get_mut(p.joints[j].0) {
            bt.rotation = *rot;
            bt.translation = *pos;
        }
    }
    p.last_pose = pose;
}

fn orbit_camera(
    p: &mut Player,
    t: &Transform,
    cam_t: &mut Transform,
    buttons: &ButtonInput<MouseButton>,
    motion: &AccumulatedMouseMotion,
    scroll: &AccumulatedMouseScroll,
    dt: f32,
) {
    if crate::looklock::looking(&buttons) {
        p.cam_yaw -= motion.delta.x * 0.004;
        p.cam_pitch = (p.cam_pitch - motion.delta.y * 0.004).clamp(-1.2, 0.5);
    }
    if scroll.delta.y != 0.0 {
        p.cam_dist = (p.cam_dist * 0.9f32.powf(scroll.delta.y)).clamp(0.8, 12.0);
    }
    let head = t.translation + Vec3::Y * 1.6;
    let back = Vec3::new(p.cam_yaw.sin() * p.cam_pitch.cos(), -p.cam_pitch.sin(), p.cam_yaw.cos() * p.cam_pitch.cos());
    cam_t.translation = cam_t.translation.lerp(head + back * p.cam_dist, (dt * 12.0).min(1.0));
    cam_t.look_at(head, Vec3::Y);
}

/// Build a skinned ped under a new (hidden) root entity: joints from `skeleton`, and every mesh of
/// `parts` skinned to them, textured through `texture` (so callers can swap texture variants).
/// Returns the root and, per skeleton bone: (joint entity, rest local transform, rest global rot).
#[allow(clippy::too_many_arguments)]
pub fn build_rig<'a>(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    skeleton: &[Bone],
    parts: &[&Drawable],
    texture: &dyn Fn(&str) -> Option<&'a Texture>,
    name: &str,
) -> (Entity, Vec<(Entity, Transform, Quat)>) {
    let root = commands.spawn((Transform::default(), Visibility::Hidden, Name::new(name.to_string()))).id();
    let rig = commands.spawn((Transform::from_xyz(0.0, HIP_HEIGHT, 0.0), Visibility::default())).id();
    commands.entity(root).add_child(rig);

    // Joints, parents before children (the skeleton is stored that way).
    let mut joints: Vec<(Entity, Transform, Quat)> = Vec::with_capacity(skeleton.len());
    let mut global: Vec<Mat4> = Vec::with_capacity(skeleton.len());
    for b in skeleton {
        let local = bone_local(b);
        // Visibility so things parented to a bone (a weapon in the hand) inherit the rig's visibility
        let e = commands.spawn((local, Visibility::default())).id();
        let parent_g = match b.parent {
            Some(p) => {
                commands.entity(joints[p].0).add_child(e);
                global[p]
            }
            None => {
                commands.entity(rig).add_child(e);
                Mat4::IDENTITY
            }
        };
        let g = parent_g * local.compute_matrix();
        let (_, g_rot, _) = g.to_scale_rotation_translation();
        global.push(g);
        joints.push((e, local, g_rot));
    }
    let inverse = bindposes.add(SkinnedMeshInverseBindposes::from(global.iter().map(|m| m.inverse()).collect::<Vec<_>>()));
    let joint_entities: Vec<Entity> = joints.iter().map(|j| j.0).collect();
    if std::env::var("LIBERTY_SKINCHECK").is_ok() {
        // debug: in the bind pose each vertex should sit near the bone it leans on most. Per part and
        // bone: how many vertices, and their mean / max distance from that bone's rest position.
        for (pi, part) in parts.iter().enumerate() {
            let mut acc: HashMap<usize, (usize, f32, f32)> = HashMap::new();
            for m in &part.meshes {
                if m.weights.len() != m.positions.len() {
                    continue;
                }
                for (v, (w, j)) in m.positions.iter().zip(m.weights.iter().zip(&m.joints)) {
                    let k = (0..4).max_by(|&a, &b| w[a].total_cmp(&w[b])).unwrap_or(0);
                    let bone = j[k] as usize;
                    let Some(g) = global.get(bone) else { continue };
                    let d = (Vec3::from(to_bevy(*v)) - g.w_axis.truncate()).length();
                    let e = acc.entry(bone).or_insert((0, 0.0, 0.0));
                    e.0 += 1;
                    e.1 += d;
                    e.2 = e.2.max(d);
                }
            }
            let mut rows: Vec<_> = acc.into_iter().collect();
            rows.sort_by_key(|r| r.0);
            for (bone, (n, sum, max)) in rows {
                info!("skin part {pi} bone {bone:3} {:24} verts {n:5} mean {:.3} max {:.3}", skeleton[bone].name, sum / n as f32, max);
            }
        }
    }

    let mut image_cache: HashMap<String, Handle<Image>> = HashMap::new();
    for part in parts {
        let mats: Vec<Handle<StandardMaterial>> = part
            .shaders
            .iter()
            .map(|s| {
                let t = s.diffuse.as_ref().and_then(|n| texture(n).map(|t| (n, t)));
                let tex = t.map(|(_, t)| image_cache.entry(t.name.clone()).or_insert_with(|| images.add(assets::gpu_image(t))).clone());
                // only hair is cut out; skin/cloth alpha channels hold other data (spec etc.)
                let masked = s.diffuse.as_deref().is_some_and(|n| n.starts_with("hair"))
                    && t.is_some_and(|(_, t)| matches!(t.format, Format::Dxt3 | Format::Dxt5));
                let mut m = assets::material(tex, if masked { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque });
                m.perceptual_roughness = 0.7;
                // normal map (uploaded as data, cached apart from colour textures) and specular map
                let normal = s.normal.as_ref().and_then(|n| texture(n)).map(|t| image_cache.entry(format!("{}#linear", t.name)).or_insert_with(|| images.add(assets::gpu_image_linear(t))).clone());
                let specular = s.specular.as_ref().and_then(|n| texture(n)).map(|t| image_cache.entry(t.name.clone()).or_insert_with(|| images.add(assets::gpu_image(t))).clone());
                assets::apply_maps(&mut m, normal, specular);
                materials.add(m)
            })
            .collect();
        for m in &part.meshes {
            let mut mesh = assets::build_mesh(m);
            let skinned = m.weights.len() == m.positions.len();
            if skinned {
                mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(m.joints.clone()));
                mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, m.weights.clone());
            }
            let mat = mats.get(m.shader).cloned().unwrap_or_else(|| materials.add(assets::material(None, AlphaMode::Opaque)));
            let mut e = commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(mat)));
            if skinned {
                e.insert(SkinnedMesh { inverse_bindposes: inverse.clone(), joints: joint_entities.clone() });
            }
            let id = e.id();
            commands.entity(rig).add_child(id);
        }
    }
    (root, joints)
}
/// The skeleton's parent: the rig sits HIP_HEIGHT above the ped's root (feet on the ground).
/// An aim clip measured once: the frame where the gun is up and held (IV's gun@<group>/fire clips
/// raise the gun, hold it, then lower it), where the barrel points there (pitch up, yaw off his
/// facing, radians), and for each spine bone the axis to bend about (horizontal, across the
/// barrel) in that bone's parent frame.
struct AimRef {
    clip: String,
    phase: f32,
    pitch: f32,
    yaw: f32,
    axes: Vec<(usize, Vec3)>,
}

fn aim_ref(clip: &Clip, name: &str, rest: &[Transform], parents: &[Option<usize>], spine: &[usize], hand: usize, hold: Transform) -> AimRef {
    // forward kinematics in his own frame (facing -Z), rig HIP_HEIGHT above the feet
    let fk = |phase: f32| -> Vec<Transform> {
        let pose = clip.pose_at(phase * clip.duration, rest);
        let mut world: Vec<Transform> = Vec::with_capacity(pose.len());
        for (j, (r, t)) in pose.iter().enumerate() {
            let local = Transform { translation: *t, rotation: *r, ..default() };
            let w = match parents.get(j).copied().flatten() {
                Some(pj) => world[pj].mul_transform(local),
                None => Transform::from_xyz(0.0, HIP_HEIGHT, 0.0).mul_transform(local),
            };
            world.push(w);
        }
        world
    };
    // the hold frame: where the gun is highest (the raised, steady part of the clip)
    let phase = (0..=32)
        .map(|k| k as f32 / 32.0)
        .max_by(|a, b| fk(*a)[hand].mul_transform(hold).translation.y.total_cmp(&fk(*b)[hand].mul_transform(hold).translation.y))
        .unwrap_or(0.5);
    let world = fk(phase);
    let barrel = world[hand].mul_transform(hold).rotation * Vec3::X;
    let flat = Vec3::new(barrel.x, 0.0, barrel.z).normalize_or(Vec3::NEG_Z);
    let across = flat.cross(Vec3::Y).normalize();
    let axes = spine
        .iter()
        .map(|&j| {
            let parent_rot = parents.get(j).copied().flatten().map_or(Quat::IDENTITY, |pj| world[pj].rotation);
            (j, parent_rot.inverse() * across)
        })
        .collect();
    AimRef { clip: name.to_string(), phase, pitch: barrel.y.clamp(-1.0, 1.0).asin(), yaw: (-flat.x).atan2(-flat.z), axes }
}

pub fn rig_of(t: &Transform) -> Transform {
    t.mul_transform(Transform::from_xyz(0.0, HIP_HEIGHT, 0.0))
}

/// The pose he's in now, as local transforms per joint (rest pose if none yet).
fn current_locals(p: &Player) -> Vec<Transform> {
    if p.last_pose.len() == p.joints.len() {
        p.last_pose.iter().map(|(r, t)| Transform { translation: *t, rotation: *r, ..default() }).collect()
    } else {
        p.joints.iter().map(|j| j.1).collect()
    }
}

/// One ragdoll frame: step the physics, pose the skeleton from the parts, keep the ped's root under
/// the pelvis; once he's lain still, line up the game's get-up clip with how he landed.
fn ragdoll_frame(
    p: &mut Player,
    t: &mut Transform,
    bones: &mut Query<&mut Transform, (Without<Player>, Without<Fly>)>,
    city: &City,
    cars: &[CarBox],
    dt: f32,
) {
    let (Some(def), Some(mut sim)) = (p.rag_def.take(), p.rag.take()) else { return };
    let chunks = city.collision.values();
    sim.step(&def, dt, chunks.clone(), cars);
    let locals = current_locals(p);
    let pel = sim.pelvis_transform().translation;
    let g = collision::ground_height(chunks.clone(), pel.x, pel.z, pel.y + 0.5).unwrap_or(pel.y - 0.2);
    t.translation = Vec3::new(pel.x, g, pel.z);
    let rig = rig_of(t);
    let globals = sim.joint_globals(&def, &rig, &locals);
    apply_pose(p, bones, crate::ragdoll::locals_from_globals(&def, &rig, &globals), dt);

    // (also: he has stood still on the balancer for a moment, without it giving up)
    let stood = matches!(sim.mode, Mode::Balance) && pel.y - g > 0.6 && sim.still > 0.8;
    if sim.recovered || stood {
        // bodyBalance held him up: back to animation from where he stands
        let pel = sim.pelvis_transform().translation;
        let g = collision::ground_height(chunks.clone(), pel.x, pel.z, pel.y + 0.5).unwrap_or(t.translation.y);
        t.translation = Vec3::new(pel.x, g, pel.z);
        p.fade = Some((p.last_pose.clone(), FADE));
        p.rag_def = Some(def);
        return;
    }
    if sim.still > 0.8 || sim.age > 8.0 {
        // front or back: which way his belly (the rest pose's forward, −Z) points now
        let pelvis_bone = def.pelvis_bone();
        let belly = globals[pelvis_bone].rotation * def.rest_global[pelvis_bone].rotation.inverse() * Vec3::NEG_Z;
        let clip_name = if belly.y < 0.0 { "getup_front_normal" } else { "getup_back_normal" };
        let head_bone = def.head_bone();
        if let Some(clip) = p.loco.as_ref().and_then(|l| l.extra.get(clip_name)) {
            // the clip's first frame, from its own root: where its pelvis and head lie
            let rest: Vec<Transform> = p.joints.iter().map(|j| j.1).collect();
            let first: Vec<Transform> =
                clip.pose_at(0.0, &rest).into_iter().map(|(r, v)| Transform { translation: v, rotation: r, ..default() }).collect();
            let cg = def.globals(&Transform::IDENTITY, &first);
            let angle = |d: Vec3| d.x.atan2(d.z);
            let d_clip = (cg[head_bone].translation - cg[pelvis_bone].translation).with_y(0.0);
            let d_world = (globals[head_bone].translation - globals[pelvis_bone].translation).with_y(0.0);
            p.yaw = angle(d_world) - angle(d_clip);
            let yaw_q = Quat::from_rotation_y(p.yaw);
            let pel_w = globals[pelvis_bone].translation;
            let at = pel_w - yaw_q * cg[pelvis_bone].translation.with_y(0.0);
            t.translation = Vec3::new(at.x, g, at.z);
            t.rotation = yaw_q;
            if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
                let pose_end: Vec<Transform> = clip.pose_at(clip.duration, &rest).into_iter().map(|(r, v)| Transform { translation: v, rotation: r, ..default() }).collect();
                let ce = def.globals(&Transform::IDENTITY, &pose_end);
                info!(
                    "get-up {clip_name}: pelvis y {:.2}, ground {:.2}, clip pelvis y start {:.2} end {:.2}, mover start {:?} end {:?}, root bone local y start {:.2}",
                    pel.y, g, cg[pelvis_bone].translation.y, ce[pelvis_bone].translation.y, clip.mover_at(0.0).0, clip.mover_at(clip.duration).0, first[0].translation.y
                );
            }
            p.fade = Some((p.last_pose.clone(), FADE));
            p.action = Some(Action { clip: clip_name.to_string(), t: 0.0, start_pos: t.translation, start_yaw: p.yaw, then_speed: 0.0 });
        }
        p.rag_def = Some(def);
        return;
    }
    p.rag_def = Some(def);
    p.rag = Some(sim);
}
/// When (seconds from now) a car's box, moving as it is, reaches point `p` (with `margin` for the
/// body's width); 0 if it's already there, None if its path misses.
pub(crate) fn time_to_hit(c: &CarBox, p: Vec3, margin: f32) -> Option<f32> {
    let q = c.rot.inverse() * (p - c.pos);
    let v = c.rot.inverse() * c.vel;
    let lo = c.lo - Vec3::new(margin, 0.3, margin);
    let hi = c.hi + Vec3::new(margin, 0.3, margin);
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    for k in 0..3 {
        let (qk, vk, lk, hk) = (q[k], v[k], lo[k], hi[k]);
        if vk.abs() < 1e-3 {
            if qk < lk || qk > hk {
                return None;
            }
            continue;
        }
        let (a, b) = ((qk - hk) / vk, (qk - lk) / vk);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
    }
    (t0 <= t1).then_some(t0)
}