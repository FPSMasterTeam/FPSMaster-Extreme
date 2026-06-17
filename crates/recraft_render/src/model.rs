use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use recraft_core::EntityKind;

use crate::texture::{
    entity_slot_origin, slot_grid_origin, EntitySlot, ENTITY_ATLAS_HEIGHT, ENTITY_ATLAS_WIDTH,
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
    /// Transient: the death fall-over tilt (radians) applied by `push_parts`
    /// about the model's forward axis through the feet. Set by `push_entity` /
    /// `push_armor` from `EntityAnim::death_roll` for the current entity and
    /// reset to 0 afterwards, so non-living parts (chests, books) are unaffected.
    pub death_roll: f32,
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

/// World scale of one model pixel: vanilla renders entity models at 1/16 block
/// per pixel (independent of the hitbox height), so a 32 px humanoid is 2.0
/// blocks tall. Tying the scale to a fixed 1/16 (rather than `height/model_px`)
/// keeps wide/short mobs — spiders, slimes, quadrupeds — in correct proportion.
const MODEL_SCALE: f32 = 1.0 / 16.0;

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
    /// Vanilla `ModelBiped.heldItemRight`: 0 = empty hand, 1 = holding an item
    /// (the right arm lowers ~PI/10), 3 = blocking (the arm lowers ~3·PI/10 and
    /// rotates -30° so the sword cants across the body). Only set for players.
    pub held_item_right: u8,
    /// Use the 1.7-style attack-swing curve (the "OldAnimations" setting). 1.8
    /// eases the arm with a quartic `f⁴`; 1.7 used a cubic `f³`, which snaps the
    /// arm forward faster. Only affects the attack swing, not the walk cycle.
    pub old_animations: bool,
    /// Death fall-over tilt in radians (vanilla `RendererLivingEntity`
    /// applyRotations death roll). 0 for a living entity.
    pub death_roll: f32,
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

    /// Reset the geometry while keeping the backing allocations, so a per-frame
    /// rebuild reuses the previous frame's buffers instead of reallocating.
    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.death_roll = 0.0;
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
    ///
    /// The box is built in the engine's own model convention (feet-up +y, front
    /// +z) — the same flip [`vbox`] bakes for every third-person limb — so
    /// [`plane_frac`]/[`box_region`] map the skin exactly as the third-person
    /// right arm does. The caller's `transform` is a verbatim vanilla
    /// `renderPlayerArm` GL chain that operates in vanilla model coords (front
    /// −z, y-down), so each engine-local vertex is converted back to vanilla
    /// coords (`y → 1.5 − y`, `z → −z`, with 1.5 = 24·1/16) before the chain.
    /// This leaves the arm geometry/pose pixel-identical while correcting the
    /// previously mirrored texture mapping.
    pub fn push_arm_box(&mut self, transform: &dyn Fn(Vec3) -> Vec3, alpha: f32) {
        let region = box_region(40.0, 16.0, 4.0, 12.0, 4.0); // player right arm
        let (ox, oy) = entity_slot_origin(EntitySlot::Player);
        let to_vanilla = |e: Vec3| transform(Vec3::new(e.x, 1.5 - e.y, -e.z));
        self.push_textured_box(
            Vec3::new(-3.0, 14.0, -2.0) * 0.0625,
            Vec3::new(1.0, 26.0, 2.0) * 0.0625,
            &region,
            [ox as f32, oy as f32],
            alpha,
            &to_vanilla,
        );
    }

    /// Append an entity model at `feet` (bottom-center of the entity), at the
    /// fixed 1/16-block model scale and rotated to face `yaw_degrees`. Players
    /// render as the skinned humanoid; known mobs render as their archetype (biped,
    /// quadruped, creeper or chicken) sampling that mob's atlas slot; armor
    /// stands (object type 78) render the wooden stand model. Unmodelled mobs
    /// and other object types emit no geometry (hidden) rather than a
    /// placeholder box.
    pub fn push_entity(
        &mut self,
        kind: EntityKind,
        feet: Vec3,
        yaw_degrees: f32,
        anim: &EntityAnim,
        skin_row: Option<u32>,
    ) {
        // Apply this entity's death tilt to every part it pushes, then clear it so
        // following non-entity geometry (chests/books) draws upright.
        self.death_roll = anim.death_roll;
        match kind {
            EntityKind::LocalPlayer | EntityKind::RemotePlayer => {
                // A downloaded skin row when one is allocated, else the shared
                // default player slot.
                let row = skin_row
                    .map(|i| PLAYER_SKIN_BASE_ROW + i)
                    .unwrap_or(EntitySlot::Player as u32);
                self.push_parts(&humanoid_parts(true), &humanoid_poses(anim), row, feet, yaw_degrees);
            }
            EntityKind::Mob(id) => {
                // Unmodelled mob types are hidden rather than drawn as a
                // placeholder box that could be mistaken for a real entity.
                if let Some(model) = mob_model(id) {
                    self.push_mob(model, feet, yaw_degrees, anim);
                }
            }
            // Armor stand (object type 78): the wooden stand model. Every other
            // object type is unmodelled and hidden (item entities are drawn in
            // the separate world-item pass, never reaching here).
            EntityKind::Object(78) => {
                self.push_parts(
                    &armor_stand_parts(),
                    &[],
                    EntitySlot::ArmorStand as u32,
                    feet,
                    yaw_degrees,
                );
            }
            // Arrow: the flight pitch rides in `anim.head_pitch` (the entity's
            // interpolated render pitch); yaw is the body yaw.
            EntityKind::Object(60) => self.push_arrow(feet, yaw_degrees, anim.head_pitch),
            EntityKind::Object(_) => {}
            // Experience orbs are drawn as colour-cycling billboards in their
            // own pass, never as an articulated model.
            EntityKind::ExperienceOrb => {}
        }
        self.death_roll = 0.0;
    }

    /// Build one known mob from its archetype: pick the part list, pose list and
    /// atlas slot, then emit the boxes. Sheep layer their wool over the body.
    fn push_mob(&mut self, model: MobModel, feet: Vec3, yaw: f32, anim: &EntityAnim) {
        match model {
            MobModel::Humanoid { slot, separate } => {
                self.push_parts(&humanoid_parts(separate), &humanoid_poses(anim), slot as u32, feet, yaw);
            }
            MobModel::Villager => {
                self.push_parts(&villager_parts(), &villager_poses(anim), EntitySlot::Villager as u32, feet, yaw);
            }
            MobModel::Enderman => {
                self.push_parts(&enderman_parts(), &enderman_poses(anim), EntitySlot::Enderman as u32, feet, yaw);
            }
            MobModel::Pig => {
                self.push_parts(&pig_parts(), &pig_poses(anim), EntitySlot::Pig as u32, feet, yaw);
            }
            MobModel::Cow { slot } => {
                self.push_parts(&cow_parts(), &cow_poses(anim), slot as u32, feet, yaw);
            }
            MobModel::Sheep => {
                let poses = sheep_poses(anim);
                self.push_parts(&sheep_parts(), &poses, EntitySlot::Sheep as u32, feet, yaw);
                self.push_parts(&sheep_wool_parts(), &poses, EntitySlot::SheepFur as u32, feet, yaw);
            }
            MobModel::Wolf => {
                self.push_parts(&wolf_parts(), &wolf_poses(anim), EntitySlot::Wolf as u32, feet, yaw);
            }
            MobModel::Spider { slot } => {
                self.push_parts(&spider_parts(), &spider_poses(anim), slot as u32, feet, yaw);
            }
            MobModel::Cat => {
                self.push_parts(&cat_parts(), &cat_poses(anim), EntitySlot::Ocelot as u32, feet, yaw);
            }
            MobModel::Creeper => {
                self.push_parts(&creeper_parts(), &creeper_poses(anim), EntitySlot::Creeper as u32, feet, yaw);
            }
            MobModel::Chicken => {
                self.push_parts(&chicken_parts(), &chicken_poses(anim), EntitySlot::Chicken as u32, feet, yaw);
            }
            MobModel::Cube { slot, size_px } => {
                self.push_parts(&cube_parts(size_px), &[], slot as u32, feet, yaw);
            }
            MobModel::Squid => {
                self.push_parts(&squid_parts(), &[], EntitySlot::Squid as u32, feet, yaw);
            }
            MobModel::Snowman => {
                self.push_parts(&snowman_parts(), &snowman_poses(anim), EntitySlot::Snowman as u32, feet, yaw);
            }
            MobModel::Bat => {
                self.push_parts(&bat_parts(), &bat_poses(anim), EntitySlot::Bat as u32, feet, yaw);
            }
            MobModel::Insect { slot } => {
                self.push_parts(&insect_parts(), &insect_poses(anim), slot as u32, feet, yaw);
            }
            MobModel::IronGolem => {
                self.push_parts(&iron_golem_parts(), &iron_golem_poses(anim), EntitySlot::IronGolem as u32, feet, yaw);
            }
            MobModel::Horse => {
                self.push_parts(&horse_parts(), &horse_poses(anim), EntitySlot::Horse as u32, feet, yaw);
            }
            MobModel::Witch => {
                self.push_parts(&witch_parts(), &witch_poses(anim), EntitySlot::Witch as u32, feet, yaw);
            }
            MobModel::Guardian => {
                self.push_parts(&guardian_parts(), &guardian_poses(anim), EntitySlot::Guardian as u32, feet, yaw);
            }
            MobModel::Wither => {
                self.push_parts(&wither_parts(), &wither_poses(anim), EntitySlot::Wither as u32, feet, yaw);
            }
            MobModel::Rabbit => {
                self.push_parts(&rabbit_parts(), &rabbit_poses(anim), EntitySlot::Rabbit as u32, feet, yaw);
            }
            MobModel::Floating { blaze: false } => {
                self.push_parts(&ghast_parts(), &[], EntitySlot::Ghast as u32, feet, yaw);
            }
            MobModel::Floating { blaze: true } => {
                self.push_parts(&blaze_parts(), &blaze_poses(anim), EntitySlot::Blaze as u32, feet, yaw);
            }
        }
    }

    /// Build a model from px-space parts (feet at y=0, +y up, +z front): scale
    /// px→world by [`MODEL_SCALE`], rotate every part about the vertical axis
    /// through `feet` by `yaw_degrees`, and sample each face's texture rect
    /// inside the atlas slot at flat grid index `slot_index`.
    fn push_parts(
        &mut self,
        parts: &[Part],
        poses: &[PartPose],
        slot_index: u32,
        feet: Vec3,
        yaw_degrees: f32,
    ) {
        let scale = MODEL_SCALE;
        let yaw = yaw_degrees.to_radians();
        let (sin, cos) = (yaw.sin(), yaw.cos());
        // Death fall-over tilt: a roll about the model's forward (Z) axis through
        // the feet, applied BEFORE the body yaw (vanilla applies it after the yaw
        // in GL post-multiply order, i.e. first to the vertex). Copied out of
        // `self` so the per-box closure doesn't borrow `self` alongside the
        // `&mut self` `push_textured_box` call.
        let (droll_sin, droll_cos) = self.death_roll.sin_cos();
        let (ox, oy) = slot_grid_origin(slot_index);
        let origin = [ox as f32, oy as f32];
        // Box bounds stay in model px; the per-part closure articulates each
        // box about its pivot, scales px→world, then rolls (death) and rotates by
        // the body yaw.
        for (i, (min_px, max_px, region)) in parts.iter().enumerate() {
            let pose = poses.get(i).copied().unwrap_or_else(PartPose::still);
            let min = Vec3::from(*min_px);
            let max = Vec3::from(*max_px);
            self.push_textured_box(min, max, region, origin, 1.0, &|local_px| {
                let articulated = apply_part_rotation(local_px, &pose) * scale;
                let rolled = rotate_z(articulated, droll_sin, droll_cos);
                rotate_y(rolled, sin, cos) + feet
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

    /// Push one transformed quad: four `(local_position, uv)` corners mapped to
    /// world by `m`, two triangles, flat `color`.
    fn push_model_quad(&mut self, m: Mat4, corners: [([f32; 3], [f32; 2]); 4], color: [f32; 4]) {
        let base = self.vertices.len() as u32;
        for (p, uv) in corners {
            self.vertices.push(ModelVertex {
                position: m.transform_point3(Vec3::from(p)).to_array(),
                color,
                uv,
            });
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Vanilla `RenderArrow`: a 3D arrow built from four cross blades along the
    /// shaft plus the fletched tail caps, oriented to `yaw`/`pitch` (its flight
    /// direction). Sampled from the 32×32 `entity/arrow.png` in its atlas slot.
    /// Arrows carry no living-entity model flip, so the geometry is placed in
    /// world axes through the verbatim vanilla GL chain (`rotate(yaw-90, Y)`,
    /// `rotate(pitch, Z)`, the 45° roll, `scale(0.05625)`, `translate(-4)`); the
    /// model pass doesn't cull, so winding is irrelevant. Lit afterwards by the
    /// caller's per-entity lightmap scale (the white vertex colour is the base).
    fn push_arrow(&mut self, feet: Vec3, yaw_degrees: f32, pitch_degrees: f32) {
        // The arrow texture occupies the top-left 32px of its 128px atlas slot.
        let (ox, oy) = entity_slot_origin(EntitySlot::Arrow);
        let uv = |u: f32, v: f32| {
            [
                (ox as f32 + u * 32.0) / ENTITY_ATLAS_WIDTH as f32,
                (oy as f32 + v * 32.0) / ENTITY_ATLAS_HEIGHT as f32,
            ]
        };

        let base = Mat4::from_translation(feet)
            * Mat4::from_rotation_y((yaw_degrees - 90.0).to_radians())
            * Mat4::from_rotation_z(pitch_degrees.to_radians())
            * Mat4::from_rotation_x(45.0_f32.to_radians())
            * Mat4::from_scale(Vec3::splat(0.05625))
            * Mat4::from_translation(Vec3::new(-4.0, 0.0, 0.0));

        let color = [1.0, 1.0, 1.0, 1.0];

        // Tail fletch caps at x=-7 (front + back). UV: u 0..5/32, v 5/32..10/32.
        let (v0, v1, uc) = (5.0 / 32.0, 10.0 / 32.0, 5.0 / 32.0);
        self.push_model_quad(
            base,
            [
                ([-7.0, -2.0, -2.0], uv(0.0, v0)),
                ([-7.0, -2.0, 2.0], uv(uc, v0)),
                ([-7.0, 2.0, 2.0], uv(uc, v1)),
                ([-7.0, 2.0, -2.0], uv(0.0, v1)),
            ],
            color,
        );
        self.push_model_quad(
            base,
            [
                ([-7.0, 2.0, -2.0], uv(0.0, v0)),
                ([-7.0, 2.0, 2.0], uv(uc, v0)),
                ([-7.0, -2.0, 2.0], uv(uc, v1)),
                ([-7.0, -2.0, -2.0], uv(0.0, v1)),
            ],
            color,
        );

        // Four shaft blades, each a further 90° about the shaft (X) axis. UV:
        // u 0..0.5 (the shaft strip), v 0..5/32.
        let sv = 5.0 / 32.0;
        for j in 1..=4 {
            let m = base * Mat4::from_rotation_x((90.0 * j as f32).to_radians());
            self.push_model_quad(
                m,
                [
                    ([-8.0, -2.0, 0.0], uv(0.0, 0.0)),
                    ([8.0, -2.0, 0.0], uv(0.5, 0.0)),
                    ([8.0, 2.0, 0.0], uv(0.5, sv)),
                    ([-8.0, 2.0, 0.0], uv(0.0, sv)),
                ],
                color,
            );
        }
    }
}

/// Which model archetype a 1.8 SpawnMob entity-type id maps to.
enum MobModel {
    /// Player-shaped biped. `separate` uses the 64x64 left-limb regions (zombie,
    /// pigman); false mirrors the right limbs for 64x32 skins (skeleton).
    Humanoid { slot: EntitySlot, separate: bool },
    Villager,
    Enderman,
    Pig,
    Cow { slot: EntitySlot },
    Sheep,
    Wolf,
    Spider { slot: EntitySlot },
    Cat,
    Creeper,
    Chicken,
    /// A single textured cube (slime / magma cube), `size_px` on a side.
    Cube { slot: EntitySlot, size_px: f32 },
    Squid,
    Snowman,
    Bat,
    /// A small ground crawler (silverfish / endermite).
    Insect { slot: EntitySlot },
    IronGolem,
    Horse,
    Witch,
    Guardian,
    Wither,
    Rabbit,
    /// Ghast (cube body + dangling tentacles) / blaze (head + rods). `blaze`
    /// switches the part list and slot.
    Floating { blaze: bool },
}

/// 1.8 SpawnMob type ids -> model archetype. Ids without a model return None and
/// are hidden. The ender dragon (63) is intentionally left unmodelled: its
/// multi-segment animated model is out of scope for a single box-archetype.
fn mob_model(id: u8) -> Option<MobModel> {
    use EntitySlot::*;
    Some(match id {
        50 => MobModel::Creeper,
        // 1.8 mob bipeds MIRROR the right arm/leg (ModelBiped, like the player's
        // legacy 64x32 layout): their 64x64 textures leave the separate-left-limb
        // regions (32,48)/(16,48) empty/transparent. So zombie/giant/pigman use
        // separate=false (mirror) like the skeleton — separate=true would sample
        // those transparent regions and drop the left arm + left leg.
        51 => MobModel::Humanoid { slot: Skeleton, separate: false },
        // Giant is a scaled-up zombie; render it as a zombie biped.
        53 | 54 => MobModel::Humanoid { slot: Zombie, separate: false },
        57 => MobModel::Humanoid { slot: ZombiePigman, separate: false },
        120 => MobModel::Villager,
        58 => MobModel::Enderman,
        52 | 59 => MobModel::Spider { slot: Spider }, // spider, cave spider
        90 => MobModel::Pig,
        91 => MobModel::Sheep,
        92 => MobModel::Cow { slot: Cow },
        96 => MobModel::Cow { slot: Mooshroom },
        93 => MobModel::Chicken,
        94 => MobModel::Squid,
        95 => MobModel::Wolf,
        97 => MobModel::Snowman,
        98 => MobModel::Cat,
        55 => MobModel::Cube { slot: Slime, size_px: 8.0 },
        62 => MobModel::Cube { slot: MagmaCube, size_px: 8.0 },
        65 => MobModel::Bat,
        60 => MobModel::Insect { slot: Silverfish },
        67 => MobModel::Insect { slot: Silverfish }, // endermite (silverfish stand-in)
        99 => MobModel::IronGolem,
        100 => MobModel::Horse,
        66 => MobModel::Witch,
        56 => MobModel::Floating { blaze: false }, // ghast
        61 => MobModel::Floating { blaze: true },  // blaze
        68 => MobModel::Guardian,
        64 => MobModel::Wither,
        101 => MobModel::Rabbit,
        _ => return None,
    })
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

/// The seven humanoid parts with the standard 1.8 skin layout. With
/// `separate_left_limbs` (64x64 player skins) the left arm/leg use their own
/// regions; without it (64x32 mob skins: zombie/skeleton/villager slots) the
/// left limbs mirror the right-limb regions. The seventh part is the hat
/// overlay (vanilla `ModelBiped.bipedHeadwear`): an inflated head box sampling
/// the head-overlay UV region at (32,0); on legacy 64x32 skins that region is
/// transparent so the overlay simply adds nothing.
fn humanoid_parts(separate_left_limbs: bool) -> [Part; 7] {
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
    // Hat overlay grown 0.5px on every axis, same head box, UV at (32,0).
    // Vanilla bipedHeadwear: rp(0,0,0), addBox(-4,-8,-4, 8,8,8, scale+0.5).
    let hat = vbox([0.0, 0.0, 0.0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [32.0, 0.0], 0.5);
    [
        ([-4.0, 0.0, -2.0], [0.0, 12.0, 2.0], leg_r), // right leg
        ([0.0, 0.0, -2.0], [4.0, 12.0, 2.0], leg_l),  // left leg
        ([-4.0, 12.0, -2.0], [4.0, 24.0, 2.0], body), // body
        ([-8.0, 12.0, -2.0], [-4.0, 24.0, 2.0], arm_r), // right arm
        ([4.0, 12.0, -2.0], [8.0, 24.0, 2.0], arm_l), // left arm
        ([-4.0, 24.0, -4.0], [4.0, 32.0, 4.0], head), // head
        hat,                                          // hat overlay
    ]
}

/// The four standard quadruped legs (vanilla `ModelQuadruped` base): front-right,
/// front-left, back-right, back-left. All 4×leg_h×4 at tex (0,16).
fn quadruped_legs(leg_h: f32) -> [Part; 4] {
    let rp_y = 24.0 - leg_h;
    [
        vbox([-3.0, rp_y, -5.0], [-2.0, 0.0, -2.0], [4.0, leg_h, 4.0], [0.0, 16.0], 0.0),
        vbox([3.0, rp_y, -5.0], [-2.0, 0.0, -2.0], [4.0, leg_h, 4.0], [0.0, 16.0], 0.0),
        vbox([-3.0, rp_y, 7.0], [-2.0, 0.0, -2.0], [4.0, leg_h, 4.0], [0.0, 16.0], 0.0),
        vbox([3.0, rp_y, 7.0], [-2.0, 0.0, -2.0], [4.0, leg_h, 4.0], [0.0, 16.0], 0.0),
    ]
}

/// Standard quadruped (vanilla `ModelQuadruped`): a horizontal 10×16×8 body on
/// four `leg_h`-tall legs with an 8×8×8 head.
fn quadruped_parts(leg_h: f32) -> [Part; 6] {
    let l = quadruped_legs(leg_h);
    let body = vbox_prone(
        [-5.0, leg_h, -8.0], [5.0, leg_h + 8.0, 8.0],
        [28.0, 8.0], [10.0, 16.0, 8.0],
    );
    let head = vbox([0.0, 18.0 - leg_h, -6.0], [-4.0, -4.0, -8.0], [8.0, 8.0, 8.0], [0.0, 0.0], 0.0);
    [l[0], l[1], l[2], l[3], body, head]
}

/// Pig (`ModelPig`): standard quadruped (leg_h=6) plus a snout box on the head.
fn pig_parts() -> [Part; 7] {
    let base = quadruped_parts(6.0);
    let snout = vbox([0.0, 12.0, -6.0], [-2.0, 0.0, -9.0], [4.0, 3.0, 1.0], [16.0, 16.0], 0.0);
    [base[0], base[1], base[2], base[3], base[4], base[5], snout]
}

/// Cow (`ModelCow`): quadruped legs (leg_h=12), a 10×16×8 body, an 8×8×6 head
/// and two horns.
fn cow_parts() -> [Part; 8] {
    let l = quadruped_legs(12.0);
    let body = vbox_prone([-5.0, 12.0, -8.0], [5.0, 20.0, 8.0], [28.0, 8.0], [10.0, 16.0, 8.0]);
    let head = vbox([0.0, 4.0, -8.0], [-4.0, -4.0, -6.0], [8.0, 8.0, 6.0], [0.0, 0.0], 0.0);
    let horn_r = vbox([0.0, 4.0, -8.0], [-4.0, -5.0, -4.0], [1.0, 3.0, 1.0], [22.0, 0.0], 0.0);
    let horn_l = vbox([0.0, 4.0, -8.0], [3.0, -5.0, -4.0], [1.0, 3.0, 1.0], [22.0, 0.0], 0.0);
    [l[0], l[1], l[2], l[3], body, head, horn_r, horn_l]
}

/// Sheep base (`ModelSheep1`): quadruped legs (leg_h=12) with a narrower 8×16×6
/// body and a 6×6×8 head.
fn sheep_parts() -> [Part; 6] {
    let l = quadruped_legs(12.0);
    let body = vbox_prone([-4.0, 12.0, -8.0], [4.0, 18.0, 8.0], [28.0, 8.0], [8.0, 16.0, 6.0]);
    let head = vbox([0.0, 6.0, -8.0], [-3.0, -4.0, -6.0], [6.0, 6.0, 8.0], [0.0, 0.0], 0.0);
    [l[0], l[1], l[2], l[3], body, head]
}

/// Sheep wool (`ModelSheep2`): inflated overlay sampled from the `sheep_fur` slot.
/// Head is 6×6×6 inflated 0.6, body is 8×16×6 inflated 1.75; legs are uninflated.
fn sheep_wool_parts() -> [Part; 6] {
    let l = quadruped_legs(12.0);
    let head = vbox([0.0, 6.0, -8.0], [-3.0, -4.0, -4.0], [6.0, 6.0, 6.0], [0.0, 0.0], 0.6);
    let body = vbox_prone(
        [-5.75, 10.25, -9.75], [5.75, 19.75, 9.75],
        [28.0, 8.0], [8.0, 16.0, 6.0],
    );
    [l[0], l[1], l[2], l[3], body, head]
}

/// Creeper: four short legs, a tall upright body and a head on top, using the
/// 1.8 creeper texture layout (head 0,0; body 16,16; legs 0,16).
fn creeper_parts() -> [Part; 6] {
    let head = box_region(0.0, 0.0, 8.0, 8.0, 8.0);
    let body = box_region(16.0, 16.0, 8.0, 12.0, 4.0);
    let leg = box_region(0.0, 16.0, 4.0, 6.0, 4.0);
    [
        ([-4.0, 0.0, 2.0], [0.0, 6.0, 6.0], leg), // front right leg
        ([0.0, 0.0, 2.0], [4.0, 6.0, 6.0], leg),  // front left leg
        ([-4.0, 0.0, -6.0], [0.0, 6.0, -2.0], leg), // back right leg
        ([0.0, 0.0, -6.0], [4.0, 6.0, -2.0], leg), // back left leg
        ([-4.0, 6.0, -2.0], [4.0, 18.0, 2.0], body), // body
        ([-4.0, 18.0, -4.0], [4.0, 26.0, 4.0], head), // head
    ]
}

/// Chicken (`ModelChicken`): two legs, a prone body, a head with a bill and chin
/// (wattle), and two wings. Order: right leg, left leg, body, head, bill, chin,
/// right wing, left wing.
fn chicken_parts() -> [Part; 8] {
    [
        vbox([-2.0, 19.0, 1.0], [-1.0, 0.0, -3.0], [3.0, 5.0, 3.0], [26.0, 0.0], 0.0),
        vbox([1.0, 19.0, 1.0], [-1.0, 0.0, -3.0], [3.0, 5.0, 3.0], [26.0, 0.0], 0.0),
        vbox_prone([-3.0, 5.0, -4.0], [3.0, 11.0, 4.0], [0.0, 9.0], [6.0, 8.0, 6.0]),
        vbox([0.0, 15.0, -4.0], [-2.0, -6.0, -2.0], [4.0, 6.0, 3.0], [0.0, 0.0], 0.0),
        vbox([0.0, 15.0, -4.0], [-2.0, -4.0, -4.0], [4.0, 2.0, 2.0], [14.0, 0.0], 0.0),
        vbox([0.0, 15.0, -4.0], [-1.0, -2.0, -3.0], [2.0, 2.0, 2.0], [14.0, 4.0], 0.0),
        vbox([-4.0, 13.0, 0.0], [0.0, 0.0, -3.0], [1.0, 4.0, 6.0], [24.0, 13.0], 0.0),
        vbox([4.0, 13.0, 0.0], [-1.0, 0.0, -3.0], [1.0, 4.0, 6.0], [24.0, 13.0], 0.0),
    ]
}

/// Armor stand (`ModelArmorStand`, armorstand/wood.png 64x64): the wooden
/// stand — head, body, two legs, two vertical side posts, a waist bar and the
/// base plate. The default vanilla stand shows no arms (the `ShowArms` flag is
/// off), so they are omitted. Order is irrelevant since the model is static.
fn armor_stand_parts() -> [Part; 8] {
    [
        vbox([0.0, 0.0, 0.0], [-1.0, -7.0, -1.0], [2.0, 7.0, 2.0], [0.0, 0.0], 0.0), // head
        vbox([0.0, 0.0, 0.0], [-6.0, 0.0, -1.5], [12.0, 3.0, 3.0], [0.0, 26.0], 0.0), // body
        vbox([-1.9, 12.0, 0.0], [-1.0, 0.0, -1.0], [2.0, 11.0, 2.0], [8.0, 0.0], 0.0), // right leg
        vbox([1.9, 12.0, 0.0], [-1.0, 0.0, -1.0], [2.0, 11.0, 2.0], [40.0, 16.0], 0.0), // left leg
        vbox([0.0, 0.0, 0.0], [-3.0, 3.0, -1.0], [2.0, 7.0, 2.0], [16.0, 0.0], 0.0), // right side
        vbox([0.0, 0.0, 0.0], [1.0, 3.0, -1.0], [2.0, 7.0, 2.0], [48.0, 16.0], 0.0), // left side
        vbox([0.0, 0.0, 0.0], [-4.0, 10.0, -1.0], [8.0, 2.0, 2.0], [0.0, 48.0], 0.0), // waist
        vbox([0.0, 12.0, 0.0], [-6.0, 11.0, -6.0], [12.0, 1.0, 12.0], [0.0, 32.0], 0.0), // base
    ]
}

/// Port a vanilla 1.8 model box into an engine [`Part`]. Vanilla model space is
/// y-down with the feet at y=24 and the entity facing -z; the engine builds
/// feet-up (+y), front +z. `rp` is the part's rotation point, `off`/`size` the
/// `addBox` offset and dimensions, `tex` the texture-offset (u, v); `grow`
/// inflates the box outward on every axis (vanilla's `addBox` scale) for
/// overlay layers while keeping the original texture rect.
fn vbox(rp: [f32; 3], off: [f32; 3], size: [f32; 3], tex: [f32; 2], grow: f32) -> Part {
    let [rx, ry, rz] = rp;
    let [ox, oy, oz] = off;
    let [w, h, d] = size;
    let min = [
        rx + ox - grow,
        24.0 - (ry + oy + h) - grow,
        -(rz + oz + d) - grow,
    ];
    let max = [rx + ox + w + grow, 24.0 - (ry + oy) + grow, -(rz + oz) + grow];
    (min, max, box_region(tex[0], tex[1], w, h, d))
}

/// Engine-space pivot of a vanilla rotation point (feet-up, front +z).
fn vpivot(rp: [f32; 3]) -> Vec3 {
    Vec3::new(rp[0], 24.0 - rp[1], -rp[2])
}

/// Same as [`vbox`] but for a part rendered lying on its back (vanilla
/// `rotateAngleX = +PI/2`, e.g. the quadruped body), baked as a static box.
/// Rotating the upright box +90° about X in the engine frame (`(x,y,z) →
/// (x,-z,y)`) maps the box's local faces onto the world faces:
///   world top(+y)    ← local back  (rect `b[2]`)
///   world bottom(-y) ← local front (rect `b[3]`)
///   world back(-z)   ← local bottom(rect `b[0]`)
///   world front(+z)  ← local top   (rect `b[1]`)
/// and the x-faces (`b[4]`, `b[5]`) stay put. The rotation also turns each
/// face's in-plane texture axes, which the upright [`plane_frac`] does not
/// account for, so the top/bottom/back rects are flipped here to land the right
/// way up (verified texel-exact against vanilla `ModelBox` + the GL rotation):
/// top needs a U-flip, bottom a V-flip, back a U-flip, front is already upright.
/// The x-end-caps (`b[4]`/`b[5]`) would additionally need a 90° transpose that a
/// `[x0,y0,x1,y1]` rect cannot encode; they are left as-is (the prior behaviour).
fn vbox_prone(min: [f32; 3], max: [f32; 3], tex: [f32; 2], size: [f32; 3]) -> Part {
    let b = box_region(tex[0], tex[1], size[0], size[1], size[2]);
    let flip_u = |r: [f32; 4]| [r[2], r[1], r[0], r[3]];
    let flip_v = |r: [f32; 4]| [r[0], r[3], r[2], r[1]];
    (
        min,
        max,
        [flip_v(b[3]), flip_u(b[2]), flip_u(b[0]), b[1], b[4], b[5]],
    )
}

/// Walk-cycle leg/arm angle: `cos(limbSwing*0.6662 + phase) * scale * amount`.
fn swing(limb_swing: f32, amount: f32, phase: f32, scale: f32) -> f32 {
    (limb_swing * 0.6662 + phase).cos() * scale * amount
}

/// Humanoid (player / biped mob) articulation: opposite-phase leg/arm swing,
/// head yaw+pitch, the attack arm-swing on the right arm, and the sneak crouch.
/// Order matches [`humanoid_parts`]: right leg, left leg, body, right arm, left
/// arm, head, hat (the hat tracks the head).
fn humanoid_poses(anim: &EntityAnim) -> [PartPose; 7] {
    use std::f32::consts::{FRAC_PI_6, PI};
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let head_y = anim.net_head_yaw.to_radians();
    let head_x = anim.head_pitch.to_radians();

    // Right arm and right leg are a half-cycle apart (natural opposite swing).
    let mut arm_r_x = swing(s, a, PI, 1.0);
    let arm_l_x = swing(s, a, 0.0, 1.0);
    let mut arm_r_y = 0.0;
    let mut arm_r_z = 0.0;
    let mut body_y = 0.0;

    // Vanilla `ModelBiped.setRotationAngles` heldItemRight branch (applied before
    // the attack swing): holding an item lowers the arm, blocking lowers it more
    // and cants it inward so the sword crosses the body.
    match anim.held_item_right {
        1 => arm_r_x = arm_r_x * 0.5 - PI / 10.0,
        3 => {
            arm_r_x = arm_r_x * 0.5 - PI / 10.0 * 3.0;
            arm_r_y = -FRAC_PI_6; // vanilla -0.5235988 (-30°)
        }
        _ => {}
    }

    if anim.swing_progress > 0.0 {
        let sp = anim.swing_progress;
        // Vanilla `ModelBiped`: ease the attack swing, then take the sine.
        // 1.8 uses a quartic (`f = (1-sp)²; f = f²`); 1.7 (OldAnimations) used a
        // cubic (`f = (1-sp)³`), which throws the arm forward more sharply.
        let f0 = 1.0 - sp;
        let f = if anim.old_animations {
            1.0 - f0 * f0 * f0
        } else {
            let q = f0 * f0;
            1.0 - q * q
        };
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
            Vec3::new(arm_r_x + arm_extra, arm_r_y, arm_r_z),
        ),
        grouped(
            Vec3::new(6.0, 24.0, 0.0),
            Vec3::new(arm_l_x + arm_extra, 0.0, 0.0),
        ),
        grouped(Vec3::new(0.0, 24.0, 0.0), Vec3::new(head_x, head_y, 0.0)),
        grouped(Vec3::new(0.0, 24.0, 0.0), Vec3::new(head_x, head_y, 0.0)), // hat tracks the head
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
fn quadruped_poses(anim: &EntityAnim, leg_h: f32, head_pivot: Vec3) -> [PartPose; 6] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let pa = swing(s, a, 0.0, 1.4);
    let pb = swing(s, a, PI, 1.4);
    [
        leg_pose(Vec3::new(-3.0, leg_h, 5.0), pb),  // front right
        leg_pose(Vec3::new(3.0, leg_h, 5.0), pa),   // front left
        leg_pose(Vec3::new(-3.0, leg_h, -7.0), pa), // back right
        leg_pose(Vec3::new(3.0, leg_h, -7.0), pb),  // back left
        PartPose::still(),                           // body
        head_pose(anim, head_pivot),
    ]
}

fn pig_poses(anim: &EntityAnim) -> [PartPose; 7] {
    let hp = vpivot([0.0, 12.0, -6.0]);
    let base = quadruped_poses(anim, 6.0, hp);
    [base[0], base[1], base[2], base[3], base[4], base[5], base[5]]
}

fn cow_poses(anim: &EntityAnim) -> [PartPose; 8] {
    let hp = vpivot([0.0, 4.0, -8.0]);
    let base = quadruped_poses(anim, 12.0, hp);
    [base[0], base[1], base[2], base[3], base[4], base[5], base[5], base[5]]
}

fn sheep_poses(anim: &EntityAnim) -> [PartPose; 6] {
    quadruped_poses(anim, 12.0, vpivot([0.0, 6.0, -8.0]))
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

/// Chicken articulation: legs alternate, head turns (bill+chin track the head),
/// wings flap via z-rotation. Order matches [`chicken_parts`].
fn chicken_poses(anim: &EntityAnim) -> [PartPose; 8] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let head = head_pose(anim, vpivot([0.0, 15.0, -4.0]));
    let wing_flap = swing(s, a, 0.0, 0.5);
    let wing = |pivot: Vec3, sign: f32| PartPose {
        pivot,
        angles: Vec3::new(0.0, 0.0, sign * wing_flap),
        group_pivot: Vec3::ZERO,
        group_angle_x: 0.0,
    };
    [
        leg_pose(vpivot([-2.0, 19.0, 1.0]), swing(s, a, 0.0, 1.4)),
        leg_pose(vpivot([1.0, 19.0, 1.0]), swing(s, a, PI, 1.4)),
        PartPose::still(),
        head,
        head,
        head,
        wing(vpivot([-4.0, 13.0, 0.0]), 1.0),
        wing(vpivot([4.0, 13.0, 0.0]), -1.0),
    ]
}

/// Villager (`ModelVillager`): a big head with a protruding nose, a long robed
/// body, crossed arms held across the belly, and two legs. Order: head, nose,
/// body, robe, right arm, left arm, crossed forearms, right leg, left leg.
fn villager_parts() -> [Part; 9] {
    [
        vbox([0.0, 0.0, 0.0], [-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [0.0, 0.0], 0.0), // head
        vbox([0.0, -2.0, 0.0], [-1.0, -1.0, -6.0], [2.0, 4.0, 2.0], [24.0, 0.0], 0.0), // nose
        vbox([0.0, 0.0, 0.0], [-4.0, 0.0, -3.0], [8.0, 20.0, 6.0], [16.0, 20.0], 0.0), // body
        vbox([0.0, 0.0, 0.0], [-4.0, 0.0, -3.0], [8.0, 18.0, 6.0], [0.0, 38.0], 0.5), // robe
        vbox([0.0, 3.0, -1.0], [-8.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0], 0.0), // right arm
        vbox([0.0, 3.0, -1.0], [4.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0], 0.0), // left arm
        vbox([0.0, 3.0, -1.0], [-4.0, 2.0, -2.0], [8.0, 4.0, 4.0], [40.0, 38.0], 0.0), // crossed forearms
        vbox([-2.0, 12.0, 0.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0], 0.0), // right leg
        vbox([2.0, 12.0, 0.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0], 0.0), // left leg
    ]
}

fn villager_poses(anim: &EntityAnim) -> [PartPose; 9] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let head = head_pose(anim, vpivot([0.0, 0.0, 0.0]));
    // The three arm boxes share one downward tilt so they read as crossed.
    let arms = PartPose {
        pivot: vpivot([0.0, 3.0, -1.0]),
        angles: Vec3::new(-0.75, 0.0, 0.0),
        group_pivot: Vec3::ZERO,
        group_angle_x: 0.0,
    };
    [
        head,
        head, // nose tracks the head
        PartPose::still(),
        PartPose::still(),
        arms,
        arms,
        arms,
        leg_pose(vpivot([-2.0, 12.0, 0.0]), swing(s, a, 0.0, 1.4)),
        leg_pose(vpivot([2.0, 12.0, 0.0]), swing(s, a, PI, 1.4)),
    ]
}

/// Enderman (`ModelEnderman`): a tall, slim biped with 2×30×2 limbs (vanilla
/// uses tex(56,0) for all four) and body at tex(32,16). The -14 Y offset in
/// vanilla puts the model top at y=46 in engine space.
fn enderman_parts() -> [Part; 6] {
    let head = box_region(0.0, 0.0, 8.0, 8.0, 8.0);
    let body = box_region(32.0, 16.0, 8.0, 12.0, 4.0);
    let limb = box_region(56.0, 0.0, 2.0, 30.0, 2.0);
    [
        ([-3.0, -4.0, -1.0], [-1.0, 26.0, 1.0], limb), // right leg
        ([1.0, -4.0, -1.0], [3.0, 26.0, 1.0], limb),   // left leg
        ([-4.0, 26.0, -2.0], [4.0, 38.0, 2.0], body),   // body
        ([-4.0, 8.0, -1.0], [-2.0, 38.0, 1.0], limb),   // right arm
        ([4.0, 8.0, -1.0], [6.0, 38.0, 1.0], limb),     // left arm
        ([-4.0, 38.0, -4.0], [4.0, 46.0, 4.0], head),   // head
    ]
}

fn enderman_poses(anim: &EntityAnim) -> [PartPose; 6] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    [
        leg_pose(Vec3::new(-2.0, 26.0, 0.0), swing(s, a, 0.0, 1.0)),
        leg_pose(Vec3::new(2.0, 26.0, 0.0), swing(s, a, PI, 1.0)),
        PartPose::still(),
        leg_pose(Vec3::new(-3.0, 36.0, 0.0), swing(s, a, PI, 0.6)),
        leg_pose(Vec3::new(5.0, 36.0, 0.0), swing(s, a, 0.0, 0.6)),
        head_pose(anim, Vec3::new(0.0, 38.0, 0.0)),
    ]
}

/// Wolf (`ModelWolf`): four legs, a horizontal body and mane, a boxy head at
/// the front and a hanging tail. Order: 4 legs (back-right, back-left,
/// front-right, front-left), body, mane, head, tail.
fn wolf_parts() -> [Part; 8] {
    let leg = box_region(0.0, 18.0, 2.0, 8.0, 2.0);
    [
        ([-3.5, 0.0, -8.0], [-1.5, 8.0, -6.0], leg), // back right
        ([-0.5, 0.0, -8.0], [1.5, 8.0, -6.0], leg),  // back left
        ([-3.5, 0.0, 3.0], [-1.5, 8.0, 5.0], leg),   // front right
        ([-0.5, 0.0, 3.0], [1.5, 8.0, 5.0], leg),    // front left
        vbox_prone([-3.0, 8.0, -6.0], [3.0, 14.0, 3.0], [18.0, 14.0], [6.0, 9.0, 6.0]), // body
        vbox_prone([-4.0, 8.0, -1.0], [4.0, 15.0, 5.0], [21.0, 0.0], [8.0, 6.0, 7.0]), // mane
        ([-4.0, 7.5, 5.0], [2.0, 13.5, 9.0], box_region(0.0, 0.0, 6.0, 6.0, 4.0)), // head
        ([-2.0, 4.0, -9.0], [0.0, 12.0, -7.0], box_region(9.0, 18.0, 2.0, 8.0, 2.0)), // tail
    ]
}

fn wolf_poses(anim: &EntityAnim) -> [PartPose; 8] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let (pa, pb) = (swing(s, a, 0.0, 1.4), swing(s, a, PI, 1.4));
    [
        leg_pose(Vec3::new(-2.5, 8.0, -7.0), pb), // back right
        leg_pose(Vec3::new(0.5, 8.0, -7.0), pa),  // back left
        leg_pose(Vec3::new(-2.5, 8.0, 4.0), pa),  // front right
        leg_pose(Vec3::new(0.5, 8.0, 4.0), pb),   // front left
        PartPose::still(),                        // body
        PartPose::still(),                        // mane
        head_pose(anim, Vec3::new(-1.0, 10.5, 7.0)),
        PartPose::still(), // tail
    ]
}

/// Spider (`ModelSpider`): head, thorax and abdomen along the body axis with
/// eight legs splayed out — four to each side, fanned front-to-back and angled
/// down. Order: head, thorax, abdomen, 4 right legs, 4 left legs.
fn spider_parts() -> [Part; 11] {
    let leg = box_region(18.0, 0.0, 16.0, 2.0, 2.0);
    let right = ([-19.0, 8.0, -1.0], [-3.0, 10.0, 1.0], leg); // extends -x
    let left = ([3.0, 8.0, -1.0], [19.0, 10.0, 1.0], leg); // extends +x
    [
        vbox([0.0, 15.0, -3.0], [-4.0, -4.0, -8.0], [8.0, 8.0, 8.0], [32.0, 4.0], 0.0), // head
        vbox([0.0, 15.0, 0.0], [-3.0, -3.0, -3.0], [6.0, 6.0, 6.0], [0.0, 0.0], 0.0), // thorax
        vbox([0.0, 15.0, 9.0], [-5.0, -4.0, -6.0], [10.0, 8.0, 12.0], [0.0, 12.0], 0.0), // abdomen
        right, right, right, right,
        left, left, left, left,
    ]
}

fn spider_poses(anim: &EntityAnim) -> [PartPose; 11] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let fans = [0.95, 0.35, -0.35, -0.95];
    // side = -1 right / +1 left; legs tilt down and fan, with a small walk wiggle.
    let mk = |side: f32, i: usize| PartPose {
        pivot: Vec3::new(3.0 * side, 9.0, 0.0),
        angles: Vec3::new(0.0, fans[i] + swing(s, a, i as f32 * PI, 0.3) * side, -0.55 * side),
        group_pivot: Vec3::ZERO,
        group_angle_x: 0.0,
    };
    [
        head_pose(anim, Vec3::new(0.0, 9.0, 3.0)),
        PartPose::still(),
        PartPose::still(),
        mk(-1.0, 0),
        mk(-1.0, 1),
        mk(-1.0, 2),
        mk(-1.0, 3),
        mk(1.0, 0),
        mk(1.0, 1),
        mk(1.0, 2),
        mk(1.0, 3),
    ]
}

/// Ocelot / cat (`ModelOcelot`): a small low body with a head, tail and four
/// legs (taller in front). Order: head, body, tail, front-right, front-left,
/// back-right, back-left legs.
fn cat_parts() -> [Part; 7] {
    let front = box_region(40.0, 0.0, 2.0, 10.0, 2.0);
    let back = box_region(8.0, 13.0, 2.0, 6.0, 2.0);
    [
        vbox([0.0, 15.0, -9.0], [-2.5, -2.0, -3.0], [5.0, 4.0, 5.0], [0.0, 0.0], 0.0), // head
        vbox_prone([-2.0, 6.0, -8.0], [2.0, 12.0, 8.0], [20.0, 0.0], [4.0, 16.0, 6.0]), // body
        vbox([0.0, 15.0, 8.0], [-0.5, 0.0, 0.0], [1.0, 8.0, 1.0], [0.0, 15.0], 0.0), // tail
        ([-2.2, 0.0, 3.0], [-0.2, 10.0, 5.0], front), // front right
        ([0.2, 0.0, 3.0], [2.2, 10.0, 5.0], front),   // front left
        ([-2.1, 0.0, -8.0], [-0.1, 6.0, -6.0], back), // back right
        ([0.1, 0.0, -8.0], [2.1, 6.0, -6.0], back),   // back left
    ]
}

fn cat_poses(anim: &EntityAnim) -> [PartPose; 7] {
    use std::f32::consts::PI;
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    [
        head_pose(anim, Vec3::new(0.0, 9.0, 9.0)),
        PartPose::still(), // body
        PartPose::still(), // tail
        leg_pose(Vec3::new(-1.2, 10.0, 4.0), swing(s, a, 0.0, 1.0)), // front right
        leg_pose(Vec3::new(1.2, 10.0, 4.0), swing(s, a, PI, 1.0)),   // front left
        leg_pose(Vec3::new(-1.1, 6.0, -7.0), swing(s, a, PI, 1.0)),  // back right
        leg_pose(Vec3::new(1.1, 6.0, -7.0), swing(s, a, 0.0, 1.0)),  // back left
    ]
}

/// Slime / magma cube: a single textured cube `size` px on a side.
fn cube_parts(size: f32) -> [Part; 1] {
    let h = size / 2.0;
    [(
        [-h, 0.0, -h],
        [h, size, h],
        box_region(0.0, 0.0, size, size, size),
    )]
}

/// Squid (`ModelSquid`): a 12×16×12 mantle with eight 2×18×2 tentacles hanging
/// beneath it in a circle of radius 5. All parts are static.
fn squid_parts() -> [Part; 9] {
    let body = box_region(0.0, 0.0, 12.0, 16.0, 12.0);
    let tent = box_region(48.0, 0.0, 2.0, 18.0, 2.0);
    let t = |x: f32, z: f32| -> Part {
        ([x - 1.0, -18.0, z - 1.0], [x + 1.0, 0.0, z + 1.0], tent)
    };
    [
        ([-6.0, 0.0, -6.0], [6.0, 16.0, 6.0], body),
        t(5.0, 0.0),
        t(3.5, -3.5),
        t(0.0, -5.0),
        t(-3.5, -3.5),
        t(-5.0, 0.0),
        t(-3.5, 3.5),
        t(0.0, 5.0),
        t(3.5, 3.5),
    ]
}

/// Snowman (`ModelSnowMan`): two stacked snow balls, a pumpkin head and two
/// 12×2×2 stick arms rotated ±1 rad around Z. Order: bottom, upper, head,
/// right arm, left arm.
fn snowman_parts() -> [Part; 5] {
    [
        ([-6.0, 0.0, -6.0], [6.0, 12.0, 6.0], box_region(0.0, 36.0, 12.0, 12.0, 12.0)),
        ([-5.0, 11.0, -5.0], [5.0, 21.0, 5.0], box_region(0.0, 16.0, 10.0, 10.0, 10.0)),
        ([-4.0, 20.0, -4.0], [4.0, 28.0, 4.0], box_region(0.0, 0.0, 8.0, 8.0, 8.0)),
        vbox([0.0, 6.0, -1.0], [-1.0, 0.0, -1.0], [12.0, 2.0, 2.0], [32.0, 0.0], 0.0),
        ([-11.0, 16.0, -2.0], [1.0, 18.0, 0.0], box_region(32.0, 0.0, 12.0, 2.0, 2.0)),
    ]
}

fn snowman_poses(anim: &EntityAnim) -> [PartPose; 5] {
    [
        PartPose::still(),
        PartPose::still(),
        head_pose(anim, Vec3::new(0.0, 20.0, 0.0)),
        PartPose {
            pivot: Vec3::new(0.0, 18.0, 1.0),
            angles: Vec3::new(0.0, 0.0, -1.0),
            group_pivot: Vec3::ZERO,
            group_angle_x: 0.0,
        },
        PartPose {
            pivot: Vec3::new(0.0, 18.0, -1.0),
            angles: Vec3::new(0.0, 0.0, 1.0),
            group_pivot: Vec3::ZERO,
            group_angle_x: 0.0,
        },
    ]
}

/// Bat (`ModelBat`): vanilla applies 0.35× scale in preRenderCallback, so all
/// coordinates are pre-multiplied by 0.35. Body tex(24,0) 6×12×6, head tex(0,0)
/// 6×6×6, wings tex(42,0) 10×16×1. Order: body, head, right wing, left wing.
fn bat_parts() -> [Part; 4] {
    [
        ([-1.05, 7.0, -1.05], [1.05, 11.2, 1.05], box_region(24.0, 0.0, 6.0, 12.0, 6.0)),
        ([-1.05, 7.35, -1.05], [1.05, 9.45, 1.05], box_region(0.0, 0.0, 6.0, 6.0, 6.0)),
        ([-4.2, 2.45, -0.875], [-0.7, 8.05, -0.525], box_region(42.0, 0.0, 10.0, 16.0, 1.0)),
        ([0.7, 2.45, -0.875], [4.2, 8.05, -0.525], box_region(42.0, 0.0, 10.0, 16.0, 1.0)),
    ]
}

fn bat_poses(anim: &EntityAnim) -> [PartPose; 4] {
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let flap = 0.4 + swing(s, a, 0.0, 0.8).abs();
    let wing = |pivot: Vec3, sign: f32| PartPose {
        pivot,
        angles: Vec3::new(0.0, 0.0, sign * flap),
        group_pivot: Vec3::ZERO,
        group_angle_x: 0.0,
    };
    [
        PartPose::still(),
        head_pose(anim, Vec3::new(0.0, 8.4, 0.0)),
        wing(Vec3::new(-1.05, 8.4, 0.0), 1.0),
        wing(Vec3::new(1.05, 8.4, 0.0), -1.0),
    ]
}

/// Silverfish / endermite: three low body segments tapering to a tail. All
/// parts are static (poses default to still).
fn insect_parts() -> [Part; 3] {
    [
        (
            [-1.5, 0.0, 2.0],
            [1.5, 3.0, 5.0],
            box_region(0.0, 0.0, 3.0, 3.0, 3.0),
        ), // front
        (
            [-2.5, 0.0, -3.0],
            [2.5, 4.0, 2.0],
            box_region(0.0, 9.0, 5.0, 4.0, 5.0),
        ), // middle
        (
            [-1.0, 0.0, -6.0],
            [1.0, 2.0, -3.0],
            box_region(20.0, 0.0, 2.0, 2.0, 3.0),
        ), // tail
    ]
}

fn insect_poses(anim: &EntityAnim) -> [PartPose; 3] {
    let _ = anim;
    [PartPose::still(), PartPose::still(), PartPose::still()]
}

/// Iron golem (`ModelIronGolem`, iron_golem.png 128x128): a tall head with a
/// nose, a wide blocky body with a hanging waist, two long arms and two thick
/// legs. Vanilla offsets the head/body/legs by -7px (`p_i46362_2_`), which is
/// folded into each rotation point here. Order: head, nose, body, waist, right
/// arm, left arm, right leg, left leg.
fn iron_golem_parts() -> [Part; 8] {
    [
        vbox([0.0, -7.0, -2.0], [-4.0, -12.0, -5.5], [8.0, 10.0, 8.0], [0.0, 0.0], 0.0), // head
        vbox([0.0, -7.0, -2.0], [-1.0, -5.0, -7.5], [2.0, 4.0, 2.0], [24.0, 0.0], 0.0), // nose
        vbox([0.0, -7.0, 0.0], [-9.0, -2.0, -6.0], [18.0, 12.0, 11.0], [0.0, 40.0], 0.0), // body
        vbox([0.0, -7.0, 0.0], [-4.5, 10.0, -3.0], [9.0, 5.0, 6.0], [0.0, 70.0], 0.5), // waist
        vbox([0.0, -7.0, 0.0], [-13.0, -2.5, -3.0], [4.0, 30.0, 6.0], [60.0, 21.0], 0.0), // right arm
        vbox([0.0, -7.0, 0.0], [9.0, -2.5, -3.0], [4.0, 30.0, 6.0], [60.0, 58.0], 0.0), // left arm
        vbox([5.0, 11.0, 0.0], [-3.5, -3.0, -3.0], [6.0, 16.0, 5.0], [60.0, 0.0], 0.0), // right leg
        vbox([-4.0, 11.0, 0.0], [-3.5, -3.0, -3.0], [6.0, 16.0, 5.0], [37.0, 0.0], 0.0), // left leg
    ]
}

fn iron_golem_poses(anim: &EntityAnim) -> [PartPose; 8] {
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    let head = head_pose(anim, vpivot([0.0, -7.0, -2.0]));
    // Legs swing opposite each other; arms sway slightly out of phase.
    let right_leg = leg_pose(vpivot([5.0, 11.0, 0.0]), swing(s, a, 0.0, 1.5));
    let left_leg = leg_pose(vpivot([-4.0, 11.0, 0.0]), swing(s, a, std::f32::consts::PI, 1.5));
    let arm = |sign: f32| leg_pose(vpivot([0.0, -7.0, 0.0]), swing(s, a, 0.0, 1.5) * sign - 0.2 * a);
    [
        head,
        head, // nose tracks the head
        PartPose::still(),
        PartPose::still(),
        arm(1.0),
        arm(-1.0),
        right_leg,
        left_leg,
    ]
}

/// Horse (`ModelHorse`, horse/horse_brown.png 128x128): a reasonable subset of
/// the vanilla model — body, a 30°-tilted neck/head group (with the two head
/// detail boxes, ears and mane) plus a three-segment hanging tail and four
/// three-box legs. Saddle/tack is omitted. Order: body, [neck, head, head-top,
/// mouth, right ear, left ear, mane], [tail base, mid, tip], then the 12 leg
/// boxes (back-left, back-right, front-left, front-right; each leg/shin/hoof).
fn horse_parts() -> [Part; 23] {
    [
        vbox([0.0, 11.0, 9.0], [-5.0, -8.0, -19.0], [10.0, 10.0, 24.0], [0.0, 34.0], 0.0), // body
        // Neck/head group (rp 0,4,-10), tilted 30° in horse_poses.
        vbox([0.0, 4.0, -10.0], [-2.05, -9.8, -2.0], [4.0, 14.0, 8.0], [0.0, 12.0], 0.0), // neck
        vbox([0.0, 4.0, -10.0], [-2.5, -10.0, -1.5], [5.0, 5.0, 7.0], [0.0, 0.0], 0.0), // head
        vbox([0.0, 3.95, -10.0], [-2.0, -10.0, -7.0], [4.0, 3.0, 6.0], [24.0, 18.0], 0.0), // upper snout
        vbox([0.0, 4.0, -10.0], [-2.0, -7.0, -6.5], [4.0, 2.0, 5.0], [24.0, 27.0], 0.0), // mouth
        vbox([0.0, 4.0, -10.0], [-2.45, -12.0, 4.0], [2.0, 3.0, 1.0], [0.0, 0.0], 0.0), // right ear
        vbox([0.0, 4.0, -10.0], [0.45, -12.0, 4.0], [2.0, 3.0, 1.0], [0.0, 0.0], 0.0), // left ear
        vbox([0.0, 4.0, -10.0], [-1.0, -11.5, 5.0], [2.0, 16.0, 4.0], [58.0, 0.0], 0.0), // mane
        // Tail (rp 0,3,14), hanging back in horse_poses.
        vbox([0.0, 3.0, 14.0], [-1.0, -1.0, 0.0], [2.0, 2.0, 3.0], [44.0, 0.0], 0.0), // tail base
        vbox([0.0, 3.0, 14.0], [-1.5, -2.0, 3.0], [3.0, 4.0, 7.0], [38.0, 7.0], 0.0), // tail mid
        vbox([0.0, 3.0, 14.0], [-1.5, -4.5, 9.0], [3.0, 4.0, 7.0], [24.0, 3.0], 0.0), // tail tip
        // Legs (static): each leg + shin + hoof.
        vbox([4.0, 9.0, 11.0], [-2.5, -2.0, -2.5], [4.0, 9.0, 5.0], [78.0, 29.0], 0.0), // BL leg
        vbox([4.0, 16.0, 11.0], [-2.0, 0.0, -1.5], [3.0, 5.0, 3.0], [78.0, 43.0], 0.0), // BL shin
        vbox([4.0, 16.0, 11.0], [-2.5, 5.1, -2.0], [4.0, 3.0, 4.0], [78.0, 51.0], 0.0), // BL hoof
        vbox([-4.0, 9.0, 11.0], [-1.5, -2.0, -2.5], [4.0, 9.0, 5.0], [96.0, 29.0], 0.0), // BR leg
        vbox([-4.0, 16.0, 11.0], [-1.0, 0.0, -1.5], [3.0, 5.0, 3.0], [96.0, 43.0], 0.0), // BR shin
        vbox([-4.0, 16.0, 11.0], [-1.5, 5.1, -2.0], [4.0, 3.0, 4.0], [96.0, 51.0], 0.0), // BR hoof
        vbox([4.0, 9.0, -8.0], [-1.9, -1.0, -2.1], [3.0, 8.0, 4.0], [44.0, 29.0], 0.0), // FL leg
        vbox([4.0, 16.0, -8.0], [-1.9, 0.0, -1.6], [3.0, 5.0, 3.0], [44.0, 41.0], 0.0), // FL shin
        vbox([4.0, 16.0, -8.0], [-2.4, 5.1, -2.1], [4.0, 3.0, 4.0], [44.0, 51.0], 0.0), // FL hoof
        vbox([-4.0, 9.0, -8.0], [-1.1, -1.0, -2.1], [3.0, 8.0, 4.0], [60.0, 29.0], 0.0), // FR leg
        vbox([-4.0, 16.0, -8.0], [-1.1, 0.0, -1.6], [3.0, 5.0, 3.0], [60.0, 41.0], 0.0), // FR shin
        vbox([-4.0, 16.0, -8.0], [-1.6, 5.1, -2.1], [4.0, 3.0, 4.0], [60.0, 51.0], 0.0), // FR hoof
    ]
}

fn horse_poses(anim: &EntityAnim) -> [PartPose; 23] {
    use std::f32::consts::{FRAC_PI_6, PI};
    let (s, a) = (anim.limb_swing, anim.limb_swing_amount);
    // The neck/head group sits at a fixed 30° tilt (vanilla setBoxRotation),
    // overlaid with head yaw/pitch.
    let neck = PartPose {
        pivot: vpivot([0.0, 4.0, -10.0]),
        angles: Vec3::new(FRAC_PI_6 + anim.head_pitch.to_radians(), anim.net_head_yaw.to_radians(), 0.0),
        group_pivot: Vec3::ZERO,
        group_angle_x: 0.0,
    };
    let tail = |angle: f32| PartPose {
        pivot: vpivot([0.0, 3.0, 14.0]),
        angles: Vec3::new(angle, 0.0, 0.0),
        group_pivot: Vec3::ZERO,
        group_angle_x: 0.0,
    };
    let leg = |x: f32, z: f32, phase: f32| leg_pose(vpivot([x, 9.0, z]), swing(s, a, phase, 0.8));
    [
        PartPose::still(), // body
        neck, neck, neck, neck, neck, neck, neck, // neck/head group + mane
        tail(-1.134464), tail(-1.134464), tail(-1.40215), // tail segments
        leg(4.0, 11.0, 0.0), leg(4.0, 11.0, 0.0), leg(4.0, 11.0, 0.0),      // back-left
        leg(-4.0, 11.0, PI), leg(-4.0, 11.0, PI), leg(-4.0, 11.0, PI),      // back-right
        leg(4.0, -8.0, PI), leg(4.0, -8.0, PI), leg(4.0, -8.0, PI),         // front-left
        leg(-4.0, -8.0, 0.0), leg(-4.0, -8.0, 0.0), leg(-4.0, -8.0, 0.0),   // front-right
    ]
}

/// Witch (`ModelWitch` extends `ModelVillager`, witch.png 64x128): the villager
/// body plus a pointed hat (brim + two cone tiers) on the head and a wart on
/// the nose. The villager parts come first (see [`villager_parts`]); then the
/// hat brim, two cone tiers and the nose wart.
fn witch_parts() -> [Part; 13] {
    let v = villager_parts();
    [
        v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8],
        // Hat brim: child of head (rp 0,0,0) at (-5,-10.03,-5), 10x2x10 @ (0,64).
        vbox([-5.0, -10.03125, -5.0], [0.0, 0.0, 0.0], [10.0, 2.0, 10.0], [0.0, 64.0], 0.0),
        // Cone tier 1 (7x4x7 @ (0,76)), centred over the brim.
        vbox([-3.25, -14.03125, -3.0], [0.0, 0.0, 0.0], [7.0, 4.0, 7.0], [0.0, 76.0], 0.0),
        // Cone tier 2 (4x4x4 @ (0,87)).
        vbox([-1.5, -18.03125, -1.0], [0.0, 0.0, 0.0], [4.0, 4.0, 4.0], [0.0, 87.0], 0.0),
        // Nose wart: child of the nose (cumulative rp 0,-4,0) @ (0,3,-6.75).
        vbox([0.0, -4.0, 0.0], [0.0, 3.0, -6.75], [1.0, 1.0, 1.0], [0.0, 0.0], -0.25),
    ]
}

fn witch_poses(anim: &EntityAnim) -> [PartPose; 13] {
    let v = villager_poses(anim);
    let head = head_pose(anim, vpivot([0.0, 0.0, 0.0]));
    [
        v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8],
        head, head, head, // hat tiers track the head
        head,             // wart tracks the head
    ]
}

/// Guardian (`ModelGuardian`, guardian.png 64x64): a boxy body with side
/// plates, top/bottom rims, a front eye and a three-segment tail. The spines
/// are omitted (animated, radially placed). Order: core, left plate, right
/// plate, top rim, bottom rim, eye, tail base, tail mid, tail tip.
fn guardian_parts() -> [Part; 9] {
    [
        vbox([0.0, 0.0, 0.0], [-6.0, 10.0, -8.0], [12.0, 12.0, 16.0], [0.0, 0.0], 0.0), // core
        vbox([0.0, 0.0, 0.0], [-8.0, 10.0, -6.0], [2.0, 12.0, 12.0], [0.0, 28.0], 0.0), // left plate
        vbox([0.0, 0.0, 0.0], [6.0, 10.0, -6.0], [2.0, 12.0, 12.0], [0.0, 28.0], 0.0), // right plate
        vbox([0.0, 0.0, 0.0], [-6.0, 8.0, -6.0], [12.0, 2.0, 12.0], [16.0, 40.0], 0.0), // top rim
        vbox([0.0, 0.0, 0.0], [-6.0, 22.0, -6.0], [12.0, 2.0, 12.0], [16.0, 40.0], 0.0), // bottom rim
        vbox([0.0, 0.0, -8.25], [-1.0, 15.0, 0.0], [2.0, 2.0, 1.0], [8.0, 0.0], 0.0), // eye
        vbox([0.0, 0.0, 0.0], [-2.0, 14.0, 7.0], [4.0, 4.0, 8.0], [40.0, 0.0], 0.0), // tail base
        vbox([0.0, 0.0, 0.0], [0.0, 14.0, 15.0], [3.0, 3.0, 7.0], [0.0, 54.0], 0.0), // tail mid
        vbox([0.0, 0.0, 0.0], [0.0, 14.0, 22.0], [2.0, 2.0, 6.0], [41.0, 32.0], 0.0), // tail tip
    ]
}

fn guardian_poses(anim: &EntityAnim) -> [PartPose; 9] {
    let _ = anim;
    [PartPose::still(); 9]
}

/// Wither (`ModelWither`, wither/wither.png 64x64): three heads on a ribbed
/// spine with a hanging tail. Order: centre head, left head, right head, ribs
/// (top bar), spine, tail.
fn wither_parts() -> [Part; 6] {
    [
        vbox([0.0, 0.0, 0.0], [-4.0, -4.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0], 0.0), // centre head
        vbox([-8.0, 4.0, 0.0], [-4.0, -4.0, -4.0], [6.0, 6.0, 6.0], [32.0, 0.0], 0.0), // left head
        vbox([10.0, 4.0, 0.0], [-4.0, -4.0, -4.0], [6.0, 6.0, 6.0], [32.0, 0.0], 0.0), // right head
        vbox([0.0, 0.0, 0.0], [-10.0, 3.9, -0.5], [20.0, 3.0, 3.0], [0.0, 16.0], 0.0), // shoulder bar
        vbox([-2.0, 6.9, -0.5], [0.0, 0.0, 0.0], [3.0, 10.0, 3.0], [0.0, 22.0], 0.0), // spine
        vbox([-2.0, 16.9, -0.5], [0.0, 0.0, 0.0], [3.0, 6.0, 3.0], [12.0, 22.0], 0.0), // tail
    ]
}

fn wither_poses(anim: &EntityAnim) -> [PartPose; 6] {
    let head = head_pose(anim, vpivot([0.0, 0.0, 0.0]));
    [
        head,
        PartPose::still(),
        PartPose::still(),
        PartPose::still(),
        PartPose::still(),
        PartPose::still(),
    ]
}

/// Rabbit (`ModelRabbit`, rabbit/brown.png 64x32): body, head with nose and two
/// ears, four limbs (two arms, two thighs, two feet) and a tail. Order: body,
/// head, nose, right ear, left ear, right arm, left arm, right thigh, left
/// thigh, right foot, left foot, tail.
fn rabbit_parts() -> [Part; 12] {
    [
        vbox([0.0, 19.0, 8.0], [-3.0, -2.0, -10.0], [6.0, 5.0, 10.0], [0.0, 0.0], 0.0), // body
        vbox([0.0, 16.0, -1.0], [-2.5, -4.0, -5.0], [5.0, 4.0, 5.0], [32.0, 0.0], 0.0), // head
        vbox([0.0, 16.0, -1.0], [-0.5, -2.5, -5.5], [1.0, 1.0, 1.0], [32.0, 9.0], 0.0), // nose
        vbox([0.0, 16.0, -1.0], [-2.5, -9.0, -1.0], [2.0, 5.0, 1.0], [52.0, 0.0], 0.0), // right ear
        vbox([0.0, 16.0, -1.0], [0.5, -9.0, -1.0], [2.0, 5.0, 1.0], [58.0, 0.0], 0.0), // left ear
        vbox([-3.0, 17.0, -1.0], [-1.0, 0.0, -1.0], [2.0, 7.0, 2.0], [0.0, 15.0], 0.0), // right arm
        vbox([3.0, 17.0, -1.0], [-1.0, 0.0, -1.0], [2.0, 7.0, 2.0], [8.0, 15.0], 0.0), // left arm
        vbox([-3.0, 17.5, 3.7], [-1.0, 0.0, 0.0], [2.0, 4.0, 5.0], [16.0, 15.0], 0.0), // right thigh
        vbox([3.0, 17.5, 3.7], [-1.0, 0.0, 0.0], [2.0, 4.0, 5.0], [30.0, 15.0], 0.0), // left thigh
        vbox([-3.0, 17.5, 3.7], [-1.0, 5.5, -3.7], [2.0, 1.0, 7.0], [8.0, 24.0], 0.0), // right foot
        vbox([3.0, 17.5, 3.7], [-1.0, 5.5, -3.7], [2.0, 1.0, 7.0], [26.0, 24.0], 0.0), // left foot
        vbox([0.0, 20.0, 7.0], [-1.5, -1.5, 0.0], [3.0, 3.0, 2.0], [52.0, 6.0], 0.0), // tail
    ]
}

fn rabbit_poses(anim: &EntityAnim) -> [PartPose; 12] {
    // Rest pose carries fixed tilts from the vanilla constructor: ears/head
    // upright, arms/thighs tucked. Head and ears track look direction.
    let head = head_pose(anim, vpivot([0.0, 16.0, -1.0]));
    let arm = leg_pose(vpivot([0.0, 17.0, -1.0]), -0.19198622); // -11° tuck
    let thigh = leg_pose(vpivot([0.0, 17.5, 3.7]), -0.34906584); // -20° tuck
    [
        PartPose::still(), // body
        head,
        head, // nose
        head, // right ear
        head, // left ear
        arm,
        arm,
        thigh,
        thigh,
        PartPose::still(), // right foot
        PartPose::still(), // left foot
        leg_pose(vpivot([0.0, 20.0, 7.0]), -0.3490659), // tail
    ]
}

/// Ghast (`ModelGhast`, ghast.png 64x32): a 16³ cube body with nine 2-wide
/// tentacles hanging beneath it (lengths from the vanilla seeded RNG). Static.
/// The vanilla body sits at engine y ≈ 8..24 with tentacles below. Order: body
/// then nine tentacles.
fn ghast_parts() -> [Part; 10] {
    // Tentacle lengths from `new Random(1660L)` (random.nextInt(7)+8, in order).
    let lengths = [13.0f32, 10.0, 9.0, 11.0, 8.0, 11.0, 12.0, 13.0, 12.0];
    // Body: addBox(-8,-8,-8, 16) at rotationPointY = 24-16 = 8 (vanilla i=-16).
    let mut parts = vec![vbox([0.0, 8.0, 0.0], [-8.0, -8.0, -8.0], [16.0, 16.0, 16.0], [0.0, 0.0], 0.0)];
    for (j, len) in lengths.iter().enumerate() {
        let fx = (((j % 3) as f32 - (j / 3 % 2) as f32 * 0.5 + 0.25) / 2.0 * 2.0 - 1.0) * 5.0;
        let fz = ((j / 3) as f32 / 2.0 * 2.0 - 1.0) * 5.0;
        // Tentacle: addBox(-1,0,-1, 2,len,2) hanging down from rotationPointY=15.
        parts.push(vbox([fx, 15.0, fz], [-1.0, 0.0, -1.0], [2.0, *len, 2.0], [0.0, 0.0], 0.0));
    }
    parts.try_into().unwrap()
}

/// Blaze (`ModelBlaze`, blaze.png 64x32): a head and twelve rotating rods in
/// three rings. The rods are placed at their rest-frame ring positions (vanilla
/// spins them via ageInTicks). Order: head then twelve rods.
fn blaze_parts() -> [Part; 13] {
    let mut parts = vec![
        // Head: addBox(-4,-4,-4, 8) centred at engine y ~ 16 (sits above the rods).
        vbox([0.0, 8.0, 0.0], [-4.0, -4.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0], 0.0),
    ];
    // Three rings of four rods (vanilla loops: radius 9/7/5, vanilla y -2/2/11,
    // where smaller y is higher). Rest frame: the animated angle starts near 0,
    // so the rods spread evenly around each ring.
    let rings = [(9.0f32, -2.0f32), (7.0, 2.0), (5.0, 11.0)];
    for (ring, &(radius, base_y)) in rings.iter().enumerate() {
        for k in 0..4 {
            let angle = std::f32::consts::TAU * ((ring * 4 + k) as f32) / 12.0;
            let (fx, fz) = (angle.cos() * radius, angle.sin() * radius);
            // Rod: addBox(0,0,0, 2,8,2) from rotationPointY (engine y ≈ 12-base_y).
            parts.push(vbox([fx, 12.0 - base_y, fz], [0.0, 0.0, 0.0], [2.0, 8.0, 2.0], [0.0, 16.0], 0.0));
        }
    }
    parts.try_into().unwrap()
}

fn blaze_poses(anim: &EntityAnim) -> [PartPose; 13] {
    let head = head_pose(anim, vpivot([0.0, 8.0, 0.0]));
    let mut poses = [PartPose::still(); 13];
    poses[0] = head;
    poses
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

// ─── Armor overlay ──────────────────────────────────────────────────────────

/// Armor material derived from item id, with the two atlas layer slots.
#[derive(Debug, Clone, Copy)]
struct ArmorMaterial {
    layer1: EntitySlot,
    layer2: EntitySlot,
}

const LEATHER: ArmorMaterial = ArmorMaterial { layer1: EntitySlot::ArmorLeather1, layer2: EntitySlot::ArmorLeather2 };
const CHAIN: ArmorMaterial = ArmorMaterial { layer1: EntitySlot::ArmorChain1, layer2: EntitySlot::ArmorChain2 };
const IRON: ArmorMaterial = ArmorMaterial { layer1: EntitySlot::ArmorIron1, layer2: EntitySlot::ArmorIron2 };
const GOLD: ArmorMaterial = ArmorMaterial { layer1: EntitySlot::ArmorGold1, layer2: EntitySlot::ArmorGold2 };
const DIAMOND: ArmorMaterial = ArmorMaterial { layer1: EntitySlot::ArmorDiamond1, layer2: EntitySlot::ArmorDiamond2 };

/// Map an armor item id to its material. Returns None for non-armor items.
fn armor_material(item_id: i16) -> Option<ArmorMaterial> {
    Some(match item_id {
        298..=301 => LEATHER,
        302..=305 => CHAIN,
        306..=309 => IRON,
        310..=313 => DIAMOND,
        314..=317 => GOLD,
        _ => return None,
    })
}

/// Helmet overlay: an inflated head box (layer 1 texture, head UV region).
/// Vanilla ModelBiped.bipedHead: rp(0,0,0), addBox(-4,-8,-4, 8,8,8, grow).
fn helmet_parts() -> [Part; 1] {
    [vbox([0.0, 0.0, 0.0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0], 1.0)]
}

/// Chestplate overlay: inflated body + two arm boxes (layer 1 texture).
/// rp/offset match the vanilla ModelBiped so the overlay tracks the skin.
fn chestplate_parts() -> [Part; 3] {
    let body = vbox([0.0, 0.0, 0.0], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 16.0], 1.0);
    // bipedRightArm: rp(-5,2,0), addBox(-3,-2,-2, 4,12,4, grow)
    let arm_r = vbox([-5.0, 2.0, 0.0], [-3.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 16.0], 1.0);
    // bipedLeftArm: rp(5,2,0), addBox(-1,-2,-2, 4,12,4, grow)
    let arm_l = vbox([5.0, 2.0, 0.0], [-1.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 16.0], 1.0);
    [body, arm_r, arm_l]
}

/// Leggings overlay: inflated body (waist) + two leg boxes (layer 2 texture).
fn leggings_parts() -> [Part; 3] {
    let body = vbox([0.0, 0.0, 0.0], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 16.0], 0.5);
    // bipedRightLeg: rp(-2,12,0), addBox(-2,0,-2, 4,12,4, grow)
    let leg_r = vbox([-2.0, 12.0, 0.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0], 0.5);
    let leg_l = vbox([2.0, 12.0, 0.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0], 0.5);
    [body, leg_r, leg_l]
}

/// Boots overlay: two inflated leg boxes (layer 1 texture, leg UV region).
fn boots_parts() -> [Part; 2] {
    let leg_r = vbox([-2.0, 12.0, 0.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0], 1.0);
    let leg_l = vbox([2.0, 12.0, 0.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0], 1.0);
    [leg_r, leg_l]
}

impl ModelMesh {
    /// Draw armor overlay boxes for the given equipment slots. Slots are indexed
    /// 0=held(ignored), 1=boots, 2=leggings, 3=chestplate, 4=helmet.
    /// `item_ids` are the item ids in each slot (None = empty).
    pub fn push_armor(
        &mut self,
        equipment: &[Option<i16>; 5],
        anim: &EntityAnim,
        feet: Vec3,
        yaw_degrees: f32,
    ) {
        // Worn armor tips with the body during the death animation.
        self.death_roll = anim.death_roll;
        let poses = humanoid_poses(anim);

        // Helmet (slot 4)
        if let Some(mat) = equipment[4].and_then(armor_material) {
            let parts = helmet_parts();
            let helmet_pose = [poses[5]]; // head pose
            self.push_parts(&parts, &helmet_pose, mat.layer1 as u32, feet, yaw_degrees);
        }

        // Chestplate (slot 3): body + right arm + left arm
        if let Some(mat) = equipment[3].and_then(armor_material) {
            let parts = chestplate_parts();
            let chest_poses = [poses[2], poses[3], poses[4]]; // body, right arm, left arm
            self.push_parts(&parts, &chest_poses, mat.layer1 as u32, feet, yaw_degrees);
        }

        // Leggings (slot 2): body + right leg + left leg
        if let Some(mat) = equipment[2].and_then(armor_material) {
            let parts = leggings_parts();
            let leg_poses = [poses[2], poses[0], poses[1]]; // body, right leg, left leg
            self.push_parts(&parts, &leg_poses, mat.layer2 as u32, feet, yaw_degrees);
        }

        // Boots (slot 1): right leg + left leg
        if let Some(mat) = equipment[1].and_then(armor_material) {
            let parts = boots_parts();
            let boot_poses = [poses[0], poses[1]]; // right leg, left leg
            self.push_parts(&parts, &boot_poses, mat.layer1 as u32, feet, yaw_degrees);
        }
        self.death_roll = 0.0;
    }

    /// Append the enchantment-glint geometry for worn armor into `self` (a
    /// dedicated glint mesh): the same armor boxes as [`push_armor`], but only
    /// for the slots flagged in `enchanted` (helmet/chest/legs/boots map to
    /// equipment slots 4/3/2/1). The renderer re-draws this additively with the
    /// scrolling glint texture, masked to the armor silhouette by the entity
    /// atlas — mirroring the held/world item glint.
    pub fn push_armor_glint(
        &mut self,
        equipment: &[Option<i16>; 5],
        enchanted: &[bool; 5],
        anim: &EntityAnim,
        feet: Vec3,
        yaw_degrees: f32,
    ) {
        self.death_roll = anim.death_roll;
        let poses = humanoid_poses(anim);

        if enchanted[4] {
            if let Some(mat) = equipment[4].and_then(armor_material) {
                let parts = helmet_parts();
                let helmet_pose = [poses[5]]; // head pose
                self.push_parts(&parts, &helmet_pose, mat.layer1 as u32, feet, yaw_degrees);
            }
        }
        if enchanted[3] {
            if let Some(mat) = equipment[3].and_then(armor_material) {
                let parts = chestplate_parts();
                let chest_poses = [poses[2], poses[3], poses[4]]; // body, right arm, left arm
                self.push_parts(&parts, &chest_poses, mat.layer1 as u32, feet, yaw_degrees);
            }
        }
        if enchanted[2] {
            if let Some(mat) = equipment[2].and_then(armor_material) {
                let parts = leggings_parts();
                let leg_poses = [poses[2], poses[0], poses[1]]; // body, right leg, left leg
                self.push_parts(&parts, &leg_poses, mat.layer2 as u32, feet, yaw_degrees);
            }
        }
        if enchanted[1] {
            if let Some(mat) = equipment[1].and_then(armor_material) {
                let parts = boots_parts();
                let boot_poses = [poses[0], poses[1]]; // right leg, left leg
                self.push_parts(&parts, &boot_poses, mat.layer1 as u32, feet, yaw_degrees);
            }
        }
        self.death_roll = 0.0;
    }
}

// ─── Held item arm attachment ───────────────────────────────────────────────

/// Rigid attachment for an item held in the right hand. It captures the exact
/// articulation [`ModelMesh::push_entity`] applies to the right-arm box, so item
/// geometry expressed in the arm's rest model-pixel frame (shoulder pivot at
/// `SHOULDER_PX`, +y up, feet at y=0 — recraft's convention) rides the arm into
/// world space identically to the rendered arm, picking up the held-item lower /
/// blocking pose and the sneak tilt for free (they live in the arm `PartPose`).
#[derive(Debug, Clone, Copy)]
pub struct ArmAttach {
    pose: PartPose,
    feet: Vec3,
    sin: f32,
    cos: f32,
}

impl ArmAttach {
    /// Right-arm shoulder pivot in model pixels (vanilla `bipedRightArm`
    /// rotationPoint, recraft's feet-up convention). The vanilla
    /// `LayerHeldItem.postRenderArm` anchor.
    pub const SHOULDER_PX: Vec3 = Vec3::new(-6.0, 24.0, 0.0);

    /// Bottom-centre of the right-arm box in model pixels — the hand, for
    /// sampling the lightmap where the held item sits.
    pub const HAND_PX: Vec3 = Vec3::new(-6.0, 12.0, 0.0);

    /// Map a point in the arm's rest model-pixel frame to world space.
    pub fn to_world(&self, arm_px: Vec3) -> Vec3 {
        rotate_y(apply_part_rotation(arm_px, &self.pose) * MODEL_SCALE, self.sin, self.cos) + self.feet
    }
}

/// Build the right-arm attachment for an entity standing at `feet` with body
/// `yaw_degrees`, posed by `anim` (which carries the held-item / blocking arm
/// state via [`EntityAnim::held_item_right`]).
pub fn arm_attach(feet: Vec3, yaw_degrees: f32, anim: &EntityAnim) -> ArmAttach {
    let pose = humanoid_poses(anim)[3]; // right arm
    let yaw = yaw_degrees.to_radians();
    let (sin, cos) = (yaw.sin(), yaw.cos());
    ArmAttach { pose, feet, sin, cos }
}

// ─── Chest block-entity ───────────────────────────────────────────────────────

/// Which chest texture a chest block-entity uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChestKind {
    Normal,
    Trapped,
    Ender,
}

impl ChestKind {
    fn slot(self) -> EntitySlot {
        match self {
            ChestKind::Normal => EntitySlot::ChestNormal,
            ChestKind::Trapped => EntitySlot::ChestTrapped,
            ChestKind::Ender => EntitySlot::ChestEnder,
        }
    }

    /// Atlas slot for the large (double) chest texture. Ender chests are never
    /// double, so they fall back to the single ender slot.
    fn double_slot(self) -> EntitySlot {
        match self {
            ChestKind::Normal => EntitySlot::ChestNormalDouble,
            ChestKind::Trapped => EntitySlot::ChestTrappedDouble,
            ChestKind::Ender => EntitySlot::ChestEnder,
        }
    }
}

/// Vanilla `ModelChest` box, in model pixels: the rotation point, the `addBox`
/// offset, the box size and the texture-offset, plus whether the lid rotation
/// applies to it. The chest model space is 16 px per block (like every entity
/// model) and the boxes are placed by the chest renderer's transform.
struct ChestBox {
    rp: [f32; 3],
    off: [f32; 3],
    size: [f32; 3],
    tex: [f32; 2],
    lid: bool,
}

/// The three `ModelChest` parts: the lid (rotates), the latch knob (rotates with
/// the lid) and the static base. Ported 1:1 from vanilla `ModelChest`.
const CHEST_BOXES: [ChestBox; 3] = [
    // chestLid: tex(0,0), addBox(0,-5,-14, 14,5,14), rp(1,7,15)
    ChestBox { rp: [1.0, 7.0, 15.0], off: [0.0, -5.0, -14.0], size: [14.0, 5.0, 14.0], tex: [0.0, 0.0], lid: true },
    // chestKnob: tex(0,0), addBox(-1,-2,-15, 2,4,1), rp(8,7,15)
    ChestBox { rp: [8.0, 7.0, 15.0], off: [-1.0, -2.0, -15.0], size: [2.0, 4.0, 1.0], tex: [0.0, 0.0], lid: true },
    // chestBelow: tex(0,19), addBox(0,0,0, 14,10,14), rp(1,6,1)
    ChestBox { rp: [1.0, 6.0, 1.0], off: [0.0, 0.0, 0.0], size: [14.0, 10.0, 14.0], tex: [0.0, 19.0], lid: false },
];

/// The three `ModelLargeChest` parts, ported 1:1 from vanilla. Same structure
/// as [`CHEST_BOXES`] but 30 px wide (spanning two block cells along the model's
/// X axis) and sampling the 128×64 `*_double` texture; the knob's rp.x is 16.
const LARGE_CHEST_BOXES: [ChestBox; 3] = [
    // chestLid: tex(0,0), addBox(0,-5,-14, 30,5,14), rp(1,7,15)
    ChestBox { rp: [1.0, 7.0, 15.0], off: [0.0, -5.0, -14.0], size: [30.0, 5.0, 14.0], tex: [0.0, 0.0], lid: true },
    // chestKnob: tex(0,0), addBox(-1,-2,-15, 2,4,1), rp(16,7,15)
    ChestBox { rp: [16.0, 7.0, 15.0], off: [-1.0, -2.0, -15.0], size: [2.0, 4.0, 1.0], tex: [0.0, 0.0], lid: true },
    // chestBelow: tex(0,19), addBox(0,0,0, 30,10,14), rp(1,6,1)
    ChestBox { rp: [1.0, 6.0, 1.0], off: [0.0, 0.0, 0.0], size: [30.0, 10.0, 14.0], tex: [0.0, 19.0], lid: false },
];

/// Lid model pixel rotation point (`chestLid.rotationPoint`), scaled to blocks.
const CHEST_LID_PIVOT_PX: [f32; 3] = [1.0, 7.0, 15.0];

/// Bake the vanilla chest renderer's `scale(1,-1,-1)` (about the cell centre)
/// into a model-space point so the geometry lives in engine space (+y up, +z
/// front) like every mob box. Vanilla flips Y/Z then re-centres into the cell;
/// the net effect on a block-space coordinate is `y → 1-y`, `z → 1-z`.
fn chest_flip(p: Vec3) -> Vec3 {
    Vec3::new(p.x, 1.0 - p.y, 1.0 - p.z)
}

/// Flip a raw model-space box `(lo, hi)` into engine space. Because the Y and Z
/// axes negate, their min/max swap; X is untouched. The result is a genuine
/// +y-top / +z-front box, so `box_region`/`plane_frac` give the right-side-up
/// vanilla texture rects with no per-vertex flip.
fn chest_flip_bounds(lo: Vec3, hi: Vec3) -> (Vec3, Vec3) {
    (
        Vec3::new(lo.x, 1.0 - hi.y, 1.0 - hi.z),
        Vec3::new(hi.x, 1.0 - lo.y, 1.0 - lo.z),
    )
}

/// Yaw (degrees) a chest's block metadata 2..5 rotates the model by, matching the
/// `j` in `TileEntityChestRenderer` (2→180, 3→0, 4→90, 5→-90; other meta → 0).
fn chest_yaw_degrees(meta: u8) -> f32 {
    match meta {
        2 => 180.0,
        4 => 90.0,
        5 => -90.0,
        _ => 0.0, // 3 (south) and any unexpected meta
    }
}

impl ModelMesh {
    /// Append a chest block-entity at world cell `(cell_x, cell_y, cell_z)`
    /// (the integer block coordinates), oriented by its block `meta` and with
    /// the lid opened by `lid_angle` (0 = closed .. 1 = fully open). Ports
    /// vanilla `ModelChest` + `TileEntityChestRenderer`'s placement transform:
    /// the model is built in 1/16-block model space, the lid (and its knob)
    /// rotate about the rear-top hinge by `-(eased * PI/2)`, then the chest GL
    /// chain (`scale(1,-1,-1)`, centre, yaw, recentre) places it in the cell.
    pub fn push_chest(&mut self, cell: [i32; 3], meta: u8, lid_angle: f32, kind: ChestKind) {
        // Vanilla lid easing: f = 1 - (1 - lidAngle)^3, then rotateAngleX = -(f*PI/2).
        let f = 1.0 - (1.0 - lid_angle.clamp(0.0, 1.0)).powi(3);
        let lid_rot = -(f * std::f32::consts::FRAC_PI_2);
        let (slr, clr) = lid_rot.sin_cos();
        // Hinge pivot in engine space (the vanilla→engine Y/Z flip baked in).
        let pivot = chest_flip(Vec3::from(CHEST_LID_PIVOT_PX) * 0.0625);

        // The vanilla scale(1,-1,-1) is folded into the box coordinates (see
        // `chest_flip`), so the yaw conjugates to `rotate_y(-yaw)`.
        let yaw = -chest_yaw_degrees(meta).to_radians();
        let (sy, cy) = yaw.sin_cos();
        let base = Vec3::new(cell[0] as f32, cell[1] as f32, cell[2] as f32);

        let (ox, oy) = entity_slot_origin(kind.slot());
        let origin = [ox as f32, oy as f32];

        for b in &CHEST_BOXES {
            // Box bounds in engine space: build the raw model box, then flip Y/Z
            // into engine space (min/max swap on the negated axes) so the visible
            // faces keep their genuine +y-top / +z-front texture rects — the same
            // convention every mob box uses.
            let rp = Vec3::from(b.rp);
            let raw_lo = (rp + Vec3::from(b.off)) * 0.0625;
            let raw_hi = (rp + Vec3::from(b.off) + Vec3::from(b.size)) * 0.0625;
            let (lo, hi) = chest_flip_bounds(raw_lo, raw_hi);
            let region = box_region(b.tex[0], b.tex[1], b.size[0], b.size[1], b.size[2]);
            let lid = b.lid;
            self.push_textured_box(lo, hi, &region, origin, 1.0, &|m| {
                // Lid parts hinge about the rear-top pivot (X rotation, unchanged
                // by the flip since both Y and Z negate). Then recentre, yaw, and
                // place at the cell — no scale flip; it's baked into the bounds.
                let m = if lid {
                    rotate_x(m - pivot, slr, clr) + pivot
                } else {
                    m
                };
                let a = m - Vec3::splat(0.5);
                base + rotate_y(a, sy, cy) + Vec3::splat(0.5)
            });
        }
    }

    /// Append the large (double) chest block-entity, ported from vanilla
    /// `ModelLargeChest` + `TileEntityChestRenderer`. The model is anchored on
    /// the canonical half — vanilla renders the double chest from the half with
    /// no chest at -X/-Z (`adjacentChestXNeg`/`ZNeg == null`) — given by `cell`
    /// and `meta`; its 30-px-wide boxes extend into the +X/+Z partner. The
    /// renderer's pre-yaw translate (meta 2 → +X, meta 5 → −Z) keeps the seam
    /// aligned so the model spans exactly the two block cells. Lid eased like a
    /// single chest; samples the 128×64 `*_double` slot.
    pub fn push_large_chest(&mut self, cell: [i32; 3], meta: u8, lid_angle: f32, kind: ChestKind) {
        let f = 1.0 - (1.0 - lid_angle.clamp(0.0, 1.0)).powi(3);
        let lid_rot = -(f * std::f32::consts::FRAC_PI_2);
        let (slr, clr) = lid_rot.sin_cos();
        let pivot = chest_flip(Vec3::from(CHEST_LID_PIVOT_PX) * 0.0625);

        let yaw = -chest_yaw_degrees(meta).to_radians();
        let (sy, cy) = yaw.sin_cos();
        let base = Vec3::new(cell[0] as f32, cell[1] as f32, cell[2] as f32);

        // The 30-px model extends along +X in model space. Two of the four
        // facings would land it on the −axis; a post-yaw shift moves it back so
        // the model always spans the canonical cell plus its +X/+Z partner.
        // (Vanilla keys this off meta 2/5 in GL's rotation handedness; the
        // equivalent here is meta 2 → +X, meta 4 → −Z.) The shift acts in world
        // axes after the yaw, so the Z/Y flip baked into the bounds negates its
        // Z component (meta 4: −Z → +Z).
        let pre = match meta {
            2 => Vec3::new(1.0, 0.0, 0.0),
            4 => Vec3::new(0.0, 0.0, 1.0),
            _ => Vec3::ZERO,
        };

        let (ox, oy) = entity_slot_origin(kind.double_slot());
        let origin = [ox as f32, oy as f32];

        for b in &LARGE_CHEST_BOXES {
            let rp = Vec3::from(b.rp);
            let raw_lo = (rp + Vec3::from(b.off)) * 0.0625;
            let raw_hi = (rp + Vec3::from(b.off) + Vec3::from(b.size)) * 0.0625;
            let (lo, hi) = chest_flip_bounds(raw_lo, raw_hi);
            let region = box_region(b.tex[0], b.tex[1], b.size[0], b.size[1], b.size[2]);
            let lid = b.lid;
            self.push_textured_box(lo, hi, &region, origin, 1.0, &|m| {
                let m = if lid {
                    rotate_x(m - pivot, slr, clr) + pivot
                } else {
                    m
                };
                let a = m - Vec3::splat(0.5);
                base + rotate_y(a, sy, cy) + pre + Vec3::splat(0.5)
            });
        }
    }
}

// ─── Sign block-entity ─────────────────────────────────────────────────────────

/// Which sign block-entity is being drawn (vanilla standing_sign / wall_sign).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignKind {
    /// Standing sign (block 63): on a post, yaw from meta 0..15 → meta*22.5°.
    Standing,
    /// Wall sign (block 68): mounted flat on a wall face, no post, meta 2..5.
    Wall,
}

