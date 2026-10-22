//! The on-screen HUD, from the game's own art: the radar (map tiles from `radar.img`, ring from
//! `pc/textures/hud.wtd`), the weapon icon (the `icon` texture in each weapon's pack) with the
//! rounds left, and the reticule while aiming (`hud_crosshair`, a quarter arc mirrored four ways).
//!
//! The radar is drawn on the CPU into one small image each frame: the map under the player,
//! turned so the camera's heading is up, inside the ring, with the health arc on the left.

use std::collections::HashMap;
use std::path::PathBuf;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use rage::wtd::{self, Format, Texture};
use rage::{Img, Key, Resource as Rsc};

use crate::Fly;
use crate::assets;
use crate::ped::Player;
use crate::weapon::{Armed, Weapons};

/// Radar image size, pixels (shown larger).
const N: usize = 160;
/// Metres per radar pixel: about 100 m from the middle to the ring.
const SCALE: f32 = 1.6;
/// The radar map: 12 x 12 tiles of 500 m, tile 0 at the north-west corner (-3000, +3000); tile
/// `row * 12 + col` is `radar<NN>.wtd`. Checked by assembling all 63 tiles into the whole map.
const TILE_M: f32 = 500.0;
const MAP_ORIGIN: (f32, f32) = (-3000.0, 3000.0);

/// Where the HUD's art is read from.
#[derive(Resource)]
pub struct HudSource {
    pub root: PathBuf,
    pub key: Key,
}

struct Rgba {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Rgba {
    /// Nearest pixel at u, v in 0..1.
    fn at(&self, u: f32, v: f32) -> [u8; 4] {
        let x = ((u * self.w as f32) as usize).min(self.w - 1);
        let y = ((v * self.h as f32) as usize).min(self.h - 1);
        let i = (y * self.w + x) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }
}

fn rgba(t: &Texture) -> Option<Rgba> {
    let (w, h) = (t.width as usize, t.height as usize);
    let mut px = Vec::with_capacity(w * h * 4);
    match t.format {
        Format::Argb8 => t.level0().chunks_exact(4).for_each(|p| px.extend_from_slice(&[p[2], p[1], p[0], p[3]])),
        Format::L8 => t.level0().iter().for_each(|&l| px.extend_from_slice(&[l, l, l, 255])),
        dxt => {
            let mut out = vec![0u32; w * h];
            match dxt {
                Format::Dxt1 => texture2ddecoder::decode_bc1(t.level0(), w, h, &mut out),
                Format::Dxt3 => texture2ddecoder::decode_bc2(t.level0(), w, h, &mut out),
                _ => texture2ddecoder::decode_bc3(t.level0(), w, h, &mut out),
            }
            .ok()?;
            // the decoder packs pixels as 0xAARRGGBB
            out.iter().for_each(|p| px.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]));
        }
    }
    Some(Rgba { w, h, px })
}

fn image(w: usize, h: usize, px: Vec<u8>) -> Image {
    Image::new(
        Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        px,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    )
}

#[derive(Resource)]
pub struct Hud {
    tiles_img: Option<Img>,
    /// Decoded radar tiles by index (None: that tile is open water, there is no file).
    tiles: HashMap<usize, Option<Rgba>>,
    ring_back: Option<Rgba>,
    ring_front: Option<Rgba>,
    radar: Handle<Image>,
    fist: Option<Handle<Image>>,
    icons: HashMap<usize, Handle<Image>>,
}

#[derive(Component)]
pub struct WeaponIcon;
#[derive(Component)]
pub struct AmmoText;
#[derive(Component)]
pub struct Reticule;

