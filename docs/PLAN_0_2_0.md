# recraft 0.2.0 开发计划

> 本文档是一份**自洽的执行说明书**：把当前零散的 28 个目标整理成按范围/大小分组、按依赖排期的开发计划。
> 执行者（人或 agent）应从「一、执行规范」读起，然后按 wave 顺序推进。

---

## 一、执行规范

1. **总纲**：按 wave（阶段）顺序推进。wave **内部**可并行的任务用 `git worktree` 并行开发，各自完成并自测后**统一 merge 回 `main`**，再进入下一 wave。一次性执行完成，不中途停下征求意见——除非命中下面第 7 条的「停下」判据。
2. **分支命名**：遵循 conventional，格式 `<type>/<scope>-<desc>`，例如 `feat/particle-system`、`fix/chest-block-render`。`type ∈ {feat, fix, perf, refactor, chore, docs}`。
3. **commit message**：遵循 conventional commits：`<type>(<scope>): <subject>`（subject 用英文、祈使句、不超过 ~72 字）。按 harness 规则在 commit 结尾追加：
   `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
4. **版本号节奏**：四个 crate（`recraft_app/core/protocol/render`）版本**同步**。起点 `0.1.0`。
   - 每个 wave 全部 merge 完成后 bump 一个 patch：`0.1.1 → 0.1.2 → 0.1.3 → 0.1.4`。
   - **所有任务结束后**统一 bump 到 `0.2.0`，commit `chore(release): 0.2.0` 并打 tag `v0.2.0`。
5. **对照基准（oracle）**：
   - `references/MCP-919/` — 反编译的 1.8.9 源码，**逻辑与数值的权威**（碰撞箱、粒子物理、模型尺寸、音效衰减等都以此为准）。
   - `references/minecraft-data/` — 方块/物品/音效数据表。
   - `references/Leafish/`、`references/minecrust/` — 同类 Rust 客户端的实现参考。
   - `local_server/paper-1.8-protocol47/` — **offline 模式** Paper 1.8.8，多人特性本地可测，**无需正版账号**。
6. **验证要求**：每个任务都给出可验证的成功标准。
   - 能本地验证的（`cargo build/test/clippy`、headless smoke、连本地服、对照 MCP-919 数值）**必须验证后才算完成**。
   - 只能靠**肉眼像素级对照原版**的部分，明确标注「待用户目视确认」，**不谎报完成**。
7. **遇到无法完成 / 需要用户的，停下**，登记到「五、需要用户协助 / 待确认」，**不死钻牛角尖**：
   - 判据：若用户能轻易提供就能解决（例：某任务需要在某服务器观察表现而缺正版账号、需要确认依赖选型、需要一份外部资源），**停下并写清需要什么**，不要在用户机器上大范围乱搜、不要去网上找公开账号。
   - 对自己评估为「无法完成」或「不完全理解需求」的任务，同样不强行做，留在待办里说明原因。
8. **改动纪律**：surgical changes，匹配既有风格，不顺手重构无关代码；改动产生的未用 import/变量要清理。

---

## 二、git worktree 协作流程

```bash
# 为一个并行任务建立独立工作区
git worktree add ../recraft-<branch> -b <branch>
# 在该 worktree 内开发、自测（cargo test/clippy/build）

