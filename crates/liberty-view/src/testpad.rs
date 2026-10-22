//! `LIBERTY_TESTPAD=1`: a flat, empty test pad out past the edge of the map, for ragdoll, brace and
//! car tests with no kerbs, buildings or slopes in the way. 400 m square, 2 m checker floor so motion
//! reads on camera. Niko starts in the middle; the ram test's car starts 24 m away on the same slab.
//! The test lab's stations (`--testlab`, testlab.rs) stand further out on it, with their own props.

use bevy::prelude::*;

use crate::collision::ChunkCollision;
use crate::stream::City;

/// The pad's centre, in GTA coordinates (north-east, over open water, well clear of the city and
/// more than 60 m from any road so the on-foot spawn doesn't snap to a pavement).
pub const PAD_GTA: [f32; 3] = [3500.0, 3500.0, 40.0];
const HALF: f32 = 200.0;
/// Collision keys: outside the streamer's chunk numbers, so they're never unloaded.
const KEY: usize = usize::MAX;
const PROPS_KEY: usize = usize::MAX - 1;

pub fn enabled() -> bool {
    std::env::var("LIBERTY_TESTPAD").is_ok()
}

/// The pad centre in Bevy space.
pub fn pad_centre() -> Vec3 {
    Vec3::from(crate::assets::to_bevy(PAD_GTA))
}

/// Where a run starts (the camera's point): the pad centre, or a test lab station's spot
/// (`LIBERTY_LAB_AT=x,y`, metres east / north of the pad centre). Niko appears under the camera,
/// which starts 6 m south of its point looking north (testlab.rs sets the camera to match), so the
/// point is 6 m north of the spot.
pub fn centre() -> Vec3 {
    pad_centre() + lab_at().map_or(Vec3::ZERO, |o| Vec3::new(o.x, 0.0, -o.y - 6.0))
}

fn lab_at() -> Option<Vec2> {
    let v: Vec<f32> = std::env::var("LIBERTY_LAB_AT").ok()?.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    (v.len() == 2).then(|| Vec2::new(v[0], v[1]))
}

// --- the test lab's stations (testlab.rs): where Niko starts on each, metres east / north of the
// pad centre, all well away from the centre so the plain pad tests there are unchanged. He faces
// north; a target ped stands 2.6 m to his west (the shot stations look west).
pub const AT_MOVE: [f32; 2] = [-150.0, -120.0];
pub const AT_JUMP: [f32; 2] = [-120.0, -120.0];
pub const AT_SHOVE: [f32; 2] = [-90.0, -120.0];
pub const AT_SHOT: [f32; 2] = [-60.0, -120.0];
pub const AT_CARHIT: [f32; 2] = [-30.0, -120.0];
pub const AT_KERB: [f32; 2] = [30.0, -120.0];
pub const AT_WATER: [f32; 2] = [60.0, -120.0];
pub const AT_SLOPE: [f32; 2] = [90.0, -120.0];
pub const AT_GROUND_KERB: [f32; 2] = [120.0, -120.0];
/// The car station: the car parks 5 m north of him and drives west over bumps and a kerb.
pub const AT_CAR: [f32; 2] = [0.0, 80.0];
/// How far west of Niko a lab station's target ped stands.
pub const TARGET_WEST: f32 = 2.6;
/// The water station's pool: its surface over the pad, and its west / east / south / north edges
/// in metres from the station spot. Niko and the target stand on a 2 m dock east of it.
pub const POOL_LEVEL: f32 = 1.7;
const POOL: [f32; 4] = [-9.0, -3.0, -4.0, 4.0];

/// A test lab station's target ped (`LIBERTY_LAB_AT` with `LIBERTY_TARGET_PED`): on the camera's line
/// (the aim camera looks over his shoulder, so the shot goes down that line, not his), level with the
/// point 2.6 m west of the station spot, facing the camera. (spot, yaw) in Bevy space at the pad's
/// height (the spawn finds the ground under it).
pub fn lab_target(cam: Vec3, fwd: Vec3) -> Option<(Vec3, f32)> {
    let o = lab_at()?;
    let want = pad_centre() + Vec3::new(o.x - TARGET_WEST, 0.0, -o.y);
    let f = fwd.with_y(0.0).normalize_or(Vec3::NEG_X);
    let on_line = cam.with_y(want.y) + f * (want - cam.with_y(want.y)).dot(f);
    Some((on_line, f.x.atan2(f.z)))
}

