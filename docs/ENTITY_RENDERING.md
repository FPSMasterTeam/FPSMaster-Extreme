# 实体渲染进度（Entity Rendering）

记录 1.8.9 生物/实体的模型与贴图实现状态。代码位置：

- 模型与骨骼动画：`crates/fpsmaster_render/src/model.rs`
- 纹理图集（实体槽位）：`crates/fpsmaster_render/src/texture.rs`
- 插值/行走相位/头部转向等动画状态：`crates/fpsmaster_core/src/entity.rs`
- App 层装配（把实体状态喂给模型）：`crates/fpsmaster_app/src/game.rs::build_entity_model`

## 架构要点

- **渲染缩放固定为 1/16 块/像素**（`MODEL_SCALE`），与 vanilla 一致，不再用 `height/model_px`。
  之前所有模型被压扁到 hitbox 高度（玩家 1.8 而非 vanilla 的 2.0），且蜘蛛/史莱姆这类宽扁
  生物无法正确比例渲染。改为 1/16 后所有模型按真实比例显示，脚底锚定在 `feet`。
- **实体纹理图集**为单列 64px 宽槽位（`EntitySlot`），每槽 64×64。新增 15 个 mob 槽位，
  共 24 个固定槽 + 1 个纯白槽 + 64 个玩家皮肤行。≤64×64 的 vanilla 贴图按 1:1 像素映射。
- **模型原型**：人形(biped)、村民、末影人、四足、羊(带羊毛覆盖层)、狼、蜘蛛、猫、
  苦力怕、鸡、立方体(史莱姆)、鱿鱼、雪傀儡、蝙蝠、虫(银鱼)。每个原型由
  `*_parts()`（盒子+UV）与 `*_poses(anim)`（关节角度）两部分组成。
- 移植 vanilla 模型用 `vbox()` 辅助：把 vanilla 的 y 向下/朝向 -z 的盒子转换成引擎的
  脚底向上(+y)/朝向 +z 约定，并保留贴图 UV 偏移。

## 逐生物状态（1.8 SpawnMob type id）

| id | 生物 | 状态 | 模型 / 纹理 | 备注 |
|----|------|------|-------------|------|
| 50 | Creeper 苦力怕 | ✅ 完整 | Creeper / creeper.png | |
| 51 | Skeleton 骷髅 | ✅ 完整 | 人形(镜像左肢) / skeleton.png | 64×32 贴图 |
| 54 | Zombie 僵尸 | ✅ 完整 | 人形(镜像左肢) / zombie.png | 1.8 mob biped 镜像右肢；其 64×64 贴图的独立左肢区为空，故 separate=false |
| 53 | Giant 巨人 | ⚠️ 近似 | 僵尸人形 / zombie.png | 未放大 |
| 57 | Zombie Pigman 僵尸猪人 | ✅ 修复 | 人形 / zombie_pigman.png | 原来错误复用僵尸纹理 |
| 120 | Villager 村民 | ✅ 新增 | 专属模型(大鼻子/长袍/抱臂) / villager.png | 原来强套 biped 致贴图错乱 |
| 58 | Enderman 末影人 | ✅ 新增 | 高瘦人形(拉长四肢) / enderman.png | |
| 52 | Spider 蜘蛛 | ✅ 新增 | 头+胸+腹+8 条腿 / spider.png | 腿张角为近似 |
| 59 | Cave Spider 洞穴蜘蛛 | ⚠️ 近似 | 蜘蛛模型 / spider.png | 复用普通蜘蛛纹理 |
| 90 | Pig 猪 | ✅ 完整 | 四足 / pig.png | 无猪鼻(近似) |
| 91 | Sheep 羊 | ✅ 新增羊毛 | 四足身体 + sheep_fur 膨胀外层 | 解决"不渲染羊毛" |
| 92 | Cow 牛 | ✅ 近似 | 四足 / cow.png | 无牛角/乳房 |
| 96 | Mooshroom 蘑菇牛 | ✅ 修复 | 四足 / mooshroom.png | 原来错误复用牛纹理 |
| 93 | Chicken 鸡 | ✅ 完整 | Chicken / chicken.png | |
| 95 | Wolf 狼 | ✅ 新增 | 狼模型(身体/鬃毛/尾巴) / wolf.png | 原来渲染成粉色猪 |
| 98 | Ocelot 豹猫 | ✅ 新增 | 猫模型 / ocelot.png | 原来渲染成猪 |
| 94 | Squid 鱿鱼 | ⚠️ 基础 | 外套 + 8 触手 / squid.png | |
| 55 | Slime 史莱姆 | ⚠️ 基础 | 贴图立方体 / slime.png | 固定尺寸，未读 size 元数据 |
| 62 | Magma Cube 岩浆怪 | ⚠️ 基础 | 贴图立方体 / magmacube.png | 同上 |
| 97 | Snowman 雪傀儡 | ⚠️ 基础 | 三段雪球+头+手臂 / snowman.png | UV 为近似 |
| 65 | Bat 蝙蝠 | ⚠️ 基础 | 身体+头+双翼(扇动) / bat.png | UV 为近似 |
| 60 | Silverfish 蠹虫 | ⚠️ 基础 | 三段身体 / silverfish.png | UV 为近似 |
| 67 | Endermite 末影螨 | ⚠️ 近似 | 银鱼模型 / silverfish.png | 复用银鱼纹理 |

