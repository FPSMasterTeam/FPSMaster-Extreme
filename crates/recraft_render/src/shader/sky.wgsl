// Fullscreen sky gradient. A view ray is reconstructed per pixel from the
// inverse rotation-only view-projection, so the horizon sits where the camera
// actually looks. The gradient runs from the horizon/fog color up to the zenith
// sky color, with an orange sunrise/sunset glow added near the horizon toward
// the sun.

struct Sky {
    inv_view_proj: mat4x4<f32>,
    horizon: vec4<f32>,
    zenith: vec4<f32>,
    // xyz = world-space sun direction, w = sunset glow strength.
    sun_dir: vec4<f32>,
    sunset: vec4<f32>,
    // xyz = camera world position, w = time (seconds).
    camera_pos: vec4<f32>,
    // x = clouds enabled.
    cloud_params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> sky: Sky;

fn hash2(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash2(i), hash2(i + vec2<f32>(1.0, 0.0)), u.x),
        mix(hash2(i + vec2<f32>(0.0, 1.0)), hash2(i + vec2<f32>(1.0, 1.0)), u.x),
        u.y,
    );
}

fn fbm(p0: vec2<f32>) -> f32 {
    var v = 0.0;
    var a = 0.5;
    var p = p0;
    for (var i = 0; i < 4; i = i + 1) {
        v = v + a * value_noise(p);
        p = p * 2.02;
        a = a * 0.5;
    }
    return v;
}

// Cloud density at a world position (xz noise + soft vertical falloff in slab).
fn cloud_density(pos: vec3<f32>, frac: f32, wind: vec2<f32>) -> f32 {
    var d = fbm(pos.xz * 0.0016 + wind);
    d = smoothstep(0.50, 0.78, d);
    // Fade in/out across the slab thickness so clouds have soft tops/bottoms.
    d = d * smoothstep(0.0, 0.25, frac) * smoothstep(1.0, 0.70, frac);
    return d;
}

// Light raymarch a cloud slab along the view ray. Returns scattered light (rgb)
// and coverage (a) for front-to-back compositing.
fn clouds(ro: vec3<f32>, rd: vec3<f32>, sun_dir: vec3<f32>, time: f32) -> vec4<f32> {
    if (rd.y < 0.05) {
        return vec4<f32>(0.0);
    }
    let bottom = 120.0;
    let top = 300.0;
    let t0 = bottom / rd.y;
    let t1 = top / rd.y;
    let steps = 16;
    let dt = (t1 - t0) / f32(steps);
    let wind = vec2<f32>(time * 0.004, time * 0.0015);
    let sun_col = vec3<f32>(1.0, 0.95, 0.85);
    let ambient = sky.zenith.rgb * 0.6 + vec3<f32>(0.2);

    var transmittance = 1.0;
    var scattered = vec3<f32>(0.0);
    for (var i = 0; i < steps; i = i + 1) {
        let t = t0 + dt * (f32(i) + 0.5);
        let pos = ro + rd * t;
        let frac = (t - t0) / (t1 - t0);
        let d = cloud_density(pos, frac, wind);
        if (d > 0.01) {
            // Cheap self-shadow: density a short way toward the sun.
            let lpos = pos + sun_dir * 40.0;
            let lfrac = clamp(frac + sun_dir.y * 40.0 / (t1 - t0), 0.0, 1.0);
            let ld = cloud_density(lpos, lfrac, wind);
            let shade = exp(-ld * 3.0);
            let lit = ambient + sun_col * (0.5 + 0.5 * shade);
            let absorb = d * 0.85;
            scattered = scattered + transmittance * lit * absorb;
            transmittance = transmittance * exp(-absorb);
            if (transmittance < 0.02) {
                break;
            }
        }
    }
    return vec4<f32>(scattered, 1.0 - transmittance);
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let pos = positions[vertex_index];
    var out: VertexOutput;
    out.clip_position = vec4<f32>(pos, 1.0, 1.0);
    out.ndc = pos;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Reconstruct the world-space view direction for this pixel.
    let world = sky.inv_view_proj * vec4<f32>(input.ndc, 1.0, 1.0);
    let dir = normalize(world.xyz / world.w);

    // Horizon → zenith gradient, weighted toward the horizon (most of the dome
    // is the deep sky color, blending to fog only near eye level).
    let t = smoothstep(0.0, 0.55, clamp(dir.y, 0.0, 1.0));
    var color = mix(sky.horizon.rgb, sky.zenith.rgb, t);

    // Sunrise/sunset glow: a horizon band facing the sun, only at dawn/dusk.
    let glow = sky.sun_dir.w;
    if (glow > 0.0) {
        let toward = max(dot(dir, sky.sun_dir.xyz), 0.0);
        let band = 1.0 - smoothstep(0.0, 0.35, abs(dir.y));
        let amount = clamp(pow(toward, 4.0) * band * glow, 0.0, 1.0);
        color = mix(color, sky.sunset.rgb, amount);
    }

    // Volumetric clouds, composited front-to-back over the sky.
    if (sky.cloud_params.x > 0.5) {
        let c = clouds(sky.camera_pos.xyz, dir, sky.sun_dir.xyz, sky.camera_pos.w);
        color = color * (1.0 - c.a) + c.rgb;
    }

    return vec4<f32>(color, 1.0);
}
