/// <reference path="../mc.d.ts" />
//
// Scaffold (demo) — press G to toggle. While on, if the block directly under
// your feet is air, it aims at a REAL adjacent block face and places a block
// there, bridging as you walk.
//
// Vanilla-legitimacy (Grim-oriented) — what the mod and the host guarantee:
//  - Only places against an EXISTING full-cube face you could actually point at:
//    the face must front the eye AND have clear line of sight (no placing
//    through, behind, or under blocks). No reachable support → does nothing.
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
// Candidate supports (no "above": you can't click the bottom of a block over
// your head). The `face` is the face of that neighbour pointing back at T.
const NEIGHBORS = [
  { d: [0, -1, 0], face: 1 }, // support below  -> click its top (+Y)
  { d: [-1, 0, 0], face: 5 }, // support west   -> click its +X
  { d: [1, 0, 0], face: 4 }, //  support east   -> click its -X
  { d: [0, 0, -1], face: 3 }, // support north  -> click its +Z
  { d: [0, 0, 1], face: 2 }, //  support south  -> click its -Z
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

// Does the eye actually have line of sight to face-centre (ax,ay,az) of block
// (nx,ny,nz)? Step along the ray: the first solid block it enters must be that
// block — otherwise something is in the way (you couldn't point at it). Also
// enforces vanilla reach.
function hasLineOfSight(ex, ey, ez, ax, ay, az, nx, ny, nz) {
  const dx = ax - ex, dy = ay - ey, dz = az - ez;
  const dist = Math.hypot(dx, dy, dz);
  if (dist > 4.5) return false;
  const steps = Math.ceil(dist / 0.0625);
  for (let i = 1; i < steps; i++) {
    const t = i / steps;
    const bx = Math.floor(ex + dx * t);
    const by = Math.floor(ey + dy * t);
    const bz = Math.floor(ez + dz * t);
    if (bx === nx && by === ny && bz === nz) return true; // reached the support
    if (!mc.world.getBlock(bx, by, bz).isAir) return false; // blocked first
  }
  return true;
}

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

  // Target = the block directly under your CURRENT feet (where you actually are).
  const tx = Math.floor(p.x);
  const ty = Math.floor(p.y) - 1;
  const tz = Math.floor(p.z);
  if (!mc.world.getBlock(tx, ty, tz).isAir) return reset("supported"); // already standing on something

  // The mod runs BEFORE physics, so aim from the predicted post-move EYE (where
  // the flying packet will report you): current eye + this tick's velocity. The
  // look + the reachability check then match where Grim ray-traces the place.
  const ex = p.x + p.vx;
  const ey = p.y + p.vy + (p.sneaking ? 1.54 : 1.62);
  const ez = p.z + p.vz;

  // Pick a solid neighbour whose clicked face you can actually point at: the
  // face must point toward the eye AND have clear line of sight (no placing
  // through or behind blocks).
  let pick = null;
  for (const n of NEIGHBORS) {
    const nx = tx + n.d[0], ny = ty + n.d[1], nz = tz + n.d[2];
    if (!solidFace(mc.world.getBlock(nx, ny, nz))) continue;
    const fd = FACE_DIR[n.face];
    const ax = nx + 0.5 + 0.5 * fd[0];
    const ay = ny + 0.5 + 0.5 * fd[1];
    const az = nz + 0.5 + 0.5 * fd[2];
    if (fd[0] * (ex - ax) + fd[1] * (ey - ay) + fd[2] * (ez - az) <= 0) continue; // face away from eye
    if (!hasLineOfSight(ex, ey, ez, ax, ay, az, nx, ny, nz)) continue; // blocked
    pick = { nx, ny, nz, face: n.face, ax, ay, az };
    break;
  }
  if (!pick) return reset("no reachable support"); // nothing you could actually point at

  // Aim at the centre of the clicked face.
  const dx = pick.ax - ex, dy = pick.ay - ey, dz = pick.az - ez;
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
