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
use recraft_render::{AtlasUv, BiomeColors, Camera, HeldItemFrame, ModelMesh, Vertex};

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

/// A held item on another player, with the arm's world-space reference frame
/// so the item model can be oriented correctly.
pub struct PlayerHeldItem {
    pub item: SlotItem,
    pub frame: HeldItemFrame,
    pub light: [f32; 2],
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

    /// Build the held item's first-person geometry for this frame.
    pub fn build_held_item(
        camera: &Camera,
        view: &FirstPersonView,
        atlas: &AtlasUv,
    ) -> (Vec<Vertex>, Vec<u32>) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let Some(ref item) = view.item else {
            return (vertices, indices);
        };

        // Common head of the chain: swing translation and the base
        // first-person pose. The item-model transforms below follow vanilla's
        // RenderItem order per branch.
        let mut m = first_person_base(view);
        match view.use_action {
            ItemUseAction::Block => {
                transform_first_person_item(&mut m, view.equip_progress, 0.0);
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
                return (vertices, indices);
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
                    push_block_cube(&mut vertices, &mut indices, &to_world, block, atlas, recraft_render::FULLBRIGHT);
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
        (vertices, indices)
    }

    /// Build geometry for every dropped item in the world this frame: block
    /// items spin as a small cube, sprite items billboard toward the camera, and
    /// all bob up and down like vanilla `RenderEntityItem`.
    pub fn build_world_items(
        camera: &Camera,
        items: &[DroppedItem],
        atlas: &AtlasUv,
    ) -> (Vec<Vertex>, Vec<u32>) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
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
                            &mut vertices,
                            &mut indices,
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
                        let spin_to_world = |v: Vec3| {
                            let p = (v - Vec3::splat(0.5)) * size;
                            base + Vec3::new(p.x * c - p.z * s, p.y, p.x * s + p.z * c)
                        };
                        push_block_cube(&mut vertices, &mut indices, &spin_to_world, block, atlas, d.light);
                    }
                }
            } else if let Some(name) = item_texture_name(item.id) {
                let name = format!("items/{name}");
                push_sprite(
                    &mut vertices,
                    &mut indices,
                    &sprite_to_world(0.25),
                    Some(&name),
                    atlas,
                    [1.0, 1.0, 1.0],
                    d.light,
                );
            }
        }
        (vertices, indices)
    }

    /// Build geometry for items held by other players. Each item is positioned
    /// at the player's right hand and oriented along the arm, replicating
    /// vanilla's `LayerHeldItem` third-person appearance.
    pub fn build_player_held_items(
        held: &[PlayerHeldItem],
        atlas: &AtlasUv,
    ) -> (Vec<Vertex>, Vec<u32>) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();

        for h in held {
            let f = &h.frame;
            let item = &h.item;
            // The item extends along the arm direction, centered at the hand.
            // `arm_dir` points shoulder→hand (roughly downward at rest),
            // `forward` is the body's front, `right` is the lateral axis.
            let size = 0.35;
            let half = size * 0.5;

            // Build a local→world transform using the arm frame:
            //  - item X → frame.right
            //  - item Y → -frame.arm_dir  (up = opposite to arm direction)
            //  - item Z → frame.forward
            // Origin: hand, shifted slightly along arm_dir so the item hangs
            // below the hand rather than centering on it.
            let origin = f.hand + f.arm_dir * half;

            let to_world = |c: Vec3| {
                let local = (c - Vec3::splat(0.5)) * size;
                origin + f.right * local.x - f.arm_dir * local.y + f.forward * local.z
            };

            if (0..256).contains(&item.id) {
                let block =
                    BlockState::new(item.id as u16, (item.damage.max(0) & 15) as u8);
                if block.is_air() || block.render_shape() == RenderShape::None {
                    continue;
                }
                match block.render_shape() {
                    RenderShape::Cross | RenderShape::Rail | RenderShape::Ladder => {
                        let name = block.texture_name(BlockFace::Side).map(str::to_owned);
                        let tint = tint3(block.tint(BlockFace::Side));
                        push_held_sprite(
                            &mut vertices,
                            &mut indices,
                            &to_world,
                            name.as_deref(),
                            atlas,
                            tint,
                            h.light,
                        );
                    }
                    _ => {
                        push_block_cube(
                            &mut vertices,
                            &mut indices,
                            &to_world,
                            block,
                            atlas,
                            h.light,
                        );
                    }
                }
            } else if let Some(name) = item_texture_name(item.id) {
                let name = format!("items/{name}");
                push_held_sprite(
                    &mut vertices,
                    &mut indices,
                    &to_world,
                    Some(&name),
                    atlas,
                    [1.0, 1.0, 1.0],
                    h.light,
                );
            }
        }
        (vertices, indices)
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
        let color = [tint[0] * shade, tint[1] * shade, tint[2] * shade, 1.0];
        let world = corners.map(to_world);
        push_quad(vertices, indices, world, uvs, color, light);
    }
}

