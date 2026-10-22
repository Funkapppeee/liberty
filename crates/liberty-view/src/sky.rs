//! The sky: a dome around the camera, coloured from the time cycle the way IV's sky shader does.
//!
//! From the game: the time-cycle columns (names per FusionShaders' TimeCycFormater: sky colour
//! 64-66, horizon colour 67-69, east horizon colour 70-72, horizon height 77, horizon brightness
//! 78, sun colour 103-105, stars brightness 108), `visualSettings.dat`'s sun disc limits, the star
//! texture (`pc/textures/skydome.wtd`), and the way the shader combines them (read from the
//! disassembled `gta_atmoscatt_clouds`: behaviour only).
//!
//! Clouds: the shader's own steps (two layers cut out of the game's noise textures by a threshold
//! and a bias, shaded by sampling the same noise a little toward the sun) with the time cycle's
//! cloud columns. The moon: the game's sheet of moon phases and its glow sheet.
//!
//! STAND-INS / not done: which cloud column feeds which step (inferred, see `timecyc::Sky`), how
//! the dome is mapped flat for the cloud textures, cloud drift speed, the moon's size, brightness
//! and phase (always full), the galaxy texture, the sun's corona term, how the stars are wrapped
//! over the dome, and the overall brightness (sky light multiplier x our exposure).

use bevy::asset::{load_internal_asset, weak_handle};
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin, NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, ShaderRef, ShaderType, SpecializedMeshPipelineError};
use bevy::render::view::NoFrustumCulling;

const SHADER: Handle<Shader> = weak_handle!("3d2b7f0a-91c4-4e0e-8a55-7b0f1c2d9e44");

#[derive(Clone, Copy, Default, ShaderType)]
pub struct SkyParams {
    pub sky: Vec4,
    pub horizon: Vec4,
    pub horizon_east: Vec4,
    pub sun_dir: Vec4,
    pub sun_color: Vec4,
    pub p: Vec4,
    pub q: Vec4,
    pub cloud_color: Vec4,
    pub sunset_color: Vec4,
    pub top_color: Vec4,
    pub c1: Vec4,
    pub c2: Vec4,
    pub c3: Vec4,
    pub top: Vec4,
    pub moon: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct SkyMaterial {
    #[uniform(0)]
    pub params: SkyParams,
    #[texture(1)]
    #[sampler(2)]
    pub stars: Handle<Image>,
    #[texture(3)]
    #[sampler(4)]
    pub perlin: Handle<Image>,
    #[texture(5)]
    #[sampler(6)]
    pub detail: Handle<Image>,
    #[texture(7)]
    #[sampler(8)]
    pub moon: Handle<Image>,
    #[texture(9)]
    #[sampler(10)]
    pub moon_glow: Handle<Image>,
}

impl Material for SkyMaterial {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    // seen from the inside
    fn specialize(_: &MaterialPipeline<Self>, descriptor: &mut RenderPipelineDescriptor, _: &MeshVertexBufferLayoutRef, _: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

#[derive(Component)]
pub struct SkyDome;

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(app, SHADER, "sky.wgsl", Shader::from_wgsl);
        app.add_plugins(MaterialPlugin::<SkyMaterial>::default());
    }
}

pub fn setup(mut commands: Commands, src: Res<crate::hud::HudSource>, mut images: ResMut<Assets<Image>>, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<SkyMaterial>>) {
    let art = std::fs::read(src.root.join("pc/textures/skydome.wtd"))
        .ok()
        .and_then(|b| rage::rsc::Resource::load(&b).ok())
        .and_then(|r| rage::wtd::read(&r).ok())
        .unwrap_or_default();
    let mut tex = |name: &str| art.iter().find(|t| t.name.eq_ignore_ascii_case(name)).map(|t| images.add(crate::assets::gpu_image(t))).unwrap_or_default();
    let (stars, perlin, detail, moon, moon_glow) = (tex("starfield"), tex("baseperlinnoise3channel"), tex("noise16_p"), tex("moon"), tex("moon_glow"));
    commands.spawn((
        Mesh3d(meshes.add(Sphere::new(1.0).mesh().uv(48, 24))),
        MeshMaterial3d(materials.add(SkyMaterial { params: SkyParams::default(), stars, perlin, detail, moon, moon_glow })),
        Transform::from_scale(Vec3::splat(5200.0)),
        SkyDome,
        NotShadowCaster,
        NotShadowReceiver,
        NoFrustumCulling,
    ));
}

/// Keep the dome on the camera and its colours on the clock.
pub fn update(
    time: Res<Time>,
    tc: Res<crate::timecyc::TimeCycle>,
    visual: Option<Res<crate::vehicle::Garage>>,
    cam: Query<&Transform, (With<Camera3d>, Without<SkyDome>)>,
    sun: Query<&Transform, (With<DirectionalLight>, Without<SkyDome>, Without<Camera3d>)>,
    mut dome: Query<(&mut Transform, &MeshMaterial3d<SkyMaterial>), With<SkyDome>>,
    mut materials: ResMut<Assets<SkyMaterial>>,
) {
    let _slow = crate::SlowGuard::new("sky::update");
    let (Ok(cam), Ok((mut t, mat))) = (cam.single(), dome.single_mut()) else { return };
    t.translation = cam.translation;
    let Some(m) = materials.get_mut(&mat.0) else { return };
    let s = tc.sky();
    let get = |name: &str, default: f32| visual.as_ref().map_or(default, |g| g.visual.get(name, default));
    let (start, end) = (get("sky.sun.centreStart", 0.98), get("sky.sun.centreEnd", 1.0));
    // the one directional light is the sun by day and the moon by night
    let to_light = sun.single().map_or(Vec3::Y, |s| s.translation.normalize_or(Vec3::Y));
    let day = tc.sun_up();
    m.params = SkyParams {
        sky: s.sky.extend(0.0),
        horizon: s.horizon.extend(0.0),
        horizon_east: s.horizon_east.extend(0.0),
        sun_dir: to_light.extend(if day { 1.0 } else { 0.0 }),
        sun_color: s.sun_color.extend(0.0),
        p: Vec4::new(s.horizon_height, s.horizon_brightness, s.brightness, s.stars),
        q: Vec4::new(start, end, s.cloud_amount, 0.0),
        cloud_color: s.cloud_color.extend(0.0),
        sunset_color: s.sunset_color.extend(0.0),
        top_color: s.top_color.extend(0.0),
        c1: s.cloud,
        c2: Vec4::new(s.cloud_edge, s.cloud2.x, s.cloud2.y, s.cloud2.z),
        c3: Vec4::new(s.cloud2.w, s.cloud_inscatter, s.detail_offset, time.elapsed_secs()),
        top: s.top,
        moon: to_light.extend(if day { 0.0 } else { 1.0 }),
    };
}