/// The `TileEntitySignRenderer` render scale `f = 2/3`.
const SIGN_RENDER_SCALE: f32 = 0.6666667;

/// Vanilla `ModelSign` boxes in model pixels (rotation point at origin): the
/// 24×12×2 board at tex(0,0) and the 2×14×2 post at tex(0,14).
const SIGN_BOARD: ([f32; 3], [f32; 3], [f32; 2]) = ([-12.0, -14.0, -1.0], [24.0, 12.0, 2.0], [0.0, 0.0]);
const SIGN_POST: ([f32; 3], [f32; 3], [f32; 2]) = ([-1.0, -2.0, -1.0], [2.0, 14.0, 2.0], [0.0, 14.0]);

/// Yaw (degrees) a standing sign rotates by for its 0..15 metadata: vanilla
/// `meta * 360 / 16` rotated by `-f1`, so the model turns clockwise with meta.
fn standing_sign_yaw(meta: u8) -> f32 {
    -(meta as f32 % 16.0) * 360.0 / 16.0
}

/// Yaw (degrees) a wall sign rotates by for its 2..5 facing metadata (rotated
/// by `-f2`); mirrors the chest facing (2→south through −180, 4/5→±90).
fn wall_sign_yaw(meta: u8) -> f32 {
    match meta {
        2 => -180.0,
        4 => -90.0,
        5 => 90.0,
        _ => 0.0, // 3 and any unexpected meta
    }
}