pub fn setup(mut commands: Commands, src: Res<HudSource>, mut images: ResMut<Assets<Image>>) {
    let art: HashMap<String, Texture> = std::fs::read(src.root.join("pc/textures/hud.wtd"))
        .ok()
        .and_then(|d| Rsc::load(&d).ok())
        .and_then(|r| wtd::read(&r).ok())
        .map(|ts| ts.into_iter().map(|t| (t.name.to_ascii_lowercase(), t)).collect())
        .unwrap_or_default();
    if art.is_empty() {
        warn!("hud: pc/textures/hud.wtd not read; the HUD draws without the game's art");
    }
    let radar = images.add(image(N, N, vec![0; N * N * 4]));
    let hud = Hud {
        tiles_img: Img::open(&src.root.join("pc/models/cdimages/radar.img"), &src.key).ok(),
        tiles: HashMap::new(),
        ring_back: art.get("radarringback").and_then(rgba),
        ring_front: art.get("radarringfront").and_then(rgba),
        radar: radar.clone(),
        fist: art.get("fist_icon").map(|t| images.add(assets::gpu_image_ui(t))),
        icons: HashMap::new(),
    };

    // radar, bottom left
    commands.spawn((ImageNode::new(radar), Node { position_type: PositionType::Absolute, left: Val::Px(28.0), bottom: Val::Px(28.0), width: Val::Px(210.0), height: Val::Px(210.0), ..default() }));
    // weapon icon and rounds, top right
    commands.spawn((
        WeaponIcon,
        ImageNode { image: hud.fist.clone().unwrap_or_default(), ..default() },
        Node { position_type: PositionType::Absolute, right: Val::Px(30.0), top: Val::Px(70.0), width: Val::Px(170.0), height: Val::Px(85.0), ..default() },
    ));
    commands.spawn((
        AmmoText,
        Text::new(""),
        TextFont { font_size: 26.0, ..default() },
        TextColor(Color::srgb(0.95, 0.95, 0.95)),
        Node { position_type: PositionType::Absolute, right: Val::Px(40.0), top: Val::Px(150.0), ..default() },
    ));
    // the reticule: the game's quarter arc, mirrored into a circle, with a dot in the middle
    if let Some(q) = art.get("hud_crosshair").and_then(rgba) {
        let (w, h) = (q.w * 2, q.h * 2);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (if x < q.w { x } else { w - 1 - x }, if y < q.h { y } else { h - 1 - y });
                let s = (sy * q.w + sx) * 4;
                px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&q.px[s..s + 4]);
            }
        }
        for (x, y) in [(w / 2 - 1, h / 2 - 1), (w / 2, h / 2 - 1), (w / 2 - 1, h / 2), (w / 2, h / 2)] {
            px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
        let size = 44.0;
        commands.spawn((
            Reticule,
            ImageNode::new(images.add(image(w, h, px))),
            Visibility::Hidden,
            Node { position_type: PositionType::Absolute, left: Val::Percent(50.0), top: Val::Percent(50.0), margin: UiRect { left: Val::Px(-size / 2.0), top: Val::Px(-size / 2.0), ..default() }, width: Val::Px(size), height: Val::Px(size), ..default() },
        ));
    }
    commands.insert_resource(hud);
}

/// The weapon icon, rounds left, and the reticule.
pub fn weapon_hud(
    armed: Res<Armed>,
    weapons: Res<Weapons>,
    mut hud: ResMut<Hud>,
    mut images: ResMut<Assets<Image>>,
    mut icon: Query<&mut ImageNode, With<WeaponIcon>>,
    mut ammo: Query<&mut Text, With<AmmoText>>,
    mut reticule: Query<&mut Visibility, With<Reticule>>,
) {
    let _slow = crate::SlowGuard::new("hud::weapon_hud");
    if let Ok(mut v) = reticule.single_mut() {
        *v = if armed.aiming { Visibility::Inherited } else { Visibility::Hidden };
    }
    if !armed.is_changed() {
        return;
    }
    let handle = match armed.weapon {
        Some(i) => {
            if !hud.icons.contains_key(&i) {
                if let Some(t) = weapons.0[i].icon() {
                    hud.icons.insert(i, images.add(assets::gpu_image_ui(t)));
                }
            }
            hud.icons.get(&i).cloned()
        }
        None => hud.fist.clone(),
    };
    if let (Ok(mut node), Some(h)) = (icon.single_mut(), handle) {
        if node.image != h {
            node.image = h;
        }
    }
    if let Ok(mut text) = ammo.single_mut() {
        let s = match crate::weapon::ammo(&armed, &weapons) {
            Some((_, left, _, true)) => format!("{left}  reloading"),
            Some((_, left, size, false)) => format!("{left} / {size}"),
            None => String::new(),
        };
        if text.0 != s {
            text.0 = s;
        }
    }
}

