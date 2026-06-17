//! First-person hand and held-item rendering, ported 1:1 from vanilla 1.8.9
//! (`ItemRenderer` + `RenderItem`, MCP-919). The exact GL transform chains
//! are replayed through glam matrices in GL view space (+X right, +Y up, −Z
//! forward) and the resulting vertices are mapped into world space along the
//! camera basis.
//!
//! Chains replicated verbatim:
//! - `doItemUsedTransformations` (swing translation) →
//!   `transformFirstPersonItem` (base pose + swing rotations) → the model's
//!   `RenderItem.preTransform` (`2×` only for non-gui3d generated items) →
//!   the model's firstperson display transform (identity for normal block
//!   items; `[0,-135,25]/[0,4,2]/1.7` for generated/handheld sprite items) →
//!   `RenderItem.renderItem`'s `scale(0.5)` + `translate(-0.5,-0.5,-0.5)`.
//! - `renderPlayerArm` for the empty hand, including the `ModelRenderer`
//!   rotation point and the 0.1 rad idle z-roll.
//! - `rotateWithPlayerRotations`: the hand lags the view by 10% of the gap.

use glam::{Mat4, Vec3};
use recraft_core::{BlockFace, BlockState, RenderShape, Tint};
use recraft_protocol::v1_8_9::packets::SlotItem;
use recraft_render::texture::item_texture_name;
use recraft_render::{ArmAttach, AtlasUv, BiomeColors, Camera, ModelMesh, Vertex};

use crate::game::{FirstPersonView, ItemUseAction};

/// One dropped item to render in the world: its stack, world position (entity
/// feet) and animation phase (`age + partialTicks`) driving bob and spin.
pub struct DroppedItem {
    pub item: SlotItem,
    pub pos: Vec3,
    pub phase: f32,
    /// `[sky_light, block_light]` in 0..1, sampled at the entity position.
    pub light: [f32; 2],
}

/// A world block-cube entity to render as a full terrain-textured cube at its
/// interpolated position: a falling block (SpawnObject kind 70) or a primed TNT
/// (kind 50). Primed TNT additionally swells and flashes white via `scale` /
/// `flash`; a falling block leaves those at `1.0` / `0.0`.
pub struct FallingBlock {
    pub block: BlockState,
    /// Entity feet position in world coordinates.
    pub pos: Vec3,
    /// `[sky_light, block_light]` in 0..1, sampled at the entity position.
    pub light: [f32; 2],
    /// Uniform scale about the block centre (1.0 = unscaled). Primed TNT swells
    /// up to 1.3 over its last 10 fuse ticks (vanilla `RenderTNTPrimed`).
    pub scale: f32,
    /// White-overlay strength 0..1 for the primed-TNT flash (0 = none). When
    /// positive the cube's colour and light are lerped toward full-bright white,
    /// approximating vanilla's additive white pass on `fuse / 5 % 2 == 0`.
    pub flash: f32,
}

/// A held item on another player: the stack, the right-arm attachment (which
/// already carries the articulated/holding/blocking arm pose), whether the
/// player is sneaking (an extra `LayerHeldItem` offset), and the hand lightmap.
pub struct PlayerHeldItem {
    pub item: SlotItem,
    pub attach: ArmAttach,
    pub sneaking: bool,
    pub light: [f32; 2],
}

/// Built item geometry for one frame: the normal textured `Vertex` data plus
/// the glint subset (geometry belonging to enchanted items only), which the
/// renderer re-draws additively with the scrolling glint texture. The glint
/// geometry is identical to the item geometry — only the pipeline/shader differ
/// — so an enchanted item's vertices are pushed to both `vertices` and `glint`.
#[derive(Default)]
pub struct ItemGeometry {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub glint_vertices: Vec<Vertex>,
    pub glint_indices: Vec<u32>,
}

impl ItemGeometry {
    /// Append `(verts, idx)` to the normal output, and additionally to the glint
    /// output when `enchanted`. Re-indexes against the destination's length.
    fn push(&mut self, verts: Vec<Vertex>, idx: Vec<u32>, enchanted: bool) {
        if enchanted {
            let base = self.glint_vertices.len() as u32;
            self.glint_vertices.extend(verts.iter().copied());
            self.glint_indices.extend(idx.iter().map(|i| i + base));
        }
        let base = self.vertices.len() as u32;
        self.vertices.extend(verts);
        self.indices.extend(idx.into_iter().map(|i| i + base));
    }

    /// Merge another built geometry into this one, re-indexing both the normal
    /// and glint streams (used to fold falling blocks and third-person held
    /// items into the shared world-item buffers).
    pub fn extend(&mut self, other: ItemGeometry) {
        let vbase = self.vertices.len() as u32;
        self.vertices.extend(other.vertices);
        self.indices.extend(other.indices.into_iter().map(|i| i + vbase));
        let gbase = self.glint_vertices.len() as u32;
        self.glint_vertices.extend(other.glint_vertices);
        self.glint_indices
            .extend(other.glint_indices.into_iter().map(|i| i + gbase));
    }
}

/// Whether a stack should render with the enchantment glint. Vanilla
/// `ItemStack.hasEffect`: true when the item carries a non-empty `ench` tag, and
/// always for the enchanted book (id 403) regardless of its `StoredEnchantments`
/// (`ItemEnchantedBook.hasEffect` is unconditional). The book's stored list is
/// also honoured so a book whose `ench` is absent still glints.
pub fn is_enchanted(item: &SlotItem) -> bool {
    if item.id == 403 {
        return true;
    }
    let Some(nbt) = item.nbt.as_ref() else {
        return false;
    };
    let non_empty_list =
        |key: &str| nbt.get(key).and_then(|t| t.as_list()).is_some_and(|l| !l.is_empty());
    non_empty_list("ench") || non_empty_list("StoredEnchantments")
}

pub struct ItemRenderer;

impl ItemRenderer {
    /// Append the first-person arm to the model pass — only with an empty
    /// hand, exactly like vanilla `renderItemInFirstPerson`.
    pub fn render_arm(mesh: &mut ModelMesh, camera: &Camera, view: &FirstPersonView) {
        if view.item.is_some() {
            return;
        }
        let matrix = arm_matrix(view);
        mesh.push_arm_box(
            &|local| view_to_world(camera, matrix.transform_point3(local)),
            1.0,
        );
    }

