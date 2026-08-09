# 1.8.9 方块渲染对照状态（T4）

记录 1.8.9 全部方块 id（1..=197）的渲染覆盖情况。数据源：`crates/fpsmaster_core/src/blocks.json`（形状/碰撞/贴图分配）+ `block.rs`（每形状几何）。

## 覆盖结论

- **无任何方块落入 magenta 兜底瓦片**。回归测试
  `fpsmaster_core::blocks::tests::every_1_8_block_id_has_a_render_def_no_magenta_fallback`
  断言：1..=197 内除两个有意例外的 id 外，每个 id 都有 registry 定义；且每个非 `none`
  形状的方块至少映射一个面贴图。
- 两个有意例外：
  - **36 活塞臂/移动方块**（`piston_extension`）：瞬态 TileEntity，正常世界中不持久存在，未建模。
  - **166 屏障**（barrier）：原版即不可见，`BlockState::render_shape` 专门返回 `None`。

## 本轮（T4）补齐的 id（此前缺定义、渲染为 magenta）

| id | 方块 | 采用形状 | 备注 / 后续 |
|----|------|----------|-------------|
| 26 | 床 | cube（近似） | 真实为低矮模型（block-entity），暂用贴图立方体 |
| 55 | 红石线 | lily（平面） | 平铺地面薄片，未做 0/15 连接朝向纹理 |
| 69 | 拉杆 | cross（近似） | 真实为底座+把手附着件（见 T25） |
| 75/76 | 红石火把灭/亮 | cross | 同火把，未做贴墙朝向（见 T25） |
| 93/94 | 中继器 灭/亮 | lily（平面） | 平铺，未做朝向/火把/延迟档位 |
| 97 | 怪物蛋（石头伪装） | cube | 统一用 stone 贴图，未按 meta 分石变种 |
| 104/105 | 南瓜/西瓜梗 | cross | 用 disconnected 帧，未做朝果方向弯曲 |
| 115 | 地狱疣 | cross | 固定生长阶段贴图 |
| 116 | 附魔台 | cube（近似） | 真实 0.75 高 + 漂浮书本（见 T3） |
| 117 | 酿造台 | cross（近似） | 真实为底座+立柱模型（见 T3/T28） |
| 119 | 末地传送门 | none（不可见） | 真实为星空 shader 平面（见 T3） |
| 122 | 龙蛋 | cube（近似） | 真实为特殊球状模型 |
| 127 | 可可果 | cross（近似） | 真实按 age/facing 为分级盒 |
| 137 | 命令方块 | cube | 完整 |
| 141/142 | 胡萝卜/马铃薯 | cross | 固定生长阶段贴图 |
| 149/150 | 比较器 灭/亮 | lily（平面） | 平铺，未做朝向/模式 |
| 151 | 阳光传感器 | cube（近似） | 真实 6/16 高薄板 |
| 153 | 下界石英矿 | cube | 完整 |
| 158 | 发射器（dropper） | cube | 用熔炉侧/顶贴图近似，未按 meta 放正面 |
| 178 | 反向阳光传感器 | cube（近似） | 同 151 |

## 近似项分类（精确化分属其他任务）

- **附着/朝向类**（拉杆、红石火把、可可、按 meta 朝向）→ 见 **T25** 火把/附着方块。
- **block-entity 特殊渲染**（附魔台、酿造台、末地/地狱传送门、告示牌、旗帜、箱子）→ 见 **T3 / T16 / T17 / T28**。
  - 头颅（144 / 物品 397）已完整实现，5 种头（骷髅/凋灵骷髅/僵尸/玩家/苦力怕）共用
    `skull_parts` 的头+帽两层，玩家头按 `Owner`/`SkullOwner` 档案下载真皮肤：
    - **世界方块** `ModelMesh::push_skull`：落地/贴墙 5 种朝向 + `Rot` 16 档转向，
      类型与转向来自 S35 UpdateBlockEntity（action 4）。
    - **物品栏图标** `gui_item::append_skull_icon`：原版无 `textures/items` 贴图，
      走 `TileEntityItemStackRenderer` 的 3D 头模型，采样实体图集的独立 GUI pass。
    - **戴在头上** `push_worn_skull`（`LayerCustomHead`）：绕颈部枢轴放大 1.1875 倍。
    - **手持/掉落物** `push_skull_item` + `ItemRenderer::build_held_skull` /
      `GameState::build_dropped_skull_models`：走模型 pass 而非方块图集物品 pass。
    - **本地放置预测**：`ItemSkull.onItemUse` 的朝向/转向规则，连 block-entity 一起预测。
- **平铺红石元件连接纹理**（红石线 0/15 连接、中继器/比较器朝向）→ 后续可加专用 flat-decal 形状。
- **流体不完整方块/斜面**（水/岩浆 8 级高度）→ 见 **T24**。

> 这些近似的目标是「不出现 magenta、形状/朝向大致正确」；像素级与原版一致需在客户端内目视确认（⚠️）。
