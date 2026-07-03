# fpsmaster 扩展系统实施计划（两层：JS + Native）

> 一份自洽的执行说明书。目标是给 fpsmaster 加一套 **Forge 级深度** 的扩展系统，分两层：
> 轻量 **JS 层**（门槛低、易分发、不碰 3D/世界渲染）+ 深度 **Native 层**（`cdylib`，可深入改渲染与逻辑）。
> 执行规范（分支命名、commit、验证纪律、worktree 流程、热点文件错峰）沿用 [`PLAN_0_2_0.md`](PLAN_0_2_0.md) 的「一、二」节，不再重复。

---

## 一、目标与已锁定决策

| 项 | 决策 |
|---|---|
| 深度目标 | 对标 Forge：可深入改逻辑、网络、渲染、（最终）新增内容 |
| 安全沙箱 | **不要求**。Native mod 与宿主同进程、全权访问；JS mod 错误可隔离但不防恶意 |
| 发布形态 | 发布版二进制会**混淆**，因此必须对外暴露一套**稳定插件 API**（`fpsmaster_ext_api` crate + 稳定 JS 全局 API） |
| 机制 | 运行时动态加载。**Native = `cdylib` + `abi_stable`**；**JS = `rquickjs`（QuickJS）** |
| 分层职责 | **JS 层不提供 3D/世界渲染**，只提供 HUD 绘制 + 一组**预置渲染修改选项**；**Native 层**可任意深入渲染 |
| 内容范围 | 分阶段：先做客户端行为/外观 mod（对 vanilla 服务器可用，不动 core）；完整新内容（new blocks/items/entities）作为后期里程碑，需 core id 空间重写，且仅单人/自有世界生效 |

### 不可回避的本质约束（执行者必须理解）

1. **深度 = 稳定 API 的广度。** Rust 无 Mixin/字节码注入等价物，混淆后内部符号 mod 够不着。所以本系统主要工作量在「把 hook 点和 API 铺广铺细」，不在选机制。
2. **Rust 无稳定 ABI。** Native 跨边界只能用 `abi_stable` 的 `RString`/`RVec`/`RBox`/`#[sabi_trait]`，禁止直接传 `String`/`Vec`/裸 trait object。
3. **Native 非跨平台。** mod 要按 `OS × arch` 出多份二进制；需提供 CI 模板缓解。JS 一份 `.js` 到处跑。
4. **内容 mod 受协议约束。** fpsmaster 是连 vanilla 1.8.9 服务器的客户端，vanilla 永不下发 mod 内容；新增内容只在 fpsmaster 掌权世界（单人/自有服务器）才成立。

---

## 二、核心架构原则：单一真相源

**内部事件总线 + 命令队列是唯一真相源；JS 层和 Native 层都只是它之上的两层绑定。**

```
                         ┌─────────────────────────────┐
   .js mods ──rquickjs──▶│                             │
                         │   ExtBus  (events out)      │──▶ fpsmaster_app 接缝
 .dylib mods ─abi_stable▶│   CmdQueue (commands in)    │◀── (packet/tick/frame/input)
                         │   ReadViews (world/entity)  │
                         └─────────────────────────────┘
                              ▲ 定义于 fpsmaster_ext_api（稳定契约 / 混淆豁免）
```

好处：
- 机制决策被这一层吸收——先把内部 API 做出来（Phase 0，反正躲不掉），之后绑 JS、绑 Native、还是两者都绑，都是增量。
- JS 与 Native 共享同一套事件/命令语义，不是两套独立 API，维护成本可控。
- `fpsmaster_ext_api` 是这层的**稳定投影**，扮演 Forge 里 MCP/SRG 映射层的角色：混淆只动内部，不动这层。

---

## 三、crate / 模块布局

| crate | 角色 | 是否对外/混淆豁免 |
|---|---|---|
| `fpsmaster_ext_api`（新，开源、独立 semver） | 稳定契约：`abi_stable` 事件/命令/handle 类型 + `#[sabi_trait] NativePlugin` + 版本常量 | **是**，发布给 mod 作者，符号白名单不混淆 |
| `fpsmaster_ext`（新） | 宿主侧扩展管理器：ExtBus + CmdQueue + ReadViews 核心、native loader、JS runtime、manifest/依赖/加载顺序、capability | 否（内部，可混淆） |
| `fpsmaster_app` | 在四个接缝接出事件、排空命令队列、持有 `fpsmaster_ext` manager | 否 |
| `fpsmaster_core`（后期） | 内容 mod 的 id 空间重写 | 否 |

