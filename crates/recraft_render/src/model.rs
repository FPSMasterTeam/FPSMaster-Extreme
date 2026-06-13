use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use recraft_core::EntityKind;

use crate::texture::{
    entity_slot_origin, EntitySlot, ENTITY_ATLAS_HEIGHT, ENTITY_ATLAS_WIDTH, ENTITY_SLOT_PX,
    ENTITY_WHITE_UV, PLAYER_SKIN_BASE_ROW,
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

/// Per-frame animation inputs for one entity, in vanilla terms. Built by the
/// app from the entity's interpolated motion and metadata.
#[derive(Debug, Clone, Copy, Default)]
pub struct EntityAnim {
    /// Walk-cycle phase (vanilla `limbSwing`).
    pub limb_swing: f32,
    /// Walk-cycle amplitude 0..1 (vanilla `limbSwingAmount`).
    pub limb_swing_amount: f32,
    /// Head yaw minus body yaw, in degrees (vanilla `netHeadYaw`).
    pub net_head_yaw: f32,
    /// Head pitch in degrees (vanilla `headPitch`).
    pub head_pitch: f32,
    /// Arm-swing (attack) progress 0..1.
    pub swing_progress: f32,
    /// Whether the entity is crouching (drives the sneak pose).
    pub sneaking: bool,
}

/// Articulation for one model part: a primary rotation about `pivot`, plus an
/// optional secondary rotation about `group_pivot` (used to tilt the upper body
/// as a unit when sneaking). All offsets are model px; angles are radians.
#[derive(Debug, Clone, Copy)]
struct PartPose {
    pivot: Vec3,
    /// Euler angles (x, y, z), applied Z·Y·X like vanilla `ModelRenderer`.
    angles: Vec3,
    group_pivot: Vec3,
    /// Secondary X rotation about `group_pivot`; 0 disables it.
    group_angle_x: f32,
}

impl PartPose {
    /// A static (unrotated) part — pivot is irrelevant when the angle is zero.
    fn still() -> Self {
        Self {
            pivot: Vec3::ZERO,
            angles: Vec3::ZERO,
            group_pivot: Vec3::ZERO,
            group_angle_x: 0.0,
        }
    }
}

fn rotate_x(v: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(v.x, v.y * cos - v.z * sin, v.y * sin + v.z * cos)
}

fn rotate_z(v: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos, v.z)
}

