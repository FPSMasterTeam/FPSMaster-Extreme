use crate::blocks::{self, BlockFace, CollisionKind, RenderLayer, Shape, Tint};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockState {
    pub id: u16,
    pub meta: u8,
}

impl BlockState {
    pub const AIR: Self = Self { id: 0, meta: 0 };
    pub const STONE: Self = Self { id: 1, meta: 0 };
    pub const GRASS: Self = Self { id: 2, meta: 0 };
    pub const DIRT: Self = Self { id: 3, meta: 0 };

    pub const fn new(id: u16, meta: u8) -> Self {
        Self { id, meta }
    }

    pub const fn is_air(self) -> bool {
        self.id == 0
    }

    fn shape(self) -> Shape {
        blocks::registry()
            .def(self.id)
            .map_or(Shape::Cube, |def| def.shape)
    }

    /// Whether this block fully occludes the neighbouring face touching it
    /// (data-driven). Unknown non-air blocks are treated as opaque cubes.
    pub fn is_opaque_cube(self) -> bool {
        if self.is_air() || self.id == 166 {
            return false;
        }
        blocks::registry().def(self.id).is_none_or(|def| def.opaque)
    }

    /// Render geometry: full cube, crossed plant, or partial boxes.
    pub fn render_shape(self) -> RenderShape {
        if self.id == 166 {
            return RenderShape::None;
        }
        match self.shape() {
            Shape::Cross => RenderShape::Cross,
            Shape::Rail => RenderShape::Rail,
            Shape::Ladder => RenderShape::Ladder,
            Shape::Cube => RenderShape::Cube,
            Shape::None => RenderShape::None,
            _ => RenderShape::Boxes,
        }
    }

    pub fn render_layer(self) -> RenderLayer {
        blocks::registry()
            .def(self.id)
            .map_or(RenderLayer::Solid, |def| def.layer)
    }

    pub fn render_alpha(self) -> f32 {
        blocks::registry().def(self.id).map_or(1.0, |def| def.alpha)
    }

