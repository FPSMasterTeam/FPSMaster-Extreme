// Chat keyword alert — demonstrates on('chat') + playSound + a timed HUD banner.
//
// (Recoloring the chat text itself is a host-side concern; the JS layer reacts
// to the event instead — a ping plus a banner that fades after a few seconds.)

const KEYWORDS = ["diamond", "help", "fpsmaster"];
let flashUntil = 0;
let tick = 0;

mc.on("tick", () => {
  tick++;
});

mc.on("chat", (e) => {
  const text = (e.text || "").toLowerCase();
  if (KEYWORDS.some((k) => text.includes(k))) {
    flashUntil = tick + 60; // ~3s at 20 tps
    const p = mc.player;
    mc.world.playSound("random.orb", p.x, p.y, p.z, { pitch: 1.5 });
    mc.log("chat keyword alert:", e.text);
  }
});

mc.drawHud((ctx) => {
  if (tick < flashUntil) {
    const w = 170;
    const x = ((ctx.width - w) / 2) | 0;
    hud.rect(x, 24, w, 14, [200, 60, 60, 200]);
    hud.text(x + 6, 27, "Keyword mentioned in chat!", { color: 0xffffffff });
  }
});
