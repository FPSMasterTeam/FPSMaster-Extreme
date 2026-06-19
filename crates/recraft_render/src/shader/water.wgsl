// Dedicated water surface shader: animated wave displacement + animated normals
// (sparkling sun glints), Fresnel sky reflection and a tight sun specular. Falls
// back to plain translucent water when the shader pack is disabled. Shares the
// chunk bind groups (camera / atlas / lighting).

struct Camera {
    view_proj: mat4x4<f32>,
    sky_brightness: f32,
    time: f32,
    tile_size: vec2<f32>,
    // Render origin in whole blocks (camera-relative rendering); see chunk.wgsl.
    origin: vec4<i32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var block_atlas: texture_2d<f32>;
@group(1) @binding(1) var block_sampler: sampler;

struct Lighting {
    light_view_proj: mat4x4<f32>,
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    ambient: vec4<f32>,
    camera_pos: vec4<f32>,
    flags: vec4<f32>,
    fog_color: vec4<f32>,
    fog_params: vec4<f32>,
};
@group(2) @binding(0) var<uniform> lighting: Lighting;
@group(2) @binding(1) var shadow_map: texture_depth_2d;
@group(2) @binding(2) var shadow_sampler: sampler_comparison;
@group(2) @binding(3) var normal_atlas: texture_2d<f32>;
@group(2) @binding(4) var specular_atlas: texture_2d<f32>;
@group(2) @binding(5) var pbr_sampler: sampler;

// Group 3: screen-space reflection inputs (copied opaque scene + depth + camera).
struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};
@group(3) @binding(0) var scene_tex: texture_2d<f32>;
@group(3) @binding(1) var scene_sampler: sampler;
// World depth, bound as an unfilterable-float texture (not texture_depth_2d): the
// GLSL backend maps a depth texture to sampler2DShadow, which has no plain
// textureLoad overload. As a float texture it reads back via texelFetch.
@group(3) @binding(2) var depth_tex: texture_2d<f32>;
@group(3) @binding(3) var<uniform> ssr_cam: PostCamera;

// March the reflected ray through the depth buffer. Returns reflected colour in
// rgb and a hit confidence in a (0 = miss). World-space steps that grow with
// distance, projected to screen each step and tested against stored depth.
fn screen_space_reflection(origin: vec3<f32>, dir: vec3<f32>) -> vec4<f32> {
    let dims = vec2<f32>(textureDimensions(depth_tex));
    var p = origin;
    // Fewer steps that grow a touch faster — keeps most of the reflection range at
    // ~35% less marching cost (reflections are rough, so coarser steps are fine).
    var step = 0.5;
    for (var i = 0; i < 18; i = i + 1) {
        p = p + dir * step;
        step = step * 1.22;
        let clip = camera.view_proj * vec4<f32>(p, 1.0);
        if (clip.w <= 0.0) {
            break;
        }
        let ndc = clip.xyz / clip.w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
            break;
        }
        let px = vec2<i32>(clamp(uv * dims, vec2<f32>(0.0), dims - 1.0));
        let scene_depth = textureLoad(depth_tex, px, 0).r;
        let delta = ndc.z - scene_depth;
        // Ray went just behind the stored surface → intersection (thickness-bounded).
        if (delta > 0.00002 && delta < 0.0025) {
            let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
            // Explicit LOD: the implicit-derivative textureSample would force FXC
            // (DX12 backend) to unroll this varying-iteration loop and fail (X3570/
            // X3511). Reflections are rough, so sampling mip 0 looks identical.
            return vec4<f32>(textureSampleLevel(scene_tex, scene_sampler, uv, 0.0).rgb, smoothstep(0.0, 0.12, edge));
        }
    }
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}

