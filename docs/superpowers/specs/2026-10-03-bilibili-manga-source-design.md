# 哔哩哔哩漫画源设计（B站源新类型）

## 目标

在**B站源体系**（与番剧、课程同级）下新增「漫画」源类型，把一部漫画作为一个视频源、一话作为一个视频条目、一页作为一个分页条目，
复用现有的源管理、扫描、筛选、重命名、通知、任务队列与下载状态位，最终在本地产出符合主流漫画库（Komga / Kavita / Mihon）规范的
漫画目录与 CBZ 文件。

漫画源**不是外源**：它使用 B站账号凭证（SESSDATA / buvid3 / buvid4）、B站错误码体系与 B站原生源的扫描流程。

## 范围

- 源类型标识：`video_source.type = 3`、`video.source_type = 3`、`AddVideoSourceRequest.source_type = "manga"`。
- 复用字段，不新增漫画专属数据列：
  - 漫画 ID → `video_source.media_id`（如 `25969`）
  - 一话 → `video` 行（`ep_id` / `episode_number` / `name` / `pubtime` / `cover` / `intro` / `upper_name`）
  - 一页 → `page` 行（`pid` / `width` / `height` / `image` / `path` / `download_status`）
- 命名与目录复用现有模板：`video_name_template`、`page_name_template`、`video_name`、`page_name`、`flat_folder`。
- 产物：每话一个 `CBZ`（含 `ComicInfo.xml`），可选 JPEG 转码与长图拼接。
- 不含：付费章节绕过、去水印、EPUB 生成。

## 数据来源（接口）

域名 `manga.bilibili.com`，twirp 协议。实测（2026-10-03）分两类：

| 接口 | 状态 | 用途 |
|---|---|---|
| `comic.v1.Comic/Search`、`HomeHot`、`GetConf`、`AppInit`、`GetEpisode` | 明文 | 搜索、配置、单话元信息 |
| `comic.v1.Comic/ComicDetail` | **加密** | 章节列表 `ep_list` |
| `comic.v1.Comic/GetImageIndex` | **加密** | 每话页面清单 `images[{path,x,y}]`、`host` |
| `comic.v1.Comic/ImageToken` | 请求明文 / 响应加密 | 页面路径 → 带 token 的可下载 URL |

加密协议：

```
POST /twirp/comic.v1.Comic/<Method>?device=pc&platform=web&ultra_sign=<S>&nov=27&a=810
请求体： {"<field>": <value>, "m2": "<AES-CBC 密文>"}
响应体： {"code":0,"msg":"","data":null,"bytesData":"<HMAC-SHA2-256 校验的密文>"}
```

实现载体为站点 Go wasm：

- `efae82c96a7eef44bee5.wasm`：`ultra_sign`，入参 `(query, body, timestamp)`。
- `e461bfa6b471a22c06fc.wasm`：加解密，入参 `(url, bytesData, buvid, platform, bodyJSON)`。

因此**不复刻算法**，通过 sidecar 运行官方 wasm（与现有 `tiktok-signer.cjs` / `douyin-signer.cjs` 同一模式）。

## 下载链路

1. `ComicDetail(media_id)` → `ep_list`，逐话写入 `video` 行。
2. `GetImageIndex(ep_id)` → `images[]`，逐页写入 `page` 行（`pid` = 页序，`width/height` 取 `x/y`）。
3. `ImageToken(images[].path)` → `complete_url`，**立即下载**（token 与完整 URL 绑定，不可缓存、不可改后缀）。
4. 全部页面就绪后打包 CBZ，写 `ComicInfo.xml`，更新 `page.path` 与 `video.download_status`。

图片 CDN 实测：

- 裸 GET 可下载（无需 Cookie / Referer），无 token 或改动 `@1100w.avif` 后缀返回 403。
- 浏览器上下文得到 AVIF（约 150KB/页），普通 HTTP 客户端得到 WebP（约 250–400KB/页）。
- 页漫约 2000×2858；条漫为 1000×600 左右的横切片，需要纵向拼接。

## 目录与命名规范

依据主流库硬性要求（Komga：库根下每个子目录 = 一个系列、每个文件 = 一本；Kavita：系列独占文件夹、`卷/册` 与 `话`
可被解析、ComicInfo 覆盖文件名；Mihon：一个文件夹或一个 CBZ = 一章）。

