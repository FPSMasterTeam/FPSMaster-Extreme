//! NVIDIA DLSS Super Resolution integration (feature `dlss`, Vulkan + RTX only).
//!
//! Compiles against `dlss_wgpu` 4.0.0 and is wired into the renderer: the device is
//! created through `dlss_wgpu` (NGX Vulkan extensions), and each frame the world's
//! jittered low-res HDR scene + depth + RG16F motion vectors are upscaled here in
//! place of the TAA resolve. Working on an RTX 5060.
//!
//! IMPORTANT: the `DlssSuperResolution` context must be built LAZILY (first render
//! frame), NOT at device-init time — an NGX feature created right after the device
//! evaluates to an all-black image. The renderer does this in `ensure_dlss`.
//!
//! Verify-deferred (visual, see docs/DLSS.md §4.5/§6): the motion-vector sign (Bevy
//! negates `motion_vector_scale`; if movement smears backwards, negate ours) and
//! wiring `reset` to teleports / dimension changes.

use std::sync::{Arc, Mutex};

use dlss_wgpu::super_resolution::{
    DlssSuperResolution, DlssSuperResolutionExposure, DlssSuperResolutionRenderParameters,
};
use dlss_wgpu::{DlssError, DlssFeatureFlags, DlssPerfQualityMode, DlssSdk};

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
        // DlssSdk::new takes an owned Device (it stores a clone internally); wgpu's
        // Device is cheaply clonable (Arc inside).
        let sdk = DlssSdk::new(project_id, device.clone())?;
        // Feature flags matching the reference (Bevy) integration:
        // - HighDynamicRange: our scene target is LINEAR HDR (Rgba16Float, values can
        //   exceed 1.0); without this DLSS assumes tonemapped SDR input → wrong output.
        // - LowResolutionMotionVectors: our MV buffer is at the (lower) render res.
        // - AutoExposure: we feed `Exposure::Automatic` (DLSS computes exposure).
        // NOT InvertedDepth — fpsmaster uses standard depth (0=near), unlike Bevy's
        // reverse-Z.
        let flags = DlssFeatureFlags::HighDynamicRange
            | DlssFeatureFlags::LowResolutionMotionVectors
            | DlssFeatureFlags::AutoExposure;
        let context = DlssSuperResolution::new(
            display_resolution,
            quality,
            flags,
            sdk.clone(),
            device,
            queue,
        )?;
        Ok(Self { sdk, context })
    }

    /// The resolution DLSS wants the scene rendered at for the chosen quality mode
    /// (drives `render_scale` instead of our manual preset). Returns `[w, h]`.
    pub fn render_resolution(&self) -> [u32; 2] {
        self.context.render_resolution()
    }

    /// The [min, max] render-resolution range for the current quality mode.
    pub fn render_resolution_range(&self) -> core::ops::RangeInclusive<[u32; 2]> {
        self.context.render_resolution_range()
    }

    /// The sub-pixel projection jitter DLSS expects for `frame_number`, in pixels.
    /// Feed this into the camera jitter instead of fpsmaster's own Halton sequence
    /// so the jitter pattern matches what DLSS was trained on.
    pub fn suggested_jitter(&self, frame_number: u32, render_resolution: [u32; 2]) -> [f32; 2] {
        self.context.suggested_jitter(frame_number, render_resolution)
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
        // Render resolution `[w, h]` in texels: fpsmaster's motion vectors are in UV
        // space, so DLSS needs this to convert them to texel space.
        mv_scale: [f32; 2],
    ) -> Result<wgpu::CommandBuffer, DlssError> {
        let params = DlssSuperResolutionRenderParameters {
            color,
            depth,
            motion_vectors,
            // Automatic exposure (DLSS computes it), paired with the AutoExposure
            // feature flag — the reference integration's approach.
            exposure: DlssSuperResolutionExposure::Automatic,
            bias: None,
            dlss_output: output,
            reset,
            jitter_offset: jitter,
            // Our input textures are exactly the render resolution, so tell NGX the
            // rendered subrect is that size. Without this it defaults to the *max*
            // render resolution, which can exceed our textures → reads garbage / black.
            partial_texture_size: Some([mv_scale[0] as u32, mv_scale[1] as u32]),
            // fpsmaster's motion vectors are `cur_uv - prev_uv` in UV space; DLSS wants
            // render-target texels, so scale by the render resolution. (VERIFY the
            // sign on screen — DLSS expects current→previous; negate in the MV shader
            // if upscaling produces reverse-direction smearing. See docs/DLSS.md §4.5.)
            motion_vector_scale: Some(mv_scale),
        };
        self.context.render(params, encoder, adapter)
    }

    /// Keep a reference alive so the SDK outlives the context.
    pub fn sdk(&self) -> &Arc<Mutex<DlssSdk>> {
        &self.sdk
    }
}