impl ModelMesh {
    /// Append a sign block-entity (vanilla `ModelSign` + `TileEntitySignRenderer`)
    /// at world cell `cell`, oriented by its block `meta`. Standing signs sit on a
    /// post and turn by `meta*22.5°`; wall signs drop the post and mount flat on
    /// the wall face (meta 2..5). Built in 1/16-block model space, scaled by the
    /// `f = 2/3` render scale (with `scale(1,-1,-1)`), yawed and placed at the cell
    /// centre `+0.75*f` up. Samples the `entity/sign.png` slot.
    pub fn push_sign(&mut self, cell: [i32; 3], meta: u8, kind: SignKind) {
        let (yaw_deg, wall_off) = match kind {
            SignKind::Standing => (standing_sign_yaw(meta), Vec3::ZERO),
            // Wall signs translate (0,-0.3125,-0.4375) in the yawed frame.
            SignKind::Wall => (wall_sign_yaw(meta), Vec3::new(0.0, -0.3125, -0.4375)),
        };
        let yaw = yaw_deg.to_radians();
        let (sy, cy) = yaw.sin_cos();
        let f = SIGN_RENDER_SCALE;
        let base = Vec3::new(cell[0] as f32, cell[1] as f32, cell[2] as f32);
        let anchor = base + Vec3::new(0.5, 0.75 * f, 0.5);

        let (ox, oy) = entity_slot_origin(EntitySlot::Sign);
        let origin = [ox as f32, oy as f32];

        // Board always; post only for standing signs.
        let boxes: &[([f32; 3], [f32; 3], [f32; 2])] = match kind {
            SignKind::Standing => &[SIGN_BOARD, SIGN_POST],
            SignKind::Wall => &[SIGN_BOARD],
        };
        for (off, size, tex) in boxes {
            let raw_lo = Vec3::from(*off) * 0.0625;
            let raw_hi = (Vec3::from(*off) + Vec3::from(*size)) * 0.0625;
            // Bake the renderer's scale(1,-1,-1) (the vanilla model-space flip)
            // into the box bounds: Y and Z negate, so their min/max swap. The box
            // then lives in engine space (+y up, +z front), letting box_region/
            // plane_frac paint the genuine top/front rects upright — the same
            // convention the chest uses. (Building in raw vanilla coords and
            // negating only in the transform left the board upside-down.)
            let lo = Vec3::new(raw_lo.x, -raw_hi.y, -raw_hi.z);
            let hi = Vec3::new(raw_hi.x, -raw_lo.y, -raw_lo.z);
            let region = box_region(tex[0], tex[1], size[0], size[1], size[2]);
            self.push_textured_box(lo, hi, &region, origin, 1.0, &|m| {
                // The Y/Z flip is baked into the bounds, so the transform only
                // scales by f (the renderSign 0.0625 is already folded into the
                // bounds), applies the wall offset, yaws and places at the anchor.
                let s = m * f + wall_off;
                anchor + rotate_y(s, sy, cy)
            });
        }
    }