> 依赖方向：`fpsmaster_app → fpsmaster_ext → fpsmaster_ext_api`。`fpsmaster_ext_api` 只依赖 `abi_stable` 与少量纯数据类型，**不依赖** core/render/protocol（避免把内部类型泄进稳定契约——契约里用自己的精简镜像类型）。

---

## 四、内部 API 草图（事件 / 命令 / 读视图）

> 下列签名为**示意**，落地时定稿到 `fpsmaster_ext_api`。命令类型尽量复用现有 `NetworkCommand` / `GuiAction` 的形状。

### 事件（host → mod，hook 点）

| 事件 | 触发点（现有接缝） | 返回 / 语义 |
|---|---|---|
| `on_clientbound_packet(&PacketView)` | main.rs 主线程 `NetworkEvent` 出队点，在 `game.rs::handle_play_packet` **之前** | `Verdict { Pass / Modify(..) / Drop }`，按包类型订阅 |
| `on_serverbound_packet(&PacketView)` | `network.rs` 发包路径 | 同上 |
| `on_tick(&TickCtx)` | `game.rs` tick 之后（20Hz） | 读视图 + 投命令 |
| `on_frame(&FrameCtx)` | 每帧 | 主要给 HUD/动画 |
| `draw_hud(&mut HudDraw)` | UiFrame 装配处 | mod 追加 `UiCommand`（见 §5） |
| `on_input(&InputEvent)` | main.rs 输入路由 | `bool`（是否消费，做自定义键位） |
| `on_block_change` / `on_chunk_load` / `on_chat` / `on_entity_spawn` … | 对应 packet handler 内派生 | 只读通知 |

### 命令（mod → host，每 tick/frame 排空）

```text
SendServerbound(PacketView)      // 注入出站包（高权限）
SetScreen / CloseScreen          // 复用 GuiAction
Chat(String) / Log(level, msg)
SpawnParticle(kind, pos, ..)     // 走现有粒子系统（内置类型）
PlaySound(event, pos)
RegisterTexture(bytes) -> TexHandle      // 加载期
RegisterBlockTint(block, mode)           // 预置渲染修改（见 §5）
RegisterHudElement(spec)                 // 加载期
// Native 专属：
RegisterRenderHook(..) / RegisterEntityRenderer(..) / RegisterBlockModel(..)
```

### 读视图（host functions，不暴露 `&mut GameState`）

```text
player(): { pos, yaw, pitch, health, on_ground, ... }
block_at(x,y,z): BlockStateView
entities(): iterator<EntityView>
world_time(), dimension(), ...
```

---

## 五、JS 层设计（`rquickjs`）

**定位**：行为/自动化/HUD/配置/网络通道 mod。门槛最低、一份 `.js` 跨平台、热重载。

**可见能力**：
- 事件订阅：`onTick`, `onPacket(type, cb)`, `onChat`, `onKey`, `onBlockChange` …
- 命令：`sendChat`, `sendPacket(限白名单类型)`, `log`, `spawnParticle`（内置类型）, `playSound`
- 读视图：`player()`, `blockAt(x,y,z)`, `entities()`, `worldTime()` …
- **HUD 绘制（允许）**：`hud.rect/text/image/itemIcon(...)` —— 直接映射到现有 `UiCommand`（命令式、每帧、廉价）。这是 JS 的主要可视能力，**不算 3D/世界渲染**。

**渲染红线（按你的要求）**：JS **不**提供任意 3D/世界渲染、不提供着色器/几何提交。只提供一组**预置渲染修改选项**——一个**固定、枚举化、宿主实现**的开关/参数集合，例如：

| 预置项 | 形态 |
|---|---|
| `setBlockTint(blockId, color \| namedMode)` | 静态 tint（注册期，meshing 原生读表，**不逐方块回调 JS**） |
| `fullbright(on)` / `setFog(mode)` | 切换宿主内置光照/雾效 |
| `blockOutline(on)` / `chunkBorders(on)` | 宿主内置叠加 |
| `entityBox(filter, color)` | 宿主内置实体框（ESP 式） |
| `nametagScale(...)` / `particleDensity(...)` | 调宿主内置参数 |
| `spawnParticle(builtinKind, ...)` | 发射内置粒子 |

> 该清单是封闭集合，新增预置项要改宿主代码——这是 JS 层「可控渲染」与 Native 层「任意渲染」的分界线。