```
<源 path>/                                  ← 漫画库根
└── 碧蓝之海/                                ← 作品目录（video_name 模板）
    ├── 碧蓝之海 - 0001 第1话.cbz
    ├── 碧蓝之海 - 0089 野岛元的受难.cbz
    ├── 碧蓝之海 - 0132 安排.cbz
    ├── Specials/                            ← 番外 / 特典 / 贺图
    │   └── 碧蓝之海 SP01 贺图.cbz
    ├── cover.jpg                            ← 系列封面
    └── .nomedia                             ← 可选
```

- 序号取 `video.episode_number`（= B漫 `ord`），补 4 位；`title` 为空时回退 `short_title`。
- 特典判定：`type != 0 || extra != 0`，或标题命中「番外 / 特典 / 贺图 / 公告」。
- 特典必须三件套：`Specials/` 目录 + 文件名 `SP%02d` + `ComicInfo.Format = Special`（Kavita 硬性要求）。
- 模板变量复用现有集合：`{{title}}`、`{{ptitle}}`、`{{pid}}`、`{{pid_pad}}`、`{{ep_id}}`、`{{pubtime}}`、`{{upper_name}}`。

CBZ 内部：

```
0001.jpg / 0002.jpg / … / 00NN.jpg
ComicInfo.xml
```

压缩方式使用 store（不压缩），与 Komga 自动转换 CBR→CBZ 的行为一致，便于流式读取。

## ComicInfo.xml 字段映射

| 字段 | 来源 |
|---|---|
| `Series` | `video_source.name` |
| `Number` | `video.episode_number`（补零）或 `SP%02d` |
| `Title` | `video.name` |
| `Summary` | `video.intro`（B漫 `evaluate`） |
| `Writer` / `Penciller` | `video.upper_name`（B漫作者数组） |
| `Genre` | 源级标签（可空） |
| `PageCount` | `page` 行数 |
| `Web` | `https://manga.bilibili.com/detail/mc{video_source.media_id}` |
| `Year` / `Month` / `Day` | `video.pubtime` |
| `LanguageISO` | `zh` |
| `Manga` | 页漫 `YesAndRightToLeft`；条漫 `Yes` |
| `Format` | 特典填 `Special` |

## 落盘策略

采用逐页落盘：`page.image` 指向单页图片文件，逐页复用 `page.download_status` 状态位实现断点续传与单页重试；
一话全部就绪后打包为 `page.path` 指向的 CBZ，并可选删除单页目录。

格式策略：

- 默认保留 CDN 返回格式（WebP）。
- 可选「统一转 JPEG」开关（复用项目 ffmpeg 调用），用于 arm NAS、老设备等对 AVIF/WebP 支持不足的场景。
- 封面固定输出 `cover.jpg`（Komga 的本地封面只支持 jpg/jpeg/png/webp/tbn，不含 avif）。

## 后端改动

| 文件 | 改动 |
|---|---|
| `crates/bili_sync_entity/src/entities/video_source.rs` | `SourceType::Manga = 3`、`VIDEO_SOURCE_TYPE_MANGA`、`is_manga_source_type()` |
| `crates/bili_sync/src/bilibili/manga.rs`（新增） | 接口调用 + 加密 sidecar + 增量流 |
| `crates/bili_sync/src/bilibili/mod.rs` | `VideoInfo::Manga` 变体（放最后）+ 模块导出 |
| `crates/bili_sync/src/adapter/manga.rs`（新增） | `MangaSource` + `VideoSource` 实现 |
| `crates/bili_sync/src/adapter/mod.rs` | `VideoSourceEnum` / `Args::Manga` / `_ActiveModel::Manga` / `video_source_from` |
| `crates/bili_sync/src/utils/scan_id_tracker.rs` | `SourceType::Manga`、`last_scanned_ids.manga`、`last_processed_manga` |
| `crates/bili_sync/src/task/video_downloader.rs` | 加载 `type = 3` 的启用源、类型标签 |
| `crates/bili_sync/src/api/handler.rs` | `add_video_source_internal` 增加 `"manga"` 分支；补 `SourceType.eq(3)` 过滤点 |
| `crates/bili_sync/src/workflow.rs` | `fill_manga_videos`；下载阶段漫画分支（取图 → 打包 → ComicInfo） |