### 未建模（渲染为纯色占位盒子）

这些受 64px 图集宽度限制（贴图过大）或模型过于复杂，暂未实现，落入
`mob_model() -> None` 的彩色盒子兜底：

- **贴图超出 64×64**：Iron Golem 铁傀儡(128²)、Horse 马(128²)、Witch 女巫(64×128)。
- **模型复杂/低频**：Ghast 恶魂、Blaze 烈焰人、Guardian 守卫者、Wither 凋灵、
  Ender Dragon 末影龙、Rabbit 兔子等。

## 本次修复的明确 bug

1. **id 100（马）被错误映射成村民** —— 已移除该别名，马现在落入兜底盒子（待建模）。
2. **僵尸猪人 / 蘑菇牛纹理串槽** —— 各自分配独立纹理槽位。
3. **所有模型被压扁** —— 渲染缩放改为 vanilla 的 1/16。

## 已知限制 / 后续工作

- **命中盒已按生物区分**（T19，0.2.0）：`entity_size()` 现按 1.8.9 SpawnMob/SpawnObject
  type id 返回各物种的 `setSize` 数值（蜘蛛 1.4×0.9、末影人 0.6×2.9、下落方块/TNT 0.98 等），
  影响攻击/交互的瞄准射线与骑乘偏移。史莱姆/岩浆怪按 size=1 取值（未读 size 元数据）。
- **无第二皮肤层**：玩家/人形未渲染帽子/外套 overlay 层。
- **近似项**：牛无角/乳房、猪无鼻；snowman/bat/silverfish 的 UV 偏移为凭 vanilla 估算，
  颜色正确但贴图细节可能略偏；蜘蛛腿张角为静态近似。
- **超宽贴图支持**：若要做铁傀儡/马/女巫，需要把图集加宽到 128px 或单独缩放其 UV。

## 验证

- `cargo test -p fpsmaster_render`：52 passed（含新增 `modeled_mobs_are_well_formed_and_textured`、
  `single_slot_mobs_sample_their_own_slot`、`sheep_layers_body_and_wool`，覆盖每个建模生物的
  网格良构性、UV 落在 [0,1]、各生物只采样自己的图集槽位、羊同时采样身体+羊毛两层）。
- `cargo test -p fpsmaster_core`：69 passed。
- `cargo build -p fpsmaster_app`、`cargo clippy -p fpsmaster_render`：通过，无新增告警。
- 尚未做应用内的可视化人工验证（运行客户端连服观察实际外观）。
