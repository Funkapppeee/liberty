//! Time of day and weather from the game's `pc/data/timecyc.dat`.
//!
//! Sections `////////// EXTRASUNNY`, `SUNNY`, … each hold 11 keyframes (`//Midnight`, `//5AM`,
//! `//6AM`, `//7AM`, `//9AM`, `//Midday`, `//6PM`, `//7PM`, `//8PM`, `//9PM`, `//10PM`) of 134
//! numbers. Columns used here (per the file header + FusionShaders' TimeCycFormater):
//! 0–2 ambient0 RGB, 3–5 ambient1 RGB, 6–8 directional light RGB, 9 unused, 10 film grain,
//! 11–13 sky bottom RGB (the fog colour), 14 fog density, and for the sky dome (sky.rs): 64–66 sky
//! colour, 67–69 horizon colour, 70–72 east horizon colour, 77 horizon height, 78 horizon
//! brightness, 103–105 sun colour, 108 stars brightness (73–102 are the clouds, 110–115 the moon),
//! 15–17 sun core RGB, 21 sun size, 23 far clip, 24 fog start, 31–34 water RGBA, 35 exposure,
//! 36 bloom threshold, 38 bloom intensity, 39–41 colour correct RGB, 42–44 colour add RGB,
//! 45 desaturation, 46 contrast, 47 gamma (48–50 the same three for the distance).
//! The sun's path isn't in the file: it rises at 5:30 and sets at 21:30 (see `sun_height`), with
//! the table's moonlight from the opposite side at night.

use std::path::Path;

use anyhow::{Context, Result};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;

const KEY_HOURS: [f32; 11] = [0.0, 5.0, 6.0, 7.0, 9.0, 12.0, 18.0, 19.0, 20.0, 21.0, 22.0];

pub struct Sky {
    pub sky: Vec3,
    pub horizon: Vec3,
    pub horizon_east: Vec3,
    pub horizon_height: f32,
    pub horizon_brightness: f32,
    pub sun_color: Vec3,
    pub stars: f32,
    pub brightness: f32,
    /// Clouds. Read from the game itself: the sky shader's parameter block as it sat in memory
    /// during the session (SUNNY 10:18, `re/find_sky_consts.py` and the threshold search): in the
    /// effect's own order, a 4-number colour = columns 81-83 (+1.0), a 4-number colour = 73-75
    /// (+1.0), threshold = 85, bias = 86, fade-out = 94, three unknowns (2, 3, 0.9), the top
    /// layer's bias / detail / threshold / height = 95-98, a colour (1.251, 1.2, 1.2) = columns
    /// 99-101 PLUS 73-75, an unknown 1.14, shadow strength = 84, then 0.132 (87), 0.68 (88),
    /// horizon brightness (78), and "thickness, edge smooth, detail scale, detail strength" =
    /// (92, 93, 90, 91). So: main cloud colour 81-83, sun-side tint 73-75, top layer colour
    /// 99-101 + 73-75. Still inferred: 88 as the in-scattering range (it equals visualSettings'
    /// default), 87 as the shadow offset, and 106 as how much cloud shows.
    pub cloud_color: Vec3,
    pub sunset_color: Vec3,
    pub top_color: Vec3,
    /// threshold, bias, shadow strength, thickness
    pub cloud: Vec4,
    /// detail scale, detail strength, shadow offset, fade-out at the horizon
    pub cloud2: Vec4,
    pub detail_offset: f32,
    /// top layer: bias, detail, threshold, height
    pub top: Vec4,
    pub cloud_edge: f32,
    pub cloud_inscatter: f32,
    pub cloud_amount: f32,
}

