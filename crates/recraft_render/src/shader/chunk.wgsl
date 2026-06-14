struct Camera {
    view_proj: mat4x4<f32>,
    // Day/night sky-light scale (vanilla getSunBrightness): 1.0 by day, ~0.2 at
    // night. Applied to each vertex's sky-light term so open ground darkens at
    // night while block-lit (torch/lava) surfaces keep their brightness.
    sky_brightness: f32,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(1) @binding(0)
var block_atlas: texture_2d<f32>;

@group(1) @binding(1)
var block_sampler: sampler;

// Shader-pack lighting (group 2): directional sun + ambient (+ shadows later).
struct Lighting {
    light_view_proj: mat4x4<f32>,
    sun_dir: vec4<f32>,    // xyz: world-space direction TO the sun (normalized)
    sun_color: vec4<f32>,  // rgb sun intensity
    ambient: vec4<f32>,    // rgb ambient intensity
    camera_pos: vec4<f32>, // xyz: world-space eye (for specular)
    // x = master enable, y = shadows, z = specular, w = shadow map texel size.
    flags: vec4<f32>,
    fog_color: vec4<f32>,  // rgb: fog (horizon) colour
    fog_params: vec4<f32>, // x: start dist, y: end dist, z: enabled, w: brightness
};

@group(2) @binding(0)
var<uniform> lighting: Lighting;
@group(2) @binding(1)
var shadow_map: texture_depth_2d;
@group(2) @binding(2)
var shadow_sampler: sampler_comparison;

struct VertexInput {
    // xyz: world position × 64 as fixed-point i16.
    // w: packed (sky_u8 << 8 | block_u8) reinterpreted as i16.
    @location(0) pos_light: vec4<i32>,
    // RGBA8 unorm: tint × face shade × AO + alpha.
    @location(1) color: vec4<f32>,
    // Normalized atlas UV as Unorm16×2.
    @location(2) uv: vec2<f32>,
    // Geometric face normal (Snorm8×4, xyz used).
    @location(3) normal: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) light: vec2<f32>,
    @location(3) world_pos: vec3<f32>,
    @location(4) normal: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = vec3<f32>(f32(input.pos_light.x), f32(input.pos_light.y), f32(input.pos_light.z)) / 64.0;
    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    let w_bits = u32(input.pos_light.w) & 0xFFFFu;
    out.light = vec2<f32>(f32((w_bits >> 8u) & 0xFFu) / 255.0, f32(w_bits & 0xFFu) / 255.0);
    out.world_pos = world_pos;
    out.normal = input.normal.xyz;
    return out;
}

fn day_night(light: vec2<f32>) -> f32 {
    return max(light.x * camera.sky_brightness, light.y);
}

// Fraction of the sun visible at this world position (1 = lit, 0 = shadowed),
// 3×3 PCF. Outside the shadow map's range everything is lit.
fn sun_shadow(world_pos: vec3<f32>, ndotl: f32) -> f32 {
    let lc = lighting.light_view_proj * vec4<f32>(world_pos, 1.0);
    let ndc = lc.xyz / lc.w;
    if (ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z > 1.0) {
        return 1.0;
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
    let bias = clamp(0.0015 * (1.0 - ndotl), 0.0004, 0.003);
    let depth = ndc.z - bias;
    let texel = lighting.flags.w;
    var sum = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * texel;
            sum = sum + textureSampleCompare(shadow_map, shadow_sampler, uv + off, depth);
        }
    }
    return sum / 9.0;
}

