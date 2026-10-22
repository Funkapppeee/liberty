// The sky dome. Colour the way IV's sky shader (gta_atmoscatt_clouds) builds it from the time
// cycle: a sky colour, plus a horizon colour (blended between its east and west values by the
// view direction) that fades out with height, then the sun, the stars, the moon, and two layers
// of clouds cut out of noise textures.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

struct Sky {
    sky: vec4<f32>,
    horizon: vec4<f32>,
    horizon_east: vec4<f32>,
    // xyz = direction to the sun, w = 1 while the sun is up
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    // x = horizon height, y = horizon brightness, z = brightness of the whole sky, w = stars
    p: vec4<f32>,
    // x, y = where the sun's disc starts and is full (sky.sun.centreStart / centreEnd),
    // z = how much of the sky has cloud (0..1)
    q: vec4<f32>,
    cloud_color: vec4<f32>,
    sunset_color: vec4<f32>,
    top_color: vec4<f32>,
    // threshold, bias, shadow strength, thickness
    c1: vec4<f32>,
    // edge smooth, detail scale, detail strength, shadow offset
    c2: vec4<f32>,
    // fade-out at the horizon, in-scattering range, detail offset, time (s)
    c3: vec4<f32>,
    // top layer: bias, detail, threshold, height
    top: vec4<f32>,
    // xyz = direction to the moon, w = 1 at night
    moon: vec4<f32>,
}
@group(2) @binding(0) var<uniform> s: Sky;
@group(2) @binding(1) var stars_tex: texture_2d<f32>;
@group(2) @binding(2) var stars_samp: sampler;
@group(2) @binding(3) var perlin_tex: texture_2d<f32>;
@group(2) @binding(4) var perlin_samp: sampler;
@group(2) @binding(5) var detail_tex: texture_2d<f32>;
@group(2) @binding(6) var detail_samp: sampler;
@group(2) @binding(7) var moon_tex: texture_2d<f32>;
@group(2) @binding(8) var moon_samp: sampler;
@group(2) @binding(9) var glow_tex: texture_2d<f32>;
@group(2) @binding(10) var glow_samp: sampler;

// Smooth made-up noise (the game's noise texture is too fine-grained for soft cloud shapes: it
// came out as specks). 0..1.
fn hash2(p: vec2<f32>) -> f32 {
    var q = fract(vec3(p.xyx) * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return fract((q.x + q.y) * q.z);
}

fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    return mix(mix(hash2(i), hash2(i + vec2(1.0, 0.0)), u.x),
               mix(hash2(i + vec2(0.0, 1.0)), hash2(i + vec2(1.0, 1.0)), u.x), u.y);
}

// Four layers, each about twice as fine and half as strong: 0..1, middle near 0.5.
fn fbm(p: vec2<f32>) -> f32 {
    var amp = 0.5;
    var sum = 0.0;
    var q = p;
    for (var i = 0; i < 4; i++) {
        sum += amp * vnoise(q);
        q = vec2(q.x * 1.6 + q.y * 1.2, q.y * 1.6 - q.x * 1.2) + vec2(17.3, 5.1);
        amp *= 0.5;
    }
    return sum / 0.9375;
}

