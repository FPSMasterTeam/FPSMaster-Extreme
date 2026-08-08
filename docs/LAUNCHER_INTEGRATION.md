# FPSMaster-Extreme × FPSMaster-Launcher 集成契约 (Phase 0)

本文件是 **FPSMaster-Extreme**（原生 Rust 客户端，产物 `fpsmaster_app`）被
**FPSMaster-Launcher**（Tauri 启动器）分发 / 更新 / 启动 时，两个仓库共同遵守的接口。
两边的实现都以本文件为准；修改接口须同步更新本文件与另一侧。

> 决策基线：
> - **一等公民原生启动** —— Launcher 新增「原生应用」类型，直接 spawn `fpsmaster_app`（不走 Java 管线）。
> - **复用 Launcher 合法资源下载** —— 1.8.9 vanilla 素材由 Launcher 下载并抽取，经 `--assets` 指给 Extreme，分发包内不含 Mojang 版权素材。

---

## 1. 产物类型

Launcher 现有 `versionType` 为 `EDGE` / `NOVA`（均为 Java Minecraft 实例）。
Extreme 是原生可执行文件，新增类型：

```
versionType = "EXTREME"     // 原生应用，不装 Minecraft、不装 loader、不走 Java 启动
```

前端 `Instance` / 后端白名单以 `versionId = "FPSMaster-Extreme"` 标识该 preset。

---

## 2. 分发包格式

CI 为每个目标平台产出一个 tarball + 校验：

```
FPSMaster-Extreme-<version>-<target>.tar.gz
FPSMaster-Extreme-<version>-<target>.tar.gz.sha256
FPSMaster-Extreme-<version>-<target>.manifest.json
```

`target` 取值（与 Launcher CI 对齐）：
`windows-x86_64` | `macos-aarch64` | `macos-x86_64` | `linux-x86_64`

**tarball 内容布局**（解压后即为 Extreme 的工作目录，见 §4）：

```
fpsmaster_app(.exe)                 # 主二进制（release，strip 后再签名）
nvngx_dlss.dll                      # 仅 Windows/Linux，可选（无则跳过 DLSS）
mods/                               # 随包内置 mod（可空）
resourcepacks/                      # 可选
sdk/                                # 可选，随包附带
# 注意：不含 local_assets/（Mojang 版权素材由 Launcher 下发，见 §5）
# 注意：不含 fpsmaster_options.txt（首启由 Extreme 生成；Launcher 不覆盖用户配置）
```

`manifest.json` 沿用 Launcher 现有 `.fpsmaster-launcher-mods.json` 的文件清单语义：

```json
{
  "versionTag": "1.0.0",
  "downloadUrl": "https://cdn.fpsmaster.top/extreme/1.0.0/FPSMaster-Extreme-1.0.0-macos-aarch64.tar.gz",
  "checksum": "<sha256 of tarball>",
  "files": [
    { "path": "fpsmaster_app", "sha1": "..." },
    { "path": "mods/coords_hud/mod.toml", "sha1": "..." }
  ]
}
```

---

## 3. Registry（新 release 系统）

> 后端已**移除**旧的 `client_versions` / `versionType` / `/versions/available` 系统，
> EDGE/NOVA/EXTREME 全部走 `release_entries`。详见 backend README「Launcher 发布体系」。

**写入（CI 发布）** —— 每个平台一条，`POST /api/v1/launcher/releases/ci`：

```json
{
  "productCode": "extreme",
  "channelCode": "release",
  "versionName": "1.0.0",
  "commitHash": "<sha>",
  "downloadUrl": "https://cdn.fpsmaster.top/extreme/1.0.0/FPSMaster-Extreme-1.0.0-macos-aarch64.tar.gz",
  "checksum": "<sha256>",
  "fileSize": 15728640,
  "manifestUrl": "https://cdn.fpsmaster.top/extreme/1.0.0/FPSMaster-Extreme-1.0.0-macos-aarch64.manifest.json",
  "minLauncherVersion": "0.3.6",
  "target": "macos-aarch64",
  "enabled": true,
  "recommended": true
}
```

请求头 `X-CI-Token: <token>`。三平台各 POST 一次（不同 `target`）。

**读取** —— Launcher 按当前平台调
`GET /api/v1/launcher/releases/available?target=<平台>`（需认证），后端只返回该平台
可安装的条目；EXTREME 每个 target 一条，`downloadUrl`/`checksum` 即该平台产物。
`nova`/`extreme` 的 `beta`/`nightly` 渠道仅 SPONSOR/ADMIN 可见。

---

## 4. 安装

- 安装目录：`{gameDir}/apps/FPSMaster-Extreme/`（**独立目录，不进 `versions/<id>/mods/`**）。
- 下载 tarball → 校验 SHA256 → 解压到安装目录 → 写 marker `.fpsmaster-launcher-app.json`。
- 更新：比对 registry 的 `versionTag`/`checksum`，不一致则重装；一致则跳过。
- macOS：解压后须清除隔离属性（`xattr -dr com.apple.quarantine`），否则已签名/公证的
  二进制仍可能被 Gatekeeper 拦（取决于下载来源）。二进制本身在 CI 阶段已签名 + 公证。

