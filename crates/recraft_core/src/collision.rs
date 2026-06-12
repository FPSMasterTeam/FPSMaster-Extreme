//! Vanilla 1.8.9 block collision shapes, ported 1:1 from the decompiled
//! client (MCP 9.19 mappings). Each arm of [`add_block_collision_boxes`]
//! mirrors that block's `addCollisionBoxesToList` override (or the `Block`
//! default driven by its `getCollisionBoundingBox` /
//! `setBlockBoundsBasedOnState`). The server validates movement against these
//! exact shapes, so any deviation — a slab of the wrong height, a missing
//! fence arm — makes the local prediction drift and provokes a setback loop
//! on partial blocks.
//!
//! Notable vanilla facts preserved here:
//! - Fences, walls and closed fence gates collide 1.5 blocks tall.
//! - Farmland collides as a FULL cube (its 15/16 box is render-only in 1.8).
//! - Snow layers collide `(layers - 1) / 8` tall — one layer is a zero-height
//!   box that clamps falling motion but never blocks walking.
//! - Piston bases collide as full cubes even while extended (only the
//!   ray-trace box shrinks).
//! - Chests use their constructor box; the double-chest variants only exist
//!   on render/ray-trace paths (1.8's shared mutable bounds), so the
//!   deterministic collision box is the single-chest one.

use glam::DVec3;

use crate::blocks::{self, CollisionKind, Shape};
use crate::physics::Aabb;
use crate::{BlockState, World};

/// Block lookup the shape helpers run against, so both the live [`World`]
/// (physics) and the renderer's chunk snapshots can drive the same
/// neighbour-dependent vanilla shapes (stairs, fences, panes).
pub type BlockLookup<'a> = dyn Fn(i32, i32, i32) -> BlockState + 'a;