    /// Texture base-name for the given face, or None to fall back to a missing
    /// placeholder.
    pub fn texture_name(self, face: BlockFace) -> Option<&'static str> {
        blocks::registry()
            .def(self.id)
            .and_then(|def| def.texture_name(self.meta, face))
    }

    pub fn tint(self, face: BlockFace) -> Tint {
        blocks::registry()
            .def(self.id)
            .map_or(Tint::None, |def| def.tint(face))
    }

    pub fn is_solid_collision(self) -> bool {
        !self.collision_boxes().is_empty()
    }

    /// Vanilla `Block.slipperiness`: the horizontal friction factor of the
    /// block a walking entity stands on (multiplied by 0.91 to form the drag).
    /// Default 0.6; the ice family is 0.98; slime blocks are 0.8. 1.8 has no
    /// blue/frosted ice, so packed ice (174) is the only other slick block.
    pub fn slipperiness(self) -> f32 {
        match self.id {
            79 | 174 => 0.98, // ice, packed ice
            165 => 0.8,       // slime block
            _ => 0.6,
        }
    }

    /// Vanilla `Entity.isOnLadder` material: ladders and vines in 1.8. Trapdoor-
    /// as-ladder is a 1.9 feature and excluded.
    pub fn is_climbable(self) -> bool {
        matches!(self.id, 65 | 106) // ladder, vine
    }

    /// `BlockWeb` — applies the cobweb stuck-speed when an entity is inside it.
    pub fn is_cobweb(self) -> bool {
        self.id == 30
    }

    /// `BlockSoulSand` — multiplies an entity's horizontal motion by 0.4 while
    /// it is inside the block (1.8 `onEntityCollidedWithBlock`).
    pub fn is_soul_sand(self) -> bool {
        self.id == 88
    }

    /// `BlockSlime` — bounces an entity that lands on it.
    pub fn is_slime_block(self) -> bool {
        self.id == 165
    }

    /// Flowing or still water (1.8 `Material.water`).
    pub fn is_water(self) -> bool {
        matches!(self.id, 8 | 9)
    }

    /// Flowing or still lava (1.8 `Material.lava`).
    pub fn is_lava(self) -> bool {
        matches!(self.id, 10 | 11)
    }

    /// Either liquid material — used by the swim-up-against-a-ledge bump, which
    /// only fires onto a position free of both collision and liquid.
    pub fn is_liquid(self) -> bool {
        self.is_water() || self.is_lava()
    }

    /// Collision boxes in unit (0..1) block space; empty means no collision
    /// (air, fluids, plants, torches, …). Derived from the block's shape and
    /// the `collision` override in the data file.
    pub fn collision_boxes(self) -> CollisionBoxes {
        if self.is_air() {
            return CollisionBoxes::none();
        }
        let Some(def) = blocks::registry().def(self.id) else {
            return CollisionBoxes::one(FULL_CUBE);
        };
        match def.collision {
            CollisionKind::None => CollisionBoxes::none(),
            CollisionKind::Full => CollisionBoxes::one(FULL_CUBE),
            CollisionKind::Auto => self.shape_boxes(def.shape, false),
        }
    }

    /// Boxes to render for a partial-shape (`RenderShape::Boxes`) block. Clamped
    /// to the unit cube (so e.g. a 1.5-tall fence collision post renders 1.0).
    pub fn render_boxes(self) -> CollisionBoxes {
        self.shape_boxes(self.shape(), true)
    }

    /// Geometry for a shape; `render` caps box heights at 1.0 for visuals while
    /// collision may exceed the cube (fences).
    fn shape_boxes(self, shape: Shape, render: bool) -> CollisionBoxes {
        match shape {
            Shape::Cube => CollisionBoxes::one(FULL_CUBE),
            Shape::Cross | Shape::Rail | Shape::Ladder | Shape::None => CollisionBoxes::none(),
            Shape::Slab => CollisionBoxes::one(slab_box(self.meta)),
            // Stairs approximated as a walkable bottom half slab.
            Shape::Stairs => CollisionBoxes::one(box3(0.0, 0.0, 0.0, 1.0, 0.5, 1.0)),
            Shape::Layer => {
                let height = if self.id == 78 {
                    ((self.meta & 0x7) as f64 + 1.0) / 8.0 // snow grows with layers
                } else {
                    0.0625 // carpet
                };
                CollisionBoxes::one(box3(0.0, 0.0, 0.0, 1.0, height, 1.0))
            }
            Shape::Fence => {
                let height = if render { 1.0 } else { 1.5 };
                CollisionBoxes::one(box3(0.375, 0.0, 0.375, 0.625, height, 0.625))
            }
            Shape::Pane => CollisionBoxes::one(box3(0.4375, 0.0, 0.4375, 0.5625, 1.0, 0.5625)),
            Shape::Cactus => CollisionBoxes::one(box3(0.0625, 0.0, 0.0625, 0.9375, 1.0, 0.9375)),
            Shape::Farmland => CollisionBoxes::one(box3(0.0, 0.0, 0.0, 1.0, 0.9375, 1.0)),
            Shape::Lily => CollisionBoxes::one(box3(0.0, 0.0, 0.0, 1.0, 0.015_625, 1.0)),
            // Trapdoor: 3/16 plate at the bottom/top, or against a wall when open.
            Shape::Trapdoor => CollisionBoxes::one(if self.meta & 4 != 0 {
                match self.meta & 3 {
                    0 => box3(0.0, 0.0, 0.8125, 1.0, 1.0, 1.0), // north
                    1 => box3(0.0, 0.0, 0.0, 1.0, 1.0, 0.1875), // south
                    2 => box3(0.8125, 0.0, 0.0, 1.0, 1.0, 1.0), // west
                    _ => box3(0.0, 0.0, 0.0, 0.1875, 1.0, 1.0), // east
                }
            } else if self.meta & 8 != 0 {
                box3(0.0, 0.8125, 0.0, 1.0, 1.0, 1.0)
            } else {
                box3(0.0, 0.0, 0.0, 1.0, 0.1875, 1.0)
            }),
            Shape::Chest => CollisionBoxes::one(box3(0.0625, 0.0, 0.0625, 0.9375, 0.875, 0.9375)),
            Shape::Plate => CollisionBoxes::one(box3(0.0625, 0.0, 0.0625, 0.9375, 0.0625, 0.9375)),
            // Button: a 6x4x2 px pad on its mounting face (meta 1-4 walls,
            // otherwise floor/ceiling).
            Shape::Button => CollisionBoxes::one(match self.meta & 7 {
                1 => box3(0.0, 0.375, 0.3125, 0.125, 0.625, 0.6875),
                2 => box3(0.875, 0.375, 0.3125, 1.0, 0.625, 0.6875),
                3 => box3(0.3125, 0.375, 0.0, 0.6875, 0.625, 0.125),
                4 => box3(0.3125, 0.375, 0.875, 0.6875, 0.625, 1.0),
                _ => box3(0.3125, 0.0, 0.375, 0.6875, 0.125, 0.625),
            }),
            // Cake: the west edge advances 2/16 per bite.
            Shape::Cake => {
                let bites = (self.meta & 7) as f64;
                CollisionBoxes::one(BlockBox {
                    min: [(1.0 + bites * 2.0) / 16.0, 0.0, 0.0625],
                    max: [0.9375, 0.5, 0.9375],
                })
            }
            Shape::Pot => CollisionBoxes::one(box3(0.3125, 0.0, 0.3125, 0.6875, 0.375, 0.6875)),
            // Anvil: full height, 1/8 inset across the facing axis
            // (meta&3: 0/2 face south/north, 1/3 face west/east).
            Shape::Anvil => CollisionBoxes::one(if self.meta & 1 != 0 {
                box3(0.0, 0.0, 0.125, 1.0, 1.0, 0.875)
            } else {
                box3(0.125, 0.0, 0.0, 0.875, 1.0, 1.0)
            }),
            // Cauldron: 5/16 floor plus four 1/8-thick walls.
            Shape::Cauldron => CollisionBoxes::from_boxes(&[
                box3(0.0, 0.0, 0.0, 1.0, 0.3125, 1.0),
                box3(0.0, 0.0, 0.0, 0.125, 1.0, 1.0),
                box3(0.875, 0.0, 0.0, 1.0, 1.0, 1.0),
                box3(0.0, 0.0, 0.0, 1.0, 1.0, 0.125),
                box3(0.0, 0.0, 0.875, 1.0, 1.0, 1.0),
            ]),
            // Hopper: bowl, tapering funnel, output stem.
            Shape::Hopper => CollisionBoxes::from_boxes(&[
                box3(0.0, 0.625, 0.0, 1.0, 1.0, 1.0),
                box3(0.25, 0.25, 0.25, 0.75, 0.625, 0.75),
                box3(0.375, 0.0, 0.375, 0.625, 0.25, 0.625),
            ]),
            // Nether portal panel: 4/16 thick across the meta axis (1=x, 2=z).
            Shape::Portal => CollisionBoxes::one(match self.meta & 3 {
                2 => box3(0.375, 0.0, 0.0, 0.625, 1.0, 1.0),
                _ => box3(0.0, 0.0, 0.375, 1.0, 1.0, 0.625),
            }),
        }
    }
}

