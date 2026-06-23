# DLSS 集成指南（NVIDIA Super Resolution）

DLSS 是时域上采样器：低分辨率渲染 + 抖动，用运动矢量把历史帧重投影、由 NVIDIA
训练的网络重建出高分辨率。它和我们已有的 **FSR2 时域放大共用同一套输入**
（抖动低分辨率颜色 + 深度 + 运动矢量 + 各分辨率），所以输入管线**已经就绪**，剩下
的主要是平台/设备层的接线。

> **平台限制（硬性）**：DLSS 只能在 **NVIDIA RTX GPU + Vulkan 后端 + Windows/Linux**
> 上编译和运行。macOS（Metal、无 NVIDIA）既编译不了也跑不了。因此这份工作**必须在
> Windows（或 Linux）机器上做**。默认构建（含 macOS）完全不碰 DLSS。

---

## 1. 现状（已在仓库里）

- **feature gate**：`recraft_render` 的可选依赖 `dlss_wgpu = { version = "4", optional = true }`
  + feature `dlss = ["dep:dlss_wgpu"]`。默认关闭 → 默认构建不编译它，跨平台不受影响。
- **骨架模块**：[`crates/recraft_render/src/dlss.rs`](../crates/recraft_render/src/dlss.rs)
  （`#[cfg(feature = "dlss")]`），封装 `dlss_wgpu` 4.x：`Dlss::new` / `render_resolution`
  / `suggested_jitter` / `suggested_mip_bias` / `render`。**这个骨架在 macOS 上没法编译
  验证，到 Windows 上预计要修少量签名**（已在代码里用 `VERIFY` 注释标出）。
- **共享输入**（做 FSR2 时建好的，DLSS 直接复用）：
  - 抖动投影（`renderer/mod.rs` 的 jitter）；
  - 渲染分辨率离屏颜色 + 深度（`render_scale<1` 时低分辨率）；
  - RG16F 运动矢量（相机 + 实体），见 [`shader/motion_vector.wgsl`](../crates/recraft_render/src/shader/motion_vector.wgsl)、`model_velocity.wgsl`；
  - 全分辨率输出目标（现在是 TAA 的 `taa_resolved`）。
- **UI 占位**：Performance 界面已有 `DLSS` 开关，持久化到 `settings.dlss`（目前不接渲染）。

---

## 2. 一次性环境搭建（Windows）

1. **NVIDIA DLSS SDK**：从 NVIDIA 开发者站下载 **DLSS Super Resolution SDK v310.5.3**
   （`dlss_wgpu` 4.0.0 对应这个版本），clone/解压到本地。
2. 设环境变量 **`DLSS_SDK`** 指向 SDK 根目录（绝对路径）。
3. 安装 **Vulkan SDK**，设 **`VULKAN_SDK`** 环境变量。
4. 安装 **clang**（`dlss_wgpu` 的 `build.rs` 用 bindgen）。
5. 一个 **RTX** 显卡 + 最新驱动。

> 这些只在 `--features dlss` 编译时需要。没有它们的机器照常 `cargo build`。

---

## 3. 编译运行

```bash
cargo run -p recraft_app --features recraft_render/dlss
```

（若给 `recraft_app` 也加一个透传 feature 会更顺手，见第 6 节。）

---

## 4. 还需接的线（到 Windows 上做）

骨架封装了 DLSS 调用本身；下面是把它接进渲染器的工作，按重要性排序。

### 4.1 设备创建（最关键、最容易卡住）

DLSS 需要在 **Vulkan 实例/设备创建时注册特定扩展**，普通 `wgpu` 默认设备缺这些扩展，
`DlssSdk::new` 会失败。`dlss_wgpu` 为此提供了帮助函数：

```rust
use dlss_wgpu::{create_instance, register_instance_extensions,
                register_device_extensions, request_device, FeatureSupport};
```

`Renderer::new`（`renderer/mod.rs`）目前用标准的 `instance.request_adapter` +
`adapter.request_device`。DLSS 开启时要改成：

1. 用 `create_instance(...)` 建 wgpu instance（强制 **Vulkan** 后端）。
2. `register_instance_extensions` / `register_device_extensions` 把 DLSS 要的 Vulkan
   扩展登记进去。
3. 用 `request_device(...)` 拿到带扩展的 `Device`/`Queue`。

> 建议做成一个 DLSS-aware 的设备初始化分支（`#[cfg(feature="dlss")]` + 运行时按
> `settings.dlss` 选择）。非 DLSS 路径保持现状。这是唯一动到设备初始化的地方。

### 4.2 渲染分辨率由 DLSS 决定

不要用 FSR 的手动预设比例，改用 DLSS 给的：

```rust
let [rw, rh] = dlss.render_resolution();  // 按 quality mode + 输出分辨率算出
renderer.set_render_scale(rw as f32 / display_w as f32);
```

### 4.3 抖动 + mip bias 用 DLSS 建议值

