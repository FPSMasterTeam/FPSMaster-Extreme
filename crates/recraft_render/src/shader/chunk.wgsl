struct Camera {
    view_proj: mat4x4<f32>,
    // Day/night sky-light scale (vanilla getSunBrightness): 1.0 by day, ~0.2 at
    // night. Applied to each vertex's sky-light term so open ground darkens at
    // night while block-lit (torch/lava) surfaces keep their brightness.
    sky_brightness: f32,
    time: f32,
    tile_size: vec2<f32>,
    // Render origin in whole blocks: subtracted from every world position so the
    // view-projection (built with the camera at this origin) stays precise far
    // from spawn. xyz used; w padding.
    origin: vec4<i32>,
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
    extra: vec4<f32>,      // x: fullbright (1 = force lightmap to full)
};

@group(2) @binding(0)
var<uniform> lighting: Lighting;
@group(2) @binding(1)
var shadow_map: texture_depth_2d;
@group(2) @binding(2)
var shadow_sampler: sampler_comparison;
@group(2) @binding(3)
var normal_atlas: texture_2d<f32>;
@group(2) @binding(4)
var specular_atlas: texture_2d<f32>;
@group(2) @binding(5)
var pbr_sampler: sampler;

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
    // Camera-relative: subtract the render origin in fixed-point (×64) i32 before
    // converting to f32, so the position stays exact at any world coordinate.
    let rel = input.pos_light.xyz - camera.origin.xyz * 64;
    let world_pos = vec3<f32>(f32(rel.x), f32(rel.y), f32(rel.z)) / 64.0;
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
    return max(light_level(light.x) * camera.sky_brightness, light_level(light.y));
}

// Vanilla per-level brightness curve: light falls off steeply toward the dark
// end (l=1 -> 1, l=0.5 -> 0.2), giving the moody gradient around light sources.
fn light_level(l: f32) -> f32 {
    return l / (4.0 - 3.0 * clamp(l, 0.0, 1.0));
}

// Vanilla-style coloured light map: day/night-scaled sky light (cool at night,
// white by day) combined with a warm torch/block-light glow, with the steep
// per-level falloff and a small moody floor so nothing is pure black.
fn vanilla_lightmap(light: vec2<f32>) -> vec3<f32> {
    let day = camera.sky_brightness;
    let sky = light_level(light.x);
    let block = light_level(light.y);
    let sky_tint = mix(vec3<f32>(0.18, 0.22, 0.34), vec3<f32>(1.0, 1.0, 0.99), day);
    let sky_term = sky_tint * (sky * day);
    let block_term = vec3<f32>(1.0, 0.60, 0.30) * block; // warm torch glow
    return max(max(sky_term, block_term), vec3<f32>(0.035, 0.04, 0.05));
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

// GGX normal distribution function.
fn ggx_distribution(ndoth: f32, roughness: f32) -> f32 {
    let a2 = roughness * roughness;
    let d = ndoth * ndoth * (a2 - 1.0) + 1.0;
    return a2 / (3.14159 * d * d);
}

// Smith geometry term (GGX, combined masking-shadowing).
fn smith_ggx(ndotv: f32, ndotl: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    let gv = ndotv / (ndotv * (1.0 - k) + k);
    let gl = ndotl / (ndotl * (1.0 - k) + k);
    return gv * gl;
}

// Schlick Fresnel approximation.
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - cos_theta, 5.0);
}

// Build TBN from geometric face normal (axis-aligned in Minecraft) and sample
// the tangent-space normal from the PBR normal atlas.
fn sample_pbr_normal(uv: vec2<f32>, geo_n: vec3<f32>) -> vec3<f32> {
    let n_tex = textureSample(normal_atlas, pbr_sampler, uv);
    var ts = n_tex.rgb * 2.0 - vec3<f32>(1.0);
    ts.x = ts.x * 1.5;
    ts.y = ts.y * 1.5;
    let abs_n = abs(geo_n);
    var tangent: vec3<f32>;
    var bitangent: vec3<f32>;
    if (abs_n.y > 0.9) {
        tangent = vec3<f32>(1.0, 0.0, 0.0);
        bitangent = vec3<f32>(0.0, 0.0, sign(geo_n.y));
    } else if (abs_n.x > 0.9) {
        tangent = vec3<f32>(0.0, 0.0, -sign(geo_n.x));
        bitangent = vec3<f32>(0.0, 1.0, 0.0);
    } else {
        tangent = vec3<f32>(sign(geo_n.z), 0.0, 0.0);
        bitangent = vec3<f32>(0.0, 1.0, 0.0);
    }
    return normalize(tangent * ts.x + bitangent * ts.y + geo_n * ts.z);
}

