//! Street lamps and other lights. IV keeps them in the model files: each drawable carries a list
//! of lights (`rage::drawable::Light`: position, direction, colour, range, strength, cone), so a
//! lamp post lights the street wherever the map places one.
//!
//! The nearest `MAX` to the camera are real lights; the rest are skipped (Bevy can't afford
//! thousands). They come on at 20:00 and go off at 06:30, as seen in the real game
//! (`TimeCycle::is_dark`). STAND-INS: every light follows that one rule (the lights' own flags,
//! which probably say when each is on, aren't decoded); IV's strength is turned into Bevy's units so that the light
//! at half its range is about a quarter of its strength (IV's own falloff curve isn't decoded).
//!
//! Coronas (the glow at the bulb): the game's own sprite (`pc/textures/coronas.wtd`, "corona"),
//! one per light that has a corona size, in the light's colour, always facing the camera, drawn
//! out to the light's own draw distance. STAND-INS: the size in metres (the light's corona number
//! taken as a radius) and the brightness; IV also fades them by distance and when hidden.

use bevy::prelude::*;

/// On a placed map object whose model has lights.
#[derive(Component)]
pub struct Lamps(pub Vec<rage::drawable::Light>);

/// On a placed traffic light: its signal lights (pure green, amber and red, three to a head) and,
/// once worked out, the traffic-light phase each one runs on.
#[derive(Component)]
pub struct Signals {
    pub lights: Vec<rage::drawable::Light>,
    pub phases: Vec<u8>,
}

/// One lit signal's glow (rebuilt a few times a second).
#[derive(Component)]
pub struct SignalGlow;

/// Is this light one of a traffic signal's three? 0 green, 1 amber, 2 red (the model files use
/// exactly these three colours for them).
pub fn signal_colour(c: [u8; 3]) -> Option<u8> {
    match c {
        [0, 255, 0] => Some(0),
        [255, 128, 0] => Some(1),
        [255, 0, 0] => Some(2),
        _ => None,
    }
}

/// One of the pool of real lights (or a corona: rebuilt together).
#[derive(Component)]
pub struct LampLight;

/// A glow sprite at a bulb; turned to face the camera every frame.
#[derive(Component)]
pub struct Corona;

/// The game's corona sprite, a square to draw it on, and one material per light colour.
#[derive(Resource)]
pub struct CoronaArt {
    image: Handle<Image>,
    quad: Handle<Mesh>,
    mats: std::collections::HashMap<[u8; 3], Handle<StandardMaterial>>,
}

pub fn setup(mut commands: Commands, src: Res<crate::hud::HudSource>, mut images: ResMut<Assets<Image>>, mut meshes: ResMut<Assets<Mesh>>) {
    let tex = std::fs::read(src.root.join("pc/textures/coronas.wtd"))
        .ok()
        .and_then(|b| rage::rsc::Resource::load(&b).ok())
        .and_then(|r| rage::wtd::read(&r).ok())
        .and_then(|ts| ts.into_iter().find(|t| t.name.eq_ignore_ascii_case("corona")));
    let Some(tex) = tex else {
        warn!("coronas.wtd: no corona texture, lamps will have no glow");
        return;
    };
    commands.insert_resource(CoronaArt { image: images.add(crate::assets::gpu_image(&tex)), quad: meshes.add(Rectangle::new(2.0, 2.0)), mats: Default::default() });
}

/// Coronas face the camera.
pub fn face_camera(cam: Query<&Transform, (With<Camera3d>, Without<Corona>)>, mut q: Query<&mut Transform, With<Corona>>) {
    let _slow = crate::SlowGuard::new("lights::face_camera");
    let Ok(cam) = cam.single() else { return };
    for mut t in &mut q {
        t.rotation = cam.rotation;
    }
}

const MAX_CORONAS: usize = 160;

const MAX: usize = 192;
const REACH: f32 = 140.0;

/// IV's light strength as Bevy lumens, now that point and spot lights fade by IV's own rule (see
/// `iv_falloff`): Bevy turns lumens into brightness as lumens / (4 pi), and its diffuse term is
/// albedo / pi, so 4 pi^2 x strength gives "albedo x strength" right under the lamp, as in IV.
/// The range no longer comes into it (it only sets where the light ends).
pub fn lumens(intensity: f32, _range: f32) -> f32 {
    4.0 * std::f32::consts::PI * std::f32::consts::PI * intensity
}

