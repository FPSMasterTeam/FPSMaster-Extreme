//! First-person rendering of the player's arm and held item (vanilla
//! ItemRenderer). Builds model geometry each frame; the swing animation
//! follows the same curves as the arm so the item tracks the hand.

use glam::Vec3;
use recraft_protocol::v1_8_9::packets::SlotItem;
use recraft_render::{Camera, ModelMesh};

/// Skin-toned first-person arm color (alpha only; the skin texture tones it).
const ARM_COLOR: [f32; 4] = [0.86, 0.71, 0.58, 1.0];

pub struct ItemRenderer;

impl ItemRenderer {
    /// Append the first-person arm and the currently held item to `mesh`.
    /// `swing` is the 0..1 swing progress (0 = at rest).
    pub fn render_first_person(
        mesh: &mut ModelMesh,
        camera: &Camera,
        swing: f32,
        held: Option<SlotItem>,
    ) {
        mesh.push_hand(camera, swing, ARM_COLOR);
        if let Some(item) = held {
            Self::render_held_item(mesh, camera, swing, item);
        }
    }

    /// A small cube riding the hand: a stand-in for the held block/item model
    /// (real block models in hand can layer on top of this later).
    fn render_held_item(mesh: &mut ModelMesh, camera: &Camera, swing: f32, item: SlotItem) {
        let forward = camera.direction();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        let pi = std::f32::consts::PI;

        // Same swing curves as the arm (vanilla swing arc + cross pull).
        let swing = swing.clamp(0.0, 1.0);
        let arc = (swing.sqrt() * pi).sin();
        let pull = (swing * swing * pi).sin();

        let center = camera.position
            + forward * (0.62 + 0.20 * arc)
            + right * (0.40 - 0.34 * pull)
            + up * (-0.34 + 0.30 * arc);
        let half = Vec3::splat(0.10);
        mesh.push_box(center - half, center + half, item_color(item.id));
    }
}

/// Deterministic tint per item id so different held items are telling apart
/// (matches the inventory swatch fallback).
fn item_color(id: i16) -> [f32; 4] {
    let id = id as u32;
    [
        (80 + (id.wrapping_mul(73) % 160)) as f32 / 255.0,
        (80 + (id.wrapping_mul(151) % 160)) as f32 / 255.0,
        (80 + (id.wrapping_mul(199) % 160)) as f32 / 255.0,
        1.0,
    ]
}
