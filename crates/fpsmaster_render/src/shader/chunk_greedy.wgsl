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

// Vanilla's torch flicker stand-in; see chunk.wgsl's `torch_gain`.
fn torch_gain() -> f32 {
    let t = camera.time;
    let flicker = sin(t * 3.1) * 0.6 + sin(t * 7.7) * 0.3 + sin(t * 13.3) * 0.15;
    return flicker * 0.1 + 1.5;
}

// Vanilla's coloured light map (matches chunk.wgsl's shaders-off path): sky and
// block terms tinted separately then SUMMED, per `EntityRenderer.updateLightmap`.
fn vanilla_lightmap(light: vec2<f32>) -> vec3<f32> {
    let sun = camera.sky_brightness;
    let sky = light_level(light.x) * (sun * 0.95 + 0.05);
    let block = light_level(light.y) * torch_gain();
    let sky_rg = sky * (sun * 0.65 + 0.35);
    let block_g = block * ((block * 0.6 + 0.4) * 0.6 + 0.4);
    let block_b = block * (block * block * 0.6 + 0.4);
    let rgb = vec3<f32>(sky_rg + block, sky_rg + block_g, sky + block_b);
    return clamp(rgb * 0.96 + vec3<f32>(0.03), vec3<f32>(0.0), vec3<f32>(1.0));
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

// Vanilla repeats the `* 0.96 + 0.03` lift after the gamma blend; see chunk.wgsl.
fn vanilla_lightmap_graded(light: vec2<f32>, gamma: f32) -> vec3<f32> {
    let lit = light_curve(vanilla_lightmap(light), gamma);
    return clamp(lit * 0.96 + vec3<f32>(0.03), vec3<f32>(0.0), vec3<f32>(1.0));
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

// sRGB -> linear, matching chunk.wgsl (see the long note there): the vertex colour
// and the lightmap are vanilla gamma-space multipliers, but the atlas is sampled as
// Rgba8UnormSrgb (linear) and the swapchain re-encodes, so they have to be decoded
// before they scale the texel or the whole shading ladder flattens out.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

fn shade(in: VsOut, texel: vec4<f32>) -> vec3<f32> {
    let gamma = lighting.fog_params.w;
    let albedo = texel.rgb * srgb_to_linear(in.color.rgb);
    // Fullbright preset: skip the lightmap darkening (keep baked AO/face shade).
    let lit = select(
        albedo * srgb_to_linear(vanilla_lightmap_graded(in.light, gamma)),
        albedo,
        lighting.extra.x > 0.5,
    );
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

// Graphics: Fast — the translucent layer drawn opaque. Matches chunk.wgsl's
// entry of the same name: REPLACE blending throws the alpha away, so a fully
// transparent texel (the empty part of `glass_pane_top_*`) would be written as
// solid black unless it is discarded first. The threshold stays well under
// stained glass's 0.4 body alpha so only genuinely empty texels drop out.
@fragment
fn fs_opaque_cutout(in: VsOut) -> @location(0) vec4<f32> {
    let texel = sample_tile(in.repeat_uv, in.tile_origin);
    if (texel.a * in.color.a < 0.05) {
        discard;
    }
    return vec4<f32>(shade(in, texel), 1.0);
}