## 前端改动

- `web/src/routes/add-source/+page.svelte`：源类型选项增加「漫画」，支持粘贴 `manga.bilibili.com/detail/mc25969` 自动解析
  `comic_id`；`sourceTypeLabelMap` 增加映射。
- 视频源列表与视频列表增加「漫画」类型标签与筛选。

## 风险

- 加密协议随站点版本漂移（当前站 `1.20.23`，构建于 2026-09-21），必须做运行时校验与失败告警。
- 风控（`SecureCollectSDK` 指纹上报）：sidecar 无浏览器环境，需要限速与失败退避。
- `complete_url` 时效性：必须即时下载。
- 付费章节：`is_locked` 直接跳过，不做绕过。
- 水印：默认保留。

## 验证方式

1. 加密 sidecar：对 `ComicDetail` / `GetImageIndex` / `ImageToken` 三个接口得到与浏览器一致的结构。
2. 下载：取一部免费章节，逐页下载成功且可被图片解码器识别。
3. 打包：CBZ 可被 Komga / Kavita 正确识别为「系列 + 话」，`ComicInfo.xml` 字段生效。
4. 断点续传：中断后重启只重下失败页。
5. 端到端：添加漫画源 → 扫描 → 下载 → 媒体库识别，走完一轮。

## 附录 A：加密链路实测记录（2026-10-03）

### wasm 归属（chunk id → 文件）

| chunk | wasm 文件 | 作用 |
|---|---|---|
| `kb0z` | `2ad56ae2f95bd54cf0b8.wasm` | 生成请求体 `m2` |
| `mCmb` | `efae82c96a7eef44bee5.wasm` | 生成 `ultra_sign` |
| `TFZa` | `e461bfa6b471a22c06fc.wasm` | 解密响应 `bytesData` |
| `aHO9` | `dda35c98742815151e46.wasm` | 风控上报（`logData`/`include`/`headers`） |
| `vY5y` | `ca09620080ee3723f9c8.wasm` | 设备采集 |

### 运行时调用约定（页面插桩实测）

```
m2        = wasm(kb0z).a1_o8iso5(seed)                                  // seed 形如 "934687_8736923"
ultra_sign= wasm(mCmb).<export>(query, bodyJSON, timestampMs)           // 返回 {"error":null,"sign":"<40 字符>"}
plaintext = wasm(TFZa).<export>(url, bytesData, buvid, platform, bodyJSON)
```

- `query` 的规范串实测为 `device=pc&platform=web&nov=27&eot=812`；最终 URL 为
  `/twirp/comic.v1.Comic/<Method>?device=pc&platform=web&ultra_sign=<sign>&nov=27&a=810`。
- `platform` 取 `web`；`buvid` 取 `buvid3`。
- 三个接口的请求体形态：`ComicDetail`/`GetImageIndex` 为 `{ "<id 字段>": <值>, "m2": "<密文>" }`；
  `ImageToken` 为 `{ "urls": "<路径数组的 JSON 字符串>" }`（无 `m2`）。

### Node 侧独立运行（已完成部分）

- 使用官方 `wasm_exec.js`（Go 1.23）加载上述 wasm，可在**无浏览器**环境导出并调用 Go 函数。
- 需要的 JS 垫片：`window`、`document`（含 `cookie`/`getElementById`）、`navigator`、`location`、`screen`、
  `XMLHttpRequest` 占位、canvas/元素通用代理。
- 已能生成服务端接受的 `m2` 与 `ultra_sign`（请求返回 `code 0`，不再出现明文调用的 `code 99`）。

### 尚未打通的环节

Node 侧请求虽然返回 `code 0`，但响应 `bytesData` 为空，需继续定位：

1. 页面生成的 `m2` 长度为 19228 字符，Node 侧仅 824 字符，怀疑 Go 模块依赖 JSVMP 包装层在调用前写入的
   会话/载荷状态（`h1_o8j1i2(seed)` 与 `b1_0ccy7b(seed)` 成对出现）。
2. 浏览器请求原样回放同样返回空 `bytesData`，不排除服务端对 `m2` 做一次性校验。
3. 需要在页面侧对 `b1_0ccy7b` 调用点做快照（`globalThis` 差异、KV SDK、localStorage），据此补齐 Node 侧状态。