/// Read back the centre 64×64 patch of an `Rgba16Float` texture (must have COPY_SRC)
/// and return its average RGB. Blocks the device. Debug/verification only.
pub fn readback_center_avg(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Result<f32, String> {
    const N: u32 = 64;
    let bytes_per_row = ((N * 8 + 255) / 256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("dbg-readback"),
        size: (bytes_per_row * N) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("dbg-copy") });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: w / 2 - N / 2, y: h / 2 - N / 2, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(N) },
        },
        wgpu::Extent3d { width: N, height: N, depth_or_array_layers: 1 },
    );
    queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| format!("poll: {e:?}"))?;
    let data = slice.get_mapped_range();
    let mut sum = 0.0f64;
    for row in 0..N {
        let base = (row * bytes_per_row) as usize;
        for px in 0..N {
            let o = base + px as usize * 8;
            let r = f16_to_f32(u16::from_le_bytes([data[o], data[o + 1]]));
            let g = f16_to_f32(u16::from_le_bytes([data[o + 2], data[o + 3]]));
            let b = f16_to_f32(u16::from_le_bytes([data[o + 4], data[o + 5]]));
            sum += (r + g + b) as f64 / 3.0;
        }
    }
    Ok((sum / (N * N) as f64) as f32)
}

/// Half-precision (f16 bits) -> f32, for reading back the Rgba16Float output.
fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as i32;
    let exp = ((h >> 10) & 0x1f) as i32;
    let mant = (h & 0x3ff) as f32;
    let mag = if exp == 0 {
        mant * 2f32.powi(-24)
    } else if exp == 0x1f {
        if h & 0x3ff == 0 { f32::INFINITY } else { f32::NAN }
    } else {
        (1.0 + mant / 1024.0) * 2f32.powi(exp - 15)
    };
    if sign == 1 { -mag } else { mag }
}

