// Post pass: HDR off-screen scene -> tone-mapped sRGB swapchain. Optional bloom
// (a bright-pass Gaussian disk sampled in HDR), exposure, ACES filmic tone map,
// then a light saturation / contrast grade. Also upscales for render scale < 1
// via the linear sampler. The colour target is sRGB, so the encode happens on
// write — this shader outputs linear [0,1].

struct Params {
    // x: bloom threshold, y: bloom intensity, z: texel.x, w: texel.y
    p: vec4<f32>,
    // x: exposure, y: saturation, z: contrast, w: bloom enabled (>0.5)
    q: vec4<f32>,
    // x: vignette amount, y: chromatic amount, z: dof strength, w: motion-blur strength
    r: vec4<f32>,
    // x: auto-exposure enabled, yzw: reserved
    s: vec4<f32>,
};

struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var depth_tex: texture_depth_2d;
@group(0) @binding(4) var<uniform> cam: PostCamera;

// Reconstruct world position from a screen UV + non-linear depth.
fn world_from_depth(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let world = cam.inv_view_proj * clip;
    return world.xyz / world.w;
}

fn load_depth(uv: vec2<f32>) -> f32 {
    let dims = vec2<f32>(textureDimensions(depth_tex));
    let px = vec2<i32>(clamp(uv * dims, vec2<f32>(0.0), dims - 1.0));
    return textureLoad(depth_tex, px, 0);
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen triangle.
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    out.uv = vec2<f32>(x, y);
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

// Narkowicz ACES filmic curve, scalar form.
fn aces_scalar(x: f32) -> f32 {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), 0.0, 1.0);
}

// Tone-map on luminance only and rescale the colour, so hue and saturation are
// preserved (no per-channel ACES hue shift / highlight desaturation). Keeps the
// filmic highlight roll-off without the colour cast.
fn tonemap(c: vec3<f32>) -> vec3<f32> {
    let l = max(dot(c, vec3<f32>(0.2126, 0.7152, 0.0722)), 1e-4);
    return c * (aces_scalar(l) / l);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let center = vec2<f32>(0.5, 0.5);
    let from_center = in.uv - center;

    // Chromatic aberration: spread the RGB taps radially, growing toward the
    // edges (like a real lens). r.y is the strength (0 = off).
    var color: vec3<f32>;
    if (params.r.y > 0.0) {
        let off = from_center * params.r.y;
        color = vec3<f32>(
            textureSample(scene_tex, scene_sampler, in.uv + off).r,
            textureSample(scene_tex, scene_sampler, in.uv).g,
            textureSample(scene_tex, scene_sampler, in.uv - off).b,
        );
    } else {
        color = textureSample(scene_tex, scene_sampler, in.uv).rgb;
    }

    // Depth of field: blur by circle-of-confusion from the distance between this
    // pixel and the auto-focused screen centre. r.z is the strength (0 = off).
    if (params.r.z > 0.0) {
        let texel = params.p.zw;
        let focus_w = world_from_depth(vec2<f32>(0.5, 0.5), load_depth(vec2<f32>(0.5, 0.5)));
        let focus_dist = length(focus_w - cam.camera_pos.xyz);
        let pix_w = world_from_depth(in.uv, load_depth(in.uv));
        let pix_dist = length(pix_w - cam.camera_pos.xyz);
        let coc = clamp(abs(pix_dist - focus_dist) / max(focus_dist, 1.0), 0.0, 1.0);
        let radius = coc * params.r.z * 8.0;
        if (radius > 0.5) {
            var acc = color;
            var n = 1.0;
            for (var k = 0; k < 8; k = k + 1) {
                let a = f32(k) / 8.0 * 6.2831853;
                let o = vec2<f32>(cos(a), sin(a)) * radius * texel;
                acc += textureSample(scene_tex, scene_sampler, in.uv + o).rgb;
                n += 1.0;
            }
            color = acc / n;
        }
    }

    // Motion blur: smear along the screen-space velocity from reprojecting this
    // pixel into the previous frame. r.w is the strength (0 = off).
    if (params.r.w > 0.0) {
        let pix_w = world_from_depth(in.uv, load_depth(in.uv));
        let prev_clip = cam.prev_view_proj * vec4<f32>(pix_w, 1.0);
        let prev_uv = vec2<f32>(
            prev_clip.x / prev_clip.w * 0.5 + 0.5,
            0.5 - prev_clip.y / prev_clip.w * 0.5,
        );
        var vel = (in.uv - prev_uv) * params.r.w;
        vel = clamp(vel, vec2<f32>(-0.04), vec2<f32>(0.04));
        if (dot(vel, vel) > 1e-9) {
            var acc = color;
            var n = 1.0;
            for (var k = 1; k <= 6; k = k + 1) {
                let t = f32(k) / 6.0;
                acc += textureSample(scene_tex, scene_sampler, in.uv - vel * t).rgb;
                n += 1.0;
            }
            color = acc / n;
        }
    }

    if (params.q.w > 0.5) {
        // Bright-pass bloom: a 7x7 Gaussian disk, keeping only the energy above
        // the threshold (meaningful now that the scene is HDR).
        let texel = params.p.zw;
        var bloom = vec3<f32>(0.0);
        var wsum = 0.0;
        for (var i = -3; i <= 3; i = i + 1) {
            for (var j = -3; j <= 3; j = j + 1) {
                let o = vec2<f32>(f32(i), f32(j));
                let w = exp(-dot(o, o) / 8.0);
                let s = textureSample(scene_tex, scene_sampler, in.uv + o * texel).rgb;
                let lum = dot(s, vec3<f32>(0.2126, 0.7152, 0.0722));
                let bright = max(lum - params.p.x, 0.0);
                bloom += s * (bright / max(lum, 0.0001)) * w;
                wsum += w;
            }
        }
        color += bloom / wsum * params.p.y;
    }

    color = color * params.q.x;        // exposure
    color = tonemap(color);            // filmic tone map (HDR -> linear [0,1])

    // Saturation around luma.
    let l = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    color = mix(vec3<f32>(l), color, params.q.y);
    // Contrast around mid-grey.
    color = clamp((color - 0.5) * params.q.z + 0.5, vec3<f32>(0.0), vec3<f32>(1.0));

    // Vignette: darken toward the corners. r.x is the strength (0 = off).
    if (params.r.x > 0.0) {
        let d = length(from_center);
        let v = smoothstep(0.9, 0.35, d); // 1 at centre -> 0 at the corners
        color = color * mix(1.0, v, params.r.x);
    }

    return vec4<f32>(color, 1.0);
}