/// Make every point and spot light fade the way IV's lights do instead of by inverse square.
///
/// IV's rule (its deferred light shader): `1.5 x ((1 - d2/R2)^2 - 1/3)`, never below zero: flat
/// near the bulb, 0.74 at 0.3 R, 0.34 at half the range, nothing beyond 0.65 R. Bevy keeps its
/// distance fade in one small function of its lighting shader; this swaps that function's body
/// in the shader Bevy has already loaded. Everything else about Bevy's lights stays.
/// Not matched: IV's surface term is `saturate((N.L - 0.2) x 1.25)`, Bevy's is plain N.L.
pub fn iv_falloff(mut shaders: ResMut<Assets<Shader>>) {
    use bevy::render::render_resource::Source;
    let handle = bevy::pbr::PBR_LIGHTING_HANDLE;
    let Some(Source::Wgsl(src)) = shaders.get(&handle).map(|s| s.source.clone()) else {
        warn!("lights: Bevy's lighting shader isn't loaded; lamps keep the inverse-square fade");
        return;
    };
    let old = "    let factor = distanceSquare * inverseRangeSquared;
    let smoothFactor = saturate(1.0 - factor * factor);
    let attenuation = smoothFactor * smoothFactor;
    return attenuation * 1.0 / max(distanceSquare, 0.0001);";
    let new = "    let x = saturate(1.0 - distanceSquare * inverseRangeSquared);
    return max(1.5 * (x * x - 1.0 / 3.0), 0.0);";
    let src = src.replace("
", "
");
    if !src.contains(old) {
        warn!("lights: Bevy's distance-fade function isn't the one expected; lamps keep the inverse-square fade");
        return;
    }
    shaders.insert(handle.id(), Shader::from_wgsl(src.replace(old, new), "liberty/pbr_lighting_iv_falloff.wgsl"));
    info!("lights: point and spot lights fade by IV's rule");
}

fn to_bevy(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0], p[2], -p[1])
}