/// Append the collision boxes of the block at `(x, y, z)` that intersect
/// `mask`, in world space. Mirrors `Block.addCollisionBoxesToList` plus all
/// 1.8.9 overrides.
pub fn add_block_collision_boxes(
    world: &World,
    x: i32,
    y: i32,
    z: i32,
    mask: Aabb,
    out: &mut Vec<Aabb>,
) {
    let block = world.block_at(x, y, z);
    let id = block.id;
    let meta = block.meta;
    let lookup = |bx: i32, by: i32, bz: i32| world.block_at(bx, by, bz);
    let ctx = Ctx {
        base: DVec3::new(x as f64, y as f64, z as f64),
        mask,
    };

    match id {
        // No collision volume: air, fluids, rails, web, plants/crops, moving
        // piston (no tile entity here), torches, fire, redstone wire, signs,
        // levers, pressure plates, buttons, portals, stems, vines, nether
        // wart, end portal, tripwire (+hook), double plants, banners.
        0
        | 6
        | 8..=11
        | 27
        | 28
        | 30..=32
        | 36..=40
        | 50
        | 51
        | 55
        | 59
        | 63
        | 66
        | 68..=70
        | 72
        | 75..=77
        | 83
        | 90
        | 104..=106
        | 115
        | 119
        | 131
        | 132
        | 141..=143
        | 147
        | 148
        | 157
        | 175..=177 => {}

        // BlockSlab: bottom or top half by meta bit 8 (doubles are separate
        // full-cube ids).
        44 | 126 | 182 => {
            if meta & 8 != 0 {
                ctx.add(out, 0.0, 0.5, 0.0, 1.0, 1.0, 1.0);
            } else {
                ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.5, 1.0);
            }
        }

        // BlockStairs: base half-slab + neighbour-dependent quarter pieces.
        _ if is_stairs(id) => {
            for b in stair_boxes(&lookup, x, y, z) {
                ctx.add(out, b[0], b[1], b[2], b[3], b[4], b[5]);
            }
        }

        // BlockFence: 1.5-tall post/arms merged into up to two boxes.
        85 | 113 | 188..=192 => {
            let [n, s, w, e] = fence_connections(&lookup, id, x, y, z);
            let z0 = if n { 0.0 } else { 0.375 };
            let z1 = if s { 1.0 } else { 0.625 };
            if n || s {
                ctx.add(out, 0.375, 0.0, z0, 0.625, 1.5, z1);
            }
            let x0 = if w { 0.0 } else { 0.375 };
            let x1 = if e { 1.0 } else { 0.625 };
            if w || e || (!n && !s) {
                ctx.add(out, x0, 0.0, 0.375, x1, 1.5, 0.625);
            }
        }

        // BlockFenceGate: nothing while open, otherwise a 1.5-tall bar across
        // the facing axis.
        107 | 183..=187 => {
            if meta & 4 == 0 {
                if matches!(horizontal(meta & 3), Facing::South | Facing::North) {
                    ctx.add(out, 0.0, 0.0, 0.375, 1.0, 1.5, 0.625);
                } else {
                    ctx.add(out, 0.375, 0.0, 0.0, 0.625, 1.5, 1.0);
                }
            }
        }

        // BlockPane (glass panes / iron bars): thin bars by connection; an
        // unconnected pane is a full cross.
        _ if is_pane(id) => {
            let [n, s, w, e] = pane_connections(&lookup, id, x, y, z);
            if (!w || !e) && (w || e || n || s) {
                if w {
                    ctx.add(out, 0.0, 0.0, 0.4375, 0.5, 1.0, 0.5625);
                } else if e {
                    ctx.add(out, 0.5, 0.0, 0.4375, 1.0, 1.0, 0.5625);
                }
            } else {
                ctx.add(out, 0.0, 0.0, 0.4375, 1.0, 1.0, 0.5625);
            }
            if (!n || !s) && (w || e || n || s) {
                if n {
                    ctx.add(out, 0.4375, 0.0, 0.0, 0.5625, 1.0, 0.5);
                } else if s {
                    ctx.add(out, 0.4375, 0.0, 0.5, 0.5625, 1.0, 1.0);
                }
            } else {
                ctx.add(out, 0.4375, 0.0, 0.0, 0.5625, 1.0, 1.0);
            }
        }

        // BlockWall: one connection-shaped box raised to 1.5.
        139 => {
            let n = wall_connects(&lookup, x, y, z - 1);
            let s = wall_connects(&lookup, x, y, z + 1);
            let w = wall_connects(&lookup, x - 1, y, z);
            let e = wall_connects(&lookup, x + 1, y, z);
            let mut x0 = 0.25;
            let mut x1 = 0.75;
            let mut z0 = 0.25;
            let mut z1 = 0.75;
            if n {
                z0 = 0.0;
            }
            if s {
                z1 = 1.0;
            }
            if w {
                x0 = 0.0;
            }
            if e {
                x1 = 1.0;
            }
            if n && s && !w && !e {
                x0 = 0.3125;
                x1 = 0.6875;
            } else if !n && !s && w && e {
                z0 = 0.3125;
                z1 = 0.6875;
            }
            ctx.add(out, x0, 0.0, z0, x1, 1.5, z1);
        }

        // BlockDoor: one 3/16 panel per half; facing/open ride on the lower
        // half's meta, hinge on the upper half's (read unconditionally from
        // the neighbour, exactly like combineMetadata).
        64 | 71 | 193..=197 => {
            let b = door_box(&lookup, x, y, z, meta);
            ctx.add(out, b[0], b[1], b[2], b[3], b[4], b[5]);
        }

        // BlockTrapDoor: 3/16 plate at top/bottom, or against a wall if open.
        96 | 167 => {
            if meta & 4 != 0 {
                match meta & 3 {
                    0 => ctx.add(out, 0.0, 0.0, 0.8125, 1.0, 1.0, 1.0), // north
                    1 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 1.0, 0.1875), // south
                    2 => ctx.add(out, 0.8125, 0.0, 0.0, 1.0, 1.0, 1.0), // west
                    _ => ctx.add(out, 0.0, 0.0, 0.0, 0.1875, 1.0, 1.0), // east
                }
            } else if meta & 8 != 0 {
                ctx.add(out, 0.0, 0.8125, 0.0, 1.0, 1.0, 1.0);
            } else {
                ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.1875, 1.0);
            }
        }

        // BlockChest / ender chest: constructor box (see module docs).
        54 | 130 | 146 => ctx.add(out, 0.0625, 0.0, 0.0625, 0.9375, 0.875, 0.9375),

        // BlockSnow (layer): (layers - 1) / 8 tall; meta 0 is a zero-height
        // box that still clamps a falling player onto the surface below.
        78 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, (meta & 7) as f64 * 0.125, 1.0),

        // BlockCarpet.
        171 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.0625, 1.0),

        // BlockCactus: inset 1/16 on the sides and the top.
        81 => ctx.add(out, 0.0625, 0.0, 0.0625, 0.9375, 0.9375, 0.9375),

        // BlockFarmland: full cube for collision in 1.8.
        60 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0),

        // BlockSoulSand.
        88 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.875, 1.0),

        // BlockLilyPad: full footprint, 1/64 tall.
        111 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.015_625, 1.0),

        // BlockBed.
        26 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.5625, 1.0),

        // BlockLadder: 1/8 plate on the wall it hangs from (FACING points
        // away from the wall; non-horizontal metas fall back to north).
        65 => match front(meta) {
            Facing::South => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 1.0, 0.125),
            Facing::West => ctx.add(out, 0.875, 0.0, 0.0, 1.0, 1.0, 1.0),
            Facing::East => ctx.add(out, 0.0, 0.0, 0.0, 0.125, 1.0, 1.0),
            _ => ctx.add(out, 0.0, 0.0, 0.875, 1.0, 1.0, 1.0),
        },

        // BlockCake: west edge advances 2/16 per bite.
        92 => ctx.add(
            out,
            (1 + i32::from(meta) * 2) as f64 / 16.0,
            0.0,
            0.0625,
            0.9375,
            0.5,
            0.9375,
        ),

        // BlockRedstoneDiode (repeaters, comparators): 1/8 slab.
        93 | 94 | 149 | 150 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.125, 1.0),

        // BlockEnchantmentTable.
        116 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.75, 1.0),

        // BlockBrewingStand: centre post + 1/8 base slab.
        117 => {
            ctx.add(out, 0.4375, 0.0, 0.4375, 0.5625, 0.875, 0.5625);
            ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.125, 1.0);
        }

        // BlockCauldron: 5/16 floor + four 1/8-thick full-height walls.
        118 => {
            ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.3125, 1.0);
            ctx.add(out, 0.0, 0.0, 0.0, 0.125, 1.0, 1.0);
            ctx.add(out, 0.0, 0.0, 0.0, 1.0, 1.0, 0.125);
            ctx.add(out, 0.875, 0.0, 0.0, 1.0, 1.0, 1.0);
            ctx.add(out, 0.0, 0.0, 0.875, 1.0, 1.0, 1.0);
        }

        // BlockEndPortalFrame: 13/16 base, plus the eye knob when filled.
        120 => {
            ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.8125, 1.0);
            if meta & 4 != 0 {
                ctx.add(out, 0.3125, 0.8125, 0.3125, 0.6875, 1.0, 0.6875);
            }
        }

        // BlockDragonEgg.
        122 => ctx.add(out, 0.0625, 0.0, 0.0625, 0.9375, 1.0, 0.9375),

        // BlockCocoa: pod size grows with age, hangs off the facing side.
        127 => {
            let age = ((meta & 15) >> 2) as f64;
            let j = 4.0 + age * 2.0;
            let k = 5.0 + age * 2.0;
            let f = j / 2.0;
            let y0 = (12.0 - k) / 16.0;
            match horizontal(meta & 3) {
                Facing::South => ctx.add(
                    out,
                    (8.0 - f) / 16.0,
                    y0,
                    (15.0 - j) / 16.0,
                    (8.0 + f) / 16.0,
                    0.75,
                    0.9375,
                ),
                Facing::West => ctx.add(
                    out,
                    0.0625,
                    y0,
                    (8.0 - f) / 16.0,
                    (1.0 + j) / 16.0,
                    0.75,
                    (8.0 + f) / 16.0,
                ),
                Facing::North => ctx.add(
                    out,
                    (8.0 - f) / 16.0,
                    y0,
                    0.0625,
                    (8.0 + f) / 16.0,
                    0.75,
                    (1.0 + j) / 16.0,
                ),
                _ => ctx.add(
                    out,
                    (15.0 - j) / 16.0,
                    y0,
                    (8.0 - f) / 16.0,
                    0.9375,
                    0.75,
                    (8.0 + f) / 16.0,
                ),
            }
        }

        // BlockDaylightDetector (and inverted variant).
        151 | 178 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.375, 1.0),

        // BlockHopper: 10/16 bowl + four 1/8-thick full-height walls.
        154 => {
            ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.625, 1.0);
            ctx.add(out, 0.0, 0.0, 0.0, 0.125, 1.0, 1.0);
            ctx.add(out, 0.0, 0.0, 0.0, 1.0, 1.0, 0.125);
            ctx.add(out, 0.875, 0.0, 0.0, 1.0, 1.0, 1.0);
            ctx.add(out, 0.0, 0.0, 0.875, 1.0, 1.0, 1.0);
        }

        // BlockAnvil: full height, 1/8 inset across the facing axis.
        145 => {
            if matches!(horizontal(meta & 3), Facing::West | Facing::East) {
                ctx.add(out, 0.0, 0.0, 0.125, 1.0, 1.0, 0.875);
            } else {
                ctx.add(out, 0.125, 0.0, 0.0, 0.875, 1.0, 1.0);
            }
        }

        // BlockSkull: half-cube on the floor, shifted box on walls.
        144 => match front(meta & 7) {
            Facing::North => ctx.add(out, 0.25, 0.25, 0.5, 0.75, 0.75, 1.0),
            Facing::South => ctx.add(out, 0.25, 0.25, 0.0, 0.75, 0.75, 0.5),
            Facing::West => ctx.add(out, 0.5, 0.25, 0.25, 1.0, 0.75, 0.75),
            Facing::East => ctx.add(out, 0.0, 0.25, 0.25, 0.5, 0.75, 0.75),
            _ => ctx.add(out, 0.25, 0.0, 0.25, 0.75, 0.5, 0.75),
        },

        // BlockFlowerPot.
        140 => ctx.add(out, 0.3125, 0.0, 0.3125, 0.6875, 0.375, 0.6875),

        // BlockPistonBase: entity collision is always the full cube; only the
        // ray-trace box shrinks while extended.
        29 | 33 => ctx.add(out, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0),

        // BlockPistonExtension (head): face plate + arm.
        34 => {
            let facing = meta & 7;
            if facing <= 5 {
                match front(facing) {
                    Facing::Down => {
                        ctx.add(out, 0.0, 0.0, 0.0, 1.0, 0.25, 1.0);
                        ctx.add(out, 0.375, 0.25, 0.375, 0.625, 1.0, 0.625);
                    }
                    Facing::Up => {
                        ctx.add(out, 0.0, 0.75, 0.0, 1.0, 1.0, 1.0);
                        ctx.add(out, 0.375, 0.0, 0.375, 0.625, 0.75, 0.625);
                    }
                    Facing::North => {
                        ctx.add(out, 0.0, 0.0, 0.0, 1.0, 1.0, 0.25);
                        ctx.add(out, 0.25, 0.375, 0.25, 0.75, 0.625, 1.0);
                    }
                    Facing::South => {
                        ctx.add(out, 0.0, 0.0, 0.75, 1.0, 1.0, 1.0);
                        ctx.add(out, 0.25, 0.375, 0.0, 0.75, 0.625, 0.75);
                    }
                    Facing::West => {
                        ctx.add(out, 0.0, 0.0, 0.0, 0.25, 1.0, 1.0);
                        ctx.add(out, 0.375, 0.25, 0.25, 0.625, 0.75, 1.0);
                    }
                    Facing::East => {
                        ctx.add(out, 0.75, 0.0, 0.0, 1.0, 1.0, 1.0);
                        ctx.add(out, 0.0, 0.375, 0.25, 0.75, 0.625, 0.75);
                    }
                }
            }
        }

        // Everything else: the data file's cube/none assignment is already
        // vanilla-accurate (unknown ids collide as full cubes).
        _ => {
            for b in block.collision_boxes().as_slice() {
                ctx.add(
                    out, b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2],
                );
            }
        }
    }
}