#[derive(Resource)]
pub struct TimeCycle {
    /// weather name → 11 keyframe rows.
    weathers: Vec<(String, Vec<Vec<f32>>)>,
    pub weather: usize,
    /// A second weather mixed in, and how much of it (0..1). IV is usually part-way between two
    /// weathers: the session's 03:18 frame was 72.5% SUNNY_WINDY + 27.5% CLOUDY on every
    /// grading number read from memory. `LIBERTY_WEATHER_MIX=CLOUDY:0.275`.
    pub mix: Option<(usize, f32)>,
    /// Game hour 0..24.
    pub hour: f32,
    /// Game hours per real second (GTA IV: 1 game minute = 2 real seconds).
    pub rate: f32,
    pub water: Option<Handle<StandardMaterial>>,
}

impl TimeCycle {
    pub fn load(root: &Path) -> Result<TimeCycle> {
        let text = std::fs::read_to_string(root.join("pc/data/timecyc.dat")).context("timecyc.dat")?;
        let mut weathers: Vec<(String, Vec<Vec<f32>>)> = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            if let Some(name) = l.strip_prefix("//////////") {
                weathers.push((name.trim().to_string(), Vec::new()));
            } else if !l.is_empty() && !l.starts_with("//") {
                let row: Vec<f32> = l.split_whitespace().filter_map(|v| v.parse().ok()).collect();
                if row.len() > 36 {
                    if let Some(w) = weathers.last_mut() {
                        w.1.push(row);
                    }
                }
            }
        }
        weathers.retain(|(_, rows)| rows.len() == KEY_HOURS.len());
        anyhow::ensure!(!weathers.is_empty(), "no complete weather sections in timecyc.dat");
        let hour = std::env::var("LIBERTY_HOUR").ok().and_then(|h| h.parse().ok()).unwrap_or(13.0);
        let weather = std::env::var("LIBERTY_WEATHER")
            .ok()
            .and_then(|w| weathers.iter().position(|(n, _)| n.eq_ignore_ascii_case(&w)))
            .unwrap_or(0);
        let mix = std::env::var("LIBERTY_WEATHER_MIX").ok().and_then(|m| {
            let (name, share) = m.split_once(':')?;
            Some((weathers.iter().position(|(n, _)| n.eq_ignore_ascii_case(name))?, share.parse::<f32>().ok()?.clamp(0.0, 1.0)))
        });
        Ok(TimeCycle { weathers, weather, mix, hour, rate: 1.0 / 120.0, water: None })
    }

    pub fn weather_names(&self) -> impl Iterator<Item = &str> {
        self.weathers.iter().map(|w| w.0.as_str())
    }

    pub fn weather_name(&self) -> &str {
        &self.weathers[self.weather].0
    }

    /// Column `c` interpolated at the current hour (wrapping 22 → 24/0).
    fn value(&self, c: usize) -> f32 {
        let a = self.value_of(self.weather, c);
        match self.mix {
            Some((other, share)) => a + (self.value_of(other, c) - a) * share,
            None => a,
        }
    }

    fn value_of(&self, weather: usize, c: usize) -> f32 {
        let rows = &self.weathers[weather].1;
        let h = self.hour.rem_euclid(24.0);
        let n = KEY_HOURS.len();
        let (mut i, mut j, mut t) = (n - 1, 0, 0.0);
        for k in 0..n {
            let (a, b) = (KEY_HOURS[k], if k + 1 < n { KEY_HOURS[k + 1] } else { 24.0 });
            if h >= a && h < b {
                i = k;
                j = (k + 1) % n;
                t = (h - a) / (b - a);
                break;
            }
        }
        let (va, vb) = (rows[i].get(c).copied().unwrap_or(0.0), rows[j].get(c).copied().unwrap_or(0.0));
        va + (vb - va) * t
    }

    fn rgb(&self, c: usize) -> Color {
        Color::srgb(self.value(c) / 255.0, self.value(c + 1) / 255.0, self.value(c + 2) / 255.0)
    }

    /// What the lit picture is multiplied by (see `apply_time`): Exposure x MidGreyValue / LumMin.
    pub fn exposure_k(&self) -> f32 {
        self.value(35) * self.value(37) / self.value(53).max(0.01)
    }

    /// What the sky dome needs (sky.rs). Colours are stored 0..1 here, not 0..255.
    pub fn sky(&self) -> Sky {
        let v3 = |c: usize| Vec3::new(self.value(c), self.value(c + 1), self.value(c + 2));
        Sky {
            sky: v3(64),
            horizon: v3(67),
            horizon_east: v3(70),
            horizon_height: self.value(77),
            horizon_brightness: self.value(78),
            sun_color: v3(103),
            stars: self.value(108),
            // the sky is lit by SkyLightMult and exposed like everything else
            brightness: self.value(60) * self.exposure_k(),
            cloud_color: v3(81),
            sunset_color: v3(73),
            top_color: v3(99) + v3(73),
            cloud: Vec4::new(self.value(85), self.value(86), self.value(84), self.value(92)),
            cloud2: Vec4::new(self.value(90), self.value(91), self.value(87), self.value(94)),
            cloud_edge: self.value(93),
            cloud_inscatter: self.value(88),
            // how much of the sky our own clouds cover. STAND-IN mapping from two of the table's
            // numbers: CloudAlpha (56: 77 on a normal day, 217 in rain) and 106 (1.0 in SUNNY,
            // 0.164 in EXTRASUNNY). About a quarter of the sky in SUNNY, most of it in rain.
            cloud_amount: (0.30 + 0.60 * ((self.value(56) / 255.0 - 0.30) / 0.55).clamp(0.0, 1.0)) * (0.55 + 0.45 * self.value(106).min(1.0)),
            detail_offset: self.value(107),
            top: Vec4::new(self.value(95), self.value(96), self.value(97), self.value(98)),
        }
    }

    /// Is the directional light the sun (true) or the moon?
    pub fn sun_up(&self) -> bool {
        self.sun_height() > -0.05
    }

    /// Street lamps and car lights are on. From the real game's picture (session of 1 Oct 2026):
    /// lamps unlit against the sky at 19:55, lit at 20:03; lit at 06:08 and 06:12, none lit by
    /// 06:35 (the morning times are +-5 minutes: the clock was extrapolated there). So on at
    /// 20:00, and off at 06:30 as the middle of what was seen.
    pub fn is_dark(&self) -> bool {
        let h = self.hour.rem_euclid(24.0);
        !(6.5..20.0).contains(&h)
    }

    /// Sun height: 0 at 5:30 and 21:30, 1 at 13:30, negative at night. STAND-IN, but bounded by
    /// the table: its directional light is sunlight from the 6AM to the 9PM keyframe and the
    /// blue-green moon at 10PM, midnight and 5AM.
    fn sun_height(&self) -> f32 {
        let h = self.hour.rem_euclid(24.0);
        ((h - 5.5) / 16.0 * std::f32::consts::PI).sin()
    }
}

