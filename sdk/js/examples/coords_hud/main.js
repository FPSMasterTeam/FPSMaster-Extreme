// Coordinates HUD — the canonical "read player state + draw HUD" mod.
//
// `mc.player` is a live snapshot (cache it in a local, like vanilla); `hud.*`
// records draw commands the host replays into the frame. Coords are GUI pixels.

const DIRS = ["South", "SW", "West", "NW", "North", "NE", "East", "SE"];

function facing(yaw) {
  const i = Math.round((((yaw % 360) + 360) % 360) / 45) % 8;
  return DIRS[i];
}

mc.drawHud((ctx) => {
  const p = mc.player;
  const lines = [
    "XYZ " + p.x.toFixed(2) + " / " + p.y.toFixed(2) + " / " + p.z.toFixed(2),
    "Facing " + facing(p.yaw) + " (" + p.yaw.toFixed(1) + "°)",
    "HP " + p.health.toFixed(1) + "   Food " + p.food,
  ];
  let y = 2;
  for (const line of lines) {
    hud.text(2, y, line, { color: 0xffff55ff, scale: 1 });
    y += 10;
  }
});

mc.on("load", () => mc.log("coords_hud ready"));