    /// World-space placement of a sign's text plane: `(center, right, up,
    /// half_width, half_height)` for the board's readable front face, derived
    /// from the same transform [`push_sign`] uses. `right`/`up` are unit vectors
    /// spanning the board; the half-extents are the board's half-size in blocks.
    /// The text renderer lays the four lines out within this rect.
    pub fn sign_text_basis(
        cell: [i32; 3],
        meta: u8,
        kind: SignKind,
    ) -> (Vec3, Vec3, Vec3, f32, f32) {
        let (yaw_deg, wall_off) = match kind {
            SignKind::Standing => (standing_sign_yaw(meta), Vec3::ZERO),
            SignKind::Wall => (wall_sign_yaw(meta), Vec3::new(0.0, -0.3125, -0.4375)),
        };
        let yaw = yaw_deg.to_radians();
        let (sy, cy) = yaw.sin_cos();
        let f = SIGN_RENDER_SCALE;
        let base = Vec3::new(cell[0] as f32, cell[1] as f32, cell[2] as f32);
        let anchor = base + Vec3::new(0.5, 0.75 * f, 0.5);
        // Same model transform as push_sign, applied to a model-px point on the
        // board front face (z = -1.05, just proud of the -1..+1 board). The
        // model-px coords convert to blocks with the renderSign 0.0625 (which
        // push_sign folds into its box bounds) before the render scale f and the
        // model-space y/z flip — without it the basis was 16× too big and high.
        let place = |m: Vec3| {
            let s = Vec3::new(m.x, -m.y, -m.z) * (0.0625 * f) + wall_off;
            anchor + rotate_y(s, sy, cy)
        };
        let center = place(Vec3::new(0.0, -8.0, -1.05));
        let right_edge = place(Vec3::new(12.0, -8.0, -1.05));
        let top_edge = place(Vec3::new(0.0, -14.0, -1.05));
        let right_vec = right_edge - center;
        let up_vec = top_edge - center;
        (
            center,
            right_vec.normalize_or_zero(),
            up_vec.normalize_or_zero(),
            right_vec.length(),
            up_vec.length(),
        )
    }