struct Ctx {
    base: DVec3,
    mask: Aabb,
}

impl Ctx {
    /// Vanilla `Block.addCollisionBoxesToList` default: offset to world space
    /// and keep the box only if it (strictly) intersects the query mask.
    #[allow(clippy::too_many_arguments)]
    fn add(&self, out: &mut Vec<Aabb>, x0: f64, y0: f64, z0: f64, x1: f64, y1: f64, z1: f64) {
        let b = Aabb::new(
            self.base + DVec3::new(x0, y0, z0),
            self.base + DVec3::new(x1, y1, z1),
        );
        if self.mask.intersects(b) {
            out.push(b);
        }
    }
}

/// Vanilla `EnumFacing` (D-U-N-S-W-E order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Facing {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

/// `EnumFacing.getFront`.
fn front(index: u8) -> Facing {
    match index % 6 {
        0 => Facing::Down,
        1 => Facing::Up,
        2 => Facing::North,
        3 => Facing::South,
        4 => Facing::West,
        _ => Facing::East,
    }
}

/// `EnumFacing.getHorizontal` (S-W-N-E order).
fn horizontal(index: u8) -> Facing {
    match index % 4 {
        0 => Facing::South,
        1 => Facing::West,
        2 => Facing::North,
        _ => Facing::East,
    }
}

