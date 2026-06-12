use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use recraft_core::EntityKind;

use crate::texture::{
    entity_slot_origin, EntitySlot, ENTITY_ATLAS_HEIGHT, ENTITY_ATLAS_WIDTH, ENTITY_WHITE_UV,
};

/// Vertex for the model pass used by entities and the first-person hand: a
/// position, an RGBA tint multiplied with the sampled texel, and a UV into the
/// entity texture atlas. Solid-color geometry points its UV at the atlas'
/// guaranteed-white texel so only the tint shows.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct ModelVertex {
    pub position: [f32; 3],
    pub color: [f32; 4],
    pub uv: [f32; 2],
}

impl ModelVertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x2];

    pub fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// Accumulated geometry for the model pass.
#[derive(Debug, Default, Clone)]
pub struct ModelMesh {
    pub vertices: Vec<ModelVertex>,
    pub indices: Vec<u32>,
}

/// Unit-cube corner table shared by every box. Indices match the `FACES` quads.
const CORNERS: [[f32; 3]; 8] = [
    [0.0, 0.0, 0.0], // 0
    [1.0, 0.0, 0.0], // 1
    [1.0, 0.0, 1.0], // 2
    [0.0, 0.0, 1.0], // 3
    [0.0, 1.0, 0.0], // 4
    [1.0, 1.0, 0.0], // 5
    [1.0, 1.0, 1.0], // 6
    [0.0, 1.0, 1.0], // 7
];

/// The six box faces as (corner indices, directional shade), ordered
/// bottom(-y), top(+y), back(-z), front(+z), right(-x), left(+x). This order is
/// also how texture rects are supplied to the skinned-box builders (and how
/// `box_region` emits them).
const FACES: [([usize; 4], f32); 6] = [
    ([0, 1, 2, 3], 0.5), // bottom -y
    ([4, 7, 6, 5], 1.0), // top +y
    ([0, 4, 5, 1], 0.8), // back -z
    ([3, 2, 6, 7], 0.8), // front +z
    ([0, 3, 7, 4], 0.7), // right -x
    ([1, 5, 6, 2], 0.7), // left +x
];

/// One model part: local px-space (min, max) box plus a texture pixel rect per
/// face (in `FACES` order), relative to the part's atlas slot.
type Part = ([f32; 3], [f32; 3], [[f32; 4]; 6]);

/// Humanoid models are 32 px tall (legs 12 + body 12 + head 8).
const HUMANOID_PX: f32 = 32.0;