**引擎**：`rquickjs`（体积小、易嵌入、ES2020 够用，不背 V8 几十 MB）。错误按 mod 隔离：单个 mod 抛异常只禁用该 mod，不拖垮宿主。

---

## 六、Native 层设计（`abi_stable` + `cdylib`）

**定位**：需要深度/性能/任意渲染/（后期）新内容的少数 mod。逃生口。

- 契约：`fpsmaster_ext_api` 用 `#[export_root_module]` + `RootModule`（prefix type，便于向后兼容地加字段）暴露入口；`#[sabi_trait] trait NativePlugin` 定义生命周期 + 全部 hook。
- 加载：`fpsmaster_ext` 扫描 `mods/*.{dylib,so,dll}`，`abi_stable` **运行时校验类型布局 + 版本**，不匹配直接拒绝加载（挡住混淆/版本错配 UB）。
- 能力：JS 能做的全都能做，**外加**：
  - 任意 HUD/世界渲染 hook（拿真实 renderer 资源，自定义几何提交、自定义实体 renderer、TESR 式 block-entity 渲染）；
  - block model / 着色器槽覆盖（超出 JS 预置集）；
  - 直接读真实状态视图（仍走 API，不给 `&mut GameState`，保内部可重构）。
- **混淆集成**：把 `abi_stable` 的 root-module 导出符号 + `fpsmaster_ext_api` 的 `#[repr(C)]`/`StableAbi` 布局加入混淆白名单，内部其余符号随便混淆。验证：用混淆后的 release 加载一个示例 native mod。

---

## 七、mod 清单 / 加载 / 依赖 / capability

`mod.toml`（每个 mod 一份）：

```toml
id = "coords_hud"
version = "1.0.0"
tier = "js"              # "js" | "native"
api = "^0.1"             # 依赖的 fpsmaster_ext_api / JS API semver
entry = "main.js"        # 或 native: "libcoords.dylib"
depends = []             # 其它 mod id + 版本范围
capabilities = ["hud", "read_world"]   # 声明所用能力，安装时给用户确认
```

- **加载顺序**：按 `depends` 拓扑排序；环依赖报错。
- **capability**：虽无沙箱，仍做「声明 + 用户确认」以建立信任分级。敏感能力（`inject_packet`、`read_player_pos`）要显式授权；纯 `hud` mod 默认放行。
- **版本化**：`fpsmaster_ext_api` 走 semver；JS API 一个全局版本号。host 拒绝 `api` 不兼容的 mod。

---

## 八、分阶段计划

> 分支族 `feat/ext-*`。建议这是 **0.2.0 之后的新里程碑**（目标版本 0.3.0，待确认，见 §十）。各阶段 merge 回 main 前跑全量 `cargo build && cargo test && cargo clippy --workspace`。

### Phase 0 — 内部事件总线 + 命令队列 + 拆 `game.rs`（无悔基座，不碰 JS/Native）

- 在 `fpsmaster_ext` 建 `ExtBus` / `CmdQueue` / `ReadViews` 核心 + 一个内部 `trait HostHooks`（即未来 `NativePlugin` 的内部原型）。
- 在四个接缝接出事件 / 排空命令：
  - 触及：`main.rs`（`NetworkEvent` 出队点接 `on_clientbound_packet`；输入路由接 `on_input`；UiFrame 装配处接 `draw_hud`）。
  - 触及：`game.rs`（`handle_play_packet` 巨型 `match` 内派生 block/chunk/chat/entity 事件；tick 后接 `on_tick`；tick 末排空命令）。顺手把 packet handler 拆成离散函数，**给 6500 行的 `game.rs` 减肥**。
  - 触及：`network.rs`（发包路径接 `on_serverbound_packet`；注入复用 `NetworkCommand`）。
- 用一个 in-tree **假 mod**（Rust 实现 `HostHooks`）验证四面打通。
- **验证**：`cargo test` 通过；运行客户端，假 mod 能 ① 在 HUD 画一行文本 ② 拦截并 log 指定 clientbound 包 ③ 发一条 chat 命令 ④ 拦一个键位。对照接入前**无行为回归**（连本地 `local_server/paper-1.8-protocol47` 跑 headless smoke）。
- 风险：`game.rs` 拆分是最大改动面，按热点文件约定让它做单一改动者。

### Phase 1 — JS 层（`rquickjs`）：行为 / HUD / 自动化

