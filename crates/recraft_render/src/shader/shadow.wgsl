// Sun shadow-map pass: render chunk geometry depth from the light's point of
// view. Solid uses no fragment (depth only); cutout alpha-tests so leaves and
// plants cast shaped shadows. The pass has only a depth attachment (no colour).

struct ShadowUniform {
    light_view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> shadow: ShadowUniform;

@group(1) @binding(0)
var block_atlas: texture_2d<f32>;
@group(1) @binding(1)
var block_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
};

@vertex
fn vs_main(
    @location(0) pos_light: vec4<i32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) normal: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    let world = vec3<f32>(f32(pos_light.x), f32(pos_light.y), f32(pos_light.z)) / 64.0;
    out.pos = shadow.light_view_proj * vec4<f32>(world, 1.0);
    out.uv = uv;
    out.alpha = color.a;
    return out;
}

// Cutout caster: discard transparent texels so the shadow takes the sprite shape.
@fragment
fn fs_cutout(in: VsOut) {
    let a = textureSample(block_atlas, block_sampler, in.uv).a * in.alpha;
    if (a < 0.5) {
        discard;
    }
}