/// `EnumFacing.rotateYCCW`.
fn rotate_yccw(facing: Facing) -> Facing {
    match facing {
        Facing::North => Facing::West,
        Facing::West => Facing::South,
        Facing::South => Facing::East,
        Facing::East => Facing::North,
        other => other,
    }
}

// --- doors ---

/// `BlockDoor.combineMetadata` + `setBoundBasedOnMeta`: the panel box for one
/// door half. Vanilla reads the upper/lower neighbour's raw meta without
/// checking that it is a door; we replicate that faithfully.
fn door_box(lookup: &BlockLookup, x: i32, y: i32, z: i32, meta: u8) -> [f64; 6] {
    let top = meta & 8 != 0;
    let lower = if top { lookup(x, y - 1, z).meta } else { meta };
    let upper = if top { meta } else { lookup(x, y + 1, z).meta };
    let facing = rotate_yccw(horizontal(lower & 3));
    let open = lower & 4 != 0;
    // MCP names this bit isHingeLeft; only the branch selection matters.
    let hinge = upper & 1 != 0;
    const F: f64 = 0.1875;
    if open {
        match facing {
            Facing::East => {
                if hinge {
                    [0.0, 0.0, 1.0 - F, 1.0, 1.0, 1.0]
                } else {
                    [0.0, 0.0, 0.0, 1.0, 1.0, F]
                }
            }
            Facing::South => {
                if hinge {
                    [0.0, 0.0, 0.0, F, 1.0, 1.0]
                } else {
                    [1.0 - F, 0.0, 0.0, 1.0, 1.0, 1.0]
                }
            }
            Facing::West => {
                if hinge {
                    [0.0, 0.0, 0.0, 1.0, 1.0, F]
                } else {
                    [0.0, 0.0, 1.0 - F, 1.0, 1.0, 1.0]
                }
            }
            _ => {
                if hinge {
                    [1.0 - F, 0.0, 0.0, 1.0, 1.0, 1.0]
                } else {
                    [0.0, 0.0, 0.0, F, 1.0, 1.0]
                }
            }
        }
    } else {
        match facing {
            Facing::East => [0.0, 0.0, 0.0, F, 1.0, 1.0],
            Facing::South => [0.0, 0.0, 0.0, 1.0, 1.0, F],
            Facing::West => [1.0 - F, 0.0, 0.0, 1.0, 1.0, 1.0],
            _ => [0.0, 0.0, 1.0 - F, 1.0, 1.0, 1.0],
        }
    }
}

