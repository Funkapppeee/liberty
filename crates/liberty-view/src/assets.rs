//! Turning rage data into Bevy meshes and GPU images (pure functions, safe off the main thread).

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use rage::drawable;
use rage::wtd::{Format, Texture};

/// A texture by pack + name (`@<model hash>` pack = embedded in the model).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub pack: String,
    pub name: String,
}

/// The textures of one material: colour, and on gta_normal* / gta_spec* shaders a normal map and
/// a specular map.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct MatKey {
    pub diffuse: Option<ImageKey>,
    pub normal: Option<ImageKey>,
    pub specular: Option<ImageKey>,
}

/// Self-lit surfaces, from the material's GTA shader name: gta_emissive* / …_emissive glow all
/// the time (signs, lamps), …emissivenight only after dark (lit windows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Glow {
    None,
    /// The number is the material's own emissiveMultiplier x 100.
    Always(u16),
    Night(u16),
}

impl Glow {
    pub fn from_shader(name: &str, multiplier: f32) -> Glow {
        let m = (multiplier.clamp(0.0, 600.0) * 100.0) as u16;
        if name.contains("emissivenight") {
            Glow::Night(m)
        } else if name.contains("emissive") {
            Glow::Always(m)
        } else {
            Glow::None
        }
    }
}

/// GTA is Z-up; Bevy is Y-up. (x, y, z) → (x, z, -y) is a pure rotation, so winding is kept.
pub fn to_bevy([x, y, z]: [f32; 3]) -> [f32; 3] {
    [x, z, -y]
}

/// A colour texture (sRGB).
/// A colour texture for the 3D world, read as stored. IV lights its textures without converting
/// them from screen values first (no sRGB decode), so neither do we: see `timecyc::apply_time`.
pub fn gpu_image(t: &Texture) -> Image {
    gpu_image_as(t, false)
}

/// A colour texture for the HUD (drawn by Bevy's UI, which does expect sRGB).
pub fn gpu_image_ui(t: &Texture) -> Image {
    gpu_image_as(t, true)
}

/// A data texture, read as stored: normal maps.
pub fn gpu_image_linear(t: &Texture) -> Image {
    gpu_image_as(t, false)
}

fn gpu_image_as(t: &Texture, srgb: bool) -> Image {
    let (format, data) = match t.format {
        Format::Dxt1 => (if srgb { TextureFormat::Bc1RgbaUnormSrgb } else { TextureFormat::Bc1RgbaUnorm }, t.data.clone()),
        Format::Dxt3 => (if srgb { TextureFormat::Bc2RgbaUnormSrgb } else { TextureFormat::Bc2RgbaUnorm }, t.data.clone()),
        Format::Dxt5 => (if srgb { TextureFormat::Bc3RgbaUnormSrgb } else { TextureFormat::Bc3RgbaUnorm }, t.data.clone()),
        Format::Argb8 => (if srgb { TextureFormat::Bgra8UnormSrgb } else { TextureFormat::Bgra8Unorm }, t.data.clone()),
        // grey → RGBA so it doesn't come out red (top mip only)
        Format::L8 => (
            if srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm },
            t.level0().iter().flat_map(|&l| [l, l, l, 255]).collect(),
        ),
    };
    let mut image = Image::default();
    image.data = Some(data);
    image.texture_descriptor.size =
        Extent3d { width: t.width as u32, height: t.height as u32, depth_or_array_layers: 1 };
    image.texture_descriptor.dimension = TextureDimension::D2;
    image.texture_descriptor.format = format;
    image.texture_descriptor.mip_level_count = if t.format == Format::L8 { 1 } else { t.levels as u32 };
    image.asset_usage = RenderAssetUsages::RENDER_WORLD;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 16,
        ..default()
    });
    image
}

pub fn build_mesh(m: &drawable::Mesh) -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.iter().map(|&p| to_bevy(p)).collect::<Vec<_>>());
    if m.uvs.len() == m.positions.len() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, m.uvs.clone());
    }
    mesh.insert_indices(Indices::U16(m.indices.clone()));
    // the game's own tangents (normal-mapped geometry): same change of axes, handedness kept
    if m.tangents.len() == m.positions.len() && m.normals.len() == m.positions.len() && m.uvs.len() == m.positions.len() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, m.tangents.iter().map(|&[x, y, z, w]| [x, z, -y, w]).collect::<Vec<_>>());
    }
    if m.normals.len() == m.positions.len() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.iter().map(|&n| to_bevy(n)).collect::<Vec<_>>());
    } else {
        mesh.compute_normals();
    }
    mesh
}

pub fn material(texture: Option<Handle<Image>>, alpha: AlphaMode) -> StandardMaterial {
    StandardMaterial {
        base_color: if texture.is_some() { Color::WHITE } else { Color::linear_rgb(0.6, 0.6, 0.6) },
        base_color_texture: texture,
        alpha_mode: alpha,
        perceptual_roughness: 0.85,
        cull_mode: None,
        double_sided: true,
        ..default()
    }
}

/// Add a shader's normal map and specular map to a material. GTA's spec map is bright where the
/// surface shines: it tints the highlight. Only gta_spec, gta_normal_spec and
/// gta_normal_spec_reflect carry one (liberty shaders <img>).
/// LIBERTY_NO_MAPS turns both off (for before/after shots).
pub fn apply_maps(m: &mut StandardMaterial, normal: Option<Handle<Image>>, specular: Option<Handle<Image>>) {
    if std::env::var("LIBERTY_NO_MAPS").is_ok() {
        return;
    }
    if normal.is_some() {
        m.normal_map_texture = normal;
        // Measured (liberty model, and the maps' own slopes): the stored tangents follow +u, the
        // renderer's cross(N, T) * w points along -v, and the maps' green channel points along +v.
        // So green is flipped. LIBERTY_NO_FLIP_NY leaves it as stored, to compare.
        m.flip_normal_map_y = std::env::var("LIBERTY_NO_FLIP_NY").is_err();
    }
    if specular.is_some() {
        // only a little glossier than the default, and no extra reflectance: Bevy's ambient light
        // has a specular part too, and a strong one shows the map as a pale pattern in the shade
        // (IV's shows in sun glints and reflections)
        m.specular_tint_texture = specular;
        m.perceptual_roughness = m.perceptual_roughness.min(0.6);
    }
}

/// All water quads as one mesh (GTA quads: triangles 0-1-2 and 2-1-3).
pub fn water_mesh(quads: &[rage::water::WaterQuad]) -> Mesh {
    let mut pos = Vec::with_capacity(quads.len() * 4);
    let mut idx = Vec::with_capacity(quads.len() * 6);
    for q in quads {
        let base = pos.len() as u32;
        pos.extend(q.corners.iter().map(|&c| to_bevy(c)));
        idx.extend([0, 1, 2, 2, 1, 3].map(|i| base + i));
    }
    let n = pos.len();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; n]);
    mesh.insert_indices(Indices::U32(idx));
    mesh
}