    /// Append the floating enchanting-table book (vanilla `ModelBook` via
    /// `TileEntityEnchantmentTableRenderer`) above the table at world cell `cell`.
    /// `time` is a free-running tick counter (`tickCount + partialTicks`): it
    /// drives a gentle vertical hover and a slow yaw, with the book held slightly
    /// open. Page-open / page-flip state is simplified to a fixed small spread.
    /// Samples the `entity/enchanting_table_book.png` slot.
    pub fn push_book(&mut self, cell: [i32; 3], time: f32) {
        // Vanilla: translate(x+0.5, y+0.75, z+0.5), then hover sin(t*0.1)*0.01,
        // yaw by -bookRotation, tilt 80° about Z, render at scale 0.0625.
        let hover = 0.1 + (time * 0.1).sin() * 0.01;
        let base = Vec3::new(cell[0] as f32, cell[1] as f32, cell[2] as f32);
        let anchor = base + Vec3::new(0.5, 0.75 + hover, 0.5);
        let yaw = (time * 0.02).sin() * 0.5; // gentle idle turn (radians)
        let (sy, cy) = yaw.sin_cos();
        let tilt = 80.0_f32.to_radians();
        let (st, ct) = tilt.sin_cos();
        // Held a touch open and fluttering, so the page seam reads as a book.
        let spread = 1.0 + (time * 0.13).sin() * 0.1;

        let (ox, oy) = entity_slot_origin(EntitySlot::EnchantBook);
        let origin = [ox as f32, oy as f32];

        for b in book_boxes(spread) {
            let raw_lo = (b.rp + b.off) * 0.0625;
            let raw_hi = (b.rp + b.off + b.size) * 0.0625;
            // Bake the model-space scale(1,-1,-1) into the bounds (Y/Z min/max
            // swap) so the box is engine space (+y up, +z front) and box_region/
            // plane_frac paint the cover/page/spine rects upright, like the chest.
            let lo = Vec3::new(raw_lo.x, -raw_hi.y, -raw_hi.z);
            let hi = Vec3::new(raw_hi.x, -raw_lo.y, -raw_lo.z);
            let region = box_region(b.tex[0], b.tex[1], b.size.x, b.size.y, b.size.z);
            // The per-part Y rotation was applied in raw vanilla space *before*
            // the flip. Conjugating it by the flip (a 180° X rotation) negates
            // the angle and flips the pivot, leaving the posed geometry identical.
            let (sby, cby) = (-b.yaw).sin_cos();
            let raw_pivot = b.rp * 0.0625;
            let pivot = Vec3::new(raw_pivot.x, -raw_pivot.y, -raw_pivot.z);
            self.push_textured_box(lo, hi, &region, origin, 1.0, &|m| {
                // Per-part yaw about its (flipped) rotation point — the flip is
                // already baked into the bounds — then the whole-book GL chain:
                // 80° Z tilt, idle yaw about Y, and place at the hovering anchor.
                let s = rotate_y(m - pivot, sby, cby) + pivot;
                let s = rotate_z(s, st, ct);
                anchor + rotate_y(s, sy, cy)
            });
        }
    }