// --- stairs ---

/// Whether the id is one of the 13 1.8 stair blocks.
pub const fn is_stairs(id: u16) -> bool {
    matches!(
        id,
        53 | 67 | 108 | 109 | 114 | 128 | 134 | 135 | 136 | 156 | 163 | 164 | 180
    )
}

/// Unit-space boxes of a stair block: the base half-slab plus the
/// neighbour-dependent quarter pieces, in `[x0, y0, z0, x1, y1, z1]` form.
/// Shared by collision and the mesher (both must agree with vanilla).
pub fn stair_boxes(lookup: &BlockLookup, x: i32, y: i32, z: i32) -> Vec<[f64; 6]> {
    let s = block_stair_state(lookup(x, y, z).meta);
    let mut boxes = Vec::with_capacity(3);
    if s.top {
        boxes.push([0.0, 0.5, 0.0, 1.0, 1.0, 1.0]);
    } else {
        boxes.push([0.0, 0.0, 0.0, 1.0, 0.5, 1.0]);
    }
    let (primary, straight) = stair_primary(lookup, x, y, z, s);
    boxes.push(primary);
    if straight {
        if let Some(b) = stair_secondary(lookup, x, y, z, s) {
            boxes.push(b);
        }
    }
    boxes
}

#[derive(Clone, Copy, PartialEq)]
struct StairState {
    facing: Facing,
    top: bool,
}

/// Stairs meta: `FACING = getFront(5 - (meta & 3))` (0=E, 1=W, 2=S, 3=N),
/// bit 4 = upside-down.
fn block_stair_state(meta: u8) -> StairState {
    StairState {
        facing: front(5 - (meta & 3)),
        top: meta & 4 != 0,
    }
}

fn stair_state_at(lookup: &BlockLookup, x: i32, y: i32, z: i32) -> Option<StairState> {
    let b = lookup(x, y, z);
    is_stairs(b.id).then(|| block_stair_state(b.meta))
}

/// `BlockStairs.isSameStair`: stairs with identical half and facing.
fn is_same_stair(lookup: &BlockLookup, x: i32, y: i32, z: i32, s: StairState) -> bool {
    stair_state_at(lookup, x, y, z) == Some(s)
}

/// `BlockStairs.func_176306_h`: the quarter on the facing side, halved when
/// the stair forms an outer corner. Returns (box, is_not_outer_corner).
fn stair_primary(lookup: &BlockLookup, x: i32, y: i32, z: i32, s: StairState) -> ([f64; 6], bool) {
    let (f, f1) = if s.top { (0.0, 0.5) } else { (0.5, 1.0) };
    let mut f2 = 0.0;
    let mut f3 = 1.0;
    let mut f4 = 0.0;
    let mut f5 = 0.5;
    let mut flag1 = true;

    match s.facing {
        Facing::East => {
            f2 = 0.5;
            f5 = 1.0;
            if let Some(n) = stair_state_at(lookup, x + 1, y, z) {
                if n.top == s.top {
                    if n.facing == Facing::North && !is_same_stair(lookup, x, y, z + 1, s) {
                        f5 = 0.5;
                        flag1 = false;
                    } else if n.facing == Facing::South && !is_same_stair(lookup, x, y, z - 1, s) {
                        f4 = 0.5;
                        flag1 = false;
                    }
                }
            }
        }
        Facing::West => {
            f3 = 0.5;
            f5 = 1.0;
            if let Some(n) = stair_state_at(lookup, x - 1, y, z) {
                if n.top == s.top {
                    if n.facing == Facing::North && !is_same_stair(lookup, x, y, z + 1, s) {
                        f5 = 0.5;
                        flag1 = false;
                    } else if n.facing == Facing::South && !is_same_stair(lookup, x, y, z - 1, s) {
                        f4 = 0.5;
                        flag1 = false;
                    }
                }
            }
        }
        Facing::South => {
            f4 = 0.5;
            f5 = 1.0;
            if let Some(n) = stair_state_at(lookup, x, y, z + 1) {
                if n.top == s.top {
                    if n.facing == Facing::West && !is_same_stair(lookup, x + 1, y, z, s) {
                        f3 = 0.5;
                        flag1 = false;
                    } else if n.facing == Facing::East && !is_same_stair(lookup, x - 1, y, z, s) {
                        f2 = 0.5;
                        flag1 = false;
                    }
                }
            }
        }
        Facing::North => {
            if let Some(n) = stair_state_at(lookup, x, y, z - 1) {
                if n.top == s.top {
                    if n.facing == Facing::West && !is_same_stair(lookup, x + 1, y, z, s) {
                        f3 = 0.5;
                        flag1 = false;
                    } else if n.facing == Facing::East && !is_same_stair(lookup, x - 1, y, z, s) {
                        f2 = 0.5;
                        flag1 = false;
                    }
                }
            }
        }
        _ => {}
    }

    ([f2, f, f4, f3, f1, f5], flag1)
}

