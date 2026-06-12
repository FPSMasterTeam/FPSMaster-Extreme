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
        if self.is_air() {
            return false;
        }
        blocks::registry().def(self.id).is_none_or(|def| def.opaque)
    }

    /// Render geometry: full cube, crossed plant, or partial boxes.
    pub fn render_shape(self) -> RenderShape {
        match self.shape() {
            Shape::Cross => RenderShape::Cross,
            Shape::Rail => RenderShape::Rail,
            Shape::Ladder => RenderShape::Ladder,
            Shape::Cube => RenderShape::Cube,
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
            Shape::Cross | Shape::Rail | Shape::Ladder => CollisionBoxes::none(),
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
        }
    }
}

/// A block's render geometry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderShape {
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

const MAX_BLOCK_BOXES: usize = 4;

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