    /// Append the end-portal surface (block 119): a flat near-black quad at the
    /// top of the block cell. Vanilla draws an animated star-field shader; this
    /// MVP is a solid dark quad (the shader is deferred). Samples the white texel
    /// tinted dark, so it needs no dedicated texture slot.
    pub fn push_end_portal(&mut self, cell: [i32; 3]) {
        // Vanilla `TileEntityEndPortalRenderer` draws the surface at y = 0.75.
        let y = cell[1] as f32 + 0.75;
        let x0 = cell[0] as f32;
        let z0 = cell[2] as f32;
        let x1 = x0 + 1.0;
        let z1 = z0 + 1.0;
        let color = [0.02, 0.02, 0.07, 1.0];
        let base = self.vertices.len() as u32;
        let corners = [
            [x0, y, z0],
            [x0, y, z1],
            [x1, y, z1],
            [x1, y, z0],
        ];
        for position in corners {
            self.vertices.push(ModelVertex {
                position,
                color,
                uv: ENTITY_WHITE_UV,
            });
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// One `ModelBook` part in model pixels: rotation point, `addBox` offset, size,
/// texture-offset and the part's Y rotation (radians). Built for a given page
/// `spread` (0 = closed, 1 = fully open) following vanilla `ModelBook`.
struct BookBox {
    rp: Vec3,
    off: Vec3,
    size: Vec3,
    tex: [f32; 2],
    yaw: f32,
}

/// The seven `ModelBook` boxes posed for a static `spread` (no animation inputs):
/// covers, spine and the two page leaves opened symmetrically by `spread`.
fn book_boxes(spread: f32) -> [BookBox; 5] {
    // Vanilla setRotationAngles with limbSwing≈0: f = 1.25 * bookSpread.
    let f = 1.25 * spread;
    let px = f.sin();
    [
        // coverRight: rp(0,0,-1), addBox(-6,-5,0, 6,10,0) tex(0,0), yaw = PI + f.
        BookBox { rp: Vec3::new(0.0, 0.0, -1.0), off: Vec3::new(-6.0, -5.0, 0.0), size: Vec3::new(6.0, 10.0, 0.0), tex: [0.0, 0.0], yaw: std::f32::consts::PI + f },
        // coverLeft: rp(0,0,1), addBox(0,-5,0, 6,10,0) tex(16,0), yaw = -f.
        BookBox { rp: Vec3::new(0.0, 0.0, 1.0), off: Vec3::new(0.0, -5.0, 0.0), size: Vec3::new(6.0, 10.0, 0.0), tex: [16.0, 0.0], yaw: -f },
        // bookSpine: addBox(-1,-5,0, 2,10,0) tex(12,0), yaw = PI/2 (fixed).
        BookBox { rp: Vec3::ZERO, off: Vec3::new(-1.0, -5.0, 0.0), size: Vec3::new(2.0, 10.0, 0.0), tex: [12.0, 0.0], yaw: std::f32::consts::FRAC_PI_2 },
        // pagesRight: addBox(0,-4,-0.99, 5,8,1) tex(0,10), yaw = f, rpX = sin(f).
        BookBox { rp: Vec3::new(px, 0.0, 0.0), off: Vec3::new(0.0, -4.0, -0.99), size: Vec3::new(5.0, 8.0, 1.0), tex: [0.0, 10.0], yaw: f },
        // pagesLeft: addBox(0,-4,-0.01, 5,8,1) tex(12,10), yaw = -f, rpX = sin(f).
        BookBox { rp: Vec3::new(px, 0.0, 0.0), off: Vec3::new(0.0, -4.0, -0.01), size: Vec3::new(5.0, 8.0, 1.0), tex: [12.0, 10.0], yaw: -f },
    ]
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

    /// Regression for the reported "zombie missing a left arm and left leg": 1.8
    /// mob bipeds mirror the right limbs and leave the 64x64 separate-left-limb
    /// regions (texture rows 48..64) empty/transparent. So no biped-mob vertex
    /// may sample below row 48 of its slot — otherwise the left arm/leg vanish.
    #[test]
    fn biped_mobs_do_not_sample_the_empty_left_limb_region() {
        for (id, slot) in [(54u8, EntitySlot::Zombie), (57, EntitySlot::ZombiePigman), (51, EntitySlot::Skeleton)] {
            let (_, oy) = entity_slot_origin(slot);
            // The empty left-limb block starts at row 48 within the slot.
            let limit = (oy as f32 + 48.0) / ENTITY_ATLAS_HEIGHT as f32;
            let mesh = build_mob(id);
            for v in &mesh.vertices {
                assert!(
                    v.uv[1] <= limit + 1e-4,
                    "mob {id} samples the empty left-limb region (v={} > {limit}); \
                     it must mirror the right limbs (separate=false)",
                    v.uv[1]
                );
            }
        }
    }

    #[test]
    fn player_humanoid_emits_seven_boxes_with_in_range_uvs() {
        let mut mesh = ModelMesh::new();
        mesh.push_entity(
            EntityKind::RemotePlayer,
            Vec3::new(0.0, 64.0, 0.0),
            45.0,
            &EntityAnim::default(),
            None,
        );
        // 7 parts (6 base + hat overlay) × 6 faces × 4 verts; ×6 indices.
        assert_eq!(mesh.vertices.len(), 168);
        assert_eq!(mesh.indices.len(), 252);
        assert_well_formed(&mesh);
        let (v0, v1) = slot_v_range(EntitySlot::Player);
        assert!(mesh.vertices.iter().all(|v| (v0..=v1).contains(&v.uv[1])));
    }

    /// The player now layers a hat overlay (vanilla bipedHeadwear) over the
    /// base head: the seventh box sampling the (32,0) head-overlay region.
    #[test]
    fn player_emits_a_hat_overlay_box() {
        let mut mesh = ModelMesh::new();
        mesh.push_entity(EntityKind::RemotePlayer, Vec3::ZERO, 0.0, &EntityAnim::default(), None);
        // Base head is box 5 (verts 120..144); the hat is the seventh box.
        assert_eq!(mesh.vertices.len(), 168, "player must emit a 7th hat box");
        // The hat samples a different U band than the base head (head front is
        // at U 8..16 px; the hat front is at U 40..48 px), so the two boxes'
        // U sets differ.
        let head_us: Vec<f32> = mesh.vertices[120..144].iter().map(|v| v.uv[0]).collect();
        let hat_us: Vec<f32> = mesh.vertices[144..168].iter().map(|v| v.uv[0]).collect();
        assert_ne!(head_us, hat_us, "hat overlay must sample its own UV region");
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
        let unknown = build_mob(63); // no 1.8 model: hidden (no geometry)
        assert_ne!(zombie.vertices.len(), chicken.vertices.len());
        assert_ne!(chicken.vertices.len(), unknown.vertices.len());
        assert!(unknown.vertices.is_empty(), "unmodelled mobs must be hidden");

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
    fn first_person_arm_samples_the_same_front_rect_as_the_third_person_arm() {
        // The empty-hand first-person arm must read its skin exactly like the
        // third-person right arm — earlier it was built in raw vanilla coords
        // (y-down, front −z) while plane_frac/box_region assume engine coords
        // (y-up, front +z), which sampled the BACK sub-rect upside-down on the
        // physical front face (a mirrored arm). Both boxes use push_textured_box
        // with FACES order, so the front face (+z, FACES index 3) is verts
        // 12..16 of each box. The first-person arm's front-face UV set must equal
        // the third-person right arm's, in atlas pixels: U∈[44,48], V∈[20,32].
        let (ox, oy) = entity_slot_origin(EntitySlot::Player);
        let to_px = |v: &ModelVertex| {
            (
                (v.uv[0] * ENTITY_ATLAS_WIDTH as f32 - ox as f32).round() as i32,
                (v.uv[1] * ENTITY_ATLAS_HEIGHT as f32 - oy as f32).round() as i32,
            )
        };

        // First-person arm (any transform: UVs are transform-independent).
        let mut fp = ModelMesh::new();
        fp.push_arm_box(&|local| local, 1.0);
        let mut fp_front: Vec<(i32, i32)> = fp.vertices[12..16].iter().map(to_px).collect();

        // Third-person right arm is humanoid part index 3 → verts 72..96; its
        // front face is verts 84..88.
        let mut tp = ModelMesh::new();
        tp.push_entity(EntityKind::RemotePlayer, Vec3::ZERO, 0.0, &EntityAnim::default(), None);
        let mut tp_front: Vec<(i32, i32)> = tp.vertices[84..88].iter().map(to_px).collect();

        fp_front.sort();
        tp_front.sort();
        assert_eq!(
            fp_front, tp_front,
            "first-person arm front face must sample the same texels as the third-person arm"
        );
        // And it is the front sub-rect (U 44..48, V 20..32), not the back (U 52..56).
        assert!(fp_front.iter().all(|&(u, v)| (44..=48).contains(&u) && (20..=32).contains(&v)));
    }

    #[test]
    fn solid_geometry_samples_the_white_texel() {
        let mut mesh = ModelMesh::new();
        mesh.push_box(Vec3::ZERO, Vec3::ONE, [1.0, 0.0, 0.0, 1.0]);
        assert!(mesh.vertices.iter().all(|v| v.uv == ENTITY_WHITE_UV));
    }

    #[test]
    fn unimplemented_entities_are_hidden() {
        let push = |kind| {
            let mut mesh = ModelMesh::new();
            mesh.push_entity(kind, Vec3::ZERO, 0.0, &EntityAnim::default(), None);
            mesh
        };
        // Unmodelled mob types and non-armor-stand objects emit no geometry.
        assert!(push(EntityKind::Mob(200)).is_empty(), "unmodelled mob must be hidden");
        assert!(push(EntityKind::Object(1)).is_empty(), "unmodelled object must be hidden");
        // The armor stand (object type 78) does render.
        assert!(!push(EntityKind::Object(78)).is_empty(), "armor stand must render");
    }

    /// Build a player mesh at the origin (yaw 0) with the given animation.
    fn player_mesh(anim: &EntityAnim) -> ModelMesh {
        let mut mesh = ModelMesh::new();
        mesh.push_entity(EntityKind::RemotePlayer, Vec3::ZERO, 0.0, anim, None);
        mesh
    }

    // Humanoid part vertex ranges (24 verts/part, in push order).
    const RIGHT_LEG: std::ops::Range<usize> = 0..24;
    const BODY: std::ops::Range<usize> = 48..72;
    const RIGHT_ARM: std::ops::Range<usize> = 72..96;
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
            mesh.push_entity(EntityKind::RemotePlayer, Vec3::ZERO, 0.0, &EntityAnim::default(), row);
            mesh
        };
        let default = player(None);
        let skinned = player(Some(0));
        // Same geometry but different V: the shared player slot (row 0) vs the
        // first per-player skin row.
        let v_default: Vec<f32> = default.vertices.iter().map(|v| v.uv[1]).collect();
        let v_skinned: Vec<f32> = skinned.vertices.iter().map(|v| v.uv[1]).collect();
        assert_ne!(v_default, v_skinned);
        // The skinned player must sample the first per-player skin slot's row in
        // the grid (its V band), not the shared Player slot.
        let (_, sy) = crate::texture::slot_grid_origin(PLAYER_SKIN_BASE_ROW);
        let region_start = sy as f32 / ENTITY_ATLAS_HEIGHT as f32;
        let region_end = (sy + ENTITY_SLOT_PX) as f32 / ENTITY_ATLAS_HEIGHT as f32;
        assert!(
            skinned
                .vertices
                .iter()
                .all(|v| (region_start - 1e-6..=region_end + 1e-6).contains(&v.uv[1])),
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

    #[test]
    fn old_animations_change_the_attack_swing_arm() {
        // Mid-swing the right arm sits at a different angle under the 1.7 cubic
        // curve than the 1.8 quartic one (other parts are unaffected by the flag).
        let new_anim = player_mesh(&EntityAnim {
            swing_progress: 0.5,
            ..EntityAnim::default()
        });
        let old_anim = player_mesh(&EntityAnim {
            swing_progress: 0.5,
            old_animations: true,
            ..EntityAnim::default()
        });
        assert!(
            parts_differ(&new_anim, &old_anim, RIGHT_ARM),
            "the 1.7 swing curve must move the attacking arm differently"
        );
        // The flag only retimes the swing — a non-swinging arm is identical.
        let rest_new = player_mesh(&EntityAnim::default());
        let rest_old = player_mesh(&EntityAnim {
            old_animations: true,
            ..EntityAnim::default()
        });
        assert!(
            !parts_differ(&rest_new, &rest_old, RIGHT_ARM),
            "with no swing the arm pose is the same regardless of the flag"
        );
    }

    /// Every 1.8 mob id that now has a model must build a well-formed, textured
    /// mesh (no panic, in-range UVs, not just the white-texel fallback box).
    #[test]
    fn modeled_mobs_are_well_formed_and_textured() {
        for id in [
            50, 51, 53, 54, 57, 120, 58, 52, 59, 90, 91, 92, 96, 93, 94, 95, 97, 98, 55, 62, 65,
            60, 67, 99, 100, 66, 56, 61, 68, 64, 101,
        ] {
            let mesh = build_mob(id);
            assert_well_formed(&mesh);
            assert!(
                mesh.vertices.iter().any(|v| v.uv != ENTITY_WHITE_UV),
                "mob {id} should be textured, not a solid fallback box"
            );
        }
    }

    /// Single-slot mobs must sample only their own atlas slot (a mismatched slot
    /// is exactly the bug that textured villagers/pigmen wrong before).
    #[test]
    fn single_slot_mobs_sample_their_own_slot() {
        use EntitySlot::*;
        for (id, slot) in [
            (54u8, Zombie),
            (51, Skeleton),
            (57, ZombiePigman),
            (120, Villager),
            (58, Enderman),
            (52, Spider),
            (90, Pig),
            (92, Cow),
            (96, Mooshroom),
            (95, Wolf),
            (98, Ocelot),
            (94, Squid),
            (55, Slime),
            (62, MagmaCube),
            (97, Snowman),
            (65, Bat),
            (60, Silverfish),
            (99, IronGolem),
            (100, Horse),
            (66, Witch),
            (56, Ghast),
            (61, Blaze),
            (68, Guardian),
            (64, Wither),
            (101, Rabbit),
        ] {
            let mesh = build_mob(id);
            let (v0, v1) = slot_v_range(slot);
            // Both axes must stay inside the slot's grid cell (the atlas is a
            // multi-column grid, so the U band identifies the column too).
            let (ox, oy) = entity_slot_origin(slot);
            let (u0, u1) = (
                ox as f32 / ENTITY_ATLAS_WIDTH as f32,
                (ox + ENTITY_SLOT_PX) as f32 / ENTITY_ATLAS_WIDTH as f32,
            );
            let _ = oy;
            assert!(
                mesh.vertices.iter().all(|v| {
                    (v0 - 1e-6..=v1 + 1e-6).contains(&v.uv[1])
                        && (u0 - 1e-6..=u1 + 1e-6).contains(&v.uv[0])
                }),
                "mob {id} sampled outside its slot"
            );
        }
    }

    /// The chest block-entity emits three well-formed, textured boxes (lid,
    /// knob, base) sampling its own atlas slot, sized to a single block cell.
    #[test]
    fn chest_emits_three_boxes_in_its_cell() {
        let mut mesh = ModelMesh::new();
        mesh.push_chest([10, 64, -7], 3, 0.0, ChestKind::Normal);
        // 3 parts × 6 faces × 4 verts; same in indices terms.
        assert_eq!(mesh.vertices.len(), 72);
        assert_eq!(mesh.indices.len(), 108);
        assert_well_formed(&mesh);
        // Textured from the chest-normal slot (not the white texel).
        assert!(mesh.vertices.iter().any(|v| v.uv != ENTITY_WHITE_UV));
        let (v0, v1) = slot_v_range(EntitySlot::ChestNormal);
        assert!(
            mesh.vertices.iter().all(|v| (v0 - 1e-6..=v1 + 1e-6).contains(&v.uv[1])),
            "chest sampled outside its slot"
        );
        // Every vertex sits within the 14/16-tall chest inside the block cell.
        for v in &mesh.vertices {
            assert!((10.0..=11.0).contains(&v.position[0]));
            assert!((64.0..=65.0).contains(&v.position[1]));
            assert!((-7.0..=-6.0).contains(&v.position[2]));
        }
    }

    /// Opening the lid rotates the lid/knob boxes (raising their top) while the
    /// base stays put, and the three chest kinds sample disjoint atlas slots.
    #[test]
    fn chest_lid_opens_and_kinds_use_distinct_slots() {
        let build = |angle: f32, kind: ChestKind| {
            let mut mesh = ModelMesh::new();
            mesh.push_chest([0, 0, 0], 3, angle, kind);
            mesh
        };
        let closed = build(0.0, ChestKind::Normal);
        let open = build(1.0, ChestKind::Normal);
        // The lid is the first part (verts 0..24); opening it changes its geometry.
        let moved = closed.vertices[0..24]
            .iter()
            .zip(&open.vertices[0..24])
            .any(|(a, b)| (Vec3::from(a.position) - Vec3::from(b.position)).length() > 1e-3);
        assert!(moved, "opening the lid must move the lid geometry");
        // The base (verts 48..72) is static.
        let base_still = closed.vertices[48..72]
            .iter()
            .zip(&open.vertices[48..72])
            .all(|(a, b)| (Vec3::from(a.position) - Vec3::from(b.position)).length() < 1e-6);
        assert!(base_still, "the base must not move when the lid opens");
        // Each kind samples its own slot's V range.
        for (kind, slot) in [
            (ChestKind::Normal, EntitySlot::ChestNormal),
            (ChestKind::Trapped, EntitySlot::ChestTrapped),
            (ChestKind::Ender, EntitySlot::ChestEnder),
        ] {
            let mesh = build(0.0, kind);
            let (v0, v1) = slot_v_range(slot);
            assert!(
                mesh.vertices.iter().all(|v| (v0 - 1e-6..=v1 + 1e-6).contains(&v.uv[1])),
                "chest kind sampled outside its slot"
            );
        }
    }

    /// The closed lid is textured right-side up: its world-UP-facing face is the
    /// box's genuine +y (top) face, so it samples the `box_region` "top" rect and
    /// its V runs the same way as a known-correct mob box's top face — not the
    /// flipped "bottom" rect that the old per-vertex scale(1,-1,-1) produced.
    #[test]
    fn chest_lid_top_face_is_textured_upright() {
        let mut mesh = ModelMesh::new();
        // meta 3 (south) → yaw 0, so engine space maps straight to world axes.
        mesh.push_chest([0, 0, 0], 3, 0.0, ChestKind::Normal);

        // The lid is part 0 (verts 0..24). Face index 1 (top, +y) is verts 4..8.
        let top: Vec<_> = mesh.vertices[4..8].to_vec();
        // It is the highest face of the lid: all four verts share the lid's max Y.
        let lid_max_y = mesh.vertices[0..24]
            .iter()
            .map(|v| v.position[1])
            .fold(f32::MIN, f32::max);
        assert!(
            top.iter().all(|v| (v.position[1] - lid_max_y).abs() < 1e-6),
            "lid face 1 must be the world-up face"
        );

        // It samples the chest's TOP rect box_region(0,0,14,5,14)[1] = [14,0,28,14],
        // relative to the chest-normal slot origin — never the bottom rect [28..42].
        let (ox, oy) = entity_slot_origin(EntitySlot::ChestNormal);
        let u_lo = (ox as f32 + 14.0) / ENTITY_ATLAS_WIDTH as f32;
        let u_hi = (ox as f32 + 28.0) / ENTITY_ATLAS_WIDTH as f32;
        let v_lo = oy as f32 / ENTITY_ATLAS_HEIGHT as f32;
        let v_hi = (oy as f32 + 14.0) / ENTITY_ATLAS_HEIGHT as f32;
        for v in &top {
            assert!(
                (u_lo - 1e-6..=u_hi + 1e-6).contains(&v.uv[0]),
                "lid top u {} outside the top rect — wrong box_region face",
                v.uv[0]
            );
            assert!((v_lo - 1e-6..=v_hi + 1e-6).contains(&v.uv[1]));
        }

        // The V coordinate increases as world-z decreases, matching plane_frac's
        // top-face convention (fv = 1 - fz). Pick the two verts at the z extremes.
        let front = top.iter().min_by(|a, b| a.position[2].total_cmp(&b.position[2])).unwrap();
        let back = top.iter().max_by(|a, b| a.position[2].total_cmp(&b.position[2])).unwrap();
        assert!(
            front.uv[1] > back.uv[1],
            "lid top V must grow toward -z (not vertically flipped): front {} back {}",
            front.uv[1],
            back.uv[1]
        );
    }

    /// The large (double) chest emits three well-formed boxes sampling the
    /// 128×64 `*_double` slot, and spans exactly the canonical cell plus its
    /// +X/+Z partner for every facing.
    #[test]
    fn large_chest_emits_three_boxes_spanning_two_cells() {
        let mut mesh = ModelMesh::new();
        mesh.push_large_chest([10, 64, -7], 3, 0.0, ChestKind::Normal);
        assert_eq!(mesh.vertices.len(), 72);
        assert_eq!(mesh.indices.len(), 108);
        assert_well_formed(&mesh);
        assert!(mesh.vertices.iter().any(|v| v.uv != ENTITY_WHITE_UV));

        // UVs stay inside the double slot's full 128px width and its V range.
        let (v0, v1) = slot_v_range(EntitySlot::ChestNormalDouble);
        let (ox, _) = entity_slot_origin(EntitySlot::ChestNormalDouble);
        let (u0, u1) = (
            ox as f32 / ENTITY_ATLAS_WIDTH as f32,
            (ox + ENTITY_SLOT_PX) as f32 / ENTITY_ATLAS_WIDTH as f32,
        );
        assert!(
            mesh.vertices.iter().all(|v| {
                (v0 - 1e-6..=v1 + 1e-6).contains(&v.uv[1])
                    && (u0 - 1e-6..=u1 + 1e-6).contains(&v.uv[0])
            }),
            "large chest sampled outside its slot"
        );

        // For every facing the model spans the canonical cell and its partner
        // (+X for meta 2/3, +Z for meta 4/5) — never the cell on the other side.
        let cell = [10i32, 64, -7];
        for (meta, axis) in [(2u8, 0usize), (3, 0), (4, 2), (5, 2)] {
            let mut m = ModelMesh::new();
            m.push_large_chest(cell, meta, 0.0, ChestKind::Normal);
            let lo = m.vertices.iter().map(|v| v.position[axis]).fold(f32::MAX, f32::min);
            let hi = m.vertices.iter().map(|v| v.position[axis]).fold(f32::MIN, f32::max);
            let c = cell[axis] as f32;
            // Spans canonical cell c .. partner c+2, never c-1.
            assert!(lo >= c - 0.001, "meta {meta} leaks into the −axis cell ({lo} < {c})");
            assert!(hi <= c + 2.001, "meta {meta} overruns the partner ({hi} > {c}+2)");
            assert!(hi - lo > 1.5, "meta {meta} must span two cells along the pairing axis");
            // The off-axis horizontal extent stays within one cell.
            let off = if axis == 0 { 2 } else { 0 };
            let olo = m.vertices.iter().map(|v| v.position[off]).fold(f32::MAX, f32::min);
            let ohi = m.vertices.iter().map(|v| v.position[off]).fold(f32::MIN, f32::max);
            assert!(ohi - olo <= 1.001, "meta {meta} too wide off-axis");
            assert!((cell[off] as f32 - 0.001..=cell[off] as f32 + 1.001).contains(&olo));
        }
    }

    /// A standing sign emits a well-formed board + post sampling its own slot,
    /// sized to a single block cell; a wall sign drops the post (one box only).
    #[test]
    fn sign_emits_board_and_post_in_its_slot() {
        let mut standing = ModelMesh::new();
        standing.push_sign([10, 64, -7], 4, SignKind::Standing);
        // 2 boxes (board + post) × 6 faces × 4 verts.
        assert_eq!(standing.vertices.len(), 48);
        assert_well_formed(&standing);
        assert!(standing.vertices.iter().any(|v| v.uv != ENTITY_WHITE_UV));
        let (v0, v1) = slot_v_range(EntitySlot::Sign);
        let (ox, _) = entity_slot_origin(EntitySlot::Sign);
        let (u0, u1) = (
            ox as f32 / ENTITY_ATLAS_WIDTH as f32,
            (ox + ENTITY_SLOT_PX) as f32 / ENTITY_ATLAS_WIDTH as f32,
        );
        assert!(
            standing.vertices.iter().all(|v| {
                (v0 - 1e-6..=v1 + 1e-6).contains(&v.uv[1])
                    && (u0 - 1e-6..=u1 + 1e-6).contains(&v.uv[0])
            }),
            "sign sampled outside its slot"
        );
        // Every vertex stays within (roughly) the cell footprint.
        for v in &standing.vertices {
            assert!((9.0..=12.0).contains(&v.position[0]));
            assert!((63.0..=66.0).contains(&v.position[1]));
            assert!((-8.0..=-5.0).contains(&v.position[2]));
        }
        // The wall sign omits the post: one box (board) only.
        let mut wall = ModelMesh::new();
        wall.push_sign([0, 0, 0], 2, SignKind::Wall);
        assert_eq!(wall.vertices.len(), 24);
        assert_well_formed(&wall);
    }

    /// The enchanting-table book emits a well-formed model from its own slot and
    /// hovers (its geometry shifts as the time counter advances).
    #[test]
    fn book_emits_geometry_and_hovers() {
        let build = |t: f32| {
            let mut mesh = ModelMesh::new();
            mesh.push_book([0, 64, 0], t);
            mesh
        };
        let a = build(0.0);
        assert_well_formed(&a);
        assert!(a.vertices.iter().any(|v| v.uv != ENTITY_WHITE_UV));
        let (v0, v1) = slot_v_range(EntitySlot::EnchantBook);
        assert!(
            a.vertices.iter().all(|v| (v0 - 1e-6..=v1 + 1e-6).contains(&v.uv[1])),
            "book sampled outside its slot"
        );
        let b = build(40.0);
        let moved = a
            .vertices
            .iter()
            .zip(&b.vertices)
            .any(|(p, q)| (Vec3::from(p.position) - Vec3::from(q.position)).length() > 1e-4);
        assert!(moved, "the book must animate over time");
    }

    /// The end-portal surface is a single flat dark quad at the top of the cell.
    #[test]
    fn end_portal_is_a_flat_quad_at_cell_top() {
        let mut mesh = ModelMesh::new();
        mesh.push_end_portal([5, 64, -2]);
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices.len(), 6);
        // All four corners sit on the same horizontal plane at y = cell + 0.75.
        assert!(mesh.vertices.iter().all(|v| (v.position[1] - 64.75).abs() < 1e-6));
        // A dark, opaque surface.
        for v in &mesh.vertices {
            assert!(v.color[0] < 0.2 && v.color[1] < 0.2 && v.color[2] < 0.3);
            assert_eq!(v.color[3], 1.0);
        }
    }

    /// The prone quadruped body (vanilla `body.rotateAngleX = +PI/2`) is baked
    /// as a static box, so its world-UP face must carry the texture rect that
    /// vanilla lands there after the rotation — the box's *back* rect `b[2]`,
    /// not the *front* rect `b[3]` the old code used (which read reversed). The
    /// V must also run head→tail (front of the animal at the rect's top edge),
    /// matching the upright convention rather than being vertically mirrored.
    #[test]
    fn prone_body_top_face_uses_the_back_rect_unmirrored() {
        // Build a pig at yaw 0 so engine axes map straight onto world axes.
        let mut mesh = ModelMesh::new();
        mesh.push_entity(EntityKind::Mob(90), Vec3::ZERO, 0.0, &EntityAnim::default(), None);

        // pig_parts order: 4 legs, body, head, snout. The body is part 4
        // (verts 96..120); FACES idx 1 (world +y top) is verts 100..104.
        let body = &mesh.vertices[96..120];
        let top = &mesh.vertices[100..104];
        // It is the body's highest face: all four verts share the body's max Y.
        let body_max_y = body.iter().map(|v| v.position[1]).fold(f32::MIN, f32::max);
        assert!(
            top.iter().all(|v| (v.position[1] - body_max_y).abs() < 1e-6),
            "body face 1 must be the world-up face"
        );

        // The body box is box_region(28,8, 10,16,8). Its *back* rect b[2] is the
        // 10×16 rect at texel [54,16,64,32]; its *front* rect b[3] is the 10×8
        // strip at [36,16,46,24]. The prone top must sample the back rect.
        let (ox, oy) = entity_slot_origin(EntitySlot::Pig);
        let back_u = (ox as f32 + 54.0..=ox as f32 + 64.0);
        let back_v = (oy as f32 + 16.0..=oy as f32 + 32.0);
        for v in top {
            let upx = v.uv[0] * ENTITY_ATLAS_WIDTH as f32;
            let vpx = v.uv[1] * ENTITY_ATLAS_HEIGHT as f32;
            assert!(
                back_u.contains(&upx) && back_v.contains(&vpx),
                "prone top must sample the back rect b[2] (54..64, 16..32), got ({upx},{vpx}) \
                 — the old code wrongly used the front rect b[3]"
            );
        }

        // Within the face, V must grow toward the back of the animal (-z): the
        // head end (+z, front) sits at the rect's top edge (small V), the tail
        // (-z) at the bottom. This is the same direction an upright box's top
        // face runs (fv = 1 - fz), proving the rect is not vertically flipped.
        let front = top.iter().min_by(|a, b| a.position[2].total_cmp(&b.position[2])).unwrap();
        let back = top.iter().max_by(|a, b| a.position[2].total_cmp(&b.position[2])).unwrap();
        // `front`/`back` here are the -z/+z verts; +z is the animal's head.
        let head = back; // larger z = front of the animal = head
        let tail = front;
        assert!(
            head.uv[1] < tail.uv[1],
            "prone top V must run head(+z)→tail(-z) like an upright top face, \
             not mirrored: head {} tail {}",
            head.uv[1],
            tail.uv[1]
        );
    }

    /// The sheep is drawn as a body layer plus an inflated wool overlay, so its
    /// mesh samples both the sheep slot and the sheep-fur slot.
    #[test]
    fn sheep_layers_body_and_wool() {
        let mesh = build_mob(91);
        assert_well_formed(&mesh);
        let (b0, b1) = slot_v_range(EntitySlot::Sheep);
        let (w0, w1) = slot_v_range(EntitySlot::SheepFur);
        assert!(
            mesh.vertices.iter().any(|v| (b0..=b1).contains(&v.uv[1])),
            "sheep is missing its body layer"
        );
        assert!(
            mesh.vertices.iter().any(|v| (w0..=w1).contains(&v.uv[1])),
            "sheep is missing its wool overlay"
        );
    }

    /// The standing sign's board front face must read upright: the readable face
    /// samples the `box_region` *front* rect (engraving region), and its V runs
    /// the right way up — the world-UP edge maps to the rect's TOP (small V), not
    /// the bottom. Before baking the renderer's scale(1,-1,-1) into the box
    /// bounds (it was negated only in the per-vertex transform) the board was
    /// textured upside-down, like the pre-fix chest lid.
    #[test]
    fn sign_board_front_face_is_textured_upright() {
        // meta 0 → yaw 0, so engine axes map straight to world axes.
        let mut mesh = ModelMesh::new();
        mesh.push_sign([0, 0, 0], 0, SignKind::Standing);

        // Board is box 0; FACES idx 3 (front, +z) is verts 12..16.
        let front = &mesh.vertices[12..16];
        // It samples the board's front rect box_region(0,0,24,12,2)[3] = [2,2,26,14].
        let (ox, oy) = entity_slot_origin(EntitySlot::Sign);
        for v in front {
            let upx = v.uv[0] * ENTITY_ATLAS_WIDTH as f32 - ox as f32;
            let vpx = v.uv[1] * ENTITY_ATLAS_HEIGHT as f32 - oy as f32;
            assert!(
                (2.0..=26.0).contains(&upx) && (2.0..=14.0).contains(&vpx),
                "sign front must sample the front rect (2..26, 2..14), got ({upx},{vpx})"
            );
        }
        // V must run upright: the highest (world-up) verts map to the rect's TOP
        // edge (small V), the lowest to the bottom.
        let top = front.iter().max_by(|a, b| a.position[1].total_cmp(&b.position[1])).unwrap();
        let bottom = front.iter().min_by(|a, b| a.position[1].total_cmp(&b.position[1])).unwrap();
        assert!(
            top.uv[1] < bottom.uv[1],
            "sign front V must grow downward (upright), not flipped: top {} bottom {}",
            top.uv[1],
            bottom.uv[1]
        );
    }

    /// The sign-text basis must sit ON the board, not 16× too high/big. Guards
    /// the model-px→block `0.0625` in `place` (without it the centre floated
    /// ~5.3 blocks up and the board read as huge).
    #[test]
    fn sign_text_basis_sits_on_the_board() {
        let (center, _right, _up, half_w, half_h) =
            ModelMesh::sign_text_basis([0, 64, 0], 0, SignKind::Standing);
        // Board centre sits ~0.83 above the cell base (the broken basis put it
        // ~5.3 blocks up, floating in the sky).
        assert!(
            (center.y - 64.83).abs() < 0.05,
            "sign text centre should sit on the board (~64.83), got {}",
            center.y
        );
        // Board half-extents: ~0.5 wide, ~0.25 tall (24×12 model px × 0.0625 × f / 2).
        assert!((half_w - 0.5).abs() < 0.02, "half_w should be ~0.5, got {half_w}");
        assert!((half_h - 0.25).abs() < 0.02, "half_h should be ~0.25, got {half_h}");
    }

    /// The enchanting-table book cover must read upright. coverRight's printed
    /// face samples the `box_region` front rect, and along the cover's tall (Y)
    /// axis the V is not vertically mirrored. Like the sign, the renderer's
    /// scale(1,-1,-1) is now baked into the bounds rather than negated only in
    /// the transform (which textured the cover upside-down).
    #[test]
    fn book_cover_front_face_is_textured_upright() {
        let mut mesh = ModelMesh::new();
        mesh.push_book([0, 0, 0], 0.0);
        // coverRight is box 0; its front face (FACES idx 3) is verts 12..16. The
        // cover is a flat quad (depth 0), so faces 3/2 are the only non-degenerate
        // ones; front samples box_region(0,0,6,10,0)[3] = [0,0,6,10].
        let front = &mesh.vertices[12..16];
        let (ox, oy) = entity_slot_origin(EntitySlot::EnchantBook);
        for v in front {
            let upx = v.uv[0] * ENTITY_ATLAS_WIDTH as f32 - ox as f32;
            let vpx = v.uv[1] * ENTITY_ATLAS_HEIGHT as f32 - oy as f32;
            assert!(
                (0.0..=6.0).contains(&upx) && (0.0..=10.0).contains(&vpx),
                "book cover front must sample the cover rect (0..6, 0..10), got ({upx},{vpx})"
            );
        }
        // The cover's box-local +y edge must map to the rect's TOP (small V). The
        // box is built so +y is its tall edge; with the flip baked into the
        // bounds, plane_frac's front-face fv = 1 - fy puts the +y edge at v=0.
        // Verify by comparing the two verts that differ only in the cover's height
        // direction: the V endpoints must be 0 (top) and 10 (bottom), one each.
        let vs: Vec<f32> = front
            .iter()
            .map(|v| (v.uv[1] * ENTITY_ATLAS_HEIGHT as f32 - oy as f32).round())
            .collect();
        assert!(vs.contains(&0.0) && vs.contains(&10.0), "cover front spans the full rect height upright");
    }
}