/// `BlockStairs.func_176304_i`: the extra quarter completing an inner corner,
/// if the stair behind forms one.
fn stair_secondary(
    lookup: &BlockLookup,
    x: i32,
    y: i32,
    z: i32,
    s: StairState,
) -> Option<[f64; 6]> {
    let (f, f1) = if s.top { (0.0, 0.5) } else { (0.5, 1.0) };
    let mut f2 = 0.0;
    let mut f3 = 0.5;
    let mut f4 = 0.5;
    let mut f5 = 1.0;
    let mut flag1 = false;

    match s.facing {
        Facing::East => {
            if let Some(n) = stair_state_at(lookup, x - 1, y, z) {
                if n.top == s.top {
                    if n.facing == Facing::North && !is_same_stair(lookup, x, y, z - 1, s) {
                        f4 = 0.0;
                        f5 = 0.5;
                        flag1 = true;
                    } else if n.facing == Facing::South && !is_same_stair(lookup, x, y, z + 1, s) {
                        f4 = 0.5;
                        f5 = 1.0;
                        flag1 = true;
                    }
                }
            }
        }
        Facing::West => {
            if let Some(n) = stair_state_at(lookup, x + 1, y, z) {
                if n.top == s.top {
                    f2 = 0.5;
                    f3 = 1.0;
                    if n.facing == Facing::North && !is_same_stair(lookup, x, y, z - 1, s) {
                        f4 = 0.0;
                        f5 = 0.5;
                        flag1 = true;
                    } else if n.facing == Facing::South && !is_same_stair(lookup, x, y, z + 1, s) {
                        f4 = 0.5;
                        f5 = 1.0;
                        flag1 = true;
                    }
                }
            }
        }
        Facing::South => {
            if let Some(n) = stair_state_at(lookup, x, y, z - 1) {
                if n.top == s.top {
                    f4 = 0.0;
                    f5 = 0.5;
                    if n.facing == Facing::West && !is_same_stair(lookup, x - 1, y, z, s) {
                        flag1 = true;
                    } else if n.facing == Facing::East && !is_same_stair(lookup, x + 1, y, z, s) {
                        f2 = 0.5;
                        f3 = 1.0;
                        flag1 = true;
                    }
                }
            }
        }
        Facing::North => {
            if let Some(n) = stair_state_at(lookup, x, y, z + 1) {
                if n.top == s.top {
                    if n.facing == Facing::West && !is_same_stair(lookup, x - 1, y, z, s) {
                        flag1 = true;
                    } else if n.facing == Facing::East && !is_same_stair(lookup, x + 1, y, z, s) {
                        f2 = 0.5;
                        f3 = 1.0;
                        flag1 = true;
                    }
                }
            }
        }
        _ => {}
    }

    flag1.then_some([f2, f, f4, f3, f1, f5])
}

// --- connection predicates ---

const fn is_wood_fence(id: u16) -> bool {
    matches!(id, 85 | 188..=192)
}

/// Whether the id is a fence block (wooden variants or nether brick).
pub const fn is_fence(id: u16) -> bool {
    is_wood_fence(id) || id == 113
}

/// Whether the id is a glass pane / iron bars block.
pub const fn is_pane(id: u16) -> bool {
    matches!(id, 101 | 102 | 160)
}

const fn is_fence_gate(id: u16) -> bool {
    matches!(id, 107 | 183..=187)
}

const GOURDS: [u16; 3] = [86, 91, 103]; // pumpkin, jack o'lantern, melon

/// The `[north, south, west, east]` fence connections at `(x, y, z)`. Shared
/// by collision and the mesher.
pub fn fence_connections(lookup: &BlockLookup, id: u16, x: i32, y: i32, z: i32) -> [bool; 4] {
    [
        fence_connects(lookup, id, x, y, z - 1),
        fence_connects(lookup, id, x, y, z + 1),
        fence_connects(lookup, id, x - 1, y, z),
        fence_connects(lookup, id, x + 1, y, z),
    ]
}

/// The `[north, south, west, east]` pane connections at `(x, y, z)`. Shared
/// by collision and the mesher.
pub fn pane_connections(lookup: &BlockLookup, id: u16, x: i32, y: i32, z: i32) -> [bool; 4] {
    [
        pane_connects(lookup, id, x, y, z - 1),
        pane_connects(lookup, id, x, y, z + 1),
        pane_connects(lookup, id, x - 1, y, z),
        pane_connects(lookup, id, x + 1, y, z),
    ]
}

