// Screen-space ambient occlusion for the rasterized path.
//
// The chunk mesh already carries vanilla's baked per-vertex AO, which darkens
// block CORNERS. What it cannot know about is contact between separate pieces of
// geometry — a chest against a wall, a mob's feet, a stair beside a pillar. This
// pass adds that, and deliberately stays subtle so it reads as contact shadow
// rather than as a second AO layer stacked on the first.
//
// Normals come from the G-buffer the chunk shader's `fs_normal` writes — an
// exact per-pixel geometric normal, which is stable under sub-pixel jitter in a
// way a depth derivative is not.
//
// That buffer is unavailable in one case: greedy (flat) meshing repurposes the
// vertex normal slot for the tile origin, so `fs_normal` cannot read one. There,
// and only there, normals fall back to a depth derivative SNAPPED TO THE NEAREST
// CARDINAL AXIS. Minecraft geometry is axis-aligned, so the snap is exact for
// essentially every surface and it suppresses the derivative's shimmer.
// `params.p.w` selects between the two.

struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};

struct SsaoParams {
    // x: world-space radius, y: strength, z: depth bias,
    // w: 1 = sample the normal G-buffer, 0 = derive normals from depth.
    p: vec4<f32>,
};

@group(0) @binding(0) var depth_tex: texture_2d<f32>;
@group(0) @binding(1) var<uniform> cam: PostCamera;
@group(0) @binding(2) var<uniform> params: SsaoParams;
@group(0) @binding(3) var normal_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    out.uv = vec2<f32>(x, y);
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

/// Camera-relative world position for a UV + non-linear depth. `inv_view_proj`
/// is the jittered, camera-relative inverse, so `length(p)` is the view distance.
fn world_from_depth(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let world = cam.inv_view_proj * clip;
    return world.xyz / world.w;
}

fn load_depth(px: vec2<i32>, dims: vec2<i32>) -> f32 {
    let c = clamp(px, vec2<i32>(0), dims - vec2<i32>(1));
    return textureLoad(depth_tex, c, 0).r;
}

/// Snap to the nearest cardinal axis. Minecraft surfaces are axis-aligned, so
/// this is not an approximation for terrain — and it is what makes a derived
/// normal stable enough to use without a G-buffer.
fn snap_axis(n: vec3<f32>) -> vec3<f32> {
    let a = abs(n);
    if (a.x >= a.y && a.x >= a.z) {
        return vec3<f32>(sign(n.x), 0.0, 0.0);
    }
    if (a.y >= a.z) {
        return vec3<f32>(0.0, sign(n.y), 0.0);
    }
    return vec3<f32>(0.0, 0.0, sign(n.z));
}

/// Interleaved-gradient noise on the PIXEL coordinate only.
///
/// Deliberately not seeded by frame or time. A frame-varying rotation makes the
/// AO field crawl while the camera is still, and with TAA off by default there
/// is nothing downstream to converge it (`rt_ao.wgsl` records the same lesson).
fn rotation_noise(px: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(px, vec2<f32>(0.06711056, 0.00583715))));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(depth_tex));
    let px = vec2<i32>(in.pos.xy);
    let depth = load_depth(px, dims);
    // Sky: nothing to occlude.
    if (depth >= 1.0) {
        return vec4<f32>(1.0);
    }

    let uv = in.uv;
    let p = world_from_depth(uv, depth);

    let texel = 1.0 / vec2<f32>(dims);
    var n: vec3<f32>;
    if (params.p.w > 0.5) {
        // Exact geometric normal, written by `fs_normal` as 0.5 + 0.5 * n.
        let encoded = textureLoad(normal_tex, px, 0).xyz * 2.0 - vec3<f32>(1.0);
        // The G-buffer is cleared to the encoded zero vector, so pixels no chunk
        // wrote (entities, the hand) decode to zero — fall back there.
        if (dot(encoded, encoded) > 0.01) {
            n = normalize(encoded);
        } else {
            n = vec3<f32>(0.0, 1.0, 0.0);
        }
    } else {
        // Derived normal, using the CLOSER neighbour on each axis so a depth
        // discontinuity does not bend it across a silhouette.
        let dr = world_from_depth(uv + vec2<f32>(texel.x, 0.0), load_depth(px + vec2<i32>(1, 0), dims));
        let dl = world_from_depth(uv - vec2<f32>(texel.x, 0.0), load_depth(px - vec2<i32>(1, 0), dims));
        let dd = world_from_depth(uv + vec2<f32>(0.0, texel.y), load_depth(px + vec2<i32>(0, 1), dims));
        let du = world_from_depth(uv - vec2<f32>(0.0, texel.y), load_depth(px - vec2<i32>(0, 1), dims));
        let ddx = select(p - dl, dr - p, length(dr - p) < length(dl - p));
        let ddy = select(p - du, dd - p, length(dd - p) < length(du - p));
        n = snap_axis(normalize(cross(ddx, ddy)));
    }

    // World units per pixel at this depth, so a fixed world radius maps to the
    // right screen radius without needing the forward projection matrix.
    let per_px = length(world_from_depth(uv + vec2<f32>(texel.x, 0.0), depth) - p);
    let radius_world = params.p.x;
    let radius_px = clamp(radius_world / max(per_px, 1e-6), 2.0, 48.0);

    // Eight directions, two radii, rotated per pixel by a fixed pattern.
    let angle = rotation_noise(in.pos.xy) * 6.2831853;
    let ca = cos(angle);
    let sa = sin(angle);
    var occlusion = 0.0;
    var samples = 0.0;
    for (var i = 0; i < 8; i = i + 1) {
        let a = f32(i) * 0.7853982; // 2pi / 8
        let base = vec2<f32>(cos(a), sin(a));
        let dir = vec2<f32>(base.x * ca - base.y * sa, base.x * sa + base.y * ca);
        for (var step = 1; step <= 2; step = step + 1) {
            let r = radius_px * (f32(step) / 2.0);
            let sp = px + vec2<i32>(dir * r);
            let sd = load_depth(sp, dims);
            if (sd >= 1.0) {
                samples = samples + 1.0;
                continue;
            }
            let s = world_from_depth(vec2<f32>(sp) * texel, sd);
            let v = s - p;
            let dist = length(v);
            if (dist < 1e-4) {
                continue;
            }
            // How far above the tangent plane the sample sits, biased so a
            // coplanar surface never shadows itself.
            let horizon = max(dot(n, v / dist) - params.p.z, 0.0);
            // Range check: geometry beyond the radius is a different surface,
            // not an occluder, and without this every silhouette grows a halo.
            let falloff = clamp(1.0 - dist / radius_world, 0.0, 1.0);
            occlusion = occlusion + horizon * falloff;
            samples = samples + 1.0;
        }
    }

    let ao = 1.0 - clamp(occlusion / max(samples, 1.0) * params.p.y, 0.0, 1.0);
    return vec4<f32>(ao, ao, ao, 1.0);
}