    /// Build the held item's first-person geometry for this frame. The whole
    /// item also feeds the glint output when it is enchanted.
    pub fn build_held_item(
        camera: &Camera,
        view: &FirstPersonView,
        atlas: &AtlasUv,
        old_animations: bool,
    ) -> ItemGeometry {
        let mut out = ItemGeometry::default();
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let Some(ref item) = view.item else {
            return out;
        };

        // Common head of the chain: swing translation and the base
        // first-person pose. The item-model transforms below follow vanilla's
        // RenderItem order per branch.
        let mut m = first_person_base(view);
        match view.use_action {
            ItemUseAction::Block => {
                // 1.8 locks the arm while blocking: no swing. 1.7 (old_animations)
                // kept the swing playing during a block — layer the same swing
                // translation + rotations vanilla uses for a free swing onto the
                // block pose so a left-click visibly swings while right-blocking.
                let swing = if old_animations { view.swing_progress } else { 0.0 };
                if old_animations {
                    do_item_used_transformations(&mut m, swing);
                }
                transform_first_person_item(&mut m, view.equip_progress, swing);
                do_block_transformations(&mut m);
            }
            ItemUseAction::Eat | ItemUseAction::Drink => {
                do_eat_drink_transformations(&mut m, view.use_ticks);
                transform_first_person_item(&mut m, view.equip_progress, 0.0);
            }
            ItemUseAction::Bow => {
                transform_first_person_item(&mut m, view.equip_progress, 0.0);
                do_bow_transformations(&mut m, view.use_ticks);
            }
            ItemUseAction::None => {
                do_item_used_transformations(&mut m, view.swing_progress);
                transform_first_person_item(&mut m, view.equip_progress, view.swing_progress);
            }
        }

        if (0..256).contains(&item.id) {
            let block = BlockState::new(item.id as u16, (item.damage.max(0) & 15) as u8);
            if block.is_air() || block.render_shape() == RenderShape::None {
                return out;
            }
            match block.render_shape() {
                // Flat blocks (torches, flowers, rails, ladders) use generated
                // sprite item models in vanilla.
                RenderShape::Cross | RenderShape::Rail | RenderShape::Ladder => {
                    gl_scale(&mut m, 2.0, 2.0, 2.0);
                    apply_first_person_display(&mut m);
                    finish_item_model(&mut m);
                    let name = block.texture_name(BlockFace::Side).map(str::to_owned);
                    let tint = tint3(block.tint(BlockFace::Side));
                    let to_world = |c: Vec3| view_to_world(camera, m.transform_point3(c));
                    push_sprite(&mut vertices, &mut indices, &to_world, name.as_deref(), atlas, tint, recraft_render::FULLBRIGHT);
                }
                _ => {
                    // 1.8 block models have NO firstperson display entry —
                    // identity — so the camera transform contributes no extra
                    // rotation. The outer first-person renderer applies 2x for
                    // gui3d models; RenderItem.renderItem's 0.5 cancels it.
                    gl_scale(&mut m, 2.0, 2.0, 2.0);
                    finish_item_model(&mut m);
                    let to_world = |c: Vec3| view_to_world(camera, m.transform_point3(c));
                    push_block_cube(&mut vertices, &mut indices, &to_world, block, atlas, recraft_render::FULLBRIGHT, 0.0);
                }
            }
        } else if let Some(name) = item_texture_name(item.id) {
            gl_scale(&mut m, 2.0, 2.0, 2.0);
            apply_first_person_display(&mut m);
            finish_item_model(&mut m);
            let name = format!("items/{name}");
            let to_world = |c: Vec3| view_to_world(camera, m.transform_point3(c));
            push_sprite(
                &mut vertices,
                &mut indices,
                &to_world,
                Some(&name),
                atlas,
                [1.0, 1.0, 1.0],
                recraft_render::FULLBRIGHT,
            );
        }
        out.push(vertices, indices, is_enchanted(item));
        out
    }

    /// Build geometry for every dropped item in the world this frame: block
    /// items spin as a small cube, sprite items billboard toward the camera, and
    /// all bob up and down like vanilla `RenderEntityItem`.
    pub fn build_world_items(
        camera: &Camera,
        items: &[DroppedItem],
        atlas: &AtlasUv,
    ) -> ItemGeometry {
        let mut out = ItemGeometry::default();
        // Camera basis so sprite items face the viewer.
        let forward = camera.direction();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();

        for d in items {
            // Vanilla `RenderEntityItem`: sin((age+partial)/10 + hoverStart) * 0.1 + 0.1,
            // lifted 0.25 blocks off the floor.
            let bob = (d.phase * 0.1).sin() * 0.1 + 0.1;
            let base = d.pos + Vec3::new(0.0, 0.25 + bob, 0.0);

            // Sprite billboard: model x→camera right, y→up, thin depth toward view.
            let sprite_to_world = |size: f32| {
                move |c: Vec3| {
                    base + right * ((c.x - 0.5) * size)
                        + up * ((c.y - 0.5) * size)
                        + forward * ((c.z - 0.5) * size * 0.25)
                }
            };

            // Build this item into its own buffer so an enchanted item's whole
            // geometry can be routed to the glint output.
            let mut v = Vec::new();
            let mut i = Vec::new();
            let item = &d.item;
            if (0..256).contains(&item.id) {
                let block = BlockState::new(item.id as u16, (item.damage.max(0) & 15) as u8);
                if block.is_air() || block.render_shape() == RenderShape::None {
                    continue;
                }
                match block.render_shape() {
                    RenderShape::Cross | RenderShape::Rail | RenderShape::Ladder => {
                        let name = block.texture_name(BlockFace::Side).map(str::to_owned);
                        let tint = tint3(block.tint(BlockFace::Side));
                        push_sprite(
                            &mut v,
                            &mut i,
                            &sprite_to_world(0.25),
                            name.as_deref(),
                            atlas,
                            tint,
                            d.light,
                        );
                    }
                    _ => {
                        // Spinning cube about Y: vanilla rotates at (age+partial)/20 rad/tick.
                        let (s, c) = (d.phase * 0.05).sin_cos();
                        let size = 0.25;
                        let spin_to_world = |corner: Vec3| {
                            let p = (corner - Vec3::splat(0.5)) * size;
                            base + Vec3::new(p.x * c - p.z * s, p.y, p.x * s + p.z * c)
                        };
                        push_block_cube(&mut v, &mut i, &spin_to_world, block, atlas, d.light, 0.0);
                    }
                }
            } else if let Some(name) = item_texture_name(item.id) {
                let name = format!("items/{name}");
                push_sprite(
                    &mut v,
                    &mut i,
                    &sprite_to_world(0.25),
                    Some(&name),
                    atlas,
                    [1.0, 1.0, 1.0],
                    d.light,
                );
            }
            out.push(v, i, is_enchanted(item));
        }
        out
    }