# wave 收尾：依次合并回 main
git switch main
git merge --no-ff <branch>          # 或 rebase 后 ff，按冲突情况选
# 全部合并后跑全量验证
cargo build && cargo test && cargo clippy --workspace
# bump patch 版本，提交，清理
git worktree remove ../recraft-<branch>
```

**热点文件（hot files）** —— 多任务都会动，安排并行时需让同一 wave 内的并行分支**尽量错开**它们，否则串行：

| 文件 | 谁会动 |
|------|--------|
| `recraft_render/src/renderer.rs` (6k 行) | 几乎所有渲染任务（新增 pass、绑定） |
| `recraft_app/src/game.rs` (5k 行) | 几乎所有 app 层任务（事件分发、装配） |
| `recraft_render/src/chunk_mesh.rs` (2k) | 所有方块渲染任务 |
| `recraft_render/src/model.rs` (1.6k) | 所有实体/模型任务 |
| `recraft_render/src/texture.rs` (2k) | 新增贴图槽位的任务 |
| `recraft_app/src/settings.rs` / `gui/options.rs` | FPS 开关、按键映射、OldAnimations 开关 |
| `recraft_core/src/{collision,physics,blocks}.rs` | 碰撞、流体、掉落物物理 |

> 约定：每个 wave 内，把「重写 `renderer.rs`/`game.rs` 大段」的任务尽量**安排为该文件的单一改动者**，其余并行分支只动各自的新模块/数据文件。冲突小的任务才真正并行。

---

## 三、任务总览（28 项，按规模与子系统分类）

规模：**S** 小（局部 bug 修复 / 单点功能）· **M** 中（一个完整特性）· **L** 大（新子系统 / 全量对照）。

| Wave | ID | 任务 | 规模 | 分支 | 依赖 |
|---|----|------|----|------|------|
| 1 | T10 | 粒子渲染系统（全类型 + 性能优化） | L | `feat/particle-system` | — |
| 1 | T13 | 音效系统（全音效 + 立体声定位） | L | `feat/sound-system` | — |
| 1 | T19 | 全碰撞箱数值/精度对照原版 | L | `fix/collision-parity` | — |
| 2 | T15 | 方块破坏纹理顶/底面缺失 bug | S | `fix/block-break-overlay` | — |
| 2 | T16 | 箱子方块渲染修复 | S | `fix/chest-block-render` | — |
| 2 | T25 | 火把贴墙渲染 + 同类附着方块 | M | `fix/torch-attachment` | — |
| 2 | T12 | 动态火焰方块 | M | `feat/animated-fire-block` | T10 |
| 2 | T3  | 特殊方块渲染（告示牌/书架/附魔台/两种传送门） | L | `feat/special-block-render` | — |
| 2 | T24 | 流体渲染 + 流动不完整方块 + 碰撞 | L | `feat/fluid-render` | T19 |
| 2 | T27 | 音符盒功能 + 音符粒子 | M | `feat/note-block` | T10, T13 |
| 2 | T4  | 1.8.9 全方块渲染对照补全（审计） | L | `feat/block-render-parity` | T3,T12,T16,T24,T25 |
| 3 | T18 | 实体模型 bug 修复 + 全实体对照还原 | L | `fix/entity-models` | — |
| 3 | T5  | 实体受伤颜色/动画对齐原版 | M | `feat/entity-hurt-flash` | — |
| 3 | T9  | 投掷物渲染（雪球/末影珍珠/末影之眼/箭等） | M | `feat/projectile-render` | — |
| 3 | T20 | 下落方块实体渲染 | S | `feat/falling-block-entity` | — |
| 3 | T26 | 经验球渲染 | S | `feat/xp-orb-render` | — |
| 3 | T17 | 箱子打开动画（普通/大/末影/陷阱） | M | `feat/chest-open-animation` | T16 |
| 3 | T7  | 掉落物立即物理模拟（消除空中卡顿） | M | `fix/dropped-item-physics` | — |
| 3 | T2  | 附魔光效 glint（手持/物品栏/盔甲） | M | `feat/enchant-glint` | T18(盔甲层) |
| 3 | T11 | 着火屏幕贴图动态化 | S | `fix/fire-overlay-anim` | — |
| 4 | T6  | 移除常驻调试 HUD + 设置项 FPS 开关 | S | `feat/fps-toggle` | — |
| 4 | T1  | 物品栏左上角玩家预览模型 | M | `feat/inventory-player-preview` | — |
| 4 | T14 | boss 血条 | M | `feat/boss-bar` | — |
| 4 | T8  | 自定义按键映射（全部按键） | M | `feat/custom-keybinds` | — |
| 4 | T21 | 聊天 tab 补全发包 | M | `feat/chat-tab-complete` | — |
| 4 | T22 | 聊天可点击文字组件 + 发包 | M | `feat/chat-components` | — |
| 4 | T23 | OldAnimations（1.7/1.8 受击/鱼竿/格挡动画） | M | `feat/old-animations` | — |
| 4 | T28 | 附魔台 GUI + 全部原版 GUI | L | `feat/vanilla-guis` | T1 |
| 5 | —  | 全量验证 + 发布 0.2.0 | — | `chore/release-0.2.0` | 全部 |

---

## 四、分阶段详细计划

> 每个任务给：**触及文件 · 计划步骤 · 验证标准**。
> 标 ⚠️ 的为「逻辑可本地验证，但像素级一致需用户目视确认」。

### Wave 1 — 渲染/物理基础设施 → 合并后 `0.1.1`

这三项跨不同 crate，文件域基本不相交，**可三路并行**。是后续多个任务的地基（粒子、音效被 T12/T27 复用；碰撞被 T24/T7 复用），所以放最前。

**T10 粒子渲染系统** `feat/particle-system` · L
- 触及：新增 `recraft_render/src/particle.rs`；`renderer.rs`（新增一个粒子 pass）；`game.rs`（粒子生成 / tick）；贴图 `assets/minecraft/textures/particle/particles.png`。
- 步骤：
  1. 对照 MCP-919 `EnumParticleTypes` 列出全部 ~40 种粒子，及各自 `EntityFX` 子类的物理（重力、初速、碰撞、贴图帧/UV、颜色、生命周期）。
  2. 设计**实例化（instanced）billboard 渲染**：一个固定容量的粒子池，每帧只更新实例 buffer（位置/UV/颜色/大小），不重建顶点——这是相对原版 per-particle 的性能优化点。
  3. CPU 端 tick 粒子物理（保持与原版数值一致），渲染面向相机的四边形，采样 particles 图集。
  4. 接入触发源：`S2A SpawnParticle` 包、方块破坏/落地/脚步、暴击等。
- 验证：`cargo test` 覆盖粒子物理数值与 MCP 对齐；本地服触发各类粒子目视 ⚠️；性能上确认万级粒子不重建顶点缓冲。

**T13 音效系统** `feat/sound-system` · L
- 触及：新增 `recraft_app/src/sound.rs`（或新 crate `recraft_audio`）；`game.rs`/`network.rs`（事件接入）；`Cargo.toml`（新增音频后端依赖）；扩展 `scripts/setup_minecraft_1_8_9_assets.py`。
- 步骤：
  1. **新增音频依赖**：使用 `kira`（基于 `cpal`，已确认）。用其 spatial 场景做 emitter/listener 定位与距离衰减，mixer 音轨对应原版音效分类，playback rate 做变调（音符盒/随机音高），tween 做音乐淡入淡出；OGG 解码经 symphonia（开 feature）。
  2. **下载缺失的音频资源**：`local_assets` 目前 0 个 `.ogg`。扩展 setup 脚本，按 Mojang **asset index**（`resources.download.minecraft.net`，公开、无需登录）下载 1.8.9 `sounds/` 与 `sounds.json`。
  3. 建立 `sound name → ogg + 子事件/音高/音量` 映射（对照 vanilla `sounds.json` / `references/minecraft-data`）。
  4. 立体声定位：监听者锚定相机，按坐标计算声相与衰减（对照原版 16 格线性 rolloff）。
  5. 接入 `S29 SoundEffect`/`S28 NamedSoundEffect` 包 + 本地事件（放/挖方块、脚步、受击、UI 点击）。
- 验证：在已知坐标播放已知音效，左右声相/距离衰减随坐标变化正确；连本地服触发游戏音效 ⚠️。

**T19 全碰撞箱对照** `fix/collision-parity` · L
- 触及：`recraft_core/src/{collision.rs, physics.rs, blocks.rs}`；`entity.rs`（`entity_size`）。
- 步骤：
  1. 逐方块对照 MCP-919 `Block.setBlockBounds` / 各 `BlockXxx`，校正 `blocks.rs` 中所有非整方块的碰撞盒（楼梯/台阶/栅栏/墙/活板门/床/蛋糕/酿造台/铁砧/漏斗/花盆/火把/拌线钩等）。
  2. 逐实体对照 `Entity`/各 `EntityXxx` 的宽高，修掉 `entity_size()` 对所有 mob 返回固定 `0.3×1.9` 的问题（见 `ENTITY_RENDERING.md` 已知限制）。
  3. 玩家碰撞用 Grim 一致性靶场（见 memory `grim-conformance-test-harness`）回归。
  4. 保持 `f64` 精度路径与原版 `addCoord`/`calculate*Offset` 一致。
- 验证：单测断言每个方块/实体盒的精确数值取自 MCP 源码；Grim 不再因碰撞误判；`cargo test -p recraft_core`。

---

### Wave 2 — 方块渲染 → 合并后 `0.1.2`

并行策略：`renderer.rs`/`chunk_mesh.rs` 是热点。把 **T3（special-block，需新增 block-entity 渲染钩子）** 和 **T24（流体，重写流体网格）** 作为 chunk_mesh 的主要改动者，分两批；小修复 T15/T16/T25 各自文件域小，可与之错开并行。**T4 是审计收尾，依赖前面落地，放本 wave 最后。**

**T15 破坏纹理顶/底面缺失** `fix/block-break-overlay` · S
- 触及：`chunk_mesh.rs`（break overlay 发射，注释已提到 `(0,1)` full-bright 的 break overlay）、`renderer.rs`。
- 步骤：定位破坏覆盖层只发了侧面 4 个 face 的逻辑，补全顶/底 face（destroy_stage_0..9 贴图覆盖全部 6 面）。
- 验证：挖任意方块，6 个面都显示裂纹 ⚠️。

**T16 箱子方块渲染修复** `fix/chest-block-render` · S
- 触及：`model.rs`（箱子作为 block-entity 模型）、`chunk_mesh.rs`（箱子不走地形图集 cube）、`texture.rs`（`entity/chest/*.png` 槽位）。
- 步骤：原版箱子是 TileEntity（模型 + 专属贴图），不能当普通 cube 贴地形图集。改为用箱子模型 + `entity/chest/normal.png` 渲染。本任务先把**静态**箱子渲染对，开动画留给 T17。
- 验证：单/双/末影/陷阱箱外观正确 ⚠️；与 T17 共用模型，注意接口对齐。

**T25 火把贴墙 + 同类附着方块** `fix/torch-attachment` · M
- 触及：`blocks.rs`（torch/lever/redstone-torch 的 meta→朝向）、`chunk_mesh.rs`（附着几何）。
- 步骤：对照 `BlockTorch`，按 meta 渲染地插/四向贴墙的火把几何；顺带检查同样靠 meta 附着的方块（拉杆、红石火把、按钮、绊线钩、梯子已有则核对）。
- 验证：火把贴在各朝向墙面位置正确，无碰撞 ⚠️。

**T12 动态火焰方块** `feat/animated-fire-block`（依赖 T10）· M
- 触及：`blocks.rs`/`block.rs`（新增 fire 渲染形状）、`chunk_mesh.rs`、`texture.rs`（`fire_layer_0/1` 动画帧）。
- 步骤：对照 `BlockFire`/`BlockFireRenderer`，火是交叉面 + 贴墙面的动画方块；实现帧动画 UV；可选叠加火焰粒子（复用 T10）与发光。
- 验证：火焰随时间播放帧动画、形状随邻接方块变化 ⚠️。

**T3 特殊方块渲染** `feat/special-block-render` · L
- 触及：`model.rs`/`renderer.rs`（**新增 block-entity 渲染层**）、`chunk_mesh.rs`、`texture.rs`、可能新增 shader（末地传送门星空）。
- 步骤（按难度拆解，可在分支内分多 commit）：
  1. 书架：纯贴图 cube（最简单，先做）。
  2. 附魔台：底座是非整高方块 + 漂浮的书 block-entity（`TileEntityEnchantmentTable` 书本翻页动画）。
  3. 告示牌：block-entity，渲染木牌模型 + 牌面文字（接 `S33 UpdateSign` 文本）。
  4. 地狱传送门：紫色半透明动画平面（`fire`/portal 帧）嵌在黑曜石框内。
  5. 末地传送门：黑底星空 shader 平面（`RenderEndPortal` 的多层深度星空），性能可简化但观感对齐。
- 验证：逐方块目视对照 ⚠️；告示牌文字本地服可测。

**T24 流体渲染 + 流动方块 + 碰撞** `feat/fluid-render`（依赖 T19）· L
- 触及：`chunk_mesh.rs`（流体网格）、`blocks.rs`/`block.rs`（流体高度/碰撞）、`physics.rs`（液体物理已有则核对）。
- 步骤：
  1. 对照 `BlockLiquid`/`BlockDynamicLiquid`/`BlockFluidRenderer`：流体有 8 级 meta 高度，顶面按四角高度插值成斜面，流向决定 UV 旋转。
  2. 渲染「流出去产生的不完整方块」（当前只有满方块），按 meta 还原各高度。
  3. 碰撞：液体无固体碰撞但参与浮力/减速（physics 已有 water/lava 路径，核对其与流体方块判定一致）。
- 验证：水流出后形成阶梯状斜面、流向 UV 正确 ⚠️；流体高度数学对齐 MCP（单测）。

**T27 音符盒功能 + 音符粒子** `feat/note-block`（依赖 T10, T13）· M
- 触及：`game.rs`/`network.rs`（`S24 BlockAction` for note block）、复用 T10 粒子、T13 音效。
- 步骤：音符盒本体已是普通 cube。处理 BlockAction：按下方方块决定音色、meta 决定音高，播放音符音效（T13）+ 在方块上方生成 note 粒子（T10），粒子颜色随音高。
- 验证：连本地服右键音符盒，听到音 + 看到对应颜色音符粒子 ⚠️。

**T4 1.8.9 全方块渲染对照补全（审计）** `feat/block-render-parity` · L
- 触及：`blocks.rs`、`block.rs`、`chunk_mesh.rs`、`texture.rs`、数据文件。
- 步骤：
  1. 以 `references/minecraft-data` 的 blocks 表 + MCP-919 `Block` 注册表为清单，逐一核对渲染器 block-id/meta→贴图与形状映射。
  2. 补齐遗漏：楼梯/台阶/栅栏/墙/玻璃板/门/活板门/床/蛋糕/炼药锅/漏斗/铁砧/酿造台/活塞/中继器/比较器/拉杆/按钮/压力板/花/作物/树苗/藤蔓/睡莲/仙人掌/甘蔗/旗帜/头颅等（旗帜/头颅为 block-entity）。
  3. 产出一张「~190 个 block id 渲染状态」清单（写入 docs），确保没有方块落入 debug 兜底色。
- 验证：遍历测试断言无 block 走 debug-color；逐类目视 ⚠️。**本任务依赖 T3/T12/T16/T24/T25 先落地**，故置于 wave 末尾。

---

### Wave 3 — 实体渲染与特效 → 合并后 `0.1.3`

`model.rs` 是热点。把 **T18（全实体模型重做）** 作为 model.rs 的主要改动者优先合，其余实体任务在其基础上做；T7（core 物理）、T11（HUD overlay）文件域独立可并行。

**T18 实体模型修复 + 全实体对照** `fix/entity-models` · L
- 触及：`model.rs`、`texture.rs`、`entity.rs`、`game.rs::build_entity_model`。
- 步骤：
  1. 逐生物对照 MCP-919 `ModelXxx`，修已知 bug：僵尸缺腿、骷髅无弓 + 骨骼盒/UV 错误（见 `ENTITY_RENDERING.md`）。
  2. 给手持物的生物加持物渲染（骷髅持弓、僵尸/其他持武器）。
  3. 核对所有 ⚠️/近似项（牛角/乳房、猪鼻、squid/slime/snowman/bat/silverfish 的 UV、蜘蛛腿角）。
  4. **【已确认】加宽实体图集到 128px，覆盖全部实体**：把当前 64px 槽位的实体图集扩展到 128px 宽，补齐所有未建模生物——铁傀儡/马/女巫（128² 贴图）、恶魂/烈焰人/守卫者/凋灵/末影龙/兔子等，逐一对照 `ModelXxx` 建模并采样自身槽位。目标：**没有任何生物落入彩色占位盒兜底**。
  5. **【已确认】盔甲层 + 第二皮肤层渲染**：给人形实体加盔甲层（头/胸/腿/靴，读装备元数据/`S04 EntityEquipment`，对照 `ModelBiped`/`LayerArmorBase` 的膨胀盒）与第二皮肤层（帽子/外套 overlay）。这同时是 T2「其他玩家盔甲 glint」的前置。
- 验证：`cargo test -p recraft_render`（well-formed/UV 落 [0,1]/各采样自身槽位，新增 128px 生物与盔甲层用例）；逐生物 + 穿盔甲目视 ⚠️。
- 备注：图集加宽到 128px 是一项较大改造（影响 `texture.rs` 槽位布局与所有现有 UV），作为本任务的第一步先落地，再补未建模生物。

**T5 实体受伤颜色/动画** `feat/entity-hurt-flash` · M
- 触及：`model.rs`（受伤红色 overlay）、`entity.rs`（`hurt_time` 已有，见 `start_hurt`）。
- 步骤：对照 `RendererLivingEntity`：受伤时模型整体叠红（hurtTime 驱动），加上受击倾斜与死亡动画（`deathTime` 旋转/倒地）。当前 `hurt_time` 已 tick，但渲染端红色 overlay 需对齐原版强度/混合方式。
- 验证：连本地服攻击生物，红闪与倾斜/死亡动画与原版一致 ⚠️。

**T9 投掷物渲染** `feat/projectile-render` · M
- 触及：`model.rs`/`renderer.rs`、`game.rs`（`S0E SpawnObject` 类型→渲染）。
- 步骤：对照各 `RenderXxx`：箭（`RenderArrow` 专属几何）、雪球/鸡蛋/末影珍珠/末影之眼/喷溅药水/经验瓶（2D 物品 billboard）、火球/钓鱼浮漂（专属）。按 SpawnObject 类型分派。
- 验证：连本地服丢雪球/末影珍珠/射箭，外观与飞行朝向正确 ⚠️。

**T20 下落方块实体** `feat/falling-block-entity` · S
- 触及：`game.rs`（SpawnObject type 70 + object-data 携带 blockstate）、`model.rs`（单方块 cube）。
- 步骤：`EntityFallingBlock` 渲染为按其 blockstate 取贴图的移动方块 cube，位置走插值。
- 验证：连本地服触发沙/沙砾/铁砧下落，渲染为对应方块下落 ⚠️。

**T26 经验球** `feat/xp-orb-render` · S
- 触及：`game.rs`（`S11 SpawnExperienceOrb`）、`model.rs`/粒子式 billboard、`texture.rs`（`experience_orb.png`）。
- 步骤：`EntityXPOrb` 渲染为上下浮动 + 颜色循环的 billboard，帧/色对照原版。
- 验证：连本地服掉经验球，浮动与发光循环正确 ⚠️。

**T17 箱子打开动画** `feat/chest-open-animation`（依赖 T16）· M
- 触及：`model.rs`（箱盖 LidAngle）、`game.rs`（`S24 BlockAction` id=1 观看人数）。
- 步骤：对照 `TileEntityChestRenderer`，根据 BlockAction 的观看人数平滑插值箱盖开合角度；覆盖单/大（双）/末影/陷阱箱。
- 验证：连本地服开/关箱子，盖子动画平滑且各类型正确 ⚠️。

**T7 掉落物立即物理** `fix/dropped-item-physics` · M
- 触及：`entity.rs`（`EntityItem` 物理）、`physics.rs`、`game.rs`。
- 步骤：定位掉落物「先在空中卡一下」的根因（多半是本地未即时模拟、等服务器速度包才动，或插值吃掉了初速）。对照 `EntityItem.onUpdate`：本地立即施加重力 `-0.04`、`0.98/0.98/0.98` 阻力与地面摩擦，服务器更新到来时再校正。
- 验证：挖方块/丢物，掉落物**立即**起弧线、不在空中停顿 ⚠️；与服务器位置不发生明显抖动。

**T2 附魔光效 glint** `feat/enchant-glint` · M
- 触及：`item_renderer.rs`（手持 + 物品栏）、`model.rs`（实体盔甲）、`renderer.rs`（附加 pass）、`texture.rs`（`enchanted_item_glint.png`）。
- 步骤：对照原版：glint 是滚动 UV 的紫色叠加层，**附加混合（additive）**再画一遍物品/盔甲网格。物品栏与手持先做；盔甲 glint 复用 **T18 已纳入的盔甲层渲染**（其他玩家盔甲也随之显示 glint）。
- 依赖：盔甲层渲染（T18）。
- 验证：附魔物品在物品栏/手持显示滚动紫光 ⚠️；穿附魔盔甲的实体显示盔甲 glint ⚠️。

**T11 着火屏幕贴图动态** `fix/fire-overlay-anim` · S
- 触及：`renderer.rs`/`ui.rs`（着火全屏 overlay）。
- 步骤：玩家着火时的全屏火焰 overlay 当前是静态；改为循环 `fire_layer_1` 帧动画（对照 `GuiIngameForge`/`EntityRenderer.renderFireInFirstPerson`）。
- 验证：着火时屏幕火焰跳动 ⚠️。

---

### Wave 4 — HUD / GUI / 输入 / 聊天 → 合并后 `0.1.4`

主要在 app 层。`game.rs`、各 `gui/*.rs`、`settings.rs` 是热点。T28（全 GUI）最大，依赖 T1 的玩家模型渲染入 GUI 视口的能力。

**T6 移除常驻调试 HUD + FPS 开关** `feat/fps-toggle` · S
- 触及：`main.rs`（当前 `f3_debug` 之外似乎常驻显示 FPS/区块）、`settings.rs`、`gui/options.rs`。
- 步骤：不开 F3 时移除左上角帧数/区块调试文本；在设置加 `show_fps` 开关（持久化到 options），开启后左上角用**纯文本小字**显示帧数；F3 仍显示完整调试。
- 验证：默认无调试文本；开关打开仅显示小字 FPS；F3 不受影响。可本地直接验证。

**T1 物品栏玩家预览模型** `feat/inventory-player-preview` · M
- 触及：`gui/inventory.rs`、`renderer.rs`（GUI 内 3D 视口/scissor）、`model.rs`、`skin.rs`。
- 步骤：对照 `GuiInventory.drawEntityOnScreen`，在物品栏左上格内用小视口渲染本地玩家 biped 模型（带皮肤），模型随鼠标转头/转身。
- 验证：物品栏出现玩家模型且随鼠标转动 ⚠️。

**T14 boss 血条** `feat/boss-bar` · M
- 触及：`game.rs`（boss 实体血量元数据）、`ui.rs`/`renderer.rs`（顶部血条）。
- 步骤：1.8.9 **没有 BossBar 包**，boss 血条是客户端从范围内 wither/末影龙实体的血量/名字（`BossStatus`）算出。渲染顶部居中的粉色血条 + 名称。
- 验证：连本地服 `/summon` 凋灵观察血条 ⚠️（末影龙需末地，较难，见「五」）。

**T8 自定义按键映射** `feat/custom-keybinds` · M
- 触及：`main.rs`（输入分发）、`game.rs`（`input.handle_key`）、`settings.rs`、`gui/options.rs`（新增 Controls 界面）。
- 步骤：对照 `KeyBinding`，把当前硬编码的 `KeyCode` 处理改为「动作→按键」映射表，持久化到 options；加一个 Controls GUI 供重绑全部按键。
- 验证：重绑任意键后生效并持久化；冲突提示；可本地验证。

**T21 聊天 tab 补全发包** `feat/chat-tab-complete` · M
- 触及：`gui/chat_screen.rs`、`chat.rs`、`network.rs`、`protocol/v1_8_9/packets.rs`（`C14 TabComplete` 出 / `S3A TabComplete` 入）。
- 步骤：聊天输入按 Tab 发 `C14 TabComplete`（带当前文本/坐标），收 `S3A` 候选填充补全。
- 验证：连本地服输入 `/` 命令 + Tab 得到补全候选。

**T22 聊天可点击文字组件** `feat/chat-components` · M
- 触及：`chat.rs`（解析 chat JSON 组件）、`gui/chat_screen.rs`（点击命中测试）、`network.rs`。
- 步骤：解析聊天 JSON 的 `clickEvent`/`hoverEvent`，支持点击 `run_command`/`suggest_command`/`open_url`/`copy`；聊天框做点击命中测试，`run_command` 时发聊天/命令包。
- 验证：连本地服用 `/tellraw` 推可点击消息，点击触发对应行为。

**T23 OldAnimations（1.7/1.8 动画）** `feat/old-animations` · M
- 触及：`model.rs`（姿势）、`item_renderer.rs`（手持动画）、`settings.rs`/`gui/options.rs`（开关）。
- 步骤：作为附属功能，实现 1.7 风格的受击/挥手节奏、鱼竿甩竿、剑格挡动画，并保留 1.8 版本，用设置开关切换（即每个动画做 1.7 + 1.8 两套）。
- 验证：切换开关后受击/鱼竿/格挡动画在 1.7、1.8 两种观感间正确切换 ⚠️。

**T28 附魔台 GUI + 全部原版 GUI** `feat/vanilla-guis`（依赖 T1）· L
- 触及：`container.rs`、`gui/inventory.rs`/新增各 GUI 文件、`item_renderer.rs`、GUI 贴图。
- 步骤：以 `S2D OpenWindow`（窗口类型字符串）为入口，对照各 `GuiContainer` 逐一实现：工作台、熔炉、箱子/大箱子/发射器/漏斗、酿造台、铁砧、附魔台、信标、村民交易、马、命令方块、告示牌编辑等。
  - 附魔台特别处理：3 个魔咒选项由 window property（附魔等级）+ 槽位驱动；显示标准银河文 + 悬停提示。
- 验证：连本地服逐个打开各容器，布局/交互/物品同步正确 ⚠️。

---

### Wave 5 — 对照补全与发布 `0.2.0`

- 全量 `cargo build && cargo test && cargo clippy --workspace`，清零告警。
- 跑 headless smoke + 连 `local_server/paper-1.8-protocol47` 做一轮综合冒烟。
- 复核「五」中所有「待用户目视确认」项是否已交付可确认的状态。
- 四个 crate 版本统一 bump `0.2.0`；commit `chore(release): 0.2.0`；tag `v0.2.0`。
- 更新 `docs/`（`ENTITY_RENDERING.md`、新增方块/粒子/音效状态表）。

---

## 五、需要用户协助 / 待确认（执行中命中即停下登记，不强行做）

1. **【已确认】T13 音频后端 = `kira`**（cpal 后端，spatial 场景 + mixer 音轨 + playback-rate 变调 + tween；OGG 经 symphonia）。
2. **【已确认·由我下载】T13 音频资源**：`local_assets` 当前 0 个 `.ogg`。**由我扩展 setup 脚本从 Mojang asset index（公开 CDN，无需账号）下载 1.8.9 声音资源**。仅当执行环境**完全禁止外网下载**时才会停下找你；否则自动完成。
3. **【已确认·纳入 0.2.0】T18 实体图集加宽 + 全实体**：把实体图集加宽到 128px，**覆盖全部实体**，目标无任何生物落入占位盒。已并入 T18 步骤 4。
4. **【已确认·纳入 0.2.0】盔甲渲染**：人形实体加盔甲层 + 第二皮肤层渲染，已并入 T18 步骤 5，并作为 T2 盔甲 glint 的前置。
5. **【需协助·验证场景】T14 末影龙血条 / T2 多人盔甲观察**：
   - 凋灵血条可在本地服 `/summon` 验证（无需账号）。
   - **末影龙**需进末地且较难本地稳定复现；**观察「其他玩家」盔甲**需要第二个客户端/玩家在线。若需要对这两项做最终目视确认，请你提供：一个能稳定出现末影龙的存档/服务器，或同时在线的第二个玩家（或同意用第二个 offline 客户端自测）。
6. **【全局·目视确认】所有标 ⚠️ 的任务**：逻辑/数值已对照 `references/MCP-919` 验证，但**像素级「和原版一模一样」的最终判断属于你**。完成后我会给出本地复现实步骤，请你目视拍板；在你确认前，这些任务标记为「已实现，待目视确认」，不计为「已验收」。

---

## 附：任务↔原始 todo 对照（防遗漏）

T1 物品栏玩家预览 · T2 附魔 glint · T3 特殊方块 · T4 全方块对照 · T5 受伤动画 · T6 FPS 开关 · T7 掉落物物理 · T8 自定义按键 · T9 投掷物 · T10 粒子系统 · T11 着火屏幕动态 · T12 动态火焰方块 · T13 音效系统 · T14 boss 条 · T15 破坏纹理顶/底面 · T16 箱子渲染 · T17 箱子开盖动画 · T18 实体模型修复 · T19 碰撞箱对照 · T20 下落方块实体 · T21 聊天 tab 补全 · T22 聊天可点击文字 · T23 OldAnimations · T24 流体渲染 · T25 火把贴墙 · T26 经验球 · T27 音符盒 · T28 全原版 GUI —— 共 28 项，与原始清单一一对应。