pub fn lamps(
    mut commands: Commands,
    time: Res<Time>,
    tc: Option<Res<crate::timecyc::TimeCycle>>,
    cam: Query<&Transform, With<Camera3d>>,
    placed: Query<(&Transform, &Lamps)>,
    mut pool: Query<(Entity, &mut Transform, Option<&mut PointLight>, Option<&mut SpotLight>), (With<LampLight>, Without<Lamps>, Without<Camera3d>)>,
    mut art: Option<ResMut<CoronaArt>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut timer: Local<f32>,
    mut logged: Local<bool>,
) {
    let _slow = crate::SlowGuard::new("lights::lamps");
    *timer -= time.delta_secs();
    if *timer > 0.0 {
        return;
    }
    *timer = 0.5;
    let Ok(cam) = cam.single() else { return };
    let on = tc.as_ref().is_none_or(|tc| tc.is_dark());
    // every light within reach, nearest first
    let mut near: Vec<(f32, Vec3, Vec3, &rage::drawable::Light)> = Vec::new();
    let mut glows: Vec<(f32, Vec3, &rage::drawable::Light)> = Vec::new();
    if on {
        for (t, lamps) in &placed {
            if t.translation.distance_squared(cam.translation) > 360.0 * 360.0 {
                continue;
            }
            for l in &lamps.0 {
                let pos = t.transform_point(to_bevy(l.pos));
                let d = pos.distance(cam.translation);
                if l.corona > 0.0 && d < l.draw_dist.min(300.0) {
                    glows.push((d, pos, l));
                }
                if d < REACH && l.range > 0.5 && l.intensity > 0.0 {
                    near.push((d, pos, (t.rotation * to_bevy(l.dir)).normalize_or(Vec3::NEG_Y), l));
                }
            }
        }
    }
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    // a close-up model and its far stand-in often carry the same lamp: keep one per spot, the
    // one that reaches further (see stream.rs)
    let mut kept: Vec<(f32, Vec3, Vec3, &rage::drawable::Light)> = Vec::with_capacity(near.len());
    for n in near {
        match kept.iter_mut().rev().take(24).find(|k| k.1.distance_squared(n.1) < 0.25) {
            Some(k) if n.3.range > k.3.range => *k = n,
            Some(_) => {}
            None => kept.push(n),
        }
    }
    let mut near = kept;
    if std::env::var("LIBERTY_LIGHT_DEBUG").is_ok() && on && !near.is_empty() && !*logged {
        *logged = true;
        info!("lamps: {} within {REACH} m, lighting the nearest {}; nearest: {:?}", near.len(), near.len().min(MAX), near[0].3);
        let mut by: std::collections::BTreeMap<(u32, u32, u8), (usize, f32)> = Default::default();
        for n in &near {
            let e = by.entry((n.3.range as u32, n.3.intensity as u32, n.3.kind)).or_insert((0, f32::MAX));
            e.0 += 1;
            e.1 = e.1.min(n.0);
        }
        for ((range, strength, kind), (count, nearest)) in by {
            info!("  lamps of range {range} m, strength {strength}, kind {kind}: {count}, nearest {nearest:.0} m away");
        }
    }
    near.truncate(MAX);
    glows.sort_by(|a, b| a.0.total_cmp(&b.0));
    glows.truncate(MAX_CORONAS);
    // rebuild the pool (twice a second; cheap next to the frame)
    for (e, ..) in &mut pool {
        commands.entity(e).despawn();
    }
    if let Some(art) = art.as_mut() {
        let (image, quad) = (art.image.clone(), art.quad.clone());
        for (_, pos, l) in glows {
            let mat = art
                .mats
                .entry(l.color)
                .or_insert_with(|| {
                    materials.add(StandardMaterial {
                        base_color: Color::linear_rgb(l.color[0] as f32 / 255.0 * 1.5, l.color[1] as f32 / 255.0 * 1.5, l.color[2] as f32 / 255.0 * 1.5),
                        base_color_texture: Some(image.clone()),
                        unlit: true,
                        alpha_mode: AlphaMode::Add,
                        fog_enabled: false,
                        cull_mode: None,
                        ..default()
                    })
                })
                .clone();
            // a little toward the camera, so the lamp's own housing doesn't cut the sprite in half
            let at = pos + (cam.translation - pos).normalize_or_zero() * 0.4;
            commands.spawn((Mesh3d(quad.clone()), MeshMaterial3d(mat), Transform::from_translation(at).with_rotation(cam.rotation).with_scale(Vec3::splat(l.corona)), LampLight, Corona, bevy::pbr::NotShadowCaster));
        }
    }
    for (_, pos, dir, l) in near {
        let color = Color::linear_rgb(l.color[0] as f32 / 255.0, l.color[1] as f32 / 255.0, l.color[2] as f32 / 255.0);
        let lumens = lumens(l.intensity, l.range);
        if l.kind == 2 {
            let outer = l.cone.to_radians().clamp(0.1, 1.5);
            commands.spawn((
                SpotLight { color, intensity: lumens, range: l.range, outer_angle: outer, inner_angle: outer * 0.7, shadows_enabled: false, ..default() },
                Transform::from_translation(pos).looking_to(dir, if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y }),
                LampLight,
            ));
        } else {
            commands.spawn((PointLight { color, intensity: lumens, range: l.range, shadows_enabled: false, ..default() }, Transform::from_translation(pos), LampLight));
        }
    }
}

