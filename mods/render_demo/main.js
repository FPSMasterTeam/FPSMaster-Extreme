// Render API demo (0.3) — exercises the new render surface so you can eyeball it:
//
//   * a procedurally-built RGBA texture drawn with hud.image (whole + sub-rect)
//   * hud.line and hud.gradient primitives
//   * F10 toggles a full-screen post effect (grayscale) — custom WGSL
//
// Textures must be registered inside a hook (the command queue isn't live during
// top-level eval), so the handle is built in `on('load')`.

/// <reference path="../mc.d.ts" />

let tex = 0;
let post = false;

// A 32x32 RGBA checker/gradient so the sub-rect blit is visibly different.
function buildTexture() {
  const w = 32,
    h = 32;
  const px = new Array(w * h * 4);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4;
      const checker = ((x >> 3) + (y >> 3)) & 1;
      px[i] = checker ? 255 : (x * 8) & 255; // R
      px[i + 1] = (y * 8) & 255; // G
      px[i + 2] = checker ? 64 : 200; // B
      px[i + 3] = 255; // A
    }
  }
  return mc.registerTexture(px, w, h);
}

// Grayscale post effect. `effect` gets the scene color; U.time/U.resolution are
// in scope if you want animation.
const GRAYSCALE = `
fn effect(uv: vec2<f32>, color: vec4<f32>) -> vec4<f32> {
  let g = dot(color.rgb, vec3<f32>(0.299, 0.587, 0.114));
  return vec4<f32>(vec3<f32>(g), color.a);
}`;

mc.on("load", () => {
  tex = buildTexture();
  mc.log("render_demo: F10 toggles the grayscale post effect");
});

mc.on("key", (e) => {
  if (e.pressed && e.key === "F10") {
    post = !post;
    if (post) mc.setPostEffect(GRAYSCALE);
    else mc.clearPostEffect();
    return true;
  }
});

mc.drawHud((ctx) => {
  const x = ctx.width - 76;
  const y = 4;
  // Whole texture (64x64), then a 16x16 sub-rect of it scaled to 32x32.
  if (tex) {
    hud.image(x, y, 64, 64, tex);
    hud.image(x, y + 68, 32, 32, tex, { src: [0, 0, 16, 16] });
  }
  // A gradient strip and a diagonal line over it.
  hud.gradient(x, y + 104, 64, 12, 0x2266ffff, 0x22ffaaff);
  hud.line(x, y + 104, x + 64, y + 116, 0xffffffff, 2);
  hud.text(x, y + 120, post ? "post: ON (F10)" : "post: off (F10)", {
    color: post ? 0xffff55ff : 0xaaaaaaff,
  });
});
