// Chat keyword alert — demonstrates onChat + playSound + a timed HUD banner.
//
// (Recoloring the chat text itself is a host-side concern; the JS layer reacts
// to the event instead — a ping plus a banner that fades after a few seconds.)

const KEYWORDS = ["diamond", "help", "recraft"];
let flashUntil = 0;
let tick = 0;

recraft.onTick(() => {
  tick++;
});

recraft.onChat((e) => {
  const text = (e.text || "").toLowerCase();
  if (KEYWORDS.some((k) => text.includes(k))) {
    flashUntil = tick + 60; // ~3s at 20 tps
    const p = recraft.player();
    recraft.playSound("random.orb", p.x, p.y, p.z, { pitch: 1.5 });
    recraft.log("chat keyword alert:", e.text);
  }
});

recraft.drawHud((ctx) => {
  if (tick < flashUntil) {
    const w = 170;
    const x = ((ctx.width - w) / 2) | 0;
    hud.rect(x, 24, w, 14, [200, 60, 60, 200]);
    hud.text(x + 6, 27, "Keyword mentioned in chat!", { color: 0xffffffff });
  }
});