/// Apply a part's Euler rotation about its pivot (vanilla glRotate order Z, Y,
/// X means the vertex is transformed Rz·Ry·Rx·v).
fn apply_part_rotation(local_px: Vec3, pose: &PartPose) -> Vec3 {
    let rel = local_px - pose.pivot;
    let (sx, cx) = pose.angles.x.sin_cos();
    let (sy, cy) = pose.angles.y.sin_cos();
    let (sz, cz) = pose.angles.z.sin_cos();
    let rotated = rotate_z(rotate_y(rotate_x(rel, sx, cx), sy, cy), sz, cz);
    let mut out = rotated + pose.pivot;
    if pose.group_angle_x != 0.0 {
        let (s, c) = pose.group_angle_x.sin_cos();
        out = rotate_x(out - pose.group_pivot, s, c) + pose.group_pivot;
    }
    out
}

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
    #[allow(clippy::too_many_arguments)]
    pub fn push_entity(
        &mut self,
        kind: EntityKind,
        feet: Vec3,
        half_width: f32,
        height: f32,
        yaw_degrees: f32,
        anim: &EntityAnim,
        skin_row: Option<u32>,
    ) {
        match kind {
            EntityKind::LocalPlayer | EntityKind::RemotePlayer => {
                // A downloaded skin row when one is allocated, else the shared
                // default player slot.
                let row = skin_row
                    .map(|i| PLAYER_SKIN_BASE_ROW + i)
                    .unwrap_or(EntitySlot::Player as u32);
                self.push_parts(
                    &humanoid_parts(true),
                    &humanoid_poses(anim),
                    HUMANOID_PX,
                    row,
                    feet,
                    height,
                    yaw_degrees,
                );
            }
            EntityKind::Mob(id) => match mob_model(id) {
                Some(MobModel::Biped(slot)) => {
                    self.push_parts(
                        &humanoid_parts(false),
                        &humanoid_poses(anim),
                        HUMANOID_PX,
                        slot as u32,
                        feet,
                        height,
                        yaw_degrees,
                    );
                }
                Some(MobModel::Quadruped { slot, leg_px }) => {
                    let (parts, model_px) = quadruped_parts(leg_px);
                    self.push_parts(
                        &parts,
                        &quadruped_poses(anim, leg_px),
                        model_px,
                        slot as u32,
                        feet,
                        height,
                        yaw_degrees,
                    );
                }
                Some(MobModel::Creeper) => {
                    let (parts, model_px) = creeper_parts();
                    self.push_parts(
                        &parts,
                        &creeper_poses(anim),
                        model_px,
                        EntitySlot::Creeper as u32,
                        feet,
                        height,
                        yaw_degrees,
                    );
                }
                Some(MobModel::Chicken) => {
                    let (parts, model_px) = chicken_parts();
                    self.push_parts(
                        &parts,
                        &chicken_poses(anim),
                        model_px,
                        EntitySlot::Chicken as u32,
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
    #[allow(clippy::too_many_arguments)]
    fn push_parts(
        &mut self,
        parts: &[Part],
        poses: &[PartPose],
        model_px: f32,
        slot_row: u32,
        feet: Vec3,
        height: f32,
        yaw_degrees: f32,
    ) {
        let scale = height / model_px;
        let yaw = yaw_degrees.to_radians();
        let (sin, cos) = (yaw.sin(), yaw.cos());
        let origin = [0.0, (slot_row * ENTITY_SLOT_PX) as f32];
        // Box bounds stay in model px; the per-part closure articulates each
        // box about its pivot, scales px→world, then rotates by the body yaw.
        for (i, (min_px, max_px, region)) in parts.iter().enumerate() {
            let pose = poses.get(i).copied().unwrap_or_else(PartPose::still);
            let min = Vec3::from(*min_px);
            let max = Vec3::from(*max_px);
            self.push_textured_box(min, max, region, origin, 1.0, &|local_px| {
                let articulated = apply_part_rotation(local_px, &pose) * scale;
                rotate_y(articulated, sin, cos) + feet
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

/// Walk-cycle leg/arm angle: `cos(limbSwing*0.6662 + phase) * scale * amount`.
fn swing(limb_swing: f32, amount: f32, phase: f32, scale: f32) -> f32 {
    (limb_swing * 0.6662 + phase).cos() * scale * amount
}

/// Humanoid (player / biped mob) articulation: opposite-phase leg/arm swing,
/// head yaw+pitch, the attack arm-swing on the right arm, and the sneak crouch.
/// Order matches [`humanoid_parts`]: right leg, left leg, body, right arm, left
/// arm, head.
fn humanoid_poses(anim: &EntityAnim) -> [PartPose; 6] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let head_y = anim.net_head_yaw.to_radians();
    let head_x = anim.head_pitch.to_radians();

    // Right arm and right leg are a half-cycle apart (natural opposite swing).
    let mut arm_r_x = swing(s, a, PI, 1.0);
    let arm_l_x = swing(s, a, 0.0, 1.0);
    let mut arm_r_z = 0.0;
    let mut body_y = 0.0;
    if anim.swing_progress > 0.0 {
        let sp = anim.swing_progress;
        let mut f = 1.0 - sp;
        f = 1.0 - f * f * f;
        let f1 = (f * PI).sin();
        let f2 = (sp * PI).sin() * -(head_x - 0.7) * 0.75;
        arm_r_x -= f1 * 1.2 + f2;
        arm_r_z = (sp * PI).sin() * -0.4;
        body_y = (sp.sqrt() * PI * 2.0).sin() * 0.2;
    }

    let waist = Vec3::new(0.0, 12.0, 0.0);
    let body_tilt = if anim.sneaking { 0.5 } else { 0.0 };
    let arm_extra = if anim.sneaking { 0.4 } else { 0.0 };
    let still = |pivot: Vec3, angles: Vec3| PartPose {
        pivot,
        angles,
        group_pivot: waist,
        group_angle_x: 0.0,
    };
    let grouped = |pivot: Vec3, angles: Vec3| PartPose {
        pivot,
        angles,
        group_pivot: waist,
        group_angle_x: body_tilt,
    };
    [
        still(Vec3::new(-2.0, 12.0, 0.0), Vec3::new(swing(s, a, 0.0, 1.4), 0.0, 0.0)),
        still(Vec3::new(2.0, 12.0, 0.0), Vec3::new(swing(s, a, PI, 1.4), 0.0, 0.0)),
        still(waist, Vec3::new(body_tilt, body_y, 0.0)),
        grouped(
            Vec3::new(-6.0, 24.0, 0.0),
            Vec3::new(arm_r_x + arm_extra, 0.0, arm_r_z),
        ),
        grouped(
            Vec3::new(6.0, 24.0, 0.0),
            Vec3::new(arm_l_x + arm_extra, 0.0, 0.0),
        ),
        grouped(Vec3::new(0.0, 24.0, 0.0), Vec3::new(head_x, head_y, 0.0)),
    ]
}

/// Head pose shared by the non-humanoid models: yaw+pitch about a neck pivot.
fn head_pose(anim: &EntityAnim, pivot: Vec3) -> PartPose {
    PartPose {
        pivot,
        angles: Vec3::new(anim.head_pitch.to_radians(), anim.net_head_yaw.to_radians(), 0.0),
        group_pivot: Vec3::ZERO,
        group_angle_x: 0.0,
    }
}

/// A simple X-swing leg pose about a hip pivot.
fn leg_pose(pivot: Vec3, angle_x: f32) -> PartPose {
    PartPose {
        pivot,
        angles: Vec3::new(angle_x, 0.0, 0.0),
        group_pivot: Vec3::ZERO,
        group_angle_x: 0.0,
    }
}

/// Quadruped articulation: diagonal leg pairs trot together; head turns.
/// Order matches [`quadruped_parts`]: front-right, front-left, back-right,
/// back-left legs, body, head.
fn quadruped_poses(anim: &EntityAnim, leg_px: f32) -> [PartPose; 6] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let pa = swing(s, a, 0.0, 1.4);
    let pb = swing(s, a, PI, 1.4);
    [
        leg_pose(Vec3::new(-3.0, leg_px, 5.0), pb), // front right
        leg_pose(Vec3::new(3.0, leg_px, 5.0), pa),  // front left
        leg_pose(Vec3::new(-3.0, leg_px, -5.0), pa), // back right
        leg_pose(Vec3::new(3.0, leg_px, -5.0), pb), // back left
        PartPose::still(),                          // body
        head_pose(anim, Vec3::new(0.0, leg_px + 4.0, 5.0)),
    ]
}

/// Creeper articulation: the same diagonal leg trot plus head turn. Order
/// matches [`creeper_parts`].
fn creeper_poses(anim: &EntityAnim) -> [PartPose; 6] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let pa = swing(s, a, 0.0, 1.4);
    let pb = swing(s, a, PI, 1.4);
    [
        leg_pose(Vec3::new(-2.0, 6.0, 4.0), pb),  // front right
        leg_pose(Vec3::new(2.0, 6.0, 4.0), pa),   // front left
        leg_pose(Vec3::new(-2.0, 6.0, -4.0), pa), // back right
        leg_pose(Vec3::new(2.0, 6.0, -4.0), pb),  // back left
        PartPose::still(),                        // body
        head_pose(anim, Vec3::new(0.0, 18.0, 0.0)),
    ]
}

/// Chicken articulation: legs alternate; head turns. Order matches
/// [`chicken_parts`]: right leg, left leg, body, head.
fn chicken_poses(anim: &EntityAnim) -> [PartPose; 4] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    [
        leg_pose(Vec3::new(-2.0, 5.0, 0.0), swing(s, a, 0.0, 1.4)),
        leg_pose(Vec3::new(2.0, 5.0, 0.0), swing(s, a, PI, 1.4)),
        PartPose::still(),
        head_pose(anim, Vec3::new(0.0, 9.0, 4.0)),
    ]
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
            &EntityAnim::default(),
            None,
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
            &EntityAnim::default(),
            None,
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
        object.push_entity(
            EntityKind::Object(1),
            Vec3::ZERO,
            0.125,
            0.25,
            0.0,
            &EntityAnim::default(),
            None,
        );
        assert_eq!(object.vertices.len(), 24); // a single box
        assert!(object.vertices.iter().all(|v| v.uv == ENTITY_WHITE_UV));
    }

    /// Build a player mesh at the origin (yaw 0) with the given animation.
    fn player_mesh(anim: &EntityAnim) -> ModelMesh {
        let mut mesh = ModelMesh::new();
        mesh.push_entity(
            EntityKind::RemotePlayer,
            Vec3::ZERO,
            0.3,
            1.8,
            0.0,
            anim,
            None,
        );
        mesh
    }

    // Humanoid part vertex ranges (24 verts/part, in push order).
    const RIGHT_LEG: std::ops::Range<usize> = 0..24;
    const BODY: std::ops::Range<usize> = 48..72;
    const HEAD: std::ops::Range<usize> = 120..144;

    fn max_y(mesh: &ModelMesh, range: std::ops::Range<usize>) -> f32 {
        mesh.vertices[range]
            .iter()
            .map(|v| v.position[1])
            .fold(f32::MIN, f32::max)
    }

    fn parts_differ(a: &ModelMesh, b: &ModelMesh, range: std::ops::Range<usize>) -> bool {
        a.vertices[range.clone()]
            .iter()
            .zip(&b.vertices[range])
            .any(|(x, y)| (Vec3::from(x.position) - Vec3::from(y.position)).length() > 1e-4)
    }

    #[test]
    fn walking_swings_legs_but_not_the_body() {
        let rest = player_mesh(&EntityAnim::default());
        let walking = player_mesh(&EntityAnim {
            limb_swing: 2.0,
            limb_swing_amount: 1.0,
            ..EntityAnim::default()
        });
        assert!(parts_differ(&rest, &walking, RIGHT_LEG), "leg should swing");
        assert!(
            !parts_differ(&rest, &walking, BODY),
            "body must stay still while walking"
        );
    }

    #[test]
    fn head_yaw_moves_only_the_head() {
        let rest = player_mesh(&EntityAnim::default());
        let turned = player_mesh(&EntityAnim {
            net_head_yaw: 60.0,
            ..EntityAnim::default()
        });
        assert!(parts_differ(&rest, &turned, HEAD), "head should turn");
        assert!(
            !parts_differ(&rest, &turned, BODY),
            "body must not move when only the head turns"
        );
        assert!(
            !parts_differ(&rest, &turned, RIGHT_LEG),
            "legs must not move when only the head turns"
        );
    }

    #[test]
    fn looking_down_tilts_the_head_below_its_resting_top() {
        let rest = player_mesh(&EntityAnim::default());
        let looking_down = player_mesh(&EntityAnim {
            head_pitch: 90.0, // straight down
            ..EntityAnim::default()
        });
        // At rest the head top is the tallest point (entity height). Pitching the
        // head fully down must lower its highest vertex.
        assert!(
            max_y(&looking_down, HEAD) < max_y(&rest, HEAD) - 0.1,
            "looking down should drop the head's top: {} !< {}",
            max_y(&looking_down, HEAD),
            max_y(&rest, HEAD)
        );
    }

    #[test]
    fn skin_row_samples_the_per_player_atlas_region() {
        let player = |row: Option<u32>| {
            let mut mesh = ModelMesh::new();
            mesh.push_entity(
                EntityKind::RemotePlayer,
                Vec3::ZERO,
                0.3,
                1.8,
                0.0,
                &EntityAnim::default(),
                row,
            );
            mesh
        };
        let default = player(None);
        let skinned = player(Some(0));
        // Same geometry but different V: the shared player slot (row 0) vs the
        // first per-player skin row.
        let v_default: Vec<f32> = default.vertices.iter().map(|v| v.uv[1]).collect();
        let v_skinned: Vec<f32> = skinned.vertices.iter().map(|v| v.uv[1]).collect();
        assert_ne!(v_default, v_skinned);
        let region_start =
            (PLAYER_SKIN_BASE_ROW * ENTITY_SLOT_PX) as f32 / ENTITY_ATLAS_HEIGHT as f32;
        assert!(
            skinned.vertices.iter().all(|v| v.uv[1] >= region_start - 1e-6),
            "skinned player must sample the per-player region"
        );
    }

    #[test]
    fn sneaking_tilts_the_body_forward() {
        let rest = player_mesh(&EntityAnim::default());
        let sneaking = player_mesh(&EntityAnim {
            sneaking: true,
            ..EntityAnim::default()
        });
        // The crouch tilts the upper body, so the body and head move while the
        // legs stay planted.
        assert!(parts_differ(&rest, &sneaking, BODY), "sneak tilts the body");
        assert!(parts_differ(&rest, &sneaking, HEAD), "sneak lowers the head");
        assert!(
            !parts_differ(&rest, &sneaking, RIGHT_LEG),
            "sneak keeps the legs planted"
        );
    }
}