    /// Build full terrain-textured cubes for the falling-block entities this
    /// frame, into the same vertex buffer the dropped items use (it binds the
    /// block atlas). Each cube spans one block, centred horizontally on the
    /// entity and sitting on its feet (vanilla `RenderFallingBlock`).
    pub fn build_falling_blocks(
        items: &[FallingBlock],
        atlas: &AtlasUv,
    ) -> (Vec<Vertex>, Vec<u32>) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for f in items {
            if f.block.is_air() || f.block.render_shape() == RenderShape::None {
                continue;
            }
            // Unit-cube model space (0..1) → world: centred horizontally on the
            // entity, base at its feet. Primed TNT scales about the block centre
            // (0.5, 0.5, 0.5) so the swell grows symmetrically.
            let base = f.pos - Vec3::new(0.5, 0.0, 0.5);
            let center = Vec3::splat(0.5);
            let to_world = |c: Vec3| base + center + (c - center) * f.scale;
            push_block_cube(&mut vertices, &mut indices, &to_world, f.block, atlas, f.light, f.flash);
        }
        (vertices, indices)
    }

    /// Build camera-facing quads for this frame's block-break debris (vanilla
    /// `EntityDiggingFX`): each is a tiny billboard sampling a 1/4-tile
    /// sub-region of the broken block's top-face terrain tile, into the same
    /// block-atlas buffer the dropped items use. The quad is wound CCW toward
    /// the camera so the alpha-blended overlay pipeline (back-face culling)
    /// keeps it — matching the particle billboard winding.
    pub fn build_block_particles(
        camera: &Camera,
        debris: &[crate::particle::BlockDebris],
        atlas: &AtlasUv,
    ) -> (Vec<Vertex>, Vec<u32>) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let forward = camera.direction();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();

        for d in debris {
            if d.block.is_air() {
                continue;
            }
            let rect = atlas.tile_rect(d.block.texture_name(BlockFace::Top));
            // Sample the 1/4-tile sub-region selected at spawn (uv_off ∈ 0..0.75
            // per axis), staying inside the tile.
            let (u0, v0) = (
                rect[0] + d.uv_off[0] * rect[2],
                rect[1] + d.uv_off[1] * rect[3],
            );
            let (du, dv) = (rect[2] * 0.25, rect[3] * 0.25);
            // UV per corner in the build_particle_mesh corner order
            // (bottom-left, top-left, top-right, bottom-right).
            let uvs = [
                [u0, v0 + dv],
                [u0, v0],
                [u0 + du, v0],
                [u0 + du, v0 + dv],
            ];
            let center = d.world_pos;
            let h = d.size;
            let corners = [
                center - right * h - up * h,
                center - right * h + up * h,
                center + right * h + up * h,
                center + right * h - up * h,
            ];
            let tint = tint3(d.block.tint(BlockFace::Top));
            let base = vertices.len() as u32;
            for (corner, uv) in corners.iter().zip(uvs) {
                vertices.push(Vertex {
                    position: (*corner).into(),
                    color: [tint[0], tint[1], tint[2], 1.0],
                    uv,
                    light: recraft_render::FULLBRIGHT,
                });
            }
            indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
        }
        (vertices, indices)
    }

    /// Build geometry for items held by other players, porting vanilla's
    /// `LayerHeldItem` + `RenderItem` THIRD_PERSON path: `postRenderArm`'s anchor
    /// (provided by [`ArmAttach`]), the post-arm offset, the per-item-type
    /// third-person display transform (block / generated sprite / handheld tool),
    /// and `RenderItem.renderItem`'s base centring. The whole chain is built in
    /// vanilla's authored (y-down, front -z) model space, then mapped to
    /// recraft's feet-up, front +z arm frame by a 180-degree rotation about X
    /// (negate Y and Z), which preserves winding.
    pub fn build_player_held_items(
        held: &[PlayerHeldItem],
        atlas: &AtlasUv,
    ) -> ItemGeometry {
        let mut out = ItemGeometry::default();

        for h in held {
            let item = &h.item;
            let mut vertices = Vec::new();
            let mut indices = Vec::new();

            // Pick the item-model kind (its third-person display transform) and
            // draw the geometry. Block cubes render as a textured cube; flat
            // blocks and every flat item render as an extruded sprite.
            let drew = if (0..256).contains(&item.id) {
                let block = BlockState::new(item.id as u16, (item.damage.max(0) & 15) as u8);
                if block.is_air() || block.render_shape() == RenderShape::None {
                    false
                } else if matches!(
                    block.render_shape(),
                    RenderShape::Cross | RenderShape::Rail | RenderShape::Ladder
                ) {
                    let to_world = held_item_to_world(h, HeldItemKind::Generated);
                    let name = block.texture_name(BlockFace::Side).map(str::to_owned);
                    let tint = tint3(block.tint(BlockFace::Side));
                    push_sprite(&mut vertices, &mut indices, &to_world, name.as_deref(), atlas, tint, h.light);
                    true
                } else {
                    let to_world = held_item_to_world(h, HeldItemKind::Block);
                    push_block_cube(&mut vertices, &mut indices, &to_world, block, atlas, h.light, 0.0);
                    true
                }
            } else if let Some(name) = item_texture_name(item.id) {
                let kind = if is_handheld_item(name) {
                    HeldItemKind::Handheld
                } else {
                    HeldItemKind::Generated
                };
                let to_world = held_item_to_world(h, kind);
                let name = format!("items/{name}");
                push_sprite(&mut vertices, &mut indices, &to_world, Some(&name), atlas, [1.0, 1.0, 1.0], h.light);
                true
            } else {
                false
            };

            if drew {
                // `held_item_to_world` maps with a 180° rotation about X (negate
                // Y and Z), which preserves triangle winding — no re-wind needed
                // (the old Y-only reflection mirrored the geometry and flipped
                // every triangle, which is why it had to swap them back).
                out.push(vertices, indices, is_enchanted(item));
            }
        }
        out
    }
}

/// The three 1.8.9 item-model display families, by their THIRD_PERSON transform.
#[derive(Clone, Copy)]
enum HeldItemKind {
    /// `item/stone`-style block items: cube, rotation [10,-45,170], scale 0.375.
    Block,
    /// `builtin/generated` flat items (food, ingredients, flat blocks): rotation
    /// [-90,0,0], scale 0.55.
    Generated,
    /// Tools/stick/bone/blaze rod: rotation [0,90,-35], scale 0.85.
    Handheld,
}

/// Vanilla 1.8.9 item ids that override the third-person display with the
/// "handheld" pose: every tool (sword/shovel/pickaxe/axe/hoe) plus stick, bone
/// and blaze rod. Matched by texture base-name so it tracks the item registry.
fn is_handheld_item(name: &str) -> bool {
    matches!(name, "stick" | "bone" | "blaze_rod")
        || name.ends_with("_sword")
        || name.ends_with("_pickaxe")
        || name.ends_with("_axe")
        || name.ends_with("_shovel")
        || name.ends_with("_hoe")
}