impl ModelMesh {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Append an axis-aligned solid-color box (no texture). Used by objects,
    /// unknown mobs and any debug geometry; samples the atlas' white texel.
    pub fn push_box(&mut self, min: Vec3, max: Vec3, color: [f32; 4]) {
        let corners = box_corners(min, max);
        for (quad, shade) in FACES {
            let base = self.vertices.len() as u32;
            let shaded = shade_color(color, shade);
            for &index in &quad {
                self.vertices.push(ModelVertex {
                    position: corners[index],
                    color: shaded,
                    uv: ENTITY_WHITE_UV,
                });
            }
            self.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    /// Append the player's right-arm box (vanilla `ModelBiped.bipedRightArm`:
    /// `addBox(-3, -2, -2, 4, 12, 4)` at 1/16 scale) through an arbitrary
    /// local→world transform — the first-person `renderPlayerArm` matrix chain
    /// is computed by the caller and folded into `transform`. Samples the
    /// player skin's right-arm region.
    pub fn push_arm_box(&mut self, transform: &dyn Fn(Vec3) -> Vec3, alpha: f32) {
        let region = box_region(40.0, 16.0, 4.0, 12.0, 4.0); // player right arm
        let (ox, oy) = entity_slot_origin(EntitySlot::Player);
        self.push_textured_box(
            Vec3::new(-3.0, -2.0, -2.0) * 0.0625,
            Vec3::new(1.0, 10.0, 2.0) * 0.0625,
            &region,
            [ox as f32, oy as f32],
            alpha,
            transform,
        );
    }

    /// Append an entity model at `feet` (bottom-center of the entity), scaled
    /// to `height` and rotated to face `yaw_degrees`. Players render as the
    /// skinned humanoid; known mobs render as their archetype (biped,
    /// quadruped, creeper or chicken) sampling that mob's atlas slot; unknown
    /// mobs render as a solid colored box at the entity AABB and objects as a
    /// small colored box.
    pub fn push_entity(
        &mut self,
        kind: EntityKind,
        feet: Vec3,
        half_width: f32,
        height: f32,
        yaw_degrees: f32,
    ) {
        match kind {
            EntityKind::LocalPlayer | EntityKind::RemotePlayer => {
                self.push_parts(
                    &humanoid_parts(true),
                    HUMANOID_PX,
                    EntitySlot::Player,
                    feet,
                    height,
                    yaw_degrees,
                );
            }
            EntityKind::Mob(id) => match mob_model(id) {
                Some(MobModel::Biped(slot)) => {
                    self.push_parts(
                        &humanoid_parts(false),
                        HUMANOID_PX,
                        slot,
                        feet,
                        height,
                        yaw_degrees,
                    );
                }
                Some(MobModel::Quadruped { slot, leg_px }) => {
                    let (parts, model_px) = quadruped_parts(leg_px);
                    self.push_parts(&parts, model_px, slot, feet, height, yaw_degrees);
                }
                Some(MobModel::Creeper) => {
                    let (parts, model_px) = creeper_parts();
                    self.push_parts(
                        &parts,
                        model_px,
                        EntitySlot::Creeper,
                        feet,
                        height,
                        yaw_degrees,
                    );
                }
                Some(MobModel::Chicken) => {
                    let (parts, model_px) = chicken_parts();
                    self.push_parts(
                        &parts,
                        model_px,
                        EntitySlot::Chicken,
                        feet,
                        height,
                        yaw_degrees,
                    );
                }
                None => {
                    // Unknown mob type: a solid colored box at the entity AABB
                    // so it never reads as a player.
                    let w = half_width.max(0.1);
                    let min = Vec3::new(feet.x - w, feet.y, feet.z - w);
                    let max = Vec3::new(feet.x + w, feet.y + height.max(0.2), feet.z + w);
                    self.push_box(min, max, entity_color(kind));
                }
            },
            EntityKind::Object(_) => {
                let w = half_width.max(0.06);
                let min = Vec3::new(feet.x - w, feet.y, feet.z - w);
                let max = Vec3::new(feet.x + w, feet.y + height.max(0.12), feet.z + w);
                self.push_box(min, max, entity_color(kind));
            }
        }
    }

    /// Build a model from px-space parts: scale `model_px` (the model's total
    /// px height) to `height`, rotate every part about the vertical axis
    /// through `feet` by `yaw_degrees`, and sample each face's texture rect
    /// inside `slot` of the entity atlas.
    fn push_parts(
        &mut self,
        parts: &[Part],
        model_px: f32,
        slot: EntitySlot,
        feet: Vec3,
        height: f32,
        yaw_degrees: f32,
    ) {
        let scale = height / model_px;
        let yaw = yaw_degrees.to_radians();
        let (sin, cos) = (yaw.sin(), yaw.cos());
        let (ox, oy) = entity_slot_origin(slot);
        let origin = [ox as f32, oy as f32];
        for (min_px, max_px, region) in parts {
            let min = Vec3::from(*min_px) * scale;
            let max = Vec3::from(*max_px) * scale;
            self.push_textured_box(min, max, region, origin, 1.0, &|local| {
                rotate_y(local, sin, cos) + feet
            });
        }
    }

    /// Push one textured box: `min`/`max` bound it in some local space,
    /// `transform` maps local points to world space, `region` gives a texture
    /// pixel rect [x0,y0,x1,y1] per face (in `FACES` order) relative to the
    /// atlas-pixel `origin`. Faces get the standard directional shading.
    fn push_textured_box(
        &mut self,
        min: Vec3,
        max: Vec3,
        region: &[[f32; 4]; 6],
        origin: [f32; 2],
        alpha: f32,
        transform: &dyn Fn(Vec3) -> Vec3,
    ) {
        for (f, (quad, shade)) in FACES.iter().enumerate() {
            let base = self.vertices.len() as u32;
            for &index in quad {
                let local = Vec3::from(CORNERS[index]) * (max - min) + min;
                let (fu, fv) = plane_frac(f, local, min, max);
                let rect = region[f];
                let u =
                    (origin[0] + rect[0] + fu * (rect[2] - rect[0])) / ENTITY_ATLAS_WIDTH as f32;
                let v =
                    (origin[1] + rect[1] + fv * (rect[3] - rect[1])) / ENTITY_ATLAS_HEIGHT as f32;
                self.vertices.push(ModelVertex {
                    position: transform(local).to_array(),
                    color: [*shade, *shade, *shade, alpha],
                    uv: [u, v],
                });
            }
            self.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// Which model + atlas slot a 1.8 SpawnMob entity-type id maps to.
enum MobModel {
    Biped(EntitySlot),
    Quadruped { slot: EntitySlot, leg_px: f32 },
    Creeper,
    Chicken,
}

/// 1.8 SpawnMob type ids -> model archetype. Unknown ids return None and are
/// drawn as a solid colored box.
fn mob_model(id: u8) -> Option<MobModel> {
    match id {
        50 => Some(MobModel::Creeper),
        51 => Some(MobModel::Biped(EntitySlot::Skeleton)),
        54 => Some(MobModel::Biped(EntitySlot::Zombie)),
        57 => Some(MobModel::Biped(EntitySlot::Zombie)), // zombie pigman
        // Villager is 120 in vanilla 1.8; 100 kept as an alias.
        100 | 120 => Some(MobModel::Biped(EntitySlot::Villager)),
        90 => Some(MobModel::Quadruped {
            slot: EntitySlot::Pig,
            leg_px: 6.0,
        }),
        91 => Some(MobModel::Quadruped {
            slot: EntitySlot::Sheep,
            leg_px: 12.0,
        }),
        92 | 96 => Some(MobModel::Quadruped {
            slot: EntitySlot::Cow,
            leg_px: 12.0,
        }), // cow, mooshroom
        95 => Some(MobModel::Quadruped {
            slot: EntitySlot::Pig,
            leg_px: 8.0,
        }), // wolf
        98 => Some(MobModel::Quadruped {
            slot: EntitySlot::Pig,
            leg_px: 6.0,
        }), // ocelot
        93 => Some(MobModel::Chicken),
        _ => None,
    }
}

/// Texture pixel rects per face (in `FACES` order: bottom, top, back, front,
/// right, left) for the standard Minecraft box unwrap of a w*h*d px box whose
/// layout starts at texture offset (u, v).
fn box_region(u: f32, v: f32, w: f32, h: f32, d: f32) -> [[f32; 4]; 6] {
    [
        [u + d + w, v, u + d + 2.0 * w, v + d], // bottom
        [u + d, v, u + d + w, v + d],           // top
        [u + 2.0 * d + w, v + d, u + 2.0 * d + 2.0 * w, v + d + h], // back
        [u + d, v + d, u + d + w, v + d + h],   // front
        [u, v + d, u + d, v + d + h],           // right
        [u + d + w, v + d, u + d + w + d, v + d + h], // left
    ]
}

/// The six humanoid parts with the standard 1.8 skin layout. With
/// `separate_left_limbs` (64x64 player skins) the left arm/leg use their own
/// regions; without it (64x32 mob skins: zombie/skeleton/villager slots) the
/// left limbs mirror the right-limb regions.
fn humanoid_parts(separate_left_limbs: bool) -> [Part; 6] {
    let head = box_region(0.0, 0.0, 8.0, 8.0, 8.0);
    let body = box_region(16.0, 16.0, 8.0, 12.0, 4.0);
    let arm_r = box_region(40.0, 16.0, 4.0, 12.0, 4.0);
    let leg_r = box_region(0.0, 16.0, 4.0, 12.0, 4.0);
    let arm_l = if separate_left_limbs {
        box_region(32.0, 48.0, 4.0, 12.0, 4.0)
    } else {
        arm_r
    };
    let leg_l = if separate_left_limbs {
        box_region(16.0, 48.0, 4.0, 12.0, 4.0)
    } else {
        leg_r
    };
    [
        ([-4.0, 0.0, -2.0], [0.0, 12.0, 2.0], leg_r), // right leg
        ([0.0, 0.0, -2.0], [4.0, 12.0, 2.0], leg_l),  // left leg
        ([-4.0, 12.0, -2.0], [4.0, 24.0, 2.0], body), // body
        ([-8.0, 12.0, -2.0], [-4.0, 24.0, 2.0], arm_r), // right arm
        ([4.0, 12.0, -2.0], [8.0, 24.0, 2.0], arm_l), // left arm
        ([-4.0, 24.0, -4.0], [4.0, 32.0, 4.0], head), // head
    ]
}

/// Quadruped (pig/cow/sheep family): a horizontal 10x8x16 body on four
/// `leg_px`-tall legs with an 8x8x8 head at the front (+z). Uses the standard
/// 1.8 quadruped texture layout (head at 0,0; legs at 0,16; body at 28,8).
/// Returns the parts and the model's total px height.
fn quadruped_parts(leg_px: f32) -> ([Part; 6], f32) {
    let head = box_region(0.0, 0.0, 8.0, 8.0, 8.0);
    let leg = box_region(0.0, 16.0, 4.0, leg_px, 4.0);
    // Vanilla models the body as a vertical texture box rotated 90° onto its
    // back, so the horizontal world box reads the texture's front/back for
    // its top/bottom and the texture's top/bottom for its z faces.
    let b = box_region(28.0, 8.0, 10.0, 16.0, 8.0);
    let body = [b[2], b[3], b[0], b[1], b[4], b[5]];
    let top = leg_px + 8.0;
    (
        [
            ([-5.0, 0.0, 3.0], [-1.0, leg_px, 7.0], leg), // front right leg
            ([1.0, 0.0, 3.0], [5.0, leg_px, 7.0], leg),   // front left leg
            ([-5.0, 0.0, -7.0], [-1.0, leg_px, -3.0], leg), // back right leg
            ([1.0, 0.0, -7.0], [5.0, leg_px, -3.0], leg), // back left leg
            ([-5.0, leg_px, -8.0], [5.0, top, 8.0], body), // body
            ([-4.0, leg_px, 5.0], [4.0, top, 13.0], head), // head
        ],
        top,
    )
}

/// Creeper: four short legs, a tall upright body and a head on top, using the
/// 1.8 creeper texture layout (head 0,0; body 16,16; legs 0,16).
fn creeper_parts() -> ([Part; 6], f32) {
    let head = box_region(0.0, 0.0, 8.0, 8.0, 8.0);
    let body = box_region(16.0, 16.0, 8.0, 12.0, 4.0);
    let leg = box_region(0.0, 16.0, 4.0, 6.0, 4.0);
    (
        [
            ([-4.0, 0.0, 2.0], [0.0, 6.0, 6.0], leg), // front right leg
            ([0.0, 0.0, 2.0], [4.0, 6.0, 6.0], leg),  // front left leg
            ([-4.0, 0.0, -6.0], [0.0, 6.0, -2.0], leg), // back right leg
            ([0.0, 0.0, -6.0], [4.0, 6.0, -2.0], leg), // back left leg
            ([-4.0, 6.0, -2.0], [4.0, 18.0, 2.0], body), // body
            ([-4.0, 18.0, -4.0], [4.0, 26.0, 4.0], head), // head
        ],
        26.0,
    )
}

/// Chicken: two legs, a small horizontal body and a head at the front, using
/// the 1.8 chicken texture layout (head 0,0; body 0,9; legs 26,0). The body
/// box is rotated like the quadruped's.
fn chicken_parts() -> ([Part; 4], f32) {
    let head = box_region(0.0, 0.0, 4.0, 6.0, 3.0);
    let leg = box_region(26.0, 0.0, 3.0, 5.0, 3.0);
    let b = box_region(0.0, 9.0, 6.0, 8.0, 6.0);
    let body = [b[2], b[3], b[0], b[1], b[4], b[5]];
    (
        [
            ([-3.0, 0.0, -1.0], [-1.0, 5.0, 1.0], leg),  // right leg
            ([1.0, 0.0, -1.0], [3.0, 5.0, 1.0], leg),    // left leg
            ([-3.0, 5.0, -4.0], [3.0, 11.0, 4.0], body), // body
            ([-2.0, 9.0, 3.0], [2.0, 15.0, 7.0], head),  // head
        ],
        15.0,
    )
}

/// Rotate a local offset about the vertical (Y) axis.
fn rotate_y(local: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(
        local.x * cos - local.z * sin,
        local.y,
        local.x * sin + local.z * cos,
    )
}

fn shade_color(color: [f32; 4], shade: f32) -> [f32; 4] {
    [
        color[0] * shade,
        color[1] * shade,
        color[2] * shade,
        color[3],
    ]
}

fn box_corners(min: Vec3, max: Vec3) -> [[f32; 3]; 8] {
    let mut out = [[0.0; 3]; 8];
    for (i, c) in CORNERS.iter().enumerate() {
        out[i] = (Vec3::from(*c) * (max - min) + min).to_array();
    }
    out
}

/// Fractional (u, v) of a corner within a face plane, mapped so the model's top
/// is the texture's top. `f` is the FACES index.
fn plane_frac(f: usize, p: Vec3, min: Vec3, max: Vec3) -> (f32, f32) {
    let span = max - min;
    let fx = if span.x > 0.0 {
        (p.x - min.x) / span.x
    } else {
        0.0
    };
    let fy = if span.y > 0.0 {
        (p.y - min.y) / span.y
    } else {
        0.0
    };
    let fz = if span.z > 0.0 {
        (p.z - min.z) / span.z
    } else {
        0.0
    };
    match f {
        0 => (fx, fz),             // bottom -y
        1 => (fx, 1.0 - fz),       // top +y
        2 => (1.0 - fx, 1.0 - fy), // back -z (mirrored)
        3 => (fx, 1.0 - fy),       // front +z
        4 => (fz, 1.0 - fy),       // right -x
        _ => (1.0 - fz, 1.0 - fy), // left +x (mirrored)
    }
}

/// Per-kind solid color for objects and unknown mobs.
fn entity_color(kind: EntityKind) -> [f32; 4] {
    match kind {
        EntityKind::LocalPlayer | EntityKind::RemotePlayer => [0.85, 0.74, 0.62, 1.0],
        EntityKind::Mob(_) => [0.62, 0.36, 0.66, 1.0],
        EntityKind::Object(_) => [0.80, 0.78, 0.30, 1.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture::ENTITY_SLOT_PX;
    use recraft_core::EntityKind;

    fn build_mob(id: u8) -> ModelMesh {
        let mut mesh = ModelMesh::new();
        mesh.push_entity(
            EntityKind::Mob(id),
            Vec3::new(1.0, 65.0, -3.0),
            0.3,
            1.9,
            30.0,
        );
        mesh
    }

    fn assert_well_formed(mesh: &ModelMesh) {
        assert!(!mesh.is_empty());
        assert_eq!(mesh.indices.len() % 6, 0);
        for v in &mesh.vertices {
            assert!(
                (0.0..=1.0).contains(&v.uv[0]),
                "u out of range: {}",
                v.uv[0]
            );
            assert!(
                (0.0..=1.0).contains(&v.uv[1]),
                "v out of range: {}",
                v.uv[1]
            );
            assert!(v.position.iter().all(|c| c.is_finite()));
        }
    }

    /// V range of an atlas slot.
    fn slot_v_range(slot: EntitySlot) -> (f32, f32) {
        let (_, oy) = entity_slot_origin(slot);
        (
            oy as f32 / ENTITY_ATLAS_HEIGHT as f32,
            (oy + ENTITY_SLOT_PX) as f32 / ENTITY_ATLAS_HEIGHT as f32,
        )
    }

    #[test]
    fn player_humanoid_emits_six_boxes_with_in_range_uvs() {
        let mut mesh = ModelMesh::new();
        mesh.push_entity(
            EntityKind::RemotePlayer,
            Vec3::new(0.0, 64.0, 0.0),
            0.3,
            1.8,
            45.0,
        );
        // 6 parts × 6 faces × 4 verts; 6 parts × 6 faces × 6 indices.
        assert_eq!(mesh.vertices.len(), 144);
        assert_eq!(mesh.indices.len(), 216);
        assert_well_formed(&mesh);
        let (v0, v1) = slot_v_range(EntitySlot::Player);
        assert!(mesh.vertices.iter().all(|v| (v0..=v1).contains(&v.uv[1])));
    }

    #[test]
    fn zombie_pig_and_creeper_are_textured_from_their_own_slots() {
        for (id, slot) in [
            (54u8, EntitySlot::Zombie),
            (90, EntitySlot::Pig),
            (50, EntitySlot::Creeper),
        ] {
            let mesh = build_mob(id);
            assert_well_formed(&mesh);
            // Textured (not the solid-color white texel).
            assert!(mesh.vertices.iter().any(|v| v.uv != ENTITY_WHITE_UV));
            // Every UV stays inside this mob's atlas slot.
            let (v0, v1) = slot_v_range(slot);
            assert!(
                mesh.vertices.iter().all(|v| (v0..=v1).contains(&v.uv[1])),
                "mob {id} sampled outside its slot"
            );
        }
    }

    #[test]
    fn different_mob_ids_produce_distinct_models() {
        let zombie = build_mob(54); // biped: 6 boxes
        let chicken = build_mob(93); // chicken: 4 boxes
        let unknown = build_mob(63); // no 1.8 mob: solid fallback box
        assert_ne!(zombie.vertices.len(), chicken.vertices.len());
        assert_ne!(chicken.vertices.len(), unknown.vertices.len());
        assert_eq!(unknown.vertices.len(), 24);
        assert!(unknown.vertices.iter().all(|v| v.uv == ENTITY_WHITE_UV));

        // Same shape but different texture slot still differs (zombie vs pig
        // sample disjoint V ranges).
        let pig = build_mob(90);
        let zombie_vs: Vec<f32> = zombie.vertices.iter().map(|v| v.uv[1]).collect();
        let pig_vs: Vec<f32> = pig.vertices.iter().map(|v| v.uv[1]).collect();
        assert_ne!(zombie_vs, pig_vs);
    }

    #[test]
    fn arm_box_applies_the_transform_and_samples_the_player_arm() {
        let build = |offset: Vec3| {
            let mut mesh = ModelMesh::new();
            mesh.push_arm_box(&|local| local + offset, 1.0);
            mesh
        };
        let rest = build(Vec3::ZERO);
        let moved = build(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(rest.vertices.len(), 24); // one box
        for mesh in [&rest, &moved] {
            assert_well_formed(mesh);
            let (v0, v1) = slot_v_range(EntitySlot::Player);
            assert!(mesh.vertices.iter().all(|v| (v0..=v1).contains(&v.uv[1])));
        }
        // The transform must move every vertex.
        let centroid = |mesh: &ModelMesh| {
            let sum: Vec3 = mesh.vertices.iter().map(|v| Vec3::from(v.position)).sum();
            sum / mesh.vertices.len() as f32
        };
        assert!((centroid(&moved) - centroid(&rest)).distance(Vec3::new(1.0, 2.0, 3.0)) < 1.0e-4);
    }

    #[test]
    fn solid_geometry_samples_the_white_texel() {
        let mut mesh = ModelMesh::new();
        mesh.push_box(Vec3::ZERO, Vec3::ONE, [1.0, 0.0, 0.0, 1.0]);
        assert!(mesh.vertices.iter().all(|v| v.uv == ENTITY_WHITE_UV));

        let mut object = ModelMesh::new();
        object.push_entity(EntityKind::Object(1), Vec3::ZERO, 0.125, 0.25, 0.0);
        assert_eq!(object.vertices.len(), 24); // a single box
        assert!(object.vertices.iter().all(|v| v.uv == ENTITY_WHITE_UV));
    }
}