/// Lit windows and signs: Always glows steadily, Night fades in after sunset. Only touches the
/// materials when the strength actually changes (each touch re-uploads them).
pub fn apply_glow(
    tc: Res<TimeCycle>,
    cache: Option<Res<crate::stream::Cache>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut last: Local<(f32, usize, f32)>,
) {
    let _slow = crate::SlowGuard::new("timecyc::apply_glow");
    let Some(cache) = cache else { return };
    let dark = (-(tc.sun_height()) * 4.0 + 0.3).clamp(0.0, 1.0);
    // Self-lit surfaces are part of the lit picture in IV, so they get the same exposure as
    // everything else (Bevy doesn't expose emissive light by itself). IV's self-lit shaders output
    // texture x vertex colour x the material's own emissiveMultiplier (the night ones also x a
    // day/night factor): the multiplier is read from each material. Not done: the vertex colour,
    // and IV's plain gta_emissive is not lit by anything else, ours still is.
    let k = tc.exposure_k();
    let changed = (dark - last.0).abs() > 0.02 || cache.glow.len() != last.1 || (k - last.2).abs() > 0.02 * k;
    if !changed {
        return;
    }
    *last = (dark, cache.glow.len(), k);
    if std::env::var("LIBERTY_LIGHT_DEBUG").is_ok() {
        let mut by: std::collections::BTreeMap<String, usize> = Default::default();
        for (_, glow) in &cache.glow {
            *by.entry(format!("{glow:?}")).or_default() += 1;
        }
        info!("glow: {} self-lit materials, k {k:.3}, dark {dark:.2}: {by:?}", cache.glow.len());
    }
    for (id, glow) in &cache.glow {
        let strength = match glow {
            crate::assets::Glow::Always(m) => *m as f32 / 100.0 * k,
            crate::assets::Glow::Night(m) => *m as f32 / 100.0 * k * dark,
            crate::assets::Glow::None => 0.0,
        };
        if let Some(m) = materials.get_mut(*id) {
            m.emissive = LinearRgba::rgb(strength, strength, strength);
        }
    }
}

