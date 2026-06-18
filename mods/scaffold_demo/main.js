/// <reference path="../mc.d.ts" />
//
// Scaffold (demo) — press G to toggle. While on, if the block directly under
// your feet is air, it aims at a REAL adjacent block face and places a block
// there, bridging as you walk.
//
// Vanilla-legitimacy — nothing here is something a manual player couldn't do:
//  - Only places against an EXISTING full-cube face (never mid-air). If there
//    is no support block, it does nothing — exactly like vanilla.
//  - Sends the look to the server BEFORE the placement: it aims one tick, and
//    only places on a later tick once that rotation has actually ridden a
//    movement packet. So at placement time the server sees you looking at the
//    block — like a real player, not a teleporting crosshair.
//  - The turn is "silent" by default (the server sees it, your camera stays
//    free). Set `silent: false` in this mod's config.json for a fully
//    hand-reproducible camera turn instead.
//  - Respects the vanilla right-click cooldown (4 ticks) and needs a block in
//    hand. The host additionally refuses any placement that would clip you or
//    target a non-replaceable block (the vanilla onPlayerRightClick gate).

const KEY = "KeyG";
const PLACE_DELAY = 4; // vanilla rightClickDelayTimer, in ticks
const cfg = mc.config.load({ silent: true });

let enabled = false;
let tick = 0;
let lastPlace = -100;
let aimedKey = null; // the target we've been aiming at
let aimedTick = -100; // when we started aiming at it
let status = "off";

// 1.8 face ids: 0 -Y, 1 +Y, 2 -Z, 3 +Z, 4 -X, 5 +X. For each neighbour of the
// target T, `face` is the face of that neighbour pointing back at T (the face a
// player would click to place into T).
const NEIGHBORS = [
  { d: [0, -1, 0], face: 1 }, // support below  -> click its top (+Y)
  { d: [-1, 0, 0], face: 5 }, // support west   -> click its +X
  { d: [1, 0, 0], face: 4 }, //  support east   -> click its -X
  { d: [0, 0, -1], face: 3 }, // support north  -> click its +Z
  { d: [0, 0, 1], face: 2 }, //  support south  -> click its -Z
  { d: [0, 1, 0], face: 0 }, //  support above  -> click its bottom (-Y)
];
// outward unit vector of each face (for the aim point)
const FACE_DIR = [
  [0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0],
];
// in-block cursor (0..15) at the centre of each clicked face
const FACE_CURSOR = [
  [8, 0, 8], [8, 15, 8], [8, 8, 0], [8, 8, 15], [0, 8, 8], [15, 8, 8],
];

const isBlockItem = (it) => it && it.id >= 1 && it.id <= 255;
const solidFace = (b) => b && !b.isAir && b.opaque;

mc.keyBinding("Scaffold toggle", KEY).onPress(() => {
  enabled = !enabled;
  if (!enabled) {
    mc.player.clearRotation();
    status = "off";
  }
});

mc.on("tick", () => {
  tick++;
  if (!enabled) return;
  if (!mc.connection.connected) {
    status = "not connected";
    return;
  }

  const p = mc.player;
  const reset = (msg) => {
    p.clearRotation();
    aimedKey = null;
    status = msg;
  };

  if (!isBlockItem(p.heldItem())) return reset("no block in hand");

  // Target = the block position directly under the feet.
  const tx = Math.floor(p.x);
  const ty = Math.floor(p.y) - 1;
  const tz = Math.floor(p.z);
  if (!mc.world.getBlock(tx, ty, tz).isAir) return reset("supported"); // already standing on something

  // Find a real full-cube face to place against (no mid-air placement).
  let pick = null;
  for (const n of NEIGHBORS) {
    const nx = tx + n.d[0], ny = ty + n.d[1], nz = tz + n.d[2];
    if (solidFace(mc.world.getBlock(nx, ny, nz))) {
      pick = { nx, ny, nz, face: n.face };
      break;
    }
  }
  if (!pick) return reset("no support"); // can't place — would be mid-air

  // Aim at the centre of the clicked face so the look hits a real face.
  const fd = FACE_DIR[pick.face];
  const ax = pick.nx + 0.5 + 0.5 * fd[0];
  const ay = pick.ny + 0.5 + 0.5 * fd[1];
  const az = pick.nz + 0.5 + 0.5 * fd[2];
  const eyeY = p.y + (p.sneaking ? 1.54 : 1.62);
  const dx = ax - p.x, dy = ay - eyeY, dz = az - p.z;
  const yaw = (Math.atan2(-dx, dz) * 180) / Math.PI;
  const pitch = (-Math.atan2(dy, Math.hypot(dx, dz)) * 180) / Math.PI;
  p.setRotation(yaw, pitch, { silent: cfg.silent });

  // Only place once this look has actually been SENT — i.e. we've been aiming
  // at the same target for at least one full tick, so a movement packet carried
  // the rotation. That makes the placement look identical to a manual one.
  const key = tx + "," + ty + "," + tz + "|" + pick.face;
  if (key !== aimedKey) {
    aimedKey = key;
    aimedTick = tick;
    status = "aiming";
    return;
  }
  if (tick - aimedTick < 1) {
    status = "aiming";
    return;
  }
  if (tick - lastPlace < PLACE_DELAY) {
    status = "cooldown";
    return;
  }

  p.placeBlock(pick.nx, pick.ny, pick.nz, pick.face, FACE_CURSOR[pick.face]);
  lastPlace = tick;
  status = "placed " + tx + "," + ty + "," + tz;
});

mc.drawHud((ctx) => {
  hud.text(4, ctx.height - 20, "Scaffold [G]: " + (enabled ? "ON" : "off"), {
    color: enabled ? 0x55ff55ff : 0xaaaaaaff,
  });
  if (enabled) hud.text(4, ctx.height - 11, status, { color: 0xffffaaff });
});
