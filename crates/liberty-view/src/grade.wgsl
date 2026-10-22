// IV's final colour step (the end of its rage_postfx pixel shader), on the finished frame:
// pull toward grey by the table's Desaturation, multiply by its ColourCorrect (128 = neutral,
// the shader doubles it), then multiply by brightness^(Gamma - 1), which lifts the shadows.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
struct Grade {
    // rgb = ColourCorrect / 255
    tint: vec4<f32>,
    // x = Desaturation (1 = full colour), y = Gamma
    params: vec4<f32>,
}
@group(0) @binding(2) var<uniform> g: Grade;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(screen, samp, in.uv);
    // the frame already holds screen values, as IV's does (textures aren't sRGB-decoded and the
    // light levels are IV's: timecyc.rs); the pow at the end only undoes the sRGB encode that
    // happens when the frame is shown
    // not clamped first: IV grades the bright (above-white) values too, which is how a lit sign
    // keeps its colour through the desaturation instead of turning white
    var c = max(src.rgb, vec3(0.0));
    let lum = dot(c, vec3(0.2125, 0.7154, 0.0721));
    c = mix(vec3(lum), c, g.params.x);
    c = c * g.tint.rgb * 2.0;
    c = c * pow(clamp(lum, 0.004, 1.0), g.params.y - 1.0);
    return vec4(pow(clamp(c, vec3(0.0), vec3(1.0)), vec3(2.2)), src.a);
}
