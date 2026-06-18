// Block tint — the "preset render modification" example.
//
// setBlockTint is part of the closed, host-implemented preset set: it registers
// a static per-block tint that the chunk mesher reads (no per-block JS callback).
// Color accepts a 0xRRGGBBAA int, an [r,g,b(,a)] array, or "#rrggbb".

recraft.onLoad(() => {
  recraft.setBlockTint(1, [120, 200, 255]); // stone   -> icy blue
  recraft.setBlockTint(98, "#88ff88"); // stone bricks -> mint
  recraft.setBlockTint(4, 0xffaa55ff); // cobblestone  -> amber
  recraft.log("block_tint: registered 3 preset tints");
});
