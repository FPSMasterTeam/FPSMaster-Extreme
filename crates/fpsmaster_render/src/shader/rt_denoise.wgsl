// Edge-aware spatial denoise of the screen-space AO buffer.
//
// A single wide (à-trous-dilated) bilateral pass: averages neighbours weighted by
// world-space distance to the centre surface, so AO blurs freely across a flat face
// but not across geometry steps/edges. World-position weighting (reconstructed from
// depth) is robust where a raw non-linear depth difference is not. One pass is enough
// to resolve the few-sample AO noise before it is composited onto the scene.
struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};
@group(0) @binding(0) var ao_tex: texture_2d<f32>;
@group(0) @binding(1) var depth_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> cam: PostCamera;

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

fn world_at(px: vec2<i32>, dims: vec2<i32>, depth: f32) -> vec3<f32> {
    let uv = (vec2<f32>(px) + vec2<f32>(0.5)) / vec2<f32>(dims);
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let w = cam.inv_view_proj * clip;
    return w.xyz / w.w;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(ao_tex));
    let c = vec2<i32>(clamp(in.uv * vec2<f32>(dims), vec2<f32>(0.0), vec2<f32>(dims) - vec2<f32>(1.0)));
    let cd = textureLoad(depth_tex, c, 0).r;
    if (cd >= 1.0) {
        return vec4<f32>(1.0);
    }
    let cp = world_at(c, dims, cd);
    // à-trous dilation: a 7x7 tap grid with a 2px step covers ~12px while staying cheap.
    let step = 2;
    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var dy = -3; dy <= 3; dy = dy + 1) {
        for (var dx = -3; dx <= 3; dx = dx + 1) {
            let p = clamp(c + vec2<i32>(dx, dy) * step, vec2<i32>(0), dims - vec2<i32>(1));
            let d = textureLoad(depth_tex, p, 0).r;
            if (d >= 1.0) {
                continue; // skip sky neighbours
            }
            let wp = world_at(p, dims, d);
            let diff = wp - cp;
            // World-space falloff: ~0.5-block sigma — blurs within a face, sharp at
            // 1-block steps (dist 1 -> weight e^-4 ~= 0.018).
            let dw = exp(-dot(diff, diff) * 4.0);
            let r2 = f32(dx * dx + dy * dy);
            let sw = exp(-r2 * 0.15);
            let w = dw * sw;
            sum = sum + textureLoad(ao_tex, p, 0).rgb * w;
            wsum = wsum + w;
        }
    }
    return vec4<f32>(select(vec3<f32>(1.0), sum / wsum, wsum > 0.0), 1.0);
}
