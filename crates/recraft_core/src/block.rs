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

    pub const fn is_opaque_cube(self) -> bool {
        match self.id {
            0 => false,
            // This is intentionally conservative for the first renderer pass.
            // Per-block 1.8.9 collision/render metadata will replace it.
            6 | 8 | 9 | 10 | 11 | 31 | 37 | 38 | 39 | 40 | 50 | 51 => false,
            _ => true,
        }
    }

    pub const fn is_solid_collision(self) -> bool {
        match self.id {
            0 | 6 | 8 | 9 | 10 | 11 | 31 | 37 | 38 | 39 | 40 | 50 | 51 => false,
            _ => true,
        }
    }
}
