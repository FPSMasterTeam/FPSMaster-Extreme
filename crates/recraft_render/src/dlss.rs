//! NVIDIA DLSS Super Resolution integration (feature `dlss`, Vulkan + RTX only).
//!
//! SCAFFOLD — NOT YET COMPILE-VERIFIED. This module is written against the public
//! `dlss_wgpu` 4.x API but was authored on a machine that cannot build it (no DLSS
//! SDK / Vulkan / NVIDIA GPU). It compiles only with `--features dlss` on a set-up
//! Windows/Linux box; expect to fix small signature mismatches there. See
//! `docs/dlss.md` for the full integration steps and the parts that still need
//! wiring into the renderer (Vulkan device creation, settings.dlss hookup).
//!
//! The temporal inputs DLSS needs already exist in recraft (built for FSR2): a
//! jittered low-res HDR scene, a depth buffer, and an RG16F motion-vector buffer,
//! all at render resolution, plus a full-res output target. This wrapper just
//! hands those `TextureView`s to `dlss_wgpu` each frame.

use std::sync::{Arc, Mutex};

use dlss_wgpu::sdk::DlssSdk;
use dlss_wgpu::super_resolution::{
    DlssSuperResolution, DlssSuperResolutionExposure, DlssSuperResolutionRenderParameters,
};
use dlss_wgpu::{DlssError, DlssFeatureFlags, DlssPerfQualityMode};

/// Wraps the DLSS SDK handle + a Super Resolution context sized to the display.
pub struct Dlss {
    sdk: Arc<Mutex<DlssSdk>>,
    context: DlssSuperResolution,
}

impl Dlss {
    /// Create the SDK + a Super Resolution context for `display_resolution`
    /// (the upscaled output size). `project_id` is the application's NVIDIA NGX
    /// project UUID. The `device`/`queue` MUST come from a Vulkan adapter whose
    /// instance + device were created through `dlss_wgpu`'s extension helpers
    /// (see docs/dlss.md) — a plain `wgpu` device is missing the required Vulkan
    /// extensions and `DlssSdk::new` will fail.
    pub fn new(
        project_id: uuid::Uuid,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        display_resolution: [u32; 2],
        quality: DlssPerfQualityMode,
    ) -> Result<Self, DlssError> {
        let sdk = DlssSdk::new(project_id, device)?;
        let context = DlssSuperResolution::new(
            display_resolution,
            quality,
            DlssFeatureFlags::empty(),
            sdk.clone(),
            device,
            queue,
        )?;
        Ok(Self { sdk, context })
    }

    /// The resolution DLSS wants the scene rendered at for the chosen quality mode
    /// (drives `render_scale` instead of our manual preset). Returns `[w, h]`.
    pub fn render_resolution(&self) -> [u32; 2] {
        // NOTE: dlss_wgpu exposes `render_resolution()` returning a Resolution-like
        // value; adjust the field access once it compiles.
        let r = self.context.render_resolution();
        [r.width, r.height]
    }

    /// The sub-pixel projection jitter DLSS expects for `frame_number`, in pixels.
    /// Feed this into the camera jitter instead of recraft's own Halton sequence
    /// so the jitter pattern matches what DLSS was trained on.
    pub fn suggested_jitter(&self, frame_number: u32, render_resolution: [u32; 2]) -> [f32; 2] {
        let j = self.context.suggested_jitter(frame_number, render_resolution);
        [j.x, j.y]
    }

    /// Suggested texture LOD bias for the lower render resolution (sharper
    /// textures that DLSS resolves cleanly). Apply to the world sampler's
    /// `lod_bias` when DLSS is active.
    pub fn suggested_mip_bias(&self, render_resolution: [u32; 2]) -> f32 {
        self.context.suggested_mip_bias(render_resolution)
    }

    /// Run DLSS for one frame: upscale the jittered low-res `color` into
    /// `output` (display res), guided by `depth` + `motion_vectors` (both render
    /// res) and this frame's `jitter` (pixels). `reset` discards history on a
    /// camera cut / teleport. Records into `encoder` and returns the DLSS command
    /// buffer to submit alongside the frame's own.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        adapter: &wgpu::Adapter,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        motion_vectors: &wgpu::TextureView,
        output: &wgpu::TextureView,
        jitter: [f32; 2],
        reset: bool,
    ) -> Result<wgpu::CommandBuffer, DlssError> {
        let params = DlssSuperResolutionRenderParameters {
            color,
            depth,
            motion_vectors,
            // No HDR exposure texture; let DLSS auto-expose. Swap to
            // `DlssSuperResolutionExposure::Manual { .. }` if a 1x1 exposure
            // target is wired up later.
            exposure: DlssSuperResolutionExposure::default(),
            bias: None,
            dlss_output: output,
            reset,
            jitter_offset: jitter,
            partial_texture_size: None,
            // recraft's motion vectors are `cur_uv - prev_uv` in UV space; DLSS
            // wants them in render-target texels. Scale by the render resolution
            // (and VERIFY the sign convention on Windows — DLSS expects the vector
            // pointing toward the previous frame). See docs/dlss.md.
            motion_vector_scale: None,
        };
        self.context.render(params, encoder, adapter)
    }

    /// Keep a reference alive so the SDK outlives the context.
    pub fn sdk(&self) -> &Arc<Mutex<DlssSdk>> {
        &self.sdk
    }
}