- `fpsmaster_ext` 接 `rquickjs`，从 `mods/` + `mod.toml` 加载 `.js`。
- 暴露 §5 的 JS API（事件订阅、命令、读视图、HUD 绘制、预置渲染选项注册）。错误按 mod 隔离 + 热重载。
- capability + manifest 解析 + 加载顺序。
- **验证**：随仓库附 3 个示例 mod 并演示运行时加载 + 热重载：① 坐标/朝向 HUD ② 关键词聊天高亮 ③ `setBlockTint` 预置渲染。`cargo test` 覆盖 manifest 解析与命令排空。
- 风险：高频包（EntityMove）喂给 JS 的成本——默认不订阅，按需 opt-in。

### Phase 2 — Native 层（`abi_stable`）+ 稳定 API crate + 混淆集成

- 定稿 `fpsmaster_ext_api`：`#[sabi_trait] NativePlugin` + `abi_stable` 类型 + 版本常量 + root module。
- `fpsmaster_ext` native loader：扫描 → 布局/版本校验 → 实例化 → 注册到 ExtBus。把内部 `HostHooks` 适配为 `NativePlugin`。
- 暴露完整内部 API（含深度读视图）。
- **验证**：建一个示例 native mod crate，编译出 `cdylib`，加载进客户端确认能拦包 + 跑 tick 逻辑；构造一个 `api` 版本不匹配的 mod，确认 `abi_stable` **拒绝加载**；用混淆后的 release 加载示例 native mod 成功。
- 风险：`abi_stable` 类型替换 std 类型的样板量；混淆白名单需与构建脚本对齐。

### Phase 3 — Native 渲染 hook（深度渲染，仅 Native）

- 加 native-only 渲染 hook：自定义 HUD/世界几何提交（拿真实 renderer 资源）、自定义实体 renderer、block model/着色器槽覆盖（超出 JS 预置集）。
- 触及：`fpsmaster_render`（`renderer.rs` 加 hook 注入点 + 资源出借接口）、`fpsmaster_ext_api`（渲染 hook trait）。
- JS 层保持在预置渲染集合，不动。
- **验证**：示例 native mod 在世界里画自定义 3D 几何 / 覆盖某方块渲染，肉眼确认（标注「待用户目视确认」）。
- 风险：render 是 present/occlusion-bound 热路径（见 render 笔记），hook 必须批处理、不得逐方块回调。

### Phase 4 — 内容 mod（`fpsmaster_core` id 空间重写，仅自有世界）

- 抽象 `BlockState` id 空间（脱离固定 1.8.9 ≤197 范围）、把 `luminance` 等 `match id` 硬编码迁进数据注册表；为 items/entities 建注册表。
- 仅在单人/自有世界生效；明确与协议边界的关系。
- 触及：`fpsmaster_core/{block,blocks,...}.rs`、协议边界转换。
- **验证**：mod 在单人世界注册一个新方块并能放置/渲染。
- 风险：最大工程，独立里程碑，前置是「fpsmaster 掌权世界」能力就绪。

---

## 九、横切关注点

- **线程模型**：所有 mod 代码只跑在主线程。包拦截在主线程 `NetworkEvent` 出队点做（包本就经 mpsc 流到主线程），避开 `rquickjs`/`abi_stable` 跨线程问题。注入 = push `NetworkCommand`。
- **性能预算**：HUD/frame hook 每帧一次（μs 级，无压力）；tick hook 20Hz/50ms 预算充裕；packet hook 仅订阅类型；**meshing 逐方块绝不回调 mod**——所有方块级修改走注册表，meshing 原生读合并表。
- **错误隔离**：JS 异常按 mod 捕获并禁用该 mod；Native 无沙箱，崩溃即宿主崩溃（文档明示，capability 标注高危）。
- **版本化**：`fpsmaster_ext_api` semver + `abi_stable` 运行时布局校验双保险；JS API 全局版本号。

---

## 十、待用户确认

1. **目标版本 / 分支**：扩展系统是否作为 `0.3.0` 新里程碑、分支族 `feat/ext-*`？（当前在 `feat/v0.2.0`，建议先收完 0.2.0 再起。）
2. **JS 引擎**：确认 `rquickjs`（vs V8/`deno_core` 重但快、vs `boa` 纯 Rust 慢）。
3. **预置渲染选项清单**（§5 表）：是否就是你要的初始集合，还要加哪些。
4. **Native 分发**：是否需要项目提供跨平台 CI 模板帮 mod 作者出多平台二进制。
5. **mods 目录位置**与 `mod.toml` 字段是否照此定。