/// `BlockFence.canConnectTo`: same-material fences, fence gates, or opaque
/// full cubes that aren't gourds (barrier never connects).
fn fence_connects(lookup: &BlockLookup, this_id: u16, x: i32, y: i32, z: i32) -> bool {
    let n = lookup(x, y, z).id;
    if n == 166 {
        return false;
    }
    if is_fence(n) {
        return is_wood_fence(n) == is_wood_fence(this_id);
    }
    if is_fence_gate(n) {
        return true;
    }
    is_material_opaque_full_cube(n) && !GOURDS.contains(&n)
}

/// `BlockWall.canConnectTo`: walls, fence gates, or opaque full cubes that
/// aren't gourds (barrier never connects).
fn wall_connects(lookup: &BlockLookup, x: i32, y: i32, z: i32) -> bool {
    let n = lookup(x, y, z).id;
    if n == 166 {
        return false;
    }
    if n == 139 || is_fence_gate(n) {
        return true;
    }
    is_material_opaque_full_cube(n) && !GOURDS.contains(&n)
}

/// `BlockPane.canPaneConnectToBlock`: full blocks or the glass family.
fn pane_connects(lookup: &BlockLookup, this_id: u16, x: i32, y: i32, z: i32) -> bool {
    let n = lookup(x, y, z).id;
    is_full_block(n) || n == this_id || matches!(n, 20 | 95 | 101 | 102 | 160)
}

/// Vanilla `material.isOpaque() && isFullCube()` — what fences and walls
/// connect to. Approximated from the data file's opaque cube flag, with the
/// few blocks whose Material disagrees with their render listed explicitly.
fn is_material_opaque_full_cube(id: u16) -> bool {
    match id {
        // Air is absent from the data file but is no cube.
        0 => false,
        // Mob spawner renders cutout but Material.rock is opaque.
        52 => true,
        // Glowstone / sea lantern are Material.glass (not opaque).
        89 | 169 => false,
        _ => match blocks::registry().def(id) {
            Some(def) => {
                def.shape == Shape::Cube && def.opaque && def.collision != CollisionKind::None
            }
            None => !is_known_partial(id),
        },
    }
}

/// Vanilla `Block.fullBlock` (latched `isOpaqueCube()` at construction) —
/// what panes connect to. Differs from the material test for a handful of
/// blocks.
fn is_full_block(id: u16) -> bool {
    match id {
        // Air and barrier are absent from the data file but not full blocks.
        0 | 166 => false,
        // Leaves latch fullBlock before fancy graphics flips isOpaqueCube.
        18 | 161 => true,
        // Glass-material light blocks still report an opaque cube here.
        89 | 169 => true,
        // Mob spawner and slime override isOpaqueCube to false.
        52 | 165 => false,
        _ => match blocks::registry().def(id) {
            Some(def) => {
                def.shape == Shape::Cube && def.opaque && def.collision != CollisionKind::None
            }
            None => !is_known_partial(id),
        },
    }
}