/// The water station's pool as a water quad (GTA space), added to the map's water.
pub fn lab_water() -> Option<rage::water::WaterQuad> {
    if !enabled() {
        return None;
    }
    let (x, y, z) = (PAD_GTA[0] + AT_WATER[0], PAD_GTA[1] + AT_WATER[1], PAD_GTA[2] + POOL_LEVEL);
    let [w, e, s, n] = POOL;
    Some(rage::water::WaterQuad { corners: [[x + w, y + s, z], [x + e, y + s, z], [x + w, y + n, z], [x + e, y + n, z]], params: [[0.0; 4]; 4], kind: 0, flag: 0.0 })
}

/// The pool's surface height (Bevy y) when `p` is over the pool.
pub fn lab_water_level(p: Vec3) -> Option<f32> {
    let q = lab_water()?;
    let (x, y) = (p.x, -p.z);
    let [a, _, _, d] = q.corners;
    (x >= a[0] && x <= d[0] && y >= a[1] && y <= d[1]).then_some(a[2])
}

/// The lab's props as triangles in Bevy space, each wound so (b - a) x (c - a) faces out.
#[derive(Default)]
struct Props(Vec<[Vec3; 3]>);

impl Props {
    /// GTA offset (east, north, up) from the pad centre, in Bevy space.
    fn p(x: f32, y: f32, z: f32) -> Vec3 {
        pad_centre() + Vec3::new(x, z, -y)
    }

    fn quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3) {
        self.0.push([a, b, c]);
        self.0.push([a, c, d]);
    }

    /// A block standing on the pad: x0..x1 east, y0..y1 north, `h` high.
    fn block(&mut self, x0: f32, x1: f32, y0: f32, y1: f32, h: f32) {
        self.wedge(x0, x1, y0, y1, [h; 4]);
    }

    /// A block whose top corners (south-west, south-east, north-east, north-west) have their own
    /// heights: ramps and bumps. Top and sides; no bottom.
    fn wedge(&mut self, x0: f32, x1: f32, y0: f32, y1: f32, z: [f32; 4]) {
        let p = Self::p;
        let (sw, se, ne, nw) = (p(x0, y0, z[0]), p(x1, y0, z[1]), p(x1, y1, z[2]), p(x0, y1, z[3]));
        let (sw0, se0, ne0, nw0) = (p(x0, y0, 0.0), p(x1, y0, 0.0), p(x1, y1, 0.0), p(x0, y1, 0.0));
        self.quad(sw, se, ne, nw);
        for (a, b, a0, b0) in [(sw, se, sw0, se0), (se, ne, se0, ne0), (ne, nw, ne0, nw0), (nw, sw, nw0, sw0)] {
            if (a - a0).length() + (b - b0).length() > 1e-3 {
                self.quad(a0, b0, b, a);
            }
        }
    }

    fn build() -> Props {
        let mut s = Props::default();
        // kerb: a 15 cm kerb 4 m ahead, a rail beside the path, a 1 m ramp up to a ledge and its drop
        let [x, y] = AT_KERB;
        s.block(x - 3.0, x + 3.0, y + 4.0, y + 7.0, 0.15);
        s.block(x + 3.5, x + 3.6, y + 2.0, y + 14.0, 0.5);
        s.wedge(x - 3.0, x + 3.0, y + 12.0, y + 16.0, [0.0, 0.0, 1.0, 1.0]);
        s.block(x - 3.0, x + 3.0, y + 16.0, y + 19.0, 1.0);
        // water: Niko and the target on a 2 m dock, the walled pool (floor = the pad) west of it
        let [x, y] = AT_WATER;
        let [w, e, so, n] = POOL;
        s.block(x + e, x + 3.0, y + so, y + n, 2.0);
        s.block(x + w - 0.3, x + w, y + so - 0.3, y + n + 0.3, 2.0);
        s.block(x + w, x + e, y + n, y + n + 0.3, 2.0);
        s.block(x + w, x + e, y + so - 0.3, y + so, 2.0);
        // ground (slope): the target stands on a 15 degree slope rising to the west
        let [x, y] = AT_SLOPE;
        let rise = 6.0 * 15f32.to_radians().tan();
        s.wedge(x - 7.5, x - 1.5, y - 4.0, y + 4.0, [rise, 0.0, 0.0, rise]);
        // ground (kerb): the target stands in the road just east of a 15 cm kerb and falls across it
        let [x, y] = AT_GROUND_KERB;
        s.block(x - 12.0, x - 2.9, y - 5.0, y + 5.0, 0.15);
        // car: the car parks 5 m north of Niko and drives west: four 8 cm bumps, then a 12 cm kerb
        // up onto a 15 m slab
        let [x, y] = AT_CAR;
        let lane = y + 5.0;
        for k in 0..4 {
            let c = x - 12.0 - 3.0 * k as f32;
            s.wedge(c - 0.25, c, lane - 2.5, lane + 2.5, [0.0, 0.08, 0.08, 0.0]);
            s.wedge(c, c + 0.25, lane - 2.5, lane + 2.5, [0.08, 0.0, 0.0, 0.08]);
        }
        s.block(x - 45.0, x - 30.0, lane - 4.0, lane + 4.0, 0.12);
        s
    }
}

