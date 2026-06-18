/// <reference path="../mc.d.ts" />
//
// Scaffold (demo) — press G to toggle. While on, if the block directly under
// your feet is air, it aims at a REAL adjacent block face and places a block
// there, bridging as you walk.
//
// Vanilla-legitimacy (Grim-oriented) — what the mod and the host guarantee:
//  - Only places against an EXISTING full-cube face (never mid-air). No support
//    block → does nothing, exactly like vanilla.
//  - Aims one tick, places the next. The host sends ext interactions in the
//    pre-flying window (like vanilla), so by placement time the look has ridden
//    a flying packet → the server sees you looking at the block (RotationPlace /
//    Post order pass).
//  - Silent look keeps your camera free; the host drives MOVEMENT with the
//    silent yaw and snaps your input to the legal 8 directions, so the server's
//    movement prediction stays in sync (Simulation / GroundSpoof). When aiming
//    straight down (block below you) yaw is left alone, so walking is unaffected.
//  - The host snaps the silent look to your own mouse-rotation quantum (the
//    sensitivity factor), so the server-visible rotation deltas stay multiples
//    of it — Grim's AimModulo360 (rotation-GCD) check sees mouse-like rotation.
//  - Respects the vanilla right-click cooldown (4 ticks) and needs a block in
//    hand. The host's onPlayerRightClick gate still refuses any placement that
//    would clip you or target a non-replaceable block.
//
// Set `silent: false` in config.json to turn the real camera instead (no
// injected rotation at all — the camera visibly moves, like manual play).

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

  // The mod runs BEFORE physics, so p.{x,y,z} is the pre-move position but the
  // flying packet will carry the POST-move position. Aim from the predicted
  // post-move position (current + this tick's velocity) so the rotation matches
  // where Grim ray-traces the placement from.
  const fx = p.x + p.vx, fy = p.y + p.vy, fz = p.z + p.vz;

  // Target = the block position directly under the (predicted) feet.
  const tx = Math.floor(fx);
  const ty = Math.floor(fy) - 1;
  const tz = Math.floor(fz);
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
  const eyeY = fy + (p.sneaking ? 1.54 : 1.62);
  const dx = ax - fx, dy = ay - eyeY, dz = az - fz;
  const horiz = Math.hypot(dx, dz);
  // When the block is (almost) straight down, yaw is irrelevant to the aim — keep
  // the real camera yaw so the host's strafe-remap leaves movement untouched.
  // Only a side-face (horizontal bridging) genuinely needs a yaw turn, and then
  // movement is snapped to the nearest legal 8-direction by the host.
  const yaw = horiz < 0.25 ? p.yaw : (Math.atan2(-dx, dz) * 180) / Math.PI;
  const pitch = (-Math.atan2(dy, horiz) * 180) / Math.PI;
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