/// A block's render geometry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderShape {
    None,
    Cube,
    Cross,
    Rail,
    Ladder,
    Boxes,
}

/// An axis-aligned box in unit (0..1) block space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockBox {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

const fn box3(x0: f64, y0: f64, z0: f64, x1: f64, y1: f64, z1: f64) -> BlockBox {
    BlockBox {
        min: [x0, y0, z0],
        max: [x1, y1, z1],
    }
}

pub const FULL_CUBE: BlockBox = box3(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);

const MAX_BLOCK_BOXES: usize = 6;

/// Up to `MAX_BLOCK_BOXES` collision boxes for a single block.
#[derive(Debug, Clone, Copy)]
pub struct CollisionBoxes {
    boxes: [BlockBox; MAX_BLOCK_BOXES],
    len: usize,
}

impl CollisionBoxes {
    fn none() -> Self {
        Self {
            boxes: [FULL_CUBE; MAX_BLOCK_BOXES],
            len: 0,
        }
    }

    fn one(b: BlockBox) -> Self {
        let mut boxes = [FULL_CUBE; MAX_BLOCK_BOXES];
        boxes[0] = b;
        Self { boxes, len: 1 }
    }

    fn from_boxes(list: &[BlockBox]) -> Self {
        let mut boxes = [FULL_CUBE; MAX_BLOCK_BOXES];
        let len = list.len().min(MAX_BLOCK_BOXES);
        boxes[..len].copy_from_slice(&list[..len]);
        Self { boxes, len }
    }

    pub fn as_slice(&self) -> &[BlockBox] {
        &self.boxes[..self.len]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

fn slab_box(meta: u8) -> BlockBox {
    if meta & 0x8 != 0 {
        box3(0.0, 0.5, 0.0, 1.0, 1.0, 1.0)
    } else {
        box3(0.0, 0.0, 0.0, 1.0, 0.5, 1.0)
    }
}
