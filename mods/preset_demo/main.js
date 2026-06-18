// Preset render demo — toggle every preset render modification with a key, and
// show their state as a HUD legend. A hands-on way to eyeball each preset.
//
//   F7  fullbright          F9  chunk borders
//   [   entity hitboxes     ]   cycle nametag scale  \  cycle particle density
//
// (The targeted-block outline is built into recraft now, like vanilla.)

/// <reference path="../recraft.d.ts" />

const s = { fullbright: false, chunks: false, ebox: false };
const nametags = [1.0, 2.0, 0.5];
const densities = [1.0, 0.2, 3.0];
let ni = 0;
let di = 0;

recraft.onLoad(() => recraft.log("preset_demo: F7 fullbright, F9 chunks, [ hitboxes, ] nametag, \\ density"));

recraft.onKey((e) => {
  if (!e.pressed) return;
  switch (e.key) {
    case "F7":
      s.fullbright = !s.fullbright;
      recraft.fullbright(s.fullbright);
      return true;
    case "F9":
      s.chunks = !s.chunks;
      recraft.chunkBorders(s.chunks);
      return true;
    case "BracketLeft":
      s.ebox = !s.ebox;
      recraft.entityBox("", [1.0, 1.0, 1.0], s.ebox);
      return true;
    case "BracketRight":
      ni = (ni + 1) % nametags.length;
      recraft.nametagScale(nametags[ni]);
      return true;
    case "Backslash":
      di = (di + 1) % densities.length;
      recraft.particleDensity(densities[di]);
      return true;
  }
});

recraft.drawHud((ctx) => {
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
