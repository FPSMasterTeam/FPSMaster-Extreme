// Motion-vector pass: per-pixel screen-space velocity (current frame -> previous
// frame) from camera reprojection. Reads the final world depth and the NDC->NDC
// reprojection `prev_view_proj` (prev_abs . inv(cur_abs), composed in f64 on the
// CPU) that the post pass already uses for motion blur.
//
// Camera-only: static terrain is exact. Entities that move independently of the
// camera are NOT captured here — they need a per-object previous transform
// written from the geometry pass (a later phase). DLSS/TAA tolerate this (the
// world is overwhelmingly static voxels); moving mobs will ghost until then.
//
// The reprojection uses the UN-jittered matrices, so the output is jitter-free
// (the jitter offset is handed to the temporal resolve separately). Output RG =
// cur_uv - prev_uv: subtract it from a pixel's uv to fetch its previous-frame
// location.

struct PostCamera {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};

// Bound as an unfilterable float (not texture_depth_2d) so the GL backend emits a
// plain sampler2D with a textureLoad overload — matches the post pass.
@group(0) @binding(0) var depth_tex: texture_2d<f32>;
@group(0) @binding(1) var<uniform> cam: PostCamera;

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

@fragment
fn fs_main(in: VsOut) -> @location(0) vec2<f32> {
    let dims = vec2<f32>(textureDimensions(depth_tex));
    let px = vec2<i32>(clamp(in.uv * dims, vec2<f32>(0.0), dims - 1.0));
    let depth = textureLoad(depth_tex, px, 0).r;
    let cur_clip = vec4<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0, depth, 1.0);
    let prev_clip = cam.prev_view_proj * cur_clip;
    let prev_uv = vec2<f32>(
        prev_clip.x / prev_clip.w * 0.5 + 0.5,
        0.5 - prev_clip.y / prev_clip.w * 0.5,
    );
    return in.uv - prev_uv;
}
