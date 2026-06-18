// Block tint — the "preset render modification" example.
//
// mc.world.setBlockTint is part of the closed, host-implemented preset set: it
// registers a static per-block tint the chunk mesher reads (no per-block JS
// callback). Color accepts a 0xRRGGBBAA int, an [r,g,b(,a)] array, or "#rrggbb".

mc.on("load", () => {
  mc.world.setBlockTint(1, [120, 200, 255]); // stone        -> icy blue
  mc.world.setBlockTint(98, "#88ff88"); //      stone bricks -> mint
  mc.world.setBlockTint(4, 0xffaa55ff); //      cobblestone  -> amber
  mc.log("block_tint: registered 3 preset tints");
});
