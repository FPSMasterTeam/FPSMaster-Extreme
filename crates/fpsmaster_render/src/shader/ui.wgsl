// GPU UI batcher: textured quads emitted by `UiFrame::tessellate` in place of
// the old CPU full-screen rasterize+upload. Each vertex carries clip-space
// position, an atlas UV and a colour. Solid fills (rects, gradients, lines,
// item swatches) sample a 1×1 white texel; text samples the font atlas; images
// sample the GUI/item/block atlases. One draw call per contiguous same-texture
// run keeps painter's order exact.
//
// Colour: vertex colours are sRGB-encoded 0..1 (converted to linear here); atlas
// textures are bound as `*Srgb`, so `textureSample` returns linear too. The
// linear product is written to the sRGB swapchain, which re-encodes it — so a
// fully-opaque glyph/sprite lands on exactly its source colour, matching the old
// CPU path. Alpha stays linear (coverage), never gamma-converted.

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@group(0) @binding(0)
var ui_texture: texture_2d<f32>;

@group(0) @binding(1)
var ui_sampler: sampler;

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.position = vec4<f32>(in.pos, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(ui_texture, ui_sampler, in.uv);
    let rgb = srgb_to_linear(in.color.rgb) * texel.rgb;
    return vec4<f32>(rgb, in.color.a * texel.a);
}