// Brightness gamma: pull the shadow/low-light end down while leaving fully-lit
// surfaces alone. Values in [0,1] are raised to `gamma` (>1 darkens; 1->1,
// 0->0); anything above 1 (bright sun) passes through linearly so daylight keeps
// its punch. This is a curve, not a flat multiply — dark gets darker, bright stays.
fn light_curve(light: vec3<f32>, gamma: f32) -> vec3<f32> {
    let low = pow(clamp(light, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(gamma));
    let high = max(light - vec3<f32>(1.0), vec3<f32>(0.0));
    return low + high;
}

// Apply the shader-pack lighting model to an albedo colour. Falls back to the
// vanilla flat day/night brightness when shaders are disabled.
fn apply_lighting(albedo: vec3<f32>, in: VertexOutput) -> vec3<f32> {
    // Brightness option (fog_params.w in 0..1) → gamma: 1.0 neutral, lower darker.
    let gamma = 1.0 + (1.0 - lighting.fog_params.w) * 1.5;
    if (lighting.flags.x < 0.5) {
        return albedo * light_curve(vec3<f32>(day_night(in.light)), gamma);
    }
    let n = normalize(in.normal);
    let ndotl = max(dot(n, lighting.sun_dir.xyz), 0.0);
    var shadow = 1.0;
    if (lighting.flags.y > 0.5) {
        shadow = sun_shadow(in.world_pos, ndotl);
    }
    let sky = in.light.x;       // skylight 0..1 — gates the sun (indoors = none)
    let block = in.light.y;     // blocklight 0..1 — torches/lava
    // Ambient must track day/night: the skylight channel stays high at night
    // (only the directional sun sets), so gate the sky-driven ambient by the
    // day/night brightness, leaving a small floor so it's never pure black.
    let day = max(camera.sky_brightness, 0.04);
    let ambient = lighting.ambient.rgb * (0.08 + 0.92 * sky) * day;
    let sun = lighting.sun_color.rgb * (ndotl * shadow * sky);
    let torch = vec3<f32>(1.0, 0.82, 0.55) * block;
    var lit = albedo * light_curve(ambient + sun + torch, gamma);

    // Blinn-Phong specular (phase 2): a tight sun highlight, sky-gated. Added
    // after the gamma curve so highlights stay crisp.
    if (lighting.flags.z > 0.5) {
        let view_dir = normalize(lighting.camera_pos.xyz - in.world_pos);
        let half_dir = normalize(lighting.sun_dir.xyz + view_dir);
        let spec = pow(max(dot(n, half_dir), 0.0), 48.0);
        lit = lit + lighting.sun_color.rgb * (spec * shadow * sky * 0.25);
    }
    return lit;
}

// Distance fog toward the sky horizon colour. Independent of the master shader
// toggle so it can be used on its own.
fn apply_fog(color: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    if (lighting.fog_params.z < 0.5) {
        return color;
    }
    let dist = length(world_pos - lighting.camera_pos.xyz);
    let f = clamp(
        (dist - lighting.fog_params.x) / max(lighting.fog_params.y - lighting.fog_params.x, 0.001),
        0.0,
        1.0,
    );
    return mix(color, lighting.fog_color.rgb, f);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    let rgb = apply_fog(apply_lighting(texel.rgb * input.color.rgb, input), input.world_pos);
    return vec4<f32>(rgb, texel.a * input.color.a);
}

// Flat-colour variant for the texture-cost benchmark: skips the atlas fetch and
// shades from the vertex colour only (no shader-pack lighting).
@fragment
fn fs_flat(input: VertexOutput) -> @location(0) vec4<f32> {
    let b = day_night(input.light);
    return vec4<f32>(input.color.rgb * b, 1.0);
}

// Depth pre-pass: colour writes are masked off in the pipeline, so this skips
// the texture fetch entirely and just lets the rasterizer record depth. It still
// declares the full VertexOutput input so the inter-stage interface matches
// vs_main (wgpu requires every vertex output to be consumed by the fragment).
@fragment
fn fs_depth(_input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}

@fragment
fn fs_cutout(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    if (texel.a * input.color.a < 0.5) {
        discard;
    }
    let rgb = apply_fog(apply_lighting(texel.rgb * input.color.rgb, input), input.world_pos);
    return vec4<f32>(rgb, 1.0);
}