/// The lit lamp of each traffic signal: a glow sprite at the lamp the cycle says is on (the same
/// clock and phases the traffic obeys), seen from the front only, and after dark a real light.
///
/// From the game: which lights a signal has and where (the model's light list), the three
/// colours, strength, reach and cone (`visualSettings.dat`, trafficLight.*), and each road
/// link's phase. STAND-INS: which head runs on which phase is worked out here from the road links
/// that drive toward its face; the glow's size; the "walk / don't walk" boxes aren't done.
#[allow(clippy::too_many_arguments)]
pub fn signals(
    mut commands: Commands,
    time: Res<Time>,
    tc: Option<Res<crate::timecyc::TimeCycle>>,
    roads: Option<Res<crate::traffic::Roads>>,
    garage: Option<Res<crate::vehicle::Garage>>,
    cam: Query<&Transform, With<Camera3d>>,
    mut placed: Query<(&Transform, &mut Signals), Without<Camera3d>>,
    old: Query<Entity, With<SignalGlow>>,
    mut art: Option<ResMut<CoronaArt>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut timer: Local<f32>,
    mut logged: Local<bool>,
) {
    *timer -= time.delta_secs();
    if *timer > 0.0 {
        return;
    }
    *timer = 0.25;
    let (Ok(cam), Some(roads)) = (cam.single(), roads) else { return };
    for e in &old {
        commands.entity(e).despawn();
    }
    let get = |name: &str, default: f32| garage.as_ref().map_or(default, |g| g.visual.get(name, default));
    let colour = |state: u8| -> [f32; 3] {
        let (name, d) = match state {
            0 => ("trafficLight.green.color", [0.1, 0.4, 0.1]),
            1 => ("trafficLight.amber.color", [1.0, 0.3, 0.0]),
            _ => ("trafficLight.red.color", [1.0, 0.1, 0.0]),
        };
        [get(&format!("{name}.red"), d[0]), get(&format!("{name}.green"), d[1]), get(&format!("{name}.blue"), d[2])]
    };
    let (strength, reach, cone) = (get("trafficLight.intensity", 20.0), get("trafficLight.falloffMax", 14.0), get("trafficLight.outerConeAngle", 90.0));
    let inner = get("trafficLight.innerConeAngle", 25.0);
    let dark = tc.as_ref().is_none_or(|tc| tc.is_dark());
    let now = time.elapsed_secs();
    let mut lit: Vec<(f32, Vec3, Vec3, u8)> = Vec::new();
    let (mut heads, mut unmatched) = (0, 0);
    let clock = std::time::Instant::now();
    let mut phased = 0usize;
    for (t, mut s) in &mut placed {
        if t.translation.distance_squared(cam.translation) > 150.0 * 150.0 {
            continue;
        }
        if s.phases.len() != s.lights.len() {
            phased += s.lights.len();
            s.phases = s.lights.iter().map(|l| roads.signal_phase(t.transform_point(to_bevy(l.pos)), (t.rotation * to_bevy(l.dir)).with_y(0.0).normalize_or_zero())).collect();
        }
        for (l, &phase) in s.lights.iter().zip(&s.phases) {
            heads += 1;
            unmatched += (phase == 0) as usize;
            // a head with no road to answer to shows red (it would otherwise show all three)
            let state = if phase == 0 { 2 } else { crate::traffic::light_state(phase, now) };
            if signal_colour(l.color) != Some(state) {
                continue;
            }
            let pos = t.transform_point(to_bevy(l.pos));
            let dir = (t.rotation * to_bevy(l.dir)).normalize_or(Vec3::NEG_Y);
            lit.push((pos.distance(cam.translation), pos, dir, state));
        }
    }
    let took = clock.elapsed().as_secs_f32();
    if took > 0.2 {
        warn!("signals: {took:.2} s working out the phases of {phased} signal lights ({heads} within 150 m)");
    }
    if std::env::var("LIBERTY_LIGHT_DEBUG").is_ok() && heads > 0 && !*logged {
        *logged = true;
        info!("signals: {heads} signal lights within 150 m, {unmatched} with no road link found, {} lit now", lit.len());
    }
    lit.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (k, (_, pos, dir, state)) in lit.into_iter().enumerate() {
        let c = colour(state);
        let peak = c[0].max(c[1]).max(c[2]).max(1e-3);
        // the glow: only from in front of the lamp
        if (cam.translation - pos).normalize_or_zero().dot(dir) > 0.15 {
            if let Some(art) = art.as_mut() {
                let (image, quad) = (art.image.clone(), art.quad.clone());
                // a key no real lamp colour uses
                let key = [state, 0, 1];
                let mat = art
                    .mats
                    .entry(key)
                    .or_insert_with(|| {
                        materials.add(StandardMaterial {
                            base_color: Color::linear_rgb(c[0] / peak * 1.4, c[1] / peak * 1.4, c[2] / peak * 1.4),
                            base_color_texture: Some(image),
                            unlit: true,
                            alpha_mode: AlphaMode::Add,
                            fog_enabled: false,
                            cull_mode: None,
                            ..default()
                        })
                    })
                    .clone();
                // STAND-IN for the lit lens: the game gives these lights no corona (size 0 in every signal
                // model) and lights the lens itself; until the lens material is lit, a lens-sized glow
                let at = pos + dir * 0.08;
                commands.spawn((Mesh3d(quad), MeshMaterial3d(mat), Transform::from_translation(at).with_rotation(cam.rotation).with_scale(Vec3::splat(0.2)), SignalGlow, Corona, bevy::pbr::NotShadowCaster));
            }
        }
        // after dark the nearest few also light the road in front of them
        if dark && k < 24 {
            commands.spawn((
                SpotLight { color: Color::linear_rgb(c[0], c[1], c[2]), intensity: lumens(strength, reach), range: reach, outer_angle: (cone * 0.5).to_radians().clamp(0.1, 1.5), inner_angle: (inner * 0.5).to_radians(), shadows_enabled: false, ..default() },
                Transform::from_translation(pos).looking_to(dir, if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y }),
                SignalGlow,
            ));
        }
    }
}