/// Build the item-model (0..1) → world closure for a player-held item: the
/// vanilla `LayerHeldItem`/`RenderItem` THIRD_PERSON transform chain in authored
/// (y-down) space, then a Y reflection into recraft's feet-up arm-pixel frame and
/// the rigid arm attachment. See [`ItemRenderer::build_player_held_items`].
fn held_item_to_world(h: &PlayerHeldItem, kind: HeldItemKind) -> impl Fn(Vec3) -> Vec3 + '_ {
    // THIRD_PERSON display: translation (already block units), rotation degrees
    // (x, y, z), uniform scale — straight from the 1.8.9 item model JSON.
    let (trans, rot, scale) = match kind {
        HeldItemKind::Block => ([0.0, 1.5 / 16.0, -2.75 / 16.0], [10.0, -45.0, 170.0], 0.375),
        HeldItemKind::Generated => ([0.0, 1.0 / 16.0, -3.0 / 16.0], [-90.0, 0.0, 0.0], 0.55),
        HeldItemKind::Handheld => ([0.0, 1.25 / 16.0, -3.5 / 16.0], [0.0, 90.0, -35.0], 0.85),
    };

    let mut m = Mat4::IDENTITY;
    // LayerHeldItem: postRenderArm anchor is supplied by ArmAttach; this is the
    // fixed post-arm offset, then the sneak nudge.
    gl_translate(&mut m, -0.0625, 0.4375, 0.0625);
    if h.sneaking {
        gl_translate(&mut m, 0.0, 0.203125, 0.0);
    }
    // ItemRenderer.renderItem: the single 2× (gui3d via renderItem, sprites via
    // preTransform) — RenderItem.renderItem's 0.5 in finish_item_model cancels it.
    gl_scale(&mut m, 2.0, 2.0, 2.0);
    // ItemCameraTransforms.applyTransform(THIRD_PERSON): translate, rotY, rotX,
    // rotZ, scale.
    gl_translate(&mut m, trans[0], trans[1], trans[2]);
    gl_rotate(&mut m, rot[1], 0.0, 1.0, 0.0);
    gl_rotate(&mut m, rot[0], 1.0, 0.0, 0.0);
    gl_rotate(&mut m, rot[2], 0.0, 0.0, 1.0);
    gl_scale(&mut m, scale, scale, scale);
    // RenderItem.renderItem: scale(0.5), translate(-0.5) to centre the 0..1 model.
    finish_item_model(&mut m);

    move |c: Vec3| {
        // Vanilla authored item space (blocks, y-down, front −z) → recraft
        // arm-pixel frame (16 px/block, y-up, front +z), anchored at the
        // shoulder. This is a 180° rotation about X — negate Y AND Z — matching
        // `push_arm_box`'s `y → 1.5−y, z → −z` convention. Negating Y alone left
        // the item facing backwards (it looked "worn on the back").
        let v = m.transform_point3(c);
        let arm_px =
            ArmAttach::SHOULDER_PX + Vec3::new(v.x * 16.0, -v.y * 16.0, -v.z * 16.0);
        h.attach.to_world(arm_px)
    }
}

// ─── GlStateManager-equivalent matrix ops (post-multiplied, like GL) ─────────

fn gl_translate(m: &mut Mat4, x: f32, y: f32, z: f32) {
    *m *= Mat4::from_translation(Vec3::new(x, y, z));
}

fn gl_rotate(m: &mut Mat4, degrees: f32, x: f32, y: f32, z: f32) {
    *m *= Mat4::from_axis_angle(Vec3::new(x, y, z), degrees.to_radians());
}

fn gl_scale(m: &mut Mat4, x: f32, y: f32, z: f32) {
    *m *= Mat4::from_scale(Vec3::new(x, y, z));
}

/// GL view space → world space along the camera basis (GL looks down −Z).
fn view_to_world(camera: &Camera, v: Vec3) -> Vec3 {
    let forward = camera.direction();
    let right = forward.cross(Vec3::Y).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    camera.position + right * v.x + up * v.y - forward * v.z
}

// ─── Vanilla transform chains ────────────────────────────────────────────────

/// `rotateWithPlayerRotations`: the whole first-person rig lags the camera.
fn first_person_base(view: &FirstPersonView) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    gl_rotate(&mut m, view.arm_lag_pitch, 1.0, 0.0, 0.0);
    gl_rotate(&mut m, view.arm_lag_yaw, 0.0, 1.0, 0.0);
    m
}

/// `doItemUsedTransformations(swingProgress)`: the swing translation.
fn do_item_used_transformations(m: &mut Mat4, swing: f32) {
    let pi = std::f32::consts::PI;
    let s = swing.clamp(0.0, 1.0);
    let f = -0.4 * (s.sqrt() * pi).sin();
    let f1 = 0.2 * (s.sqrt() * pi * 2.0).sin();
    let f2 = -0.2 * (s * pi).sin();
    gl_translate(m, f, f1, f2);
}

/// `transformFirstPersonItem(equipProgress, swingProgress)`.
fn transform_first_person_item(m: &mut Mat4, equip: f32, swing: f32) {
    let pi = std::f32::consts::PI;
    gl_translate(m, 0.56, -0.52, -0.719_999_97);
    gl_translate(m, 0.0, equip * -0.6, 0.0);
    gl_rotate(m, 45.0, 0.0, 1.0, 0.0);
    let s = swing.clamp(0.0, 1.0);
    let f = (s * s * pi).sin();
    let f1 = (s.sqrt() * pi).sin();
    gl_rotate(m, f * -20.0, 0.0, 1.0, 0.0);
    gl_rotate(m, f1 * -20.0, 0.0, 0.0, 1.0);
    gl_rotate(m, f1 * -80.0, 1.0, 0.0, 0.0);
    gl_scale(m, 0.4, 0.4, 0.4);
}

/// `ItemRenderer` `EnumAction.BLOCK` branch: cant the raised sword into the
/// vanilla 1.8 blocking pose (applied after `transformFirstPersonItem(_, 0)`).
fn do_block_transformations(m: &mut Mat4) {
    gl_translate(m, -0.5, 0.2, 0.0);
    gl_rotate(m, 30.0, 0.0, 1.0, 0.0);
    gl_rotate(m, -80.0, 1.0, 0.0, 0.0);
    gl_rotate(m, 60.0, 0.0, 1.0, 0.0);
}