// How thick the cloud is at a spot on the ceiling. Stretched one way so it comes out as streaks
// and wisps; `cover` (0..1) sets how much of the sky has any.
fn cloud_density(p: vec2<f32>, cover: f32) -> f32 {
    let n = fbm(vec2(p.x * 0.6, p.y * 1.0));
    let edge = mix(0.66, 0.34, cover);
    return smoothstep(edge, edge + 0.30, n);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let d = normalize(in.world_position.xyz - view.world_position);
    // IV: horizon amount = (1 - saturate(up * height)) * brightness; colour between east and west by x
    let amount = (1.0 - clamp(d.y * s.p.x, 0.0, 1.0)) * s.p.y;
    let horizon = mix(s.horizon_east.rgb, s.horizon.rgb, d.x * 0.5 + 0.5);
    var col = s.sky.rgb + horizon * amount;

    // sun: IV's disc is a smoothstep on (dot * 0.5 + 0.5) between centreStart and centreEnd
    let toward = dot(d, s.sun_dir.xyz);
    let facing = toward * 0.5 + 0.5;
    let t = clamp((facing - s.q.x) / max(s.q.y - s.q.x, 1e-4), 0.0, 1.0);
    let disc = t * t * (3.0 - 2.0 * t) * s.sun_dir.w;
    let sun = s.sun_color.rgb * disc;
    // IV: the sky is weighted by (1 - sun), the sun added as (sun^4 + sun)
    col = col * (1.0 - clamp(disc, 0.0, 1.0)) + sun * sun * sun * sun + sun;

    // the dome's flat map: the middle is straight up. STAND-IN scale: the real sky shows a
    // handful of broad cloud masses from horizon to horizon, so the noise is spread 4 times wider
    // than one copy across the dome
    let uv = d.xz * 0.125 + 0.5;
    let centre = uv - 0.5;
    let drift = vec2(s.c3.w * 0.0012, s.c3.w * 0.0005);

    // ---- moon: one tile of the game's sheet of phases, on a square facing the camera
    var moon_alpha = 0.0;
    if s.moon.w > 0.0 {
        let m = s.moon.xyz;
        let mx = normalize(cross(vec3(0.0, 1.0, 0.0), m));
        let my = cross(m, mx);
        let rel = d - m;
        let muv = vec2(dot(mx, rel), -dot(my, rel)) / 0.12 + 0.5;
        if dot(d, m) > 0.0 && muv.x > 0.0 && muv.x < 1.0 && muv.y > 0.0 && muv.y < 1.0 {
            // sheet: 4 across, 6 down; the full moon is the second in the fourth row
            let tile = (vec2(1.0, 3.0) + muv) / vec2(4.0, 6.0);
            let mt = textureSampleLevel(moon_tex, moon_samp, tile, 0.0);
            let gt = textureSampleLevel(glow_tex, glow_samp, tile, 0.0);
            moon_alpha = mt.a;
            col = col + (mt.rgb * mt.a * 4.0 + gt.r * 0.6) * s.moon.w;
        }
    }

    // stars, above the horizon only, hidden behind the moon
    let star_uv = vec2(atan2(d.z, d.x) / 6.2831853, acos(clamp(d.y, -1.0, 1.0)) / 3.1415927) * vec2(8.0, 4.0);
    col = col + textureSample(stars_tex, stars_samp, star_uv).rgb * s.p.w * clamp(d.y * 8.0, 0.0, 1.0) * (1.0 - moon_alpha);

    // ---- clouds: OUR OWN, made to look like the real game's sky (author note: "just make a
    // volumetric one that looks like it"). Not IV's method: IV cuts two flat layers out of a noise
    // map it draws each frame, and that map's settings aren't known. This is a slab of cloud
    // above the city, walked through in a few steps along the line of sight, so it has thickness,
    // lit sides and shaded undersides, and thins out toward the horizon. From the game: the
    // cloud colours (lit = the top-layer colour, shaded = the main colour) and how much cloud the
    // weather has. The noise is made up.
    if d.y > 0.01 {
        let cover = s.q.z;
        // where the line of sight meets a flat ceiling: this is what gives the far clouds their
        // flattened, receding look
        let plane = d.xz / max(d.y, 0.06);
        let wind = vec2(s.c3.w * 0.012, s.c3.w * 0.004);
        let warp = vec2(fbm(plane * 0.5 + vec2(3.1, 1.7) + wind), fbm(plane * 0.5 + vec2(8.3, 2.8) + wind)) - 0.5;
        let sun2 = normalize(s.sun_dir.xz + vec2(1e-4, 0.0));
        var alpha = 0.0;
        var light = 0.0;
        for (var i = 0; i < 5; i++) {
            let h = 1.0 + 0.07 * f32(i);
            let p = plane * h * 1.1 + wind + warp * 2.2;
            // fat in the middle of the slab, thin at its top and bottom
            let shape = sin(3.14159 * (f32(i) + 0.5) / 5.0);
            let dens = cloud_density(p, cover) * shape;
            if dens > 0.001 {
                let a = (1.0 - alpha) * (1.0 - exp(-dens * 1.6));
                // how much cloud lies between this spot and the sun
                let toward_sun = cloud_density(p + sun2 * 0.3, cover);
                light += a * exp(-toward_sun * 2.2);
                alpha += a;
            }
        }
        if alpha > 0.001 {
            let lit = light / alpha;
            // a bright rim when looking toward the sun through thin cloud
            let rim = pow(max(toward, 0.0), 8.0) * (1.0 - alpha) * 0.6 * s.sun_dir.w;
            // STAND-IN: the real sky's wisps are brighter than the sky behind them, so the lit
            // colour is never darker than the sky it sits on, and the shaded side only goes
            // part-way to the table's dark cloud colour (more in heavy cloud)
            let sky_lum = dot(col, vec3(0.299, 0.587, 0.114));
            let top_lum = max(dot(s.top_color.rgb, vec3(0.299, 0.587, 0.114)), 1e-3);
            let bright = s.top_color.rgb * max(1.0, sky_lum * 1.35 / top_lum);
            let shade = mix(bright, s.cloud_color.rgb, 0.25 + 0.5 * cover);
            let cloud = mix(shade, bright, lit) + s.sun_color.rgb * rim;
            // far clouds fade into the horizon haze
            let far = smoothstep(0.02, 0.22, d.y);
            col = mix(col, cloud, clamp(alpha * far, 0.0, 1.0));
        }
    }

    return vec4(min(col * s.p.z, vec3(1.6)), 1.0);
}
