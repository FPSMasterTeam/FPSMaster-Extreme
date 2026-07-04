pub mod camera;
pub mod chunk_mesh;
/// NVIDIA DLSS upscaler (Vulkan + RTX only). Compiled only with `--features dlss`
/// (see docs/dlss.md); absent from the default cross-platform build.
#[cfg(feature = "dlss")]
pub mod dlss;
pub mod font;
pub mod i18n;
pub mod gui_item;
pub mod item_names;
mod mesh_worker;
pub mod model;
pub mod particle;
pub mod renderer;
pub mod sky;
pub mod texture;
pub mod ui;

pub use camera::{Camera, Frustum};
pub use chunk_mesh::{
    block_tint, build_section_mesh, build_world_mesh, set_block_tints, BiomeColors, ChunkMesh,
    ChunkMeshBuffers, ChunkVertex, EmissiveLight, TintTable, Vertex, FULLBRIGHT,
};
pub use font::char_width;
pub use item_names::{build_tooltip, item_display_name};
pub use model::{
    arm_attach, ArmAttach, ChestKind, EntityAnim, ModelMesh, ModelVertex, SignKind,
};
pub use particle::{build_particle_mesh, ParticleBillboard};
pub use renderer::{
    InventoryPreview, LineVertex, RenderStats, Renderer, RendererError, SignTextDraw,
};
pub use texture::{
    AtlasUv, OverlayTextures, PackMeta, PbrAtlases, TextureAtlasImage, discover_resource_packs,
    list_lang_codes, read_asset_string,
};
pub use ui::{
    gui_pixel_scale, gui_pixel_scale_capped, text_height, text_width, GuiAtlas, GuiTexture,
    UiBatch, UiColor, UiFrame, UiGeometry, UiRect, UiTextureId, UiVertex,
};