/// Vanilla `performDrinking` (EAT/DRINK): vertical bobble then rotate the item
/// toward the face, applied BEFORE `transformFirstPersonItem(equip, 0)`.
fn do_eat_drink_transformations(m: &mut Mat4, use_ticks: f32) {
    let pi = std::f32::consts::PI;
    let max_duration = 32.0f32;
    let remaining = (max_duration - use_ticks).max(1.0);
    let ratio = remaining / max_duration;
    let bobble = if ratio < 0.8 {
        (remaining / 4.0 * pi).cos().abs() * 0.1
    } else {
        0.0
    };
    gl_translate(m, 0.0, bobble, 0.0);
    let f = 1.0 - ratio.powi(27);
    gl_translate(m, f * 0.6, f * -0.5, 0.0);
    gl_rotate(m, f * 90.0, 0.0, 1.0, 0.0);
    gl_rotate(m, f * 10.0, 1.0, 0.0, 0.0);
    gl_rotate(m, f * 30.0, 0.0, 0.0, 1.0);
}

/// Vanilla `doBowTransformations`: the bow draws back over ~20 ticks with a
/// subtle wobble, applied AFTER `transformFirstPersonItem(equip, 0)`.
fn do_bow_transformations(m: &mut Mat4, use_ticks: f32) {
    gl_rotate(m, -18.0, 0.0, 0.0, 1.0);
    gl_rotate(m, -12.0, 0.0, 1.0, 0.0);
    gl_rotate(m, -8.0, 1.0, 0.0, 0.0);
    gl_translate(m, -0.9, 0.2, 0.0);
    let elapsed = use_ticks;
    let pull = ((elapsed / 20.0).powi(2) + elapsed / 20.0 * 2.0) / 3.0;
    let pull = pull.min(1.0);
    if pull > 0.1 {
        let wobble = ((elapsed - 0.1) * 1.3).sin() * (pull - 0.1);
        gl_translate(m, 0.0, wobble * 0.01, 0.0);
    }
    gl_translate(m, 0.0, 0.0, pull * 0.1);
    gl_scale(m, 1.0, 1.0, 1.0 + pull * 0.2);
}

/// The 1.8 generated/handheld firstperson display transform
/// (rotation [0,-135,25], translation [0,4,2]/16, scale 1.7), applied in the
/// vanilla order: translate, rotate Y, rotate X, rotate Z, scale.
fn apply_first_person_display(m: &mut Mat4) {
    gl_translate(m, 0.0, 4.0 / 16.0, 2.0 / 16.0);
    gl_rotate(m, -135.0, 0.0, 1.0, 0.0);
    gl_rotate(m, 25.0, 0.0, 0.0, 1.0);
    gl_scale(m, 1.7, 1.7, 1.7);
}

/// `RenderItem.renderItem`: scale(0.5) then center the 0..1 model.
fn finish_item_model(m: &mut Mat4) {
    gl_scale(m, 0.5, 0.5, 0.5);
    gl_translate(m, -0.5, -0.5, -0.5);
}

/// `renderPlayerArm` (empty hand), including the `ModelRenderer` rotation
/// point translate and the 0.1 rad idle z-roll from `setRotationAngles`.
fn arm_matrix(view: &FirstPersonView) -> Mat4 {
    let pi = std::f32::consts::PI;
    let s = view.swing_progress.clamp(0.0, 1.0);
    let mut m = first_person_base(view);
    let f = -0.3 * (s.sqrt() * pi).sin();
    let f1 = 0.4 * (s.sqrt() * pi * 2.0).sin();
    let f2 = -0.4 * (s * pi).sin();
    gl_translate(&mut m, f, f1, f2);
    gl_translate(&mut m, 0.640_000_05, -0.6, -0.719_999_97);
    gl_translate(&mut m, 0.0, view.equip_progress * -0.6, 0.0);
    gl_rotate(&mut m, 45.0, 0.0, 1.0, 0.0);
    let f3 = (s * s * pi).sin();
    let f4 = (s.sqrt() * pi).sin();
    gl_rotate(&mut m, f4 * 70.0, 0.0, 1.0, 0.0);
    gl_rotate(&mut m, f3 * -20.0, 0.0, 0.0, 1.0);
    gl_translate(&mut m, -1.0, 3.6, 3.5);
    gl_rotate(&mut m, 120.0, 0.0, 0.0, 1.0);
    gl_rotate(&mut m, 200.0, 1.0, 0.0, 0.0);
    gl_rotate(&mut m, -135.0, 0.0, 1.0, 0.0);
    gl_translate(&mut m, 5.6, 0.0, 0.0);
    // ModelRenderer.render(0.0625): rotation point (-5, 2, 0) then the idle
    // wobble rotateAngleZ = 0.1 rad with all-zero animation inputs.
    gl_translate(&mut m, -5.0 * 0.0625, 2.0 * 0.0625, 0.0);
    gl_rotate(&mut m, 0.1_f32.to_degrees(), 0.0, 0.0, 1.0);
    m
}

// ─── Geometry ────────────────────────────────────────────────────────────────