/// Simplified held sprite: front and back quads through the caller's transform
/// (oriented by the arm frame, not billboarded). No per-pixel extrusion —
/// just the two flat faces, which is enough at third-person distance.
fn push_held_sprite(
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

    const ZF: f32 = 0.53;
    const ZB: f32 = 0.47;

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

    #[test]
    fn block_items_build_a_textured_cube() {
        let (vertices, indices) =
            ItemRenderer::build_held_item(&camera(), &view(Some(1)), &atlas());
        assert_eq!(vertices.len(), 24);
        assert_eq!(indices.len(), 36);
    }

    #[test]
    fn sprite_items_build_extruded_geometry() {
        let uv = atlas();
        let (vertices, indices) =
            ItemRenderer::build_held_item(&camera(), &view(Some(276)), &uv);
        // Front + back quads (8 verts) plus extruded silhouette edges; every
        // face is a quad, so verts are a multiple of 4 and indices = 6/quad.
        assert!(vertices.len() >= 8, "at least the front/back faces");
        assert_eq!(vertices.len() % 4, 0);
        assert_eq!(indices.len(), vertices.len() / 4 * 6);
        assert!(!uv.is_missing_tile(Some("items/diamond_sword")));
    }

    #[test]
    fn flat_blocks_hold_as_sprites() {
        let (vertices, _) = ItemRenderer::build_held_item(&camera(), &view(Some(50)), &atlas());
        assert!(
            vertices.len() >= 8 && vertices.len() % 4 == 0,
            "torch should render as an extruded sprite"
        );
    }

    #[test]
    fn barrier_item_builds_nothing() {
        let (vertices, indices) =
            ItemRenderer::build_held_item(&camera(), &view(Some(166)), &atlas());
        assert!(vertices.is_empty() && indices.is_empty());
    }

    #[test]
    fn empty_hand_builds_nothing() {
        let (vertices, indices) = ItemRenderer::build_held_item(&camera(), &view(None), &atlas());
        assert!(vertices.is_empty() && indices.is_empty());
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
        let (vertices, indices) =
            ItemRenderer::build_world_items(&camera(), &[dropped(1)], &atlas());
        assert_eq!(vertices.len(), 24); // one cube
        assert_eq!(indices.len(), 36);
        // The cube sits roughly at the item's world position (bobbed up a little).
        let centroid: Vec3 =
            vertices.iter().map(|v| Vec3::from(v.position)).sum::<Vec3>() / vertices.len() as f32;
        assert!((centroid - Vec3::new(0.0, 64.0, -3.0)).length() < 1.0);
    }

    #[test]
    fn dropped_sprite_billboards_toward_the_camera() {
        let (vertices, indices) =
            ItemRenderer::build_world_items(&camera(), &[dropped(276)], &atlas());
        assert!(vertices.len() >= 8);
        assert_eq!(vertices.len() % 4, 0);
        assert_eq!(indices.len(), vertices.len() / 4 * 6);
    }

    #[test]
    fn no_dropped_items_builds_nothing() {
        let (vertices, indices) = ItemRenderer::build_world_items(&camera(), &[], &atlas());
        assert!(vertices.is_empty() && indices.is_empty());
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
