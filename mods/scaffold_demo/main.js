/// <reference path="../mc.d.ts" />
//
// Scaffold (demo) — press G to toggle. While on, if the block directly under
// your feet is air, it aims at a REAL adjacent block face and places a block
// there, bridging as you walk.
//
// Vanilla-legitimacy (Grim-oriented) — what the mod and the host guarantee:
//  - The clicked block + face come from a real voxel ray cast along the look it
//    will send (same as vanilla/Grim getLook), so the placement is always the
//    face the crosshair actually hits — never through, behind, or under a block.
//    No face whose ray-hit lands the block at the target → does nothing.
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

// Unit look vector from yaw/pitch (vanilla convention: yaw 0 → +Z, pitch + → down).
function lookVec(yaw, pitch) {
  const yr = (yaw * Math.PI) / 180, pr = (pitch * Math.PI) / 180;
  const cp = Math.cos(pr);
  return [-Math.sin(yr) * cp, -Math.sin(pr), Math.cos(yr) * cp];
}

// Voxel DDA ray cast from (ox,oy,oz) along unit dir; returns the first solid
// block hit and the face it ENTERED through (the face you'd be clicking), or
// null within `maxDist`. This is the real "what does my crosshair hit" — the
// placement is derived from it so the look and the clicked face always agree.
function raycast(ox, oy, oz, dir, maxDist) {
  const [dx, dy, dz] = dir;
  let x = Math.floor(ox), y = Math.floor(oy), z = Math.floor(oz);
  const sx = Math.sign(dx), sy = Math.sign(dy), sz = Math.sign(dz);
  const tdx = dx !== 0 ? 1 / Math.abs(dx) : Infinity;
  const tdy = dy !== 0 ? 1 / Math.abs(dy) : Infinity;
  const tdz = dz !== 0 ? 1 / Math.abs(dz) : Infinity;
  const init = (o, d) =>
    d > 0 ? (Math.floor(o) + 1 - o) / d : d < 0 ? (o - Math.floor(o)) / -d : Infinity;
  let tmx = init(ox, dx), tmy = init(oy, dy), tmz = init(oz, dz);
  let t = 0, face = -1;
  while (t <= maxDist) {
    if (tmx <= tmy && tmx <= tmz) {
      x += sx; t = tmx; tmx += tdx; face = sx > 0 ? 4 : 5; // entered -X / +X face
    } else if (tmy <= tmz) {
      y += sy; t = tmy; tmy += tdy; face = sy > 0 ? 0 : 1; // entered -Y / +Y face
    } else {
      z += sz; t = tmz; tmz += tdz; face = sz > 0 ? 2 : 3; // entered -Z / +Z face
    }
    if (t > maxDist) break;
    const b = mc.world.getBlock(x, y, z);
    if (b && !b.isAir && b.opaque) return { x, y, z, face };
  }
  return null;
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

  // For each candidate support, aim at its face centre and RAY CAST with that
  // exact look. Keep it only if the ray's first hit is a face whose placement
  // lands the new block at T — then the look provably hits the clicked face, so
  // Grim's own ray cast agrees. This also rejects occluded / behind-block spots.
  let place = null;
  for (const n of NEIGHBORS) {
    const nx = tx + n.d[0], ny = ty + n.d[1], nz = tz + n.d[2];
    if (!solidFace(mc.world.getBlock(nx, ny, nz))) continue;
    const fd = FACE_DIR[n.face];
    const ax = nx + 0.5 + 0.5 * fd[0], ay = ny + 0.5 + 0.5 * fd[1], az = nz + 0.5 + 0.5 * fd[2];
    const dx = ax - ex, dy = ay - ey, dz = az - ez;
    const horiz = Math.hypot(dx, dz);
    // Straight-down aim keeps the real yaw (so the strafe-remap leaves walking
    // alone); a side face needs a real yaw turn.
    const yaw = horiz < 0.25 ? p.yaw : (Math.atan2(-dx, dz) * 180) / Math.PI;
    const pitch = (-Math.atan2(dy, horiz) * 180) / Math.PI;
    const hit = raycast(ex, ey, ez, lookVec(yaw, pitch), 4.5);
    if (!hit) continue;
    const hd = FACE_DIR[hit.face];
    if (hit.x + hd[0] === tx && hit.y + hd[1] === ty && hit.z + hd[2] === tz) {
      place = { nx: hit.x, ny: hit.y, nz: hit.z, face: hit.face, yaw, pitch };
      break;
    }
  }
  if (!place) return reset("no reachable support"); // nothing you could actually point at

  p.setRotation(place.yaw, place.pitch, { silent: cfg.silent });

  // Only place once this look has actually been SENT — i.e. we've been aiming
  // at the same target for at least one full tick, so a movement packet carried
  // the rotation. That makes the placement look identical to a manual one.
  const key = tx + "," + ty + "," + tz + "|" + place.face;
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

  p.placeBlock(place.nx, place.ny, place.nz, place.face, FACE_CURSOR[place.face]);
  lastPlace = tick;
  status = "placed " + tx + "," + ty + "," + tz;
});

mc.drawHud((ctx) => {
  hud.text(4, ctx.height - 20, "Scaffold [G]: " + (enabled ? "ON" : "off"), {
    color: enabled ? 0x55ff55ff : 0xaaaaaaff,
  });
  if (enabled) hud.text(4, ctx.height - 11, status, { color: 0xffffaaff });
});
