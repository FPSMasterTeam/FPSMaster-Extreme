// Dedicated water surface shader: animated wave displacement + animated normals
// (sparkling sun glints), Fresnel sky reflection and a tight sun specular. Falls
// back to plain translucent water when the shader pack is disabled. Shares the
// chunk bind groups (camera / atlas / lighting).

struct Camera {
    view_proj: mat4x4<f32>,
    sky_brightness: f32,
    time: f32,
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
    var world_pos = vec3<f32>(f32(input.pos_light.x), f32(input.pos_light.y), f32(input.pos_light.z)) / 64.0;
    var normal = input.normal.xyz;

    // Displace + perturb normals only on the top surface, and only with shaders
    // on (so plain water keeps its flat vanilla look).
    if (lighting.flags.x > 0.5 && normal.y > 0.5) {
        let t = camera.time;
        let amp = 0.06;
        world_pos.y += wave_height(world_pos.xz, t) * amp;
        // Analytic gradient of the wave for the surface normal.
        let e = 0.35;
        let hx = (wave_height(world_pos.xz + vec2<f32>(e, 0.0), t)
            - wave_height(world_pos.xz - vec2<f32>(e, 0.0), t)) * amp;
        let hz = (wave_height(world_pos.xz + vec2<f32>(0.0, e), t)
            - wave_height(world_pos.xz - vec2<f32>(0.0, e), t)) * amp;
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

fn day_night(light: vec2<f32>) -> f32 {
    return max(light.x * camera.sky_brightness, light.y);
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
    var base = texel.rgb * in.color.rgb;
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
    let ambient = lighting.ambient.rgb * (0.2 + 0.8 * sky) * day;
    let diffuse = lighting.sun_color.rgb * (ndotl * sky * 0.5);
    var color = base * (ambient + diffuse);

    // Fresnel: more mirror-like at grazing angles.
    let fresnel = clamp(0.02 + 0.98 * pow(1.0 - max(dot(n, view_dir), 0.0), 5.0), 0.0, 1.0);
    let refl_dir = reflect(-view_dir, n);
    let reflection = sky_reflection(refl_dir);
    color = mix(color, reflection, fresnel * 0.85);

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

    // Push alpha up where the reflection is strong so it reads as a surface.
    alpha = clamp(alpha + fresnel * 0.3, 0.0, 1.0);
    return vec4<f32>(color, alpha);
}