/// Headless self-test of the DLSS render path: build a context, upscale a constant
/// mid-grey frame (zero motion), and read back the centre of the output. Returns the
/// output's average RGB, so a caller can tell whether DLSS produces a non-black image
/// from valid inputs — isolating "is DLSS broken?" from "is the renderer wiring
/// broken?". Logs progress. Independent of any window.
pub fn run_selftest() -> Result<f32, String> {
    use dlss_wgpu::{create_instance, request_device, DlssPerfQualityMode, FeatureSupport};

    let project_id = uuid::Uuid::from_u128(0x9a1d_5f00_7c3e_4b21_8f6a_2e9c_0d4b_1a37);
    let instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    let mut support = FeatureSupport::default();
    let instance = create_instance(project_id, &instance_desc, &mut support)
        .map_err(|e| format!("create_instance: {e:?}"))?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .map_err(|e| format!("request_adapter: {e:?}"))?;
    // Mirror the in-game device EXACTLY: the same optional feature set + experimental
    // token (testing whether a device feature is what breaks DLSS).
    let rt = adapter.features().contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY);
    let in_game_features = adapter.features()
        & (wgpu::Features::TIMESTAMP_QUERY
            | wgpu::Features::MULTI_DRAW_INDIRECT_COUNT
            | wgpu::Features::EXPERIMENTAL_RAY_QUERY);
    log::info!("selftest: device features = {in_game_features:?}");
    let device_desc = wgpu::DeviceDescriptor {
        label: Some("dlss-selftest"),
        required_features: in_game_features,
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: Default::default(),
        experimental_features: if rt {
            unsafe { wgpu::ExperimentalFeatures::enabled() }
        } else {
            wgpu::ExperimentalFeatures::disabled()
        },
    };
    log::info!("selftest: device requests EXPERIMENTAL_RAY_QUERY = {rt}");
    let (device, queue) = request_device(project_id, &adapter, &device_desc, &mut support, None)
        .map_err(|e| format!("request_device: {e:?}"))?;
    log::info!("selftest: super_resolution_supported = {}", support.super_resolution_supported);
    if !support.super_resolution_supported {
        return Err("super resolution not supported".into());
    }

    let display = [1920u32, 1080u32];
    let mut dlss = Dlss::new(project_id, &device, &queue, display, DlssPerfQualityMode::Auto)
        .map_err(|e| format!("Dlss::new: {e:?}"))?;
    let [rw, rh] = dlss.render_resolution();
    let range = dlss.render_resolution_range();
    log::info!("selftest: display {display:?}, render res [{rw}, {rh}], range {range:?}");

    let tex = |label: &str, w: u32, h: u32, fmt: wgpu::TextureFormat, usage: wgpu::TextureUsages| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: fmt,
            usage,
            view_formats: &[],
        })
    };
    let hdr = wgpu::TextureFormat::Rgba16Float;
    let attach = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
    let color = tex("color", rw, rh, hdr, attach | wgpu::TextureUsages::COPY_SRC);
    let depth = tex("depth", rw, rh, wgpu::TextureFormat::Depth32Float, attach);
    let mv = tex("mv", rw, rh, wgpu::TextureFormat::Rg16Float, attach);
    let output = tex(
        "output",
        display[0],
        display[1],
        hdr,
        attach | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );
    let v = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());

    // Clear color -> mid grey, depth -> far, mv -> zero, so DLSS has valid inputs.
    // Crucially also clear the OUTPUT: NGX needs it in a defined layout/state before
    // it writes, otherwise it produces black (this is the real in-game fix).
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("selftest-clear") });
    {
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear-output"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &v(&output),
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
            multiview_mask: None,
        });
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear-color"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &v(&color),
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.5, g: 0.5, b: 0.5, a: 1.0 }), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
            multiview_mask: None,
        });
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear-mv"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &v(&mv),
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
            multiview_mask: None,
        });
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear-depth"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &v(&depth),
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            occlusion_query_set: None,
            timestamp_writes: None,
            multiview_mask: None,
        });
    }
    queue.submit(Some(enc.finish()));

    // Run DLSS for several frames (temporal upscalers may need a few to converge);
    // reset only on the first. Submit each encoder + DLSS command buffer.
    for frame in 0..32u32 {
        let jitter = dlss.suggested_jitter(frame, [rw, rh]);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("selftest-dlss") });
        let cmd = dlss
            .render(&mut enc, &adapter, &v(&color), &v(&depth), &v(&mv), &v(&output), jitter, frame == 0, [rw as f32, rh as f32])
            .map_err(|e| format!("dlss.render: {e:?}"))?;
        queue.submit([enc.finish(), cmd]);
    }

    // Read back a 64x64 centre patch of an Rgba16Float texture and average its RGB.
    let readback = |tex: &wgpu::Texture, w: u32, h: u32| -> Result<f32, String> {
        const N: u32 = 64;
        let bytes_per_row = ((N * 8 + 255) / 256) * 256;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("selftest-readback"),
            size: (bytes_per_row * N) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("selftest-copy") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: w / 2 - N / 2, y: h / 2 - N / 2, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(N) },
            },
            wgpu::Extent3d { width: N, height: N, depth_or_array_layers: 1 },
        );
        queue.submit(Some(enc.finish()));
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("poll: {e:?}"))?;
        let data = slice.get_mapped_range();
        let mut sum = 0.0f64;
        for row in 0..N {
            let base = (row * bytes_per_row) as usize;
            for px in 0..N {
                let o = base + px as usize * 8;
                let r = f16_to_f32(u16::from_le_bytes([data[o], data[o + 1]]));
                let g = f16_to_f32(u16::from_le_bytes([data[o + 2], data[o + 3]]));
                let b = f16_to_f32(u16::from_le_bytes([data[o + 4], data[o + 5]]));
                sum += (r + g + b) as f64 / 3.0;
            }
        }
        Ok((sum / (N * N) as f64) as f32)
    };

    // Sanity: the input we cleared to 0.5 must read back ~0.5 (proves the readback
    // works). Then the DLSS output.
    let color_avg = readback(&color, rw, rh)?;
    let output_avg = readback(&output, display[0], display[1])?;
    log::info!("selftest: input color avg = {color_avg:.4} (expect ~0.5), output avg = {output_avg:.4}");
    Ok(output_avg)
}