我们已有 jitter 基础设施（`renderer/mod.rs` 的 `jitter_offset`）。DLSS 开启时，把
Halton 换成 DLSS 训练匹配的序列：

```rust
let jitter = dlss.suggested_jitter(frame_number, [rw, rh]);  // 像素单位
// 应用到投影（和现有 jitter 一样：NDC 平移 = jitter * 2 / 渲染分辨率）
let mip_bias = dlss.suggested_mip_bias([rw, rh]);  // 设到世界采样器 lod_bias
```

### 4.4 每帧调用 DLSS（取代 TAA resolve）

DLSS 开启时，**跳过 TAA resolve pass**，改为：

```rust
let cmd = dlss.render(
    &mut encoder,
    &adapter,
    color_view,          // 低分辨率抖动 HDR 场景（offscreen `color`）
    depth_view,          // 低分辨率深度
    mv_view,             // RG16F 运动矢量
    dlss_output_view,    // 全分辨率输出（复用 taa_resolved 那张）
    jitter,              // 本帧抖动（像素）
    reset,               // 相机切换/传送时 true，丢弃历史
)?;
queue.submit([encoder.finish(), cmd]);  // DLSS 返回独立 CommandBuffer，一起提交
```

之后 post/bloom/曝光读 `dlss_output`（和现在读 `taa_resolved` 一样，已是显示分辨率）。

### 4.5 运动矢量约定（务必在 Windows 上核对）

我们的运动矢量是 `cur_uv - prev_uv`（**UV 空间**，见 `motion_vector.wgsl`）。DLSS 期望
**渲染目标像素空间、指向上一帧**的矢量。两条路二选一：

- 用 `DlssSuperResolutionRenderParameters.motion_vector_scale = Some([rw, rh])` 把 UV
  缩放成像素；或
- 直接在 MV shader 里输出像素空间矢量（DLSS 专用变体）。

**符号**：若放大后画面"反向拖影"，把矢量取反（DLSS 的 current→previous 约定可能和我们
相反）。这是最需要实测调的一点。

### 4.6 接 `settings.dlss` 开关

`settings.dlss` 已持久化。要做的：
- DLSS 开 → 走 4.1 的 Vulkan 设备 + 4.2~4.4 的 DLSS 路径，并互斥关掉 FSR/TAA（三者都是
  时域上采样器，同时开会打架）。
- DLSS 不可用（非 RTX / 非 Vulkan / 未编译该 feature）→ 灰掉开关或回退到 **FSR2**（我们
  已有，跨平台兜底）。
- 把 `GuiAction::SetDlss` 接到 renderer（目前 Performance 界面只持久化，不触发渲染）。

---

## 5. 与现有管线的关系

```
低分辨率渲染(jitter) ──► 颜色/深度/运动矢量(渲染分辨率)
                                  │
                  ┌───────────────┼────────────────┐
                  ▼               ▼                ▼
              TAA resolve     FSR2(=TAA+         DLSS.render()
              (render=1)      render_scale<1)   (RTX/Vulkan)
                  └───────────────┴────────────────┘
                                  ▼
                       全分辨率输出 ──► post/bloom/曝光 ──► swapchain
```

三者是**同一套输入、不同的上采样器**，互斥选择。DLSS 只是把 TAA/FSR2 的 resolve 换成
NVIDIA 的网络；输入管线完全复用。

---

## 6. 坑与注意事项

- **线程安全**：`dlss_wgpu` 用 `Arc<Mutex<DlssSdk>>`。若 recraft 以后做并行帧/录制，注意
  DLSS 调用要串行化（SDK 非线程安全）。
- **DLL 分发**：发布 Windows 构建时随包带 `nvngx_dlss.dll`（及 ray reconstruction 用到的
  额外 DLL）+ DLSS 编程指南 9.5 节的版权/许可文本。`DLSS_SDK` 环境变量**运行时不需要**。
- **`reset` 标志**：相机瞬移、维度切换、重生时置 true，否则历史会从错误位置重投影 → 拖影。
- **NGX project id**：`DlssSdk::new` 要一个 NVIDIA NGX 工程 UUID。开发期可用 SDK 示例里的
  占位 id；正式发布要按 NVIDIA 流程申请。
- **可选**：`dlss_wgpu` 的 `debug_overlay` feature 在设了 `DLSS_SDK` 时链开发版 DLL，方便调试。
- **`recraft_app` 透传 feature**（可选，省得写长命令）：在 `recraft_app/Cargo.toml` 加
  `[features] dlss = ["recraft_render/dlss"]`，就能 `cargo run -p recraft_app --features dlss`。

---

## 7. 验收

- 非 RTX / 默认构建：完全不受影响（DLSS 不编译）。
- RTX + `--features dlss`：DLSS 开 → 世界在更低分辨率渲染（FPS 升）、画面经 DLSS 重建到全
  分辨率、移动稳定无拖影；关 → 回退 FSR2/原生。
- 运动矢量正确性：开 DLSS 走动/转视角不应有反向拖影或抖动（不对就调 4.5 的 scale/sign）。