/// Draw the radar: the map around the player with the camera's heading up, the ring, the health
/// arc (always full: nothing hurts Niko yet), and his arrow.
pub fn radar(
    mut hud: ResMut<Hud>,
    mut images: ResMut<Assets<Image>>,
    cam: Query<(&GlobalTransform, &Fly)>,
    player: Query<&GlobalTransform, With<Player>>,
) {
    let _slow = crate::SlowGuard::new("hud::radar");
    let Ok((cam, fly)) = cam.single() else { return };
    // on foot or in a car the map is centred on Niko, flying on the camera
    let centre = if fly.walking || fly.driving.is_some() { player.single().map(|p| p.translation()).unwrap_or(cam.translation()) } else { cam.translation() };
    let (px, py) = (centre.x, -centre.z);
    let f = cam.forward().as_vec3();
    let fwd = Vec2::new(f.x, -f.z).normalize_or(Vec2::Y);
    let right = Vec2::new(fwd.y, -fwd.x);
    let him = player.single().map(|p| p.forward().as_vec3()).unwrap_or(f);
    let him = Vec2::new(him.x, -him.z).normalize_or(fwd);

    // the tiles under the radar's reach
    let reach = N as f32 * SCALE;
    let tile_of = |x: f32, y: f32| -> Option<usize> {
        let (c, r) = (((x - MAP_ORIGIN.0) / TILE_M).floor(), ((MAP_ORIGIN.1 - y) / TILE_M).floor());
        ((0.0..12.0).contains(&c) && (0.0..12.0).contains(&r)).then(|| r as usize * 12 + c as usize)
    };
    for (dx, dy) in [(-reach, -reach), (reach, -reach), (-reach, reach), (reach, reach), (0.0, 0.0)] {
        if let Some(i) = tile_of(px + dx, py + dy) {
            if !hud.tiles.contains_key(&i) {
                let tile = hud.tiles_img.as_ref().and_then(|img| {
                    let e = img.find(&format!("radar{i:02}.wtd"))?;
                    let res = Rsc::load(&img.read(e).ok()?).ok()?;
                    wtd::read(&res).ok()?.first().and_then(rgba)
                });
                hud.tiles.insert(i, tile);
            }
        }
    }

    let mut out = vec![0u8; N * N * 4];
    let half = N as f32 / 2.0;
    const WATER: [u8; 4] = [96, 125, 150, 255];
    let over = |dst: &mut [u8], src: [u8; 4]| {
        let a = src[3] as f32 / 255.0;
        for k in 0..3 {
            dst[k] = (src[k] as f32 * a + dst[k] as f32 * (1.0 - a)) as u8;
        }
        dst[3] = dst[3].max(src[3]);
    };
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 + 0.5 - half, y as f32 + 0.5 - half);
            let r = (dx * dx + dy * dy).sqrt() / half;
            let dst = &mut out[(y * N + x) * 4..(y * N + x) * 4 + 4];
            let (u, v) = ((x as f32 + 0.5) / N as f32, (y as f32 + 0.5) / N as f32);
            if let Some(b) = &hud.ring_back {
                over(dst, b.at(u, v));
            }
            if r < 0.84 {
                // the map: screen up is the camera's heading
                let w = Vec2::new(px, py) + right * (dx * SCALE) - fwd * (dy * SCALE);
                let (tx, ty) = ((w.x - MAP_ORIGIN.0) / TILE_M, (MAP_ORIGIN.1 - w.y) / TILE_M);
                let c = tile_of(w.x, w.y).and_then(|i| hud.tiles.get(&i)).and_then(|t| t.as_ref()).map_or(WATER, |t| t.at(tx.fract(), ty.fract()));
                dst.copy_from_slice(&[c[0], c[1], c[2], 235]);
                // his arrow: a small white triangle in the middle, pointing the way he faces
                let p = Vec2::new(dx, -dy);
                let ahead = Vec2::new(him.dot(right), him.dot(fwd));
                let (along, across) = (p.dot(ahead), p.dot(Vec2::new(ahead.y, -ahead.x)));
                if along > -4.0 && along < 6.0 && across.abs() < (6.0 - along) * 0.45 {
                    dst.copy_from_slice(&[255, 255, 255, 255]);
                }
            } else if r > 0.875 && r < 0.965 && dx < 0.0 {
                // health: the left half of the ring's channel, green
                over(dst, [70, 150, 60, 255]);
            }
            if let Some(fr) = &hud.ring_front {
                over(dst, fr.at(u, v));
            }
        }
    }
    if let Some(img) = images.get_mut(&hud.radar) {
        img.data = Some(out);
    }
}