// Apply the shader-pack lighting model to an albedo colour. Falls back to the
// vanilla flat day/night brightness when shaders are disabled.
fn apply_lighting(albedo: vec3<f32>, in: VertexOutput) -> vec3<f32> {
    // Fullbright preset: skip all lightmap/shader darkening (keep baked shading).
    if (lighting.extra.x > 0.5) {
        return albedo;
    }
    // Brightness option (fog_params.w in 0..1) → gamma: 1.0 neutral, lower darker.
    let gamma = 1.0 + (1.0 - lighting.fog_params.w) * 1.5;
    if (lighting.flags.x < 0.5) {
        // Vanilla path: coloured light map (warm block light + day/night sky) with
        // the steep per-level falloff, then the brightness gamma.
        return albedo * light_curve(vanilla_lightmap(in.light), gamma);
    }
    let geo_n = normalize(in.normal);
    let pbr_on = lighting.fog_color.w > 0.5;
    let n = select(geo_n, sample_pbr_normal(in.uv, geo_n), pbr_on);
    let ndotl = max(dot(n, lighting.sun_dir.xyz), 0.0);
    // Sun visibility (1 = lit, 0 = shadowed): rasterized shadow-map PCF by default,
    // or hardware ray-traced (sharp/soft) when ray tracing is active. The two
    // variants are supplied by the prepended rt_stub.wgsl / rt_common.wgsl.
    // `in.clip_position.xy` is the framebuffer pixel coord, used to seed the RT noise.
    let shadow = sun_visibility(in.world_pos, geo_n, ndotl, in.clip_position.xy);
    let sky = in.light.x;
    let block = in.light.y;
    let day = max(camera.sky_brightness, 0.04);
    // Ambient term. RTAO is no longer applied inline — it's a denoised screen-space
    // pass (rt_ao.wgsl) multiplied onto the scene before the temporal upscale.
    let ambient = lighting.ambient.rgb * (0.08 + 0.92 * sky * day);
    let torch = vec3<f32>(1.0, 0.82, 0.55) * block;

    if (pbr_on) {
        let view_dir = normalize(lighting.camera_pos.xyz - in.world_pos);
        let spec_tex = textureSample(specular_atlas, pbr_sampler, in.uv);
        let smoothness = spec_tex.r;
        let metallic = spec_tex.g;
        let emissive = spec_tex.b;
        let roughness = max((1.0 - smoothness) * (1.0 - smoothness), 0.04);
        let f0 = mix(vec3<f32>(0.04), albedo, metallic);
        let half_dir = normalize(lighting.sun_dir.xyz + view_dir);
        let ndotv = max(dot(n, view_dir), 0.001);
        let ndoth = max(dot(n, half_dir), 0.0);
        let vdoth = max(dot(view_dir, half_dir), 0.0);
        let D = ggx_distribution(ndoth, roughness);
        let G = smith_ggx(ndotv, ndotl, roughness);
        let F = fresnel_schlick(vdoth, f0);
        let spec_brdf = (D * G * F) / max(4.0 * ndotv * ndotl, 0.001);
        let kd = (vec3<f32>(1.0) - F) * (1.0 - metallic);
        let sun_light = lighting.sun_color.rgb * (ndotl * shadow * sun_sky_gate(sky, in.world_pos));
        let ao = clamp(dot(n, geo_n), 0.3, 1.0);
        var lit = (kd * albedo + spec_brdf) * sun_light
            + albedo * light_curve(ambient * ao + torch, gamma);
        let env_spec = f0 * smoothness * smoothness * ambient * ao * sky * 0.5;
        lit = lit + env_spec;
        lit = lit + albedo * emissive * 3.0;
        return lit;
    }

    let sun = lighting.sun_color.rgb * (ndotl * shadow * sun_sky_gate(sky, in.world_pos));
    var lit = albedo * light_curve(ambient + sun + torch, gamma);
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

// Normal G-buffer output: encode the geometric normal as 0.5 + 0.5*n into an
// Rgba8 target, feeding the screen-space RTAO pass an exact, stable per-pixel normal
// (instead of the jitter-sensitive depth-derivative reconstruction). Solid + cutout
// variants; cutout alpha-tests so holes don't write a normal.
@fragment
fn fs_normal(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(normalize(input.normal) * 0.5 + 0.5, 1.0);
}

@fragment
fn fs_normal_cutout(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, input.uv);
    if (texel.a * input.color.a < 0.5) {
        discard;
    }
    return vec4<f32>(normalize(input.normal) * 0.5 + 0.5, 1.0);
}