/// T: +1 hour · Y: next weather. Time flows at GTA pace.
pub fn advance_time(time: Res<Time>, keys: Res<ButtonInput<KeyCode>>, mut tc: ResMut<TimeCycle>) {
    tc.hour = (tc.hour + time.delta_secs() * tc.rate).rem_euclid(24.0);
    if keys.just_pressed(KeyCode::KeyT) {
        tc.hour = (tc.hour + 1.0).rem_euclid(24.0);
    }
    if keys.just_pressed(KeyCode::KeyY) {
        tc.weather = (tc.weather + 1) % tc.weathers.len();
        info!("weather: {}", tc.weather_name());
    }
}

/// Drive sun, ambient, sky/fog and water from the table.
pub fn apply_time(
    tc: Res<TimeCycle>,
    mut sun: Query<(&mut DirectionalLight, &mut Transform)>,
    mut ambient: ResMut<AmbientLight>,
    mut clear: ResMut<ClearColor>,
    mut fog: Query<&mut DistanceFog>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut look: Query<(&mut crate::grade::Grade, &mut bevy::core_pipeline::bloom::Bloom, &mut bevy::render::camera::Exposure)>,
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    cams: Query<Entity, With<Camera3d>>,
    mut ambient_cube: Local<Option<(Handle<Image>, [f32; 6])>>,
) {
    let _slow = crate::SlowGuard::new("timecyc::apply_time");
    // IV's brightness: the picture is (light) x Exposure x MidGreyValue / adapted luminance, and
    // the adapted luminance is the frame's average held between LumMin and LumMax.
    // STAND-IN: we hold it at LumMin (no auto-brightness yet). By the table's own numbers a sunlit
    // street sits about at LumMin, so IV only dims further when the view is mostly bright sky.
    let k = tc.value(35) * tc.value(37) / tc.value(53).max(0.01);
    let raw = |c: usize, mult: f32| LinearRgba::new(tc.value(c) / 255.0 * mult, tc.value(c + 1) / 255.0 * mult, tc.value(c + 2) / 255.0 * mult, 1.0);
    // the look: IV's own grading numbers for this hour and weather, used the way its shader does
    for (mut g, mut bloom, mut exposure) in &mut look {
        // Bevy multiplies light by 1 / (1.2 x 2^ev100): make that equal k
        exposure.ev100 = -(1.2 * k).log2();
        if std::env::var("LIBERTY_NO_GRADE").is_ok() {
            *g = crate::grade::Grade::NEUTRAL;
            bloom.intensity = 0.0;
            continue;
        }
        g.tint = Vec4::new(tc.value(39) / 255.0, tc.value(40) / 255.0, tc.value(41) / 255.0, 0.0);
        // Saturation = 2 x the table's Desaturation: the same "half means unchanged" scale as
        // ColourCorrect. Measured, not read from the shader's settings (those weren't found in
        // memory): the real clear sky at SUNNY 12:28 is (0.465, 0.66, 0.788) from a sky colour
        // of (0.682, 0.820, 0.911), which fits saturation 1.5 (table 0.752) to 2% on all three
        // channels and cannot be reached with 0.752. LIBERTY_SAT overrides the multiplier.
        let sat = std::env::var("LIBERTY_SAT").ok().and_then(|v| v.parse().ok()).unwrap_or(2.0);
        g.params = Vec4::new(tc.value(45) * sat, tc.value(47), 0.0, 0.0);
        // STAND-IN: how strong the bloom is follows the table (1.2 to 5), but IV adds a blurred
        // copy above a threshold in its own light units; here it scales Bevy's bloom instead
        // IV's bloom ADDS a blurred copy of the picture: (blur - BloomThreshold) x BloomIntensity x
        // 0.25 (its post shader). At midday that is 0.72 of the picture again, which is a large
        // part of how bright IV looks (checked against a real frame at 11:50: ours was 1.6 to 2
        // times too dark without it). STAND-IN: the blur itself is Bevy's, wider than IV's.
        // Bevy adds its blur in about 8 nested steps, each scaled by `intensity`, so a flat picture
        // comes out (1 + intensity)^8 times brighter: pick the intensity that makes that
        // 1 + 0.25 x BloomIntensity.
        bloom.intensity = (1.0 + 0.25 * tc.value(38)).powf(1.0 / 8.0) - 1.0;
        bloom.low_frequency_boost = 0.0;
        bloom.high_pass_frequency = 1.0;
        bloom.composite_mode = bevy::core_pipeline::bloom::BloomCompositeMode::Additive;
        bloom.prefilter.threshold = tc.value(36);
        bloom.prefilter.threshold_softness = 0.0;
    }
    let height = tc.sun_height();
    let day = height.clamp(0.0, 1.0);
    let brightness = 0.06 + 0.94 * day.powf(0.5);

    // sun sweeps east → south → west (northern hemisphere: GTA south = Bevy +Z); moon opposite
    let h = tc.hour.rem_euclid(24.0);
    let azimuth = (h - 13.5) / 16.0 * std::f32::consts::PI;
    let dir = if height > -0.05 {
        // up to 72 degrees high: in the real game at 11:50 the sun stands about 68 degrees up
        // (Niko's short shadow at Star Junction), where the old path had it at 49
        let elev = (height.max(0.03) * 72.0f32).to_radians();
        Vec3::new(azimuth.sin() * elev.cos(), elev.sin(), azimuth.cos() * elev.cos())
    } else {
        Vec3::new(-azimuth.sin(), 0.8, -azimuth.cos()).normalize()
    };
    // the table's directional light (sun by day, its blue-green moon by night) x DirLightMult.
    // Bevy's diffuse is light / pi, so pi x the multiplier gives IV's colour x light.
    let (light_col, lux) = (Color::LinearRgba(raw(6, 1.0)), std::f32::consts::PI * tc.value(57));
    for (mut l, mut t) in &mut sun {
        l.color = light_col;
        l.illuminance = lux;
        *t = Transform::from_translation(dir).looking_at(Vec3::ZERO, Vec3::Y);
    }

    // ambient. IV's lighting shader (deferred_lighting): ambient = Amb0 + Amb1 x (0 on surfaces
    // facing up, 0.5 on walls, 1 facing down), times the model's baked per-vertex shading. Done
    // with a tiny cube map of exactly that, used as the camera's environment light (so the ground
    // gets Amb0 alone: the teal shade of the real game's streets). The baked shading isn't applied.
    let (a0, a1) = (raw(0, tc.value(58)), raw(3, tc.value(59)));
    ambient.brightness = 0.0;
    let want = [a0.red, a0.green, a0.blue, a1.red, a1.green, a1.blue];
    let stale = ambient_cube.as_ref().is_none_or(|(_, have)| have.iter().zip(&want).any(|(h, w)| (h - w).abs() > 0.01 * w.abs().max(0.1)));
    if stale {
        const N: usize = 8;
        const SCALE: f32 = 16.0; // colours go up to about 9: stored / 16 in 8 bits, given back by `intensity`
        let mut data = Vec::with_capacity(6 * N * N * 4);
        for face in 0..6 {
            for y in 0..N {
                for x in 0..N {
                    let (u, v) = ((x as f32 + 0.5) / N as f32 * 2.0 - 1.0, (y as f32 + 0.5) / N as f32 * 2.0 - 1.0);
                    // only how far up the direction points matters: faces 2 / 3 are +Y / -Y
                    let up = match face {
                        2 => 1.0,
                        3 => -1.0,
                        _ => -v,
                    } / (1.0 + u * u + v * v).sqrt();
                    let f = 0.5 - 0.5 * up;
                    for c in 0..3 {
                        data.push((((want[c] + want[c + 3] * f) / SCALE).clamp(0.0, 1.0) * 255.0) as u8);
                    }
                    data.push(255);
                }
            }
        }
        let handle = match ambient_cube.as_ref() {
            Some((h, _)) => h.clone(),
            None => {
                let mut image = Image::new(
                    bevy::render::render_resource::Extent3d { width: N as u32, height: N as u32, depth_or_array_layers: 6 },
                    bevy::render::render_resource::TextureDimension::D2,
                    data.clone(),
                    bevy::render::render_resource::TextureFormat::Rgba8Unorm,
                    bevy::asset::RenderAssetUsages::default(),
                );
                image.texture_view_descriptor = Some(bevy::render::render_resource::TextureViewDescriptor { dimension: Some(bevy::render::render_resource::TextureViewDimension::Cube), ..default() });
                let h = images.add(image);
                for cam in &cams {
                    commands.entity(cam).insert(bevy::pbr::environment_map::EnvironmentMapLight { diffuse_map: h.clone(), specular_map: h.clone(), intensity: SCALE, ..default() });
                }
                h
            }
        };
        if let Some(image) = images.get_mut(&handle) {
            image.data = Some(data);
        }
        *ambient_cube = Some((handle, want));
    }

    // fog (and the clear colour behind the dome): the sky dome's own colour at the horizon, so
    // far buildings fade into the sky they stand against. STAND-IN: IV has its own fog colour
    // (columns 11-13) and density (14), whose units aren't decoded.
    let s = tc.sky();
    let h = (s.sky + (s.horizon + s.horizon_east) * 0.5 * s.horizon_brightness) * s.brightness;
    let sky = Color::LinearRgba(LinearRgba::new(h.x.min(1.0), h.y.min(1.0), h.z.min(1.0), 1.0));
    if std::env::var("LIBERTY_LIGHT_DEBUG").is_ok() {
        info!("light {:.2}h: k {k:.3}, sun x{:.1}, ambient {:?}, sky {:?}", tc.hour, tc.value(57), a0, sky.to_linear());
    }
    clear.0 = sky;
    let fog_start = tc.value(24).max(40.0) * 12.0;
    let far = tc.value(23).max(500.0) * 2.5;
    for mut f in &mut fog {
        f.color = sky;
        f.falloff = FogFalloff::Linear { start: fog_start, end: far.max(fog_start + 500.0) };
    }

    if let Some(w) = tc.water.as_ref().and_then(|h| materials.get_mut(h)) {
        // the table's water colour is the shader's dark base (reflections come on top in GTA);
        // unlit, it reads black — keep our tuned teal and dim it with the daylight
        let c = LinearRgba::rgb(0.10, 0.29, 0.36) * (0.2 + 0.8 * brightness);
        w.base_color = Color::LinearRgba(LinearRgba { alpha: 0.92, ..c });
    }
}