---

## 5. 资源（1.8.9 assets）—— Launcher 下发

Extreme 运行需要 vanilla 1.8.9 的 `assets/minecraft/{textures,sounds,lang,...}` 资源树。
这些素材**不随分发包**，由 Launcher 复用其 Minecraft 安装管线提供：

1. Launcher 用现有 `minecraft_core::install_vanilla`（或等价逻辑）合法下载 1.8.9
   **client jar**（Mojang / BMCLAPI，SHA1 校验）。
2. 从该 jar 抽取 `assets/` 到一个稳定目录，例如：
   `{gameDir}/apps/FPSMaster-Extreme/local_assets/minecraft-1.8.9/`
   （等价于 Extreme 仓库 `scripts/setup_minecraft_1_8_9_assets.py` 的产物布局）。
3. 该抽取只需在首次 / jar 变更时做一次，之后复用。

> 说明：Launcher `install_vanilla` 下载的 objects 哈希库里**没有**方块/物品贴图——
> 1.8.9 的贴图在 client jar 内部。所以「抽 jar 的 `assets/`」是必需步骤，
> 不能只把 objects 目录指过去。

Extreme 的资源解析顺序（`fpsmaster_app` 现状，不需改动即可被驱动）：
1. `FPSMASTER_ASSET_PATH` 环境变量
2. `--assets <path>` 命令行参数
3. `./local_assets/minecraft-1.8.9/assets/minecraft/`（相对工作目录）
4. 用户本机 `.minecraft` 的 1.8.9.jar
5. 调试用纯色 atlas 兜底

Launcher **推荐用 `--assets` 显式指定**（§6），避免依赖第 3/4 项的隐式查找。

---

## 6. 启动契约

Launcher 通过 `std::process::Command` 直接 spawn 原生二进制（**不经 Java / `build_vanilla_launch_plan`**）。

```
<install_dir>/fpsmaster_app \
    --assets   <install_dir>/local_assets/minecraft-1.8.9/assets/minecraft \
    --connect  <host:port>       # 可选：快速加入服务器；缺省进主菜单
    --username <name>            # 缺省 "FPSMaster"
```

**硬性要求：**

- **工作目录（`current_dir`）必须设为 `<install_dir>`。**
  `fpsmaster_app` 以相对路径解析 `mods/`、`resourcepacks/`、`local_assets/`、
  `fpsmaster_options.txt`。不设 `current_dir` 会导致 mod / 资源包 / 配置找不到。
- 进程监控（PID / 内存 / 退出码）复用 Launcher 现有 `GameRuntimeStats` 那套。

**当前 `fpsmaster_app` 已支持、Launcher 可直接用的参数：**

| 参数 | 语义 | 缺省 |
|------|------|------|
| `--assets <path>` | vanilla 资源目录（指到 `assets/minecraft`） | 走 §5 的查找链 |
| `--connect <host:port>` | 启动即连服（**offline 模式**） | 进主菜单 |
| `--username <name>` | 游戏内用户名 | `FPSMaster` |

其余参数（`--window`、`--demo`、`--profile-frames` 等）是调试用途，Launcher 不需传。

---

## 7. 鉴权（v1 现状与后续）

- **v1：offline 模式。** `--connect` 目前用 `NetworkHandle::connect_offline_1_8_9`，
  只带用户名，不做微软在线鉴权。Launcher 传 `--username` 即可（可用 Launcher 里
  当前 Minecraft 账号的名字），但连不上开启正版验证（online-mode=true）的服务器。
- **后续（v2）：** 若要打通正版服，需要
  (a) Extreme 侧补完在线鉴权（RSA/AES 握手 + Mojang sessionserver），
  (b) 契约新增透传 access token / UUID 的方式（建议走临时文件或环境变量，
      **不放命令行**，避免 token 出现在进程列表）。
  该项作为独立里程碑，不阻塞 v1。

---

## 8. 两侧改动清单（落地追踪）

**Extreme 侧**
- [ ] 跨平台打包脚本：产出 §2 的 tar.gz + sha256 + manifest（现 `scripts/package.ps1` 仅 Windows/zip）
- [ ] macOS 代码签名 + 公证（strip 之后）
- [ ] 分发包剔除 `local_assets/`
- [ ] CI：三平台构建 → 上传后端 → 通知 registry
- [ ] （v2）在线鉴权 + token 透传

**Launcher 侧**
- [ ] 前端 `constants.ts` 新增 EXTREME preset、`types.ts` 加 `"EXTREME"`
- [ ] `is_launcher_preset_version_id` 白名单加 `"FPSMaster-Extreme"`
- [ ] 后端 `install_native_app` 命令：下载/校验/解压到 `apps/FPSMaster-Extreme/` + marker（§4）
- [ ] 后端 1.8.9 assets 抽取：从 client jar 抽 `assets/` 到安装目录（§5）
- [ ] 后端 `launch_native_app` 命令：设 `current_dir` + 传参 spawn（§6）
- [ ] 前端 Home 展示 + 图标、安装/启动流程接线
