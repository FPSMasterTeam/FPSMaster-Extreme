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

// The reflected-ray lookup (`reflect_ray`) + its group(3) bindings are supplied by a
// prepended module: water_ssr.wgsl (screen-space march, default) or water_rt.wgsl
// (hardware ray trace). Both return reflected colour in rgb + a hit confidence in a.

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

    let sky = in.light.x;

    // Underwater body: the refracted scene with depth-based (Beer-Lambert) absorption —
    // clear and textured in the shallows, darkening to a deep teal with depth so the
    // water reads as a real body of water rather than a transparent sheet.
    let refr = refract_water(in.clip_position.xy, in.world_pos, n);
    var color = refr.rgb;

    // Fresnel: more mirror-like at grazing angles. Raised floor so even a
    // top-down view keeps a clear reflection.
    let fresnel = clamp(0.12 + 0.88 * pow(1.0 - max(dot(n, view_dir), 0.0), 5.0), 0.0, 1.0);
    let refl_dir = reflect(-view_dir, n);
    // Screen-space reflection of the terrain, falling back to the sky where the
    // ray leaves the screen or hits nothing.
    var reflection = sky_reflection(refl_dir);
    let refl = reflect_ray(in.world_pos + n * 0.05, refl_dir);
    reflection = mix(reflection, refl.rgb, refl.a);
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
    // opaque at grazing angles where the reflection dominates. Where RT refraction
    // already supplied the underwater view, the surface is fully opaque (the shader,
    // not the alpha blend, provides the see-through — otherwise it double-composites).
    // Where refraction supplied the underwater view, the surface is opaque (the shader,
    // not the alpha blend, provides the see-through — otherwise it double-composites the
    // background). Otherwise keep the alpha-blended translucency.
    alpha = clamp(max(alpha, 0.72) + fresnel * 0.28, 0.0, 1.0);
    alpha = max(alpha, refr.a);
    return vec4<f32>(color, alpha);
}