fn push_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    corners: [Vec3; 4],
    uvs: [[f32; 2]; 4],
    color: [f32; 4],
    light: [f32; 2],
) {
    let base = vertices.len() as u32;
    for (corner, uv) in corners.iter().zip(uvs) {
        vertices.push(Vertex {
            position: (*corner).into(),
            color,
            uv,
            light,
        });
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Constant tints for tinted block faces (plains biome colors).
fn tint3(tint: Tint) -> [f32; 3] {
    let biome = BiomeColors::default();
    match tint {
        Tint::None => [1.0, 1.0, 1.0],
        Tint::Grass => biome.grass,
        Tint::Foliage => biome.foliage,
        Tint::Water => [0.247, 0.463, 0.894],
        Tint::Rgb(rgb) => rgb,
    }
}

/// A held block: the unit-cube model with its real per-face textures and the
/// vanilla baked diffuse (up 1.0, down 0.5, ±z 0.8, ±x 0.6), transformed by
/// the first-person chain.
fn push_block_cube(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    to_world: &dyn Fn(Vec3) -> Vec3,
    block: BlockState,
    atlas: &AtlasUv,
    light: [f32; 2],
    flash: f32,
) {
    // Per face: vanilla FaceBakery vertex order (outward CCW), the texture
    // face, and the diffuse shade. Outward winding matters because first-person
    // items use vanilla back-face culling.
    let v = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
    let faces: [([Vec3; 4], BlockFace, f32); 6] = [
        (
            [v(0.0, 0.0, 1.0), v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(1.0, 0.0, 1.0)],
            BlockFace::Bottom,
            0.5,
        ),
        (
            [v(0.0, 1.0, 0.0), v(0.0, 1.0, 1.0), v(1.0, 1.0, 1.0), v(1.0, 1.0, 0.0)],
            BlockFace::Top,
            1.0,
        ),
        (
            [v(1.0, 1.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 0.0, 0.0), v(0.0, 1.0, 0.0)],
            BlockFace::Side,
            0.8,
        ),
        (
            [v(0.0, 1.0, 1.0), v(0.0, 0.0, 1.0), v(1.0, 0.0, 1.0), v(1.0, 1.0, 1.0)],
            BlockFace::Side,
            0.8,
        ),
        (
            [v(0.0, 1.0, 0.0), v(0.0, 0.0, 0.0), v(0.0, 0.0, 1.0), v(0.0, 1.0, 1.0)],
            BlockFace::Side,
            0.6,
        ),
        (
            [v(1.0, 1.0, 1.0), v(1.0, 0.0, 1.0), v(1.0, 0.0, 0.0), v(1.0, 1.0, 0.0)],
            BlockFace::Side,
            0.6,
        ),
    ];
    for (corners, face, shade) in faces {
        let rect = atlas.tile_rect(block.texture_name(face));
        let uvs = [
            [rect[0], rect[1]],
            [rect[0], rect[1] + rect[3]],
            [rect[0] + rect[2], rect[1] + rect[3]],
            [rect[0] + rect[2], rect[1]],
        ];
        let tint = tint3(block.tint(face));
        let mut color = [tint[0] * shade, tint[1] * shade, tint[2] * shade, 1.0];
        let mut light = light;
        // Primed-TNT white flash: lerp colour + lightmap toward full-bright white
        // (vanilla overlays an additive all-white, full-bright TNT here).
        if flash > 0.0 {
            for c in color.iter_mut().take(3) {
                *c += (1.0 - *c) * flash;
            }
            light[0] += (1.0 - light[0]) * flash;
            light[1] += (1.0 - light[1]) * flash;
        }
        let world = corners.map(to_world);
        push_quad(vertices, indices, world, uvs, color, light);
    }
}

/// A held sprite item: vanilla `ItemModelGenerator` geometry — the 16×16
/// layer extruded into a 1px-deep slab (z 7.5/16 to 8.5/16). Front and back
/// faces carry the texture (alpha-cut by the cutout shader), and every pixel
/// edge on the silhouette gets a side face so the item reads as a solid 3D
/// object instead of two crossing planes when viewed at an angle.
fn push_sprite(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    to_world: &dyn Fn(Vec3) -> Vec3,
    name: Option<&str>,
    atlas: &AtlasUv,
    tint: [f32; 3],
    light: [f32; 2],
) {
    let rect = atlas.tile_rect(name);
    let uv = |u: f32, v: f32| [rect[0] + u * rect[2], rect[1] + v * rect[3]];
    let color = [tint[0], tint[1], tint[2], 1.0];
    let w = |x: f32, y: f32, z: f32| to_world(Vec3::new(x, y, z));

    const ZF: f32 = 8.5 / 16.0; // front (+Z, "south")
    const ZB: f32 = 7.5 / 16.0; // back (−Z, "north")

    // Vanilla ItemModelGenerator emits one south-facing quad and one
    // north-facing quad. The renderer keeps back-face culling enabled for
    // items, so only the visible side draws; otherwise the far side of a thin,
    // diagonal sprite (swords) shows through the alpha holes as a second sword.
    push_quad(
        vertices,
        indices,
        [w(0.0, 1.0, ZF), w(0.0, 0.0, ZF), w(1.0, 0.0, ZF), w(1.0, 1.0, ZF)],
        [uv(0.0, 0.0), uv(0.0, 1.0), uv(1.0, 1.0), uv(1.0, 0.0)],
        color,
        light,
    );
    push_quad(
        vertices,
        indices,
        [w(1.0, 1.0, ZB), w(1.0, 0.0, ZB), w(0.0, 0.0, ZB), w(0.0, 1.0, ZB)],
        [uv(1.0, 0.0), uv(1.0, 1.0), uv(0.0, 1.0), uv(0.0, 0.0)],
        color,
        light,
    );

    // Per-pixel silhouette extrusion: a side face wherever an opaque texel
    // borders a transparent one (or the tile edge). Side faces are flat-shaded
    // a touch darker so the 1px depth reads. (No mask → flat front/back only.)
    let Some(mask) = atlas.tile_alpha_mask(name) else {
        return;
    };
    let opaque = |tx: i32, ty: i32| {
        (0..16).contains(&tx) && (0..16).contains(&ty) && mask[(ty * 16 + tx) as usize]
    };
    let edge = [color[0] * 0.85, color[1] * 0.85, color[2] * 0.85, 1.0];
    let px = 1.0 / 16.0;
    for ty in 0..16i32 {
        for tx in 0..16i32 {
            if !opaque(tx, ty) {
                continue;
            }
            // Texel (tx,ty): u∈[tx,tx+1]/16, v∈[ty,ty+1]/16; model x=u, y=1−v.
            let (x0, x1) = (tx as f32 * px, (tx as f32 + 1.0) * px);
            let (y0, y1) = (1.0 - (ty as f32 + 1.0) * px, 1.0 - ty as f32 * px);
            let suv = uv((tx as f32 + 0.5) * px, (ty as f32 + 0.5) * px);
            let suvs = [suv; 4];
            if !opaque(tx - 1, ty) {
                push_quad(vertices, indices,
                    [w(x0, y0, ZF), w(x0, y1, ZF), w(x0, y1, ZB), w(x0, y0, ZB)], suvs, edge, light);
            }
            if !opaque(tx + 1, ty) {
                push_quad(vertices, indices,
                    [w(x1, y0, ZB), w(x1, y1, ZB), w(x1, y1, ZF), w(x1, y0, ZF)], suvs, edge, light);
            }
            if !opaque(tx, ty - 1) {
                push_quad(vertices, indices,
                    [w(x0, y1, ZB), w(x0, y1, ZF), w(x1, y1, ZF), w(x1, y1, ZB)], suvs, edge, light);
            }
            if !opaque(tx, ty + 1) {
                push_quad(vertices, indices,
                    [w(x0, y0, ZF), w(x0, y0, ZB), w(x1, y0, ZB), w(x1, y0, ZF)], suvs, edge, light);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recraft_protocol::nbt::NbtTag;
    use recraft_protocol::v1_8_9::packets::SlotItem;
    use recraft_render::TextureAtlasImage;

    fn atlas() -> AtlasUv {
        TextureAtlasImage::load_default().uv_table()
    }

    fn camera() -> Camera {
        Camera::new(Vec3::new(0.0, 70.0, 0.0), 1.0)
    }

    fn view(id: Option<i16>) -> FirstPersonView {
        FirstPersonView {
            item: id.map(|id| SlotItem::new(id, 1, 0)),
            equip_progress: 0.0,
            swing_progress: 0.0,
            arm_lag_pitch: 0.0,
            arm_lag_yaw: 0.0,
            use_action: crate::game::ItemUseAction::None,
            use_ticks: 0.0,
        }
    }

    /// A blocking-sword first-person view with the given swing progress.
    fn blocking_view(swing: f32) -> FirstPersonView {
        FirstPersonView {
            item: Some(SlotItem::new(276, 1, 0)),
            equip_progress: 0.0,
            swing_progress: swing,
            arm_lag_pitch: 0.0,
            arm_lag_yaw: 0.0,
            use_action: crate::game::ItemUseAction::Block,
            use_ticks: 1.0,
        }
    }

    fn centroid(g: &ItemGeometry) -> Vec3 {
        g.vertices.iter().map(|v| Vec3::from(v.position)).sum::<Vec3>() / g.vertices.len() as f32
    }

    #[test]
    fn block_pose_anchors_at_the_vanilla_block_transform() {
        // transformFirstPersonItem(0,0) then doBlockTransformations applied to the
        // model center must reproduce the vanilla 1.8 block pose exactly: the
        // generated-sprite center maps through the documented chain. Compare a
        // direct matrix replay against build_held_item's geometry centroid.
        let mut m = first_person_base(&blocking_view(0.0));
        transform_first_person_item(&mut m, 0.0, 0.0);
        do_block_transformations(&mut m);
        gl_scale(&mut m, 2.0, 2.0, 2.0);
        apply_first_person_display(&mut m);
        finish_item_model(&mut m);
        let center = m.transform_point3(Vec3::new(0.5, 0.5, 8.0 / 16.0));
        let g = ItemRenderer::build_held_item(&camera(), &blocking_view(0.0), &atlas(), false);
        let world_center = view_to_world(&camera(), center);
        assert!(
            (centroid(&g) - world_center).length() < 0.05,
            "block-pose centroid {:?} vs anchor {world_center:?}",
            centroid(&g)
        );
    }

    #[test]
    fn swing_while_blocking_only_moves_with_old_animations() {
        let cam = camera();
        let uv = atlas();
        let rest = ItemRenderer::build_held_item(&cam, &blocking_view(0.0), &uv, false);

        // 1.8: a swing in progress does NOT move the blocking sword (arm locked).
        let locked = ItemRenderer::build_held_item(&cam, &blocking_view(0.5), &uv, false);
        assert!(
            (centroid(&rest) - centroid(&locked)).length() < 1.0e-6,
            "1.8 block pose must ignore swing_progress"
        );

        // 1.7 (old_animations): the same swing visibly displaces the hand.
        let swung = ItemRenderer::build_held_item(&cam, &blocking_view(0.5), &uv, true);
        assert!(
            (centroid(&rest) - centroid(&swung)).length() > 0.02,
            "1.7 must apply the swing while blocking"
        );
        // With no swing, old_animations on matches the rest block pose (the swing
        // transforms collapse to identity at swing=0).
        let on_rest = ItemRenderer::build_held_item(&cam, &blocking_view(0.0), &uv, true);
        assert!((centroid(&rest) - centroid(&on_rest)).length() < 1.0e-6);
    }

    #[test]
    fn player_held_block_anchors_near_the_hand() {
        use recraft_render::{arm_attach, ArmAttach, EntityAnim};
        // A player at an offset position/holding pose: the item must sit at the
        // rendered hand, not at the origin or metres away (gross-anchor guard).
        let anim = EntityAnim { held_item_right: 1, ..Default::default() };
        let attach = arm_attach(Vec3::new(10.0, 64.0, -3.0), 0.0, &anim);
        let hand = attach.to_world(ArmAttach::HAND_PX);
        let held = vec![PlayerHeldItem {
            item: SlotItem::new(1, 1, 0), // stone block
            attach,
            sneaking: false,
            light: [1.0, 1.0],
        }];
        let g = ItemRenderer::build_player_held_items(&held, &atlas());
        assert_eq!(g.vertices.len(), 24, "a held block is one cube");
        assert!(
            (centroid(&g) - hand).length() < 0.6,
            "held block centroid {:?} should sit at the hand {hand:?}",
            centroid(&g)
        );
    }

    #[test]
    fn player_held_item_extends_in_front_not_behind_the_hand() {
        use recraft_render::{arm_attach, ArmAttach, EntityAnim};
        // Rest arm pose, body facing +z (yaw 0): the item must extend toward the
        // front (+z), not the back. Mapping with a Y-only flip (missing the Z
        // negation) put it behind the player — it looked "worn on the back".
        let attach = arm_attach(Vec3::ZERO, 0.0, &EntityAnim::default());
        let hand = attach.to_world(ArmAttach::HAND_PX);
        let held = vec![PlayerHeldItem {
            item: SlotItem::new(276, 1, 0), // diamond sword (handheld pose)
            attach,
            sneaking: false,
            light: [1.0, 1.0],
        }];
        let g = ItemRenderer::build_player_held_items(&held, &atlas());
        assert!(
            centroid(&g).z > hand.z,
            "held item must extend in front of the hand (+z): centroid {:?} vs hand {hand:?}",
            centroid(&g)
        );
    }

    #[test]
    fn handheld_items_are_classified_by_name() {
        assert!(is_handheld_item("diamond_sword"));
        assert!(is_handheld_item("iron_pickaxe"));
        assert!(is_handheld_item("golden_hoe"));
        assert!(is_handheld_item("stick"));
        assert!(is_handheld_item("blaze_rod"));
        // Bow / fishing rod / shears use the generated third-person pose in 1.8.9.
        assert!(!is_handheld_item("bow"));
        assert!(!is_handheld_item("fishing_rod"));
        assert!(!is_handheld_item("apple"));
    }

    #[test]
    fn block_items_build_a_textured_cube() {
        let g = ItemRenderer::build_held_item(&camera(), &view(Some(1)), &atlas(), false);
        assert_eq!(g.vertices.len(), 24);
        assert_eq!(g.indices.len(), 36);
        // A plain stone block is not enchanted → no glint geometry.
        assert!(g.glint_vertices.is_empty() && g.glint_indices.is_empty());
    }

    #[test]
    fn sprite_items_build_extruded_geometry() {
        let uv = atlas();
        let g = ItemRenderer::build_held_item(&camera(), &view(Some(276)), &uv, false);
        // Front + back quads (8 verts) plus extruded silhouette edges; every
        // face is a quad, so verts are a multiple of 4 and indices = 6/quad.
        assert!(g.vertices.len() >= 8, "at least the front/back faces");
        assert_eq!(g.vertices.len() % 4, 0);
        assert_eq!(g.indices.len(), g.vertices.len() / 4 * 6);
        assert!(!uv.is_missing_tile(Some("items/diamond_sword")));
    }

    #[test]
    fn flat_blocks_hold_as_sprites() {
        let g = ItemRenderer::build_held_item(&camera(), &view(Some(50)), &atlas(), false);
        assert!(
            g.vertices.len() >= 8 && g.vertices.len() % 4 == 0,
            "torch should render as an extruded sprite"
        );
    }

    #[test]
    fn barrier_item_builds_nothing() {
        let g = ItemRenderer::build_held_item(&camera(), &view(Some(166)), &atlas(), false);
        assert!(g.vertices.is_empty() && g.indices.is_empty());
    }

    #[test]
    fn empty_hand_builds_nothing() {
        let g = ItemRenderer::build_held_item(&camera(), &view(None), &atlas(), false);
        assert!(g.vertices.is_empty() && g.indices.is_empty());
    }

    #[test]
    fn enchanted_item_emits_matching_glint_geometry() {
        // An enchanted sword (a non-empty `ench` tag) re-draws its full geometry
        // through the glint output.
        let mut item = SlotItem::new(276, 1, 0);
        let mut ench = std::collections::HashMap::new();
        ench.insert("id".to_string(), NbtTag::Short(16));
        ench.insert("lvl".to_string(), NbtTag::Short(5));
        item.nbt = Some(std::collections::HashMap::from([(
            "ench".to_string(),
            NbtTag::List(vec![NbtTag::Compound(ench)]),
        )]));
        let v = FirstPersonView {
            item: Some(item),
            equip_progress: 0.0,
            swing_progress: 0.0,
            arm_lag_pitch: 0.0,
            arm_lag_yaw: 0.0,
            use_action: crate::game::ItemUseAction::None,
            use_ticks: 0.0,
        };
        let g = ItemRenderer::build_held_item(&camera(), &v, &atlas(), false);
        assert_eq!(g.glint_vertices.len(), g.vertices.len());
        assert_eq!(g.glint_indices.len(), g.indices.len());
        assert!(!g.glint_vertices.is_empty());
    }

    #[test]
    fn enchanted_book_always_glints_without_nbt() {
        // Enchanted book (id 403) glows unconditionally (vanilla hasEffect).
        assert!(is_enchanted(&SlotItem::new(403, 1, 0)));
        // A plain item with no NBT does not.
        assert!(!is_enchanted(&SlotItem::new(276, 1, 0)));
    }

    fn dropped(id: i16) -> DroppedItem {
        DroppedItem {
            item: SlotItem::new(id, 1, 0),
            pos: Vec3::new(0.0, 64.0, -3.0),
            phase: 5.0,
            light: [1.0, 0.0],
        }
    }

    #[test]
    fn dropped_block_builds_a_spinning_cube() {
        let g = ItemRenderer::build_world_items(&camera(), &[dropped(1)], &atlas());
        assert_eq!(g.vertices.len(), 24); // one cube
        assert_eq!(g.indices.len(), 36);
        // The cube sits roughly at the item's world position (bobbed up a little).
        let centroid: Vec3 =
            g.vertices.iter().map(|v| Vec3::from(v.position)).sum::<Vec3>() / g.vertices.len() as f32;
        assert!((centroid - Vec3::new(0.0, 64.0, -3.0)).length() < 1.0);
    }

    #[test]
    fn dropped_sprite_billboards_toward_the_camera() {
        let g = ItemRenderer::build_world_items(&camera(), &[dropped(276)], &atlas());
        assert!(g.vertices.len() >= 8);
        assert_eq!(g.vertices.len() % 4, 0);
        assert_eq!(g.indices.len(), g.vertices.len() / 4 * 6);
    }

    #[test]
    fn no_dropped_items_builds_nothing() {
        let g = ItemRenderer::build_world_items(&camera(), &[], &atlas());
        assert!(g.vertices.is_empty() && g.indices.is_empty());
    }

    #[test]
    fn block_debris_builds_one_quad_each_within_the_block_tile() {
        let uv = atlas();
        let debris = vec![
            crate::particle::BlockDebris {
                world_pos: Vec3::new(0.0, 64.0, -3.0),
                size: 0.06,
                block: BlockState::STONE,
                uv_off: [0.0, 0.0],
            },
            crate::particle::BlockDebris {
                world_pos: Vec3::new(1.0, 64.0, -3.0),
                size: 0.06,
                block: BlockState::STONE,
                uv_off: [0.75, 0.5],
            },
        ];
        let (v, i) = ItemRenderer::build_block_particles(&camera(), &debris, &uv);
        // One camera-facing quad per debris particle.
        assert_eq!(v.len(), 8);
        assert_eq!(i.len(), 12);
        // Every sampled UV must land inside the broken block's top-face tile.
        let rect = uv.tile_rect(BlockState::STONE.texture_name(BlockFace::Top));
        for vert in &v {
            let [u, w] = vert.uv;
            assert!(
                u >= rect[0] - 1e-6 && u <= rect[0] + rect[2] + 1e-6,
                "u {u} outside tile [{}, {}]",
                rect[0],
                rect[0] + rect[2]
            );
            assert!(
                w >= rect[1] - 1e-6 && w <= rect[1] + rect[3] + 1e-6,
                "v {w} outside tile [{}, {}]",
                rect[1],
                rect[1] + rect[3]
            );
        }
    }

    #[test]
    fn air_block_debris_builds_nothing() {
        let debris = vec![crate::particle::BlockDebris {
            world_pos: Vec3::new(0.0, 64.0, -3.0),
            size: 0.06,
            block: BlockState::AIR,
            uv_off: [0.0, 0.0],
        }];
        let (v, i) = ItemRenderer::build_block_particles(&camera(), &debris, &atlas());
        assert!(v.is_empty() && i.is_empty());
    }

    #[test]
    fn rest_pose_matches_the_vanilla_anchor() {
        // With no swing/equip/lag, the block-model center (0.5,0.5,0.5) must
        // land exactly on transformFirstPersonItem's translation.
        let mut m = first_person_base(&view(None));
        do_item_used_transformations(&mut m, 0.0);
        transform_first_person_item(&mut m, 0.0, 0.0);
        gl_scale(&mut m, 2.0, 2.0, 2.0);
        finish_item_model(&mut m);
        let center = m.transform_point3(Vec3::splat(0.5));
        assert!((center.x - 0.56).abs() < 1.0e-6);
        assert!((center.y + 0.52).abs() < 1.0e-6);
        assert!((center.z + 0.72).abs() < 1.0e-5);
    }

    #[test]
    fn sprite_rest_pose_uses_vanilla_pretransform_scale() {
        let mut m = first_person_base(&view(None));
        do_item_used_transformations(&mut m, 0.0);
        transform_first_person_item(&mut m, 0.0, 0.0);
        gl_scale(&mut m, 2.0, 2.0, 2.0);
        apply_first_person_display(&mut m);
        finish_item_model(&mut m);
        let a = m.transform_point3(Vec3::new(0.0, 0.0, 8.5 / 16.0));
        let b = m.transform_point3(Vec3::new(1.0, 0.0, 8.5 / 16.0));
        let expected = 0.4 * 2.0 * 1.7 * 0.5;
        assert!((a.distance(b) - expected).abs() < 1.0e-6);
    }
}