/// Block ids absent from the data file that are NOT full opaque cubes (the
/// data file fallback otherwise treats unknown ids as full cubes, which is
/// right for collision but wrong for connection tests).
const fn is_known_partial(id: u16) -> bool {
    matches!(
        id,
        26 | 29 | 33 | 34 | 36 | 51 | 54 | 55 | 63 | 64 | 68..=72 | 75..=77 | 90 | 92..=94 | 96
            | 104 | 105 | 115..=120 | 122 | 127 | 130..=132 | 140..=151 | 154 | 167 | 176..=178
            | 183..=187 | 193..=197
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockState;

    const EVERYTHING: Aabb = Aabb {
        min: DVec3::new(-64.0, -64.0, -64.0),
        max: DVec3::new(64.0, 320.0, 64.0),
    };

    fn boxes_at(world: &World, x: i32, y: i32, z: i32) -> Vec<Aabb> {
        let mut out = Vec::new();
        add_block_collision_boxes(world, x, y, z, EVERYTHING, &mut out);
        out
    }

    fn assert_box(b: Aabb, expected: [f64; 6]) {
        let got = [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z];
        for (g, e) in got.iter().zip(expected.iter()) {
            assert!((g - e).abs() < 1.0e-9, "expected {expected:?}, got {got:?}");
        }
    }

    #[test]
    fn lone_fence_is_a_single_1_5_tall_post() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(85, 0));
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 1);
        assert_box(boxes[0], [0.375, 0.0, 0.375, 0.625, 1.5, 0.625]);
    }

    #[test]
    fn fence_grows_arm_toward_stone_neighbour() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(85, 0));
        world.set_block(0, 0, -1, BlockState::STONE); // north
                                                      // Vanilla emits only the north-south arm here (it already covers the
                                                      // post); the east-west box is skipped once any z arm exists.
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 1);
        assert_box(boxes[0], [0.375, 0.0, 0.0, 0.625, 1.5, 0.625]);
    }

    #[test]
    fn wood_fence_ignores_nether_fence_but_joins_gate() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(85, 0));
        world.set_block(0, 0, -1, BlockState::new(113, 0)); // nether fence north
        world.set_block(1, 0, 0, BlockState::new(107, 0)); // closed gate east
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 1);
        assert_box(boxes[0], [0.375, 0.0, 0.375, 1.0, 1.5, 0.625]);
    }

    #[test]
    fn unconnected_pane_is_a_full_cross() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(102, 0));
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 2);
        assert_box(boxes[0], [0.0, 0.0, 0.4375, 1.0, 1.0, 0.5625]);
        assert_box(boxes[1], [0.4375, 0.0, 0.0, 0.5625, 1.0, 1.0]);
    }

    #[test]
    fn pane_connected_west_only_keeps_half_bar() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(102, 0));
        world.set_block(-1, 0, 0, BlockState::STONE);
        // Vanilla emits just the western half-bar — no centre stub on the
        // unconnected axis.
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 1);
        assert_box(boxes[0], [0.0, 0.0, 0.4375, 0.5, 1.0, 0.5625]);
    }

    #[test]
    fn straight_east_stairs_are_slab_plus_high_quarter() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(53, 0)); // east-facing bottom
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 2);
        assert_box(boxes[0], [0.0, 0.0, 0.0, 1.0, 0.5, 1.0]);
        assert_box(boxes[1], [0.5, 0.5, 0.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn outer_corner_stairs_shrink_the_quarter() {
        let mut world = World::new();
        // East-facing stair whose east neighbour faces south => quarter is
        // halved toward +z (f4 = 0.5) and no inner-corner piece is added.
        world.set_block(0, 0, 0, BlockState::new(53, 0));
        world.set_block(1, 0, 0, BlockState::new(53, 2)); // south-facing
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 2);
        assert_box(boxes[1], [0.5, 0.5, 0.5, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn inner_corner_stairs_gain_a_quarter() {
        let mut world = World::new();
        // East-facing stair whose west neighbour faces south => extra quarter
        // at z in [0.5, 1].
        world.set_block(0, 0, 0, BlockState::new(53, 0));
        world.set_block(-1, 0, 0, BlockState::new(53, 2));
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 3);
        assert_box(boxes[1], [0.5, 0.5, 0.0, 1.0, 1.0, 1.0]);
        assert_box(boxes[2], [0.0, 0.5, 0.5, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn upside_down_stairs_flip_to_the_lower_half() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(53, 4)); // east-facing top
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 2);
        assert_box(boxes[0], [0.0, 0.5, 0.0, 1.0, 1.0, 1.0]);
        assert_box(boxes[1], [0.5, 0.0, 0.0, 1.0, 0.5, 1.0]);
    }

    #[test]
    fn snow_layer_heights_match_vanilla() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(78, 0)); // 1 layer
        world.set_block(1, 0, 0, BlockState::new(78, 7)); // 8 layers
        let one = boxes_at(&world, 0, 0, 0);
        assert_eq!(one.len(), 1);
        assert_box(one[0], [0.0, 0.0, 0.0, 1.0, 0.0, 1.0]);
        let eight = boxes_at(&world, 1, 0, 0);
        assert_box(eight[0], [1.0, 0.0, 0.0, 2.0, 0.875, 1.0]);
    }

    #[test]
    fn farmland_collides_as_a_full_cube() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(60, 5));
        let boxes = boxes_at(&world, 0, 0, 0);
        assert_eq!(boxes.len(), 1);
        assert_box(boxes[0], [0.0, 0.0, 0.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn closed_door_panel_follows_lower_meta_facing() {
        let mut world = World::new();
        // Lower half facing east (meta 0), closed; upper half above (meta 8).
        world.set_block(0, 0, 0, BlockState::new(64, 0));
        world.set_block(0, 1, 0, BlockState::new(64, 8));
        let lower = boxes_at(&world, 0, 0, 0);
        assert_box(lower[0], [0.0, 0.0, 0.0, 0.1875, 1.0, 1.0]);
        let upper = boxes_at(&world, 0, 1, 0);
        assert_box(upper[0], [0.0, 1.0, 0.0, 0.1875, 2.0, 1.0]);
    }

    #[test]
    fn open_fence_gate_has_no_collision() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(107, 4));
        assert!(boxes_at(&world, 0, 0, 0).is_empty());
    }

    #[test]
    fn redstone_wire_and_sign_have_no_collision() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(55, 0));
        world.set_block(1, 0, 0, BlockState::new(63, 0));
        assert!(boxes_at(&world, 0, 0, 0).is_empty());
        assert!(boxes_at(&world, 1, 0, 0).is_empty());
    }

    #[test]
    fn chest_and_soul_sand_are_lowered() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(54, 2));
        world.set_block(1, 0, 0, BlockState::new(88, 0));
        assert_box(
            boxes_at(&world, 0, 0, 0)[0],
            [0.0625, 0.0, 0.0625, 0.9375, 0.875, 0.9375],
        );
        assert_box(
            boxes_at(&world, 1, 0, 0)[0],
            [1.0, 0.0, 0.0, 2.0, 0.875, 1.0],
        );
    }
}
