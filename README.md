<a id="readme-en"></a>

# GXPlayer

Windows-only desktop music player with a Rust-native playback/DSP pipeline and a Tauri UI.

**[中文说明 ↓](#readme-zh)**

**Intent:** a daily-driver player for personal use—not a store-ready mass-market product.

The audio stream **never** passes through WebView or Web Audio. Local playback is the independent core; online metadata, user-supplied LX source scripts, caching, lyrics, and optional spatial processing are separate layers.

| | |
|---|---|
| Version | 1.0.0 |
| Platform | Windows (WebView2) |
| License | MIT (`LICENSE`; bundled-asset notices in `THIRD_PARTY.md`) |

## What it does / does not

**Does:**

- Local library: file/folder import, tags and duration, favorites, playlists, history, missing-file checks, backup/restore
- Native decode and playback: Symphonia + resampling + cpal; local and progressive HTTP streams, seek, output device selection
- Online: metadata search and lyrics; resolve `musicUrl` via user-imported LX scripts into Rust progressive playback with fallback
- Windows: System Media Transport Controls, media keys, tray and window preferences
- Sound: named presets with light tweaks (below); default is transparent DSP bypass

**Does not:**

- Bundle community LX / playable source scripts—the user imports or drops them in
- Does not claim shared-mode device bit-perfect output
- No accounts, download storefront, or cross-platform support (Windows only for now)
- Does not promise “install and stream everything”—without a working source, online playback is limited

## Sound presets

One switchable chain under the hood: `EQ → Crossfeed → stereo HRTF (KEMAR ±30°) → linked limiter`.

The product surface is five named presets—not a semi-pro console.

| Preset | Behavior |
|--------|----------|
| **Bypass / 原声** | Full DSP chain off, zero extra DSP latency (**default**, bit-transparent within the DSP stage) |
| **Headphone daily / 耳机日常** | Crossfeed-focused; linked limiter remains on, with almost no tonal change |
| **Vocal / 人声** | Restrained EQ + light crossfeed; linked limiter remains on |
| **Bass / 低音** | Restrained low-end EQ + light crossfeed; linked limiter remains on |
| **Spatial / 空间** | Crossfeed + fixed front-speaker HRTF (can sound dull); linked limiter is required |

- Headphone daily, Vocal, and Bass expose **Intensity**; Spatial exposes only **Spatial amount** (HRTF mix)
- **Hold to hear untreated**: hot-path A/B with constant-latency alignment—not the same as selecting Bypass
- Sound choice is **persisted**; first run / clean config still defaults to Bypass
- If system effects such as Dolby are on, keep the player on **Bypass** to avoid stacked spatial processing

Legacy `music` / `cinema_game` modes remain compatibility mappings only. Authoritative state is full `DspSettings` plus active preset and tweak fields.

## User-provided LX sources

GXPlayer never ships playable sources. On startup it scans this app-data directory and imports every valid `.js` file through the same validation and sandbox path as manual imports:

```text
%APPDATA%\com.gxplayer.desktop\sources\drop-in
```

- Missing directories or individual invalid scripts do not block startup
- An existing active source is preserved; otherwise the first valid drop-in may become active
- Fallback order is configured in the Sources view. In automatic mode, new imports follow stable import order; after the user saves an explicit order, new sources stay opt-in
- A failed full-track resolve never silently starts a preview while advancing the queue

Sources run in a hidden sandbox WebView plus an isolated Worker: synchronous crypto stays in-sandbox; network I/O goes only through the bounded Rust SSRF-checked bridge. Contracts: `docs/architecture/lx-contract.md`, `docs/architecture/source-fallback.md`.

## Development

**Requirements:** Windows, WebView2, Rust stable (MSVC; see `rust-toolchain.toml`), Node.js **≥ 22.12**, npm.

```powershell
npm ci
npm run test:unit
npm run build
node scripts/check-version.mjs
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
npm run tauri dev
```

Root helpers: [`dev.bat`](dev.bat) (hot reload) and [`build.bat`](build.bat) (installer; follow the script for output paths).

Release checklist and signing notes: [`docs/releasing.md`](docs/releasing.md).

Legacy WPF projects and large third-party datasets stay outside this repository as read-only references.

## Documentation

| Path | Contents |
|------|----------|
| [`docs/architecture/`](docs/architecture/) | Thread model, player state machine, data contracts, LX, fallback, Windows media session |
| [`docs/phase-*-checklist.md`](docs/) | Phase acceptance records |
| [`docs/releasing.md`](docs/releasing.md) | Release checklist |
| [`docs/project-review-2026-07-12.md`](docs/project-review-2026-07-12.md) | Historical review snapshot (not the current roadmap) |
| [`docs/performance-exploration-2026-07-13.md`](docs/performance-exploration-2026-07-13.md) | Startup and package-size notes |

The currently scoped core is implemented—local/streaming playback, LX sandbox, metadata, library/UI, and optional spatial DSP. Prefer architecture docs and checklists over repeating every gate here.

## License

MIT License. Third-party fonts, KEMAR HRTF data, and related terms: `THIRD_PARTY.md` and `third_party/licenses/`.

---

<a id="readme-zh"></a>

# GXPlayer（中文）

Windows 专用桌面音乐播放器：Rust 原生播放 / DSP 管线 + Tauri UI。

**[Back to English ↑](#readme-en)**

**定位：** 自用优先的日用播放器，不是面向大众商店的完整发行产品。

音频流**从不**经过 WebView 或 Web Audio。本地播放是独立核心；在线元数据、用户自备的 LX 音源脚本、缓存、歌词与可选空间处理是外围能力。

| | |
|---|---|
| 版本 | 1.0.0 |
| 平台 | Windows（WebView2） |
| 许可 | MIT（见 `LICENSE`；随附资产的第三方声明见 `THIRD_PARTY.md`） |

## 能做什么 / 不做什么

**能：**

- 本地库：导入文件/文件夹、标签与时长、收藏、歌单、历史、缺文件核对与备份恢复
- 原生解码播放：Symphonia + 重采样 + cpal；本地与渐进 HTTP 流、seek、输出设备选择
- 在线：搜索与歌词等元数据；通过用户导入的 LX 脚本解析 `musicUrl`，再走 Rust 渐进播放与回退
- Windows：任务栏媒体控件（SMTC）、媒体键、托盘与窗口偏好
- 音效：命名预设 + 少量微调（见下），默认原声直通

**不：**

- **不捆绑**任何社区 LX / 可播音源脚本（须自行放入或导入）
- 不宣称共享模式下的设备级 bit-perfect
- 不做账号体系、下载站、跨平台（当前仅 Windows）
- 不把「装完就能听全网」当作目标——没有有效音源时，在线能力会很弱

## 音效预设

底层是同一条可开关链：`EQ → Crossfeed → 立体声 HRTF（KEMAR ±30°）→ 联动限幅`。

产品面是五个命名预设，不是半专业调音台。

| 预设 | 行为 |
|------|------|
| **原声** | 整条 DSP 链关闭，零额外 DSP 延迟（**默认**，DSP 阶段保持比特级直通） |
| **耳机日常** | 以轻度串音（Crossfeed）为主；联动限幅保持开启，几乎不改音色 |
| **人声** | 克制 EQ + 轻串音；联动限幅保持开启 |
| **低音** | 克制低频 EQ + 轻串音；联动限幅保持开启 |
| **空间** | 串音 + 固定前方音箱感 HRTF（可能偏闷）；联动限幅强制开启 |

- 「耳机日常」「人声」「低音」可调 **强度**；「空间」只显示 **空间感**（HRTF mix）
- **按住听未处理**：热路径 A/B（恒延迟对齐），不是瞬时切到「原声」预设
- 听感选择会**持久化**；首次 / 干净配置仍默认原声
- 若系统开着杜比等音效，建议播放器保持 **原声**，避免双重空间处理打架

旧的 music / cinema_game 双模式仅作兼容映射；权威状态是完整 `DspSettings` + 当前预设与微调。

## 用户自备 LX 音源

程序本身不提供可播源。启动时会扫描下列目录中的合法 `.js`，经与手动导入相同的校验与沙箱路径导入：

```text
%APPDATA%\com.gxplayer.desktop\sources\drop-in
```

- 目录缺失或个别脚本无效不阻止启动
- 已有激活源会保留；否则第一个有效 drop-in 可成为激活源
- 回退顺序在「音源」页配置：自动模式下按稳定导入序；用户保存显式顺序后，新源默认不自动进回退链
- 完整曲解析失败时，不会在推进队列时静默改播官方试听

音源在隐藏沙箱 WebView + 隔离 Worker 中执行：同步 crypto 在沙箱内完成，网络只经 Rust 侧 SSRF 约束桥接。契约见 `docs/architecture/lx-contract.md` 与 `docs/architecture/source-fallback.md`。

## 开发

**环境：** Windows、WebView2、Rust stable（MSVC，见 `rust-toolchain.toml`）、Node.js **≥ 22.12**、npm。

```powershell
npm ci
npm run test:unit
npm run build
node scripts/check-version.mjs
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
npm run tauri dev
```

也可使用仓库根目录的 [`dev.bat`](dev.bat)（开发热重载）与 [`build.bat`](build.bat)（打安装包；产物路径以脚本提示为准）。

发行检查清单与签名等步骤见 [`docs/releasing.md`](docs/releasing.md)。

历史 WPF 工程与大体量第三方数据在仓库外，仅作只读参考。

## 文档

| 路径 | 内容 |
|------|------|
| [`docs/architecture/`](docs/architecture/) | 线程模型、播放状态机、数据契约、LX、回退、Windows 媒体会话 |
| [`docs/phase-*-checklist.md`](docs/) | 各阶段验收记录 |
| [`docs/releasing.md`](docs/releasing.md) | 发布检查清单 |
| [`docs/project-review-2026-07-12.md`](docs/project-review-2026-07-12.md) | 历史评审快照（非当前路线图） |
| [`docs/performance-exploration-2026-07-13.md`](docs/performance-exploration-2026-07-13.md) | 启动与体积探索笔记 |

当前规划范围内的核心能力已实现（本地/流式播放、LX 沙箱、元数据、库与 UI、可选空间 DSP 等）；细节以 architecture 文档与 checklist 为准，而非本 README 的重复罗列。

## 许可

MIT License。第三方字体、KEMAR HRTF 等归属与条款见 `THIRD_PARTY.md` 与 `third_party/licenses/`。
