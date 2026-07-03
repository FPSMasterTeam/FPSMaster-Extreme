// Greedy (flat-lighting) chunk shader. Merged multi-block quads carry a per-block
// REPEAT uv and the tile's atlas origin; this wraps `fract(repeat_uv)` within the
// tile so the texture tiles instead of stretching, and uses textureSampleGrad with
// the unwrapped gradient so mips stay clean across internal tile seams. Lighting is
// flat per-face (the vanilla shaders-off look), no normals/AO.

const GREEDY_UV_SCALE: f32 = 16.0;

struct Camera {
    view_proj: mat4x4<f32>,
    sky_brightness: f32,
    time: f32,
    tile_size: vec2<f32>, // (du, dv) of one atlas tile
    // Render origin in whole blocks (camera-relative rendering); see chunk.wgsl.
    origin: vec4<i32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var block_atlas: texture_2d<f32>;
@group(1) @binding(1) var block_sampler: sampler;

// Group 2 layout matches the lit pipeline; we only read the fog fields.
struct Lighting {
    light_view_proj: mat4x4<f32>,
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    ambient: vec4<f32>,
    camera_pos: vec4<f32>,
    flags: vec4<f32>,
    fog_color: vec4<f32>,
    fog_params: vec4<f32>, // x start, y end, z enabled, w brightness gamma
    extra: vec4<f32>,      // x fullbright (1 = force lightmap to full)
};
@group(2) @binding(0) var<uniform> lighting: Lighting;

struct VsIn {
    @location(0) pos_light: vec4<i32>,
    @location(1) color: vec4<f32>,
    // Unorm16 0..1; × GREEDY_UV_SCALE = the per-block repeat coordinate (0..16).
    @location(2) repeat_uv: vec2<f32>,
    // Unorm16 0..1: the tile's atlas-space origin (u0, v0).
    @location(3) tile_origin: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) repeat_uv: vec2<f32>,
    @location(2) tile_origin: vec2<f32>,
    @location(3) light: vec2<f32>,
    @location(4) world_pos: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    // Camera-relative: subtract the render origin in fixed-point i32 (see chunk.wgsl).
    let rel = in.pos_light.xyz - camera.origin.xyz * 64;
    let world_pos = vec3<f32>(f32(rel.x), f32(rel.y), f32(rel.z)) / 64.0;
    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = in.color;
    out.repeat_uv = in.repeat_uv * GREEDY_UV_SCALE;
    out.tile_origin = in.tile_origin;
    let w_bits = u32(in.pos_light.w) & 0xFFFFu;
    out.light = vec2<f32>(f32((w_bits >> 8u) & 0xFFu) / 255.0, f32(w_bits & 0xFFu) / 255.0);
    out.world_pos = world_pos;
    return out;
}

fn light_level(l: f32) -> f32 {
    return l / (4.0 - 3.0 * clamp(l, 0.0, 1.0));
}

// Vanilla-style coloured light map (matches chunk.wgsl's shaders-off path),
// with vanilla's final lightmap lift (`* 0.96 + 0.03`).
fn vanilla_lightmap(light: vec2<f32>) -> vec3<f32> {
    let day = camera.sky_brightness;
    let sky = light_level(light.x);
    let block = light_level(light.y);
    let sky_tint = mix(vec3<f32>(0.18, 0.22, 0.34), vec3<f32>(1.0, 1.0, 0.99), day);
    let sky_term = sky_tint * (sky * day);
    let block_term = vec3<f32>(1.0, 0.60, 0.30) * block;
    return max(sky_term, block_term) * 0.96 + vec3<f32>(0.03);
}

// Vanilla brightness blend (matches chunk.wgsl's light_curve): 0 = Moody base,
// 1 = lifted `1 - (1-x)^4`; only ever brightens the dark end.
fn light_curve(light: vec3<f32>, brightness: f32) -> vec3<f32> {
    let low = clamp(light, vec3<f32>(0.0), vec3<f32>(1.0));
    let inv = vec3<f32>(1.0) - low;
    let lifted = vec3<f32>(1.0) - inv * inv * inv * inv;
    let high = max(light - vec3<f32>(1.0), vec3<f32>(0.0));
    return mix(low, lifted, brightness) + high;
}

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

// Sample the atlas with per-tile wrapping + manual gradients (so the mip level is
// computed from the continuous repeat coordinate, not the fract'd one — no seams).
fn sample_tile(repeat_uv: vec2<f32>, tile_origin: vec2<f32>) -> vec4<f32> {
    let atlas_uv = tile_origin + fract(repeat_uv) * camera.tile_size;
    let ddx = dpdx(repeat_uv) * camera.tile_size;
    let ddy = dpdy(repeat_uv) * camera.tile_size;
    return textureSampleGrad(block_atlas, block_sampler, atlas_uv, ddx, ddy);
}

fn shade(in: VsOut, texel: vec4<f32>) -> vec3<f32> {
    let gamma = lighting.fog_params.w;
    let albedo = texel.rgb * in.color.rgb;
    // Fullbright preset: skip the lightmap darkening (keep baked AO/face shade).
    let lit = select(albedo * light_curve(vanilla_lightmap(in.light), gamma), albedo, lighting.extra.x > 0.5);
    return apply_fog(lit, in.world_pos);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = sample_tile(in.repeat_uv, in.tile_origin);
    return vec4<f32>(shade(in, texel), texel.a * in.color.a);
}

@fragment
fn fs_cutout(in: VsOut) -> @location(0) vec4<f32> {
    let texel = sample_tile(in.repeat_uv, in.tile_origin);
    if (texel.a * in.color.a < 0.5) {
        discard;
    }
    return vec4<f32>(shade(in, texel), 1.0);
}
