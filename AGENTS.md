# AGENTS.md — 仓库开发规矩（给 AI coding agent）

> 本文件写给在本仓库里改代码的 AI agent。维护者本机的 NAS 环境、账号与内部发布流水线细节不在本文范围。
> 仓库文件是唯一事实来源：本文与代码/配置冲突时，以仓库文件为准；本文没写到的，先读文件再动手。

## 1. 项目是什么

bili-sync-up 是 [bili-sync](https://github.com/amtoaer/bili-sync) 的升级优化版：Rust + Tokio 驱动的 B 站 / YouTube / TikTok / 抖音同步下载工具，面向 NAS 用户，自带 Web 管理界面和 VitePress 文档站。

| 层 | 技术栈 | 位置 |
|---|---|---|
| 后端 | Rust 1.97.1（`rust-toolchain.toml` 钉死）、Tokio、Axum 0.8、SeaORM 1.1、SQLite | `crates/` |
| 前端 | SvelteKit 5 + TypeScript + Tailwind 4，构建产物 `web/build` | `web/` |
| 文档 | VitePress 1.6.3，生效配置 `docs/.vitepress/config.js`（没有 .mts） | `docs/` |
| 打包 | Dockerfile、docker-compose.yml、群晖 SPK 打包脚本 | 根目录 / `synology/` |

## 2. 仓库结构

```
crates/bili_sync/            bin bili-sync-rs：main.rs、build.rs、api/、bilibili/、config/、task/、utils/、adapter/
                            以及根级大文件 youtube.rs / douyin.rs / tiktok.rs / workflow.rs
crates/bili_sync_entity/     SeaORM 实体
crates/bili_sync_migration/  SeaORM migration
web/                         SvelteKit 前端（npm ci → npm run build 产出 web/build）
docs/                        VitePress 文档站
synology/                    SPK 打包脚本与打包指南
scripts/                     辅助脚本（签名、图片压缩等）
.github/workflows/           CI：quality.yml 门禁、build.yml 构建发布、docs-deploy.yml 文档部署、docker-build.yml 镜像
```

workspace 三个 crate，`Cargo.toml` 的 `default-members` 只有 `crates/bili_sync`。
`bili_sync` **没有 lib target**：所有测试内联在源码的 `#[cfg(test)]` 模块里，仓库没有 `tests/` 目录。

## 3. 构建与验证（顺序不能错）

**铁律：先构建前端，再构建后端。**
`crates/bili_sync/src/task/http_server.rs` 用 `#[folder = "../../web/build"]` 把前端产物 rust-embed 进二进制；`crates/bili_sync/build.rs` 找不到 `web/build` 时只打一条 `cargo:warning`、不会报错——直接编后端会静默产出旧 UI / 空 UI 的二进制。

```bash
# 1) 前端（固定 Node 20）
cd web && npm ci
cd web && npm run check:node20     # svelte-kit sync + svelte-check
cd web && npm run build:node20     # 产物 web/build
# Windows 本地没装 Node 20 时，check:node20 / build:node20 内部用 npx -p node@20 包装

# 2) 后端
cargo check -p bili_sync
cargo build                        # target/debug/bili-sync-rs，默认监听 0.0.0.0:12345
cargo build --release

# 3) 文档
cd docs && npm ci && npm run docs:build   # 产物 docs/.vitepress/dist
```

改完必跑的四件套：`cargo check -p bili_sync`、`cd web && npm run check`、`cd web && npm run build`、`git diff --check`。

Windows 有一键脚本 `make.bat`（`setup` / `dev` / `build` / `release` / `test` / `fmt` / `lint` / `docs` / `docs-build` / `docker` / `compose` / `package` / `clean`）。注意它是 **GBK 编码**（cmd 里中文 echo 需要），改它时不要转成 UTF-8。

格式化：`cargo fmt --all -- --check`（与 CI 一致）；只改某个文件时用 `rustfmt --check <file>` 定点检查。`rustfmt.toml`：`max_width = 120`、`tab_spaces = 4`、`newline_style = "Unix"`、`edition = "2021"`。
**禁止全局 `cargo fmt`**：Windows 上会把无关文件的行尾改成 CRLF，制造大量无关 diff（踩过）。

前端格式：Prettier 配置在 `web/.prettierrc`（`useTabs`、单引号、`printWidth: 100`）；构建时由 `web/scripts/generate-app-meta.mjs` 生成的版本号等文件属于生成物，禁止手改、禁止格式化。

## 4. 测试

```bash
cargo test -p bili_sync --bin bili-sync-rs <filter> -- --nocapture
```

- 没有 lib target，`cargo test -p bili_sync --lib` 会报 `no library targets found`。
- 测试内联在源码 `#[cfg(test)]`；现有 13 个 `#[ignore]`（依赖网络 / 真实凭证 / ffmpeg），跑全量前先看 `.github/workflows/quality.yml` 末尾注释。
- 跑需要编译 `bili_sync` 的命令前，先 `mkdir -p web/build`，否则 rust-embed 的告警/潜在失败会干扰判断。

## 5. CI 门禁现状（以 `.github/workflows/quality.yml` 为准）

quality.yml 目前在 PR 上实际启用的三道关：

1. `cargo fmt --all -- --check`（Rust 格式检查）
2. `npm run check`（前端 svelte-check，Node 20）
3. `npm run docs:build`（文档站构建，Node 18）

clippy 与 cargo test 目前在 quality.yml 末尾**整段注释、暂缓启用**（原因写在注释里：clippy 有存量告警，test 需要先验证干净 runner 基线）。clippy 的目标形态是 `cargo clippy --workspace --all-targets -- -D warnings`，最近有 PR 正在启用它——**提交前请重读 quality.yml 的实际内容，不要照抄本文**。

其他 workflow：

- `build.yml`：PR 上跑前端构建 + 4 个 Windows/Linux 平台；macOS 仅在非 PR 时跑（push main / tag），release 与 SPK 只在 tag 时跑。
- `docs-deploy.yml`：只在 push main 且改动 `docs/**` 时部署 GitHub Pages。
- `docker-build.yml`：push `v*` tag 时自动构建并推 Docker Hub 与 CNB 两个 registry。

CI 绿只说明能编译 / 格式对 / 类型过，**不说明功能修好了**（见第 7 节）。

## 6. 改动规矩

- **先开 PR 再合并，绝不直接 push main**。PR 标题、描述、提交信息一律中文。
- 一个修复一个 PR；新修复开新 PR，不要往已合并或待合并的旧 PR 上叠无关改动。
- **用户可感知变更必须写 changelog**：`docs/changelog.md` 顶部 `## 未发布` 段；发版时把 `## 未发布` 原地改名为 `## vX.Y.Z (YYYY-MM-DD)`。前端版本号由 `web/scripts/generate-app-meta.mjs` 从 changelog 第一个 `## v...` 提取，忘记改名会静默停在旧版本。
- 版本号只允许三段 `X.Y.Z`（四段会让 Cargo 报 `unexpected character '.' after patch version number`）。改版本同步 4 处：`Cargo.toml` 的 `[workspace.package] version`、`Cargo.lock` 里三个 workspace crate（`bili_sync` / `bili_sync_entity` / `bili_sync_migration`）、`docs/changelog.md`、`docs/introduction.md`。
  - 坑：`Cargo.lock` 里第三方 crate `crc` 的版本也可能是 `3.4.0`，替换时必须按 `name = "<crate>"` 锚定，不能全局替换版本字符串。
- 配置已全部存进 SQLite（`<CONFIG_DIR>/data.sqlite`），`config.toml` 只在 `crates/bili_sync/src/config/migrator.rs` 里被读取一次做迁移（迁移后备份成 `config.toml.backup.<时间戳>`）。不要新写读写 `config.toml` 的代码，也不要在文档里教用户改它。
- 文档站把 markdown 当 Vue 模板编译：行内代码里的 `{{ }}` 必须写成 `<code v-pre>{{...}}</code>`。历史故障：`docs/changelog.md` 里的 `` `{{truncate_bytes title 200}}` `` 让文档站从 2026-11-04 起连续部署失败 20+ 次。

## 7. 「修好」的判定

- 静态检查 / CI 绿 ≠ 修好：必须跑通用户可见的运行时路径（启动程序、调用相关 API / 界面、看真实日志或数据）。
- 结论区分「已确认」（有代码、日志或复现证据）和「假设」；「设计过 / 以前实现过」不等于现在的代码里还有。
- UI 改动要实际打开页面验证；下载 / 扫描类改动要有真实任务跑通的证据。
- Docker 问题先 `docker inspect` 看真实挂载和权限，不要先改 compose、更不要先删数据。
- 只读任务（排障、追溯、可行性分析）保持只读，不要顺手改代码。

## 8. 常见真坑（每条都踩过）

- **筛选只在扫描插入时生效**：唯一过滤点是 `crates/bili_sync/src/utils/model.rs` 的 `create_videos()`；`filter_unfilled_videos` / `filter_unhandled_video_pages` 不会重放筛选，改筛选前已入队的视频照旧下载。
- **空关键词列表会被存成 `None`**（`params.whitelist_keywords.as_ref().filter(|v| !v.is_empty())`）：页面上「设置了关键词但保存成空」会静默关掉白/黑名单过滤。
- `reset_video` / `reset_all_videos`（`crates/bili_sync/src/api/handler.rs`）只改状态位，不改 `video.path` / `page.path`，重下会复用旧坏目录；要换路径用 `reset_video_source_path`。
- `os error 3` 是本地建目录 / 建文件失败的下游表现——先查权限、磁盘空间、路径长度，不要只盯着报错行。
- `Season` 目录不能当「非 Season 选项失效」的证据：合集自己也会建 `Season 01/02`。
- 模板分隔符改动需要迁移存量记录，不只影响新下载（模板引擎在 `crates/bili_sync/src/workflow.rs`）。
- 抖音「合集」= mix 接口（`/aweme/v1/web/mix/...`），`count` 参数无效、固定 15 条/页；收藏夹和合集共用 `/collection/{id}`，要按 `result_type` 判类型（`crates/bili_sync/src/douyin.rs`）。
- 课程 / 付费课（pugv）：`SourceType::Pugv`；课程没有 bvid，用 `av<aid>`；`payed == 0` 不能当未购买，要按 `payment.price` 判 Free / Unpurchased。
- 弹幕目前只输出 ASS（`crates/bili_sync/src/bilibili/danmaku/ass_writer.rs` 是唯一写入器）；XML 弹幕尚未实现，不要按 XML 写文档或代码。
- **检索卫生**：裸 `rg` 必须带排除项（`-g '!target/**' -g '!dist/**' -g '!*.json' -g '!*.log' -g '!*.sqlite*' -g '!.git/**'`），否则一次扫描可能爆上百万 token 被截断。
- `logs-debug-*.csv` 含明文 B 站 Cookie（`crates/bili_sync/src/utils/file_logger.rs`），**绝不提交、绝不公开**。
- 文档站构建要放在有 exec 权限的文件系统上：`/tmp` 这类 noexec 挂载会让 esbuild 报 `EACCES`。

## 9. 发布（只在明确要求时做）

- 发版顺序：改版本 4 处 → PR 合并 main → 打**附注** tag `git tag -a vX.Y.Z -m "vX.Y.Z" <sha>` → `git push origin vX.Y.Z` → `build.yml` 自动跑前端 / 6 平台二进制 / release / SPK。
- release notes 用 `gh release edit vX.Y.Z --notes-file <UTF-8 文件>`，不要直接在命令行传中文（防乱码）。
- SPK 打包硬规则见 `synology/PACKAGING-GUIDE.md`：外层 `.spk` 必须是未压缩 tar 且条目不能带 `./` 前缀；`package.tgz` 必须用 `tar --mode='u+rwx,go+rx'` 打包。构建脚本 `synology/build-spk.sh`。
- 日常不需要手动跑 `docker-build.yml`，push tag 会自动推镜像。

## 10. 文档站（`docs/`）

- 生效配置只有 `docs/.vitepress/config.js`；导航 / sidebar / `srcExclude` 改动后必须本地 `npm run docs:build` 验证。
- 站点 base 是 `/bili-sync-up/`，站内链接用相对路径或站内绝对路径，不要写死线上域名。
- 文档必须和代码一致：默认值、默认路径、端口、API 路径以代码为准；不确定就写「以实际界面为准」，不要编。
- 内部笔记（`docs/superpowers/plans/`、`docs/SYSTEM_CONFIG_QUEUE_SUMMARY.md`）已用 `srcExclude` 排除，新增同类文件记得加进 `srcExclude`，不要留下无入口的公开页。

---

本文与仓库文件冲突时，以仓库文件为准。