struct VertexInput {
    @location(0) pos_light: vec4<i32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
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

// Sum-of-sines wave height at a world xz position.
fn wave_height(p: vec2<f32>, t: f32) -> f32 {
    var h = 0.0;
    h += sin(p.x * 0.7 + t * 1.6) * 0.5;
    h += sin(p.y * 0.9 - t * 1.3) * 0.4;
    h += sin((p.x + p.y) * 0.5 + t * 2.1) * 0.3;
    h += sin((p.x - p.y) * 1.3 - t * 1.1) * 0.15;
    return h;
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // Camera-relative: subtract the render origin in fixed-point i32 (see chunk.wgsl).
    let rel = input.pos_light.xyz - camera.origin.xyz * 64;
    var world_pos = vec3<f32>(f32(rel.x), f32(rel.y), f32(rel.z)) / 64.0;
    var normal = input.normal.xyz;

    // Displace + perturb normals only on the top surface, and only with shaders
    // on (so plain water keeps its flat vanilla look).
    if (lighting.flags.x > 0.5 && normal.y > 0.5) {
        let t = camera.time;
        let amp = 0.06;
        // Wave phase must use the ABSOLUTE world xz (add the origin back), or the
        // pattern would slide as the render origin follows the player.
        let wxz = world_pos.xz + vec2<f32>(f32(camera.origin.x), f32(camera.origin.z));
        world_pos.y += wave_height(wxz, t) * amp;
        // Analytic gradient of the wave for the surface normal.
        let e = 0.35;
        let hx = (wave_height(wxz + vec2<f32>(e, 0.0), t)
            - wave_height(wxz - vec2<f32>(e, 0.0), t)) * amp;
        let hz = (wave_height(wxz + vec2<f32>(0.0, e), t)
            - wave_height(wxz - vec2<f32>(0.0, e), t)) * amp;
        normal = normalize(vec3<f32>(-hx / (2.0 * e), 1.0, -hz / (2.0 * e)));
    }

    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    let w_bits = u32(input.pos_light.w) & 0xFFFFu;
    out.light = vec2<f32>(f32((w_bits >> 8u) & 0xFFu) / 255.0, f32(w_bits & 0xFFu) / 255.0);
    out.world_pos = world_pos;
    out.normal = normal;
    return out;
}

fn light_level(l: f32) -> f32 {
    return l / (4.0 - 3.0 * clamp(l, 0.0, 1.0));
}

fn day_night(light: vec2<f32>) -> f32 {
    return max(light_level(light.x) * camera.sky_brightness, light_level(light.y));
}

// A cheap sky colour for the reflected ray: horizon (fog colour) blending up to a
// brighter zenith, plus a soft sun disc toward the sun direction.
fn sky_reflection(dir: vec3<f32>) -> vec3<f32> {
    let up = clamp(dir.y, 0.0, 1.0);
    let horizon = lighting.fog_color.rgb;
    let zenith = horizon * 0.6 + vec3<f32>(0.10, 0.20, 0.40) * camera.sky_brightness;
    var col = mix(horizon, zenith, up);
    let sun = max(dot(normalize(dir), lighting.sun_dir.xyz), 0.0);
    col += lighting.sun_color.rgb * pow(sun, 64.0) * 1.5;
    return col;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_atlas, block_sampler, in.uv);
    // Deeper, darker water body: darken and tint the texel toward a deep blue.
    var base = texel.rgb * in.color.rgb * vec3<f32>(0.30, 0.45, 0.62);
    var alpha = texel.a * in.color.a;

    // Plain translucent water when the shader pack is off.
    if (lighting.flags.x < 0.5) {
        return vec4<f32>(base * day_night(in.light), alpha);
    }

    let n = normalize(in.normal);
    let view_dir = normalize(lighting.camera_pos.xyz - in.world_pos);

    // Lit base (diffuse sun + day/night ambient), gated by skylight.
    let sky = in.light.x;
    let ndotl = max(dot(n, lighting.sun_dir.xyz), 0.0);
    let day = max(camera.sky_brightness, 0.04);
    let ambient = lighting.ambient.rgb * (0.2 + 0.8 * sky * day);
    let diffuse = lighting.sun_color.rgb * (ndotl * sky * 0.5);
    var color = base * (ambient + diffuse);

    // Fresnel: more mirror-like at grazing angles. Raised floor so even a
    // top-down view keeps a clear reflection.
    let fresnel = clamp(0.12 + 0.88 * pow(1.0 - max(dot(n, view_dir), 0.0), 5.0), 0.0, 1.0);
    let refl_dir = reflect(-view_dir, n);
    // Screen-space reflection of the terrain, falling back to the sky where the
    // ray leaves the screen or hits nothing.
    var reflection = sky_reflection(refl_dir);
    let ssr = screen_space_reflection(in.world_pos + n * 0.05, refl_dir);
    reflection = mix(reflection, ssr.rgb, ssr.a);
    // Stronger reflection overall.
    color = mix(color, reflection, fresnel);

    // Tight sun specular highlight — the sparkle ("波光粼粼").
    let half_dir = normalize(lighting.sun_dir.xyz + view_dir);
    let spec = pow(max(dot(n, half_dir), 0.0), 200.0);
    color += lighting.sun_color.rgb * spec * sky * 2.5;

    // Distance fog toward the horizon, matching the terrain.
    if (lighting.fog_params.z > 0.5) {
        let dist = length(in.world_pos - lighting.camera_pos.xyz);
        let f = clamp(
            (dist - lighting.fog_params.x) / max(lighting.fog_params.y - lighting.fog_params.x, 0.001),
            0.0, 1.0,
        );
        color = mix(color, lighting.fog_color.rgb, f);
    }

    // Lower visibility (less see-through): a high opacity floor, rising toward
    // opaque at grazing angles where the reflection dominates.
    alpha = clamp(max(alpha, 0.72) + fresnel * 0.28, 0.0, 1.0);
    return vec4<f32>(color, alpha);
}
