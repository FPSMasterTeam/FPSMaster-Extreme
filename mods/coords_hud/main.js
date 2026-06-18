// Coordinates HUD — the canonical "read player state + draw HUD" mod.
//
// `recraft.player()` returns a live snapshot; `hud.*` records draw commands that
// the host replays into the frame. Coordinates are in GUI pixels.

const DIRS = ["South", "SW", "West", "NW", "North", "NE", "East", "SE"];

function facing(yaw) {
  const i = Math.round((((yaw % 360) + 360) % 360) / 45) % 8;
  return DIRS[i];
}

recraft.drawHud((ctx) => {
  const p = recraft.player();
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

recraft.onLoad(() => recraft.log("coords_hud ready"));