pub fn spawn_testpad(
    mut commands: Commands,
    mut city: ResMut<City>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    if !enabled() {
        return;
    }
    let c = pad_centre();
    // collision: one big quad, split into 20 m tiles so the ground grid stays cheap to query
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let n = 20;
    let step = 2.0 * HALF / n as f32;
    for i in 0..=n {
        for j in 0..=n {
            verts.push([c.x - HALF + i as f32 * step, c.y, c.z - HALF + j as f32 * step]);
        }
    }
    let at = |i: u32, j: u32| i * (n as u32 + 1) + j;
    for i in 0..n as u32 {
        for j in 0..n as u32 {
            tris.push([at(i, j), at(i, j + 1), at(i + 1, j + 1)]);
            tris.push([at(i, j), at(i + 1, j + 1), at(i + 1, j)]);
        }
    }
    city.collision.insert(KEY, ChunkCollision::new(&verts, &tris));

    // the floor: a 2 m checker, light and mid grey
    let size = 64u32;
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let light = ((x / 32) + (y / 32)) % 2 == 0;
            let v = if light { 170 } else { 120 };
            px.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let mut img = Image::new(
        bevy::render::render_resource::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        bevy::render::render_resource::TextureDimension::D2,
        px,
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    img.sampler = bevy::image::ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
        address_mode_u: bevy::image::ImageAddressMode::Repeat,
        address_mode_v: bevy::image::ImageAddressMode::Repeat,
        mag_filter: bevy::image::ImageFilterMode::Nearest,
        ..default()
    });
    let tex = images.add(img);
    // one checker tile (two squares) per 4 m
    let reps = 2.0 * HALF / 4.0;
    let mut mesh = Mesh::from(Plane3d::default().mesh().size(2.0 * HALF, 2.0 * HALF));
    if let Some(bevy::render::mesh::VertexAttributeValues::Float32x2(uv)) = mesh.attribute_mut(Mesh::ATTRIBUTE_UV_0) {
        for t in uv.iter_mut() {
            t[0] *= reps;
            t[1] *= reps;
        }
    }
    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(materials.add(StandardMaterial { base_color_texture: Some(tex), perceptual_roughness: 0.9, ..default() })),
        Transform::from_translation(c),
    ));
    info!("test pad at GTA {PAD_GTA:?}: {} m square", 2.0 * HALF);

    // the test lab's props (kerbs, ramp, dock and pool, slope, the car's bumps): one collision set
    // and one flat-shaded mesh from the same triangles
    let props = Props::build();
    let verts: Vec<[f32; 3]> = props.0.iter().flatten().map(|v| v.to_array()).collect();
    let tris: Vec<[u32; 3]> = (0..props.0.len() as u32).map(|i| [3 * i, 3 * i + 1, 3 * i + 2]).collect();
    city.collision.insert(PROPS_KEY, ChunkCollision::new(&verts, &tris));
    let normals: Vec<[f32; 3]> = props.0.iter().flat_map(|[a, b, c]| [(*b - *a).cross(*c - *a).normalize_or_zero().to_array(); 3]).collect();
    let mut mesh = Mesh::new(bevy::render::mesh::PrimitiveTopology::TriangleList, bevy::asset::RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, verts);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.75, 0.55, 0.35), perceptual_roughness: 0.9, ..default() })),
        Transform::default(),
    ));
}
