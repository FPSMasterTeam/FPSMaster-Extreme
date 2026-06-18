// Preset render demo — toggle every preset render modification with a key, and
// show their state as a HUD legend. A hands-on way to eyeball each preset.
//
//   F7  fullbright          F9  chunk borders
//   [   entity hitboxes     ]   cycle nametag scale  \  cycle particle density
//
// (The targeted-block outline is built into recraft now, like vanilla.)

/// <reference path="../mc.d.ts" />

const s = { fullbright: false, chunks: false, ebox: false };
const nametags = [1.0, 2.0, 0.5];
const densities = [1.0, 0.2, 3.0];
let ni = 0;
let di = 0;

mc.on("load", () =>
  mc.log("preset_demo: F7 fullbright, F9 chunks, [ hitboxes, ] nametag, \\ density")
);

mc.on("key", (e) => {
  if (!e.pressed) return;
  switch (e.key) {
    case "F7":
      s.fullbright = !s.fullbright;
      mc.world.fullbright(s.fullbright);
      return true;
    case "F9":
      s.chunks = !s.chunks;
      mc.world.chunkBorders(s.chunks);
      return true;
    case "BracketLeft":
      s.ebox = !s.ebox;
      // Colors are 0-255 (or 0xRRGGBBAA / "#fff"); [255,255,255] = white.
      mc.world.entityBox("", [255, 255, 255], s.ebox);
      return true;
    case "BracketRight":
      ni = (ni + 1) % nametags.length;
      mc.world.nametagScale(nametags[ni]);
      return true;
    case "Backslash":
      di = (di + 1) % densities.length;
      mc.world.particleDensity(densities[di]);
      return true;
  }
});

mc.drawHud((ctx) => {
  const yes = 0xffff_55ff;
  const no = 0xaaaaaaff;
  const rows = [
    ["F7 fullbright", s.fullbright],
    ["F9 chunkBorders", s.chunks],
    ["[ hitboxes", s.ebox],
    ["] nametag x" + nametags[ni], ni !== 0],
    ["\\ particles x" + densities[di], di !== 0],
  ];
  let y = ctx.height - 9 * rows.length - 4;
  for (const [label, on] of rows) {
    hud.text(4, y, label + (typeof on === "boolean" ? (on ? "  ON" : "") : ""), {
      color: on ? yes : no,
    });
    y += 9;
  }
});
