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

图片 CDN 实测（2026-10-03 落地时修正）：

- `ImageToken` 返回的 `complete_url` 指向加密原图 CDN `mangaup.hdslb.com`。裸 GET（不带 `code=DanmakuInfo`）
  会被直接挡回 `HTTP 400 {"code":250617}`；**必须在 URL 上补 `&code=DanmakuInfo`**，同一链路立即 `200` 并返回原图 JPEG。
- 反过来，**不能**再加 `@1100w.avif` 一类阅读器缩放后缀（会 400 / `2410`）。Node 侧拿到的就是原图。
- 实测原图约 1.1–2.0 MB/页、`2000×2858` 级别的页漫；比浏览器阅读器的 1100w 缩略质量更高。
- 加密原图 CDN **不接受 Range 分片**：请求带 `Range` 时返回 `200 OK` 全量体，而项目通用 `UnifiedDownloader`
  会因「Range 响应异常: 200 OK」判定整话下载失败，因此漫画页改用一次性普通 GET（见下文 `Client::media_request`）。
- 条漫为 `1000×600` 左右的横切片，需要纵向拼接（当前版本先按单页落盘，拼接留待后续）。

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
| `crates/bili_sync/src/bilibili/client.rs` | 新增 `media_request()`：一次性普通 GET，绕开 Range 分片 |
| `crates/bili_sync/src/adapter/manga.rs`（新增） | `MangaSource` + `VideoSource` 实现 |
| `crates/bili_sync/src/adapter/mod.rs` | `VideoSourceEnum::MangaSource` / `Args::Manga` / `_ActiveModel::Manga` / `manga_from` |
| `crates/bili_sync/src/manga_download.rs`（新增） | 话级下载（`.pages/` 暂存）→ store 模式 CBZ + `ComicInfo.xml` → 落库与断点续传 |
| `crates/bili_sync/src/utils/scan_id_tracker.rs` | `SourceType::Manga`、`last_scanned_ids.manga`、`last_processed_manga` |
| `crates/bili_sync/src/task/video_downloader.rs` | 加载 `type = 3` 的启用源、类型标签 |
| `crates/bili_sync/src/api/handler.rs` | `add_video_source_internal` 增加 `"manga"` 分支；13 处 `"bangumi \| pugv"` 全部扩为 `\| "manga"`；新增 `bili_source_type_code()` / `bili_source_type_label()` 替代硬编码；新增 `GET /api/manga/comic` |
| `crates/bili_sync/src/api/request.rs` / `response.rs` | `VideosRequest.manga`、`ResetSpecificTasksRequest.manga`、`SourceChargeVisibilityFilters.manga`、dashboard 的 `enabled_manga` / `total_manga` |
| `crates/bili_sync/src/main.rs` / `task/http_server.rs` | 注册 `mod manga_download`、路由与任务字段 |
| `crates/bili_sync/src/workflow.rs` | `fill_manga_videos` / `process_manga_video`；`download_video_pages` 开头的漫画分支（取图 → 打包 → ComicInfo） |

## 前端改动

- `web/src/lib/consts.ts`：新增 `MANGA` 类型与 `BookOpen` 图标。
- `web/src/lib/types.ts` / `api.ts`：`manga` 字段、`VideoCategory`、`MangaComicResponse`、dashboard 统计字段、`getMangaComic()`。
- `web/src/lib/utils/videos.ts`：请求参数带上 `params.manga`。
- `web/src/routes/add-source/+page.svelte`：源类型选项增加「漫画」，支持粘贴 `manga.bilibili.com/detail/mc25969` 等链接自动解析
  `comic_id`（`normalizeComicId`）；输入防抖调用 `fetchMangaComic` 校验并预览作品信息；提交前规范化；
  `sourceTypeLabelMap` 增加映射（顺带补回缺失的 `YouTubeSource` 类型导入）。
- `web/src/routes/+page.svelte`：首页「当前监听」增加漫画统计卡片。
- `web/src/routes/video-sources/+page.svelte`：漫画源显示漫画 ID 行、隐藏不适用的「充电重试」。

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

### 2026-10-03 第二轮实测：已打通主链路

**关键发现：请求必须携带 `x-bili-data-sn` 头**（32 位大写十六进制）。缺少该头时服务端返回 `code 0` 但
`bytesData` 为空（软拒绝）；带上正确值后立即返回完整密文。

实测结论：

| 项 | 结论 |
|---|---|
| `x-bili-data-sn` 是否随机可用 | ❌ 随机值 / 全 0 / 空值均被拒，必须精确匹配 |
| 是否与 cookie 相关 | ❌ 换成随机 buvid3/4、甚至不带登录态，同一浏览器产出的值不变 |
| 是否与 TLS 指纹相关 | ❌ curl_cffi `chrome131/124` 模拟无效 |
| 是否与请求内容相关 | ❌ 与 `m2`、`body`、`url`、时间戳的 MD5 均不匹配 |
| 实际性质 | **设备序列号**：同一机器恒定（实测恒为 `1E74C20E5720FBF3BB351965D7A9DFC1`） |
| `m2` 长度是否重要 | ❌ Node 侧仅 824 字符（页面为 19228）同样被服务端接受 |

**纯 Node（无浏览器）已验证可用的完整链路**：

```
m2        = wasm(2ad56ae2).a1_o8iso5("<id>_<nonce>")                     // 824 字符即可
ultra_sign= wasm(efae82c9).y1_z2w2a3("device=pc&platform=web&nov=27&eot=812", body, tsMs).sign
POST      /twirp/comic.v1.Comic/<Method>?...&ultra_sign=<sign>&nov=27&a=810
          headers: Content-Type / Referer / Origin / Cookie / x-bili-data-sn
plaintext = wasm(e461bfa6).c1_r9k2m7(url, bytesData, buvid3, "web", body)
```

- `ComicDetail(25969)` → 解密得到 **95710 字节** 明文（`{"id":25969,"title":"碧蓝之海",...}`）。
- `GetImageIndex(934687)` → 解密得到 **19 页** 页面清单（与浏览器一致）。

**wasm 导出名（运行时确认，Node 侧可直接调用）**：

| wasm | 导出名 | 签名 |
|---|---|---|
| `2ad56ae2…` | `a1_o8iso5` | `(seed) -> string` |
| `efae82c9…` | `y1_z2w2a3` | `(query, body, timestamp) -> {error, sign}` |
| `e461bfa6…` | `c1_r9k2m7` | `(url, bytesData, buvid, platform, bodyJSON) -> {error, data}` |
| `ca0962…` | `h2_process_report` / `h2_reset_state` | 上报 |
| `dda35c…` | `a1_h17mj9` | 上报（4 参数） |

### 仍未打通：`ImageToken` 的 `m1`

**已破解（第三轮实测）**：`m1` = **客户端 ECDH P-256 公钥**（raw 未压缩点，65 字节，`0x04` 开头）的 base64，
共 88 字符。reader.js 中的实现为：

```js
generateECDHKeyPair()                                    // crypto.subtle.generateKey({name:'ECDH', namedCurve:'P-256'}, ...)
publicKey = crypto.subtle.exportKey('raw', keyPair.publicKey)
m1 = btoa(String.fromCharCode.apply(null, new Uint8Array(publicKey)))
```

Node 侧用 `crypto.createECDH('prime256v1').getPublicKey()` 自行生成同样格式的公钥即可，服务端接受。

### `x-bili-data-sn` 的真实性质

**是站点构建产物中的硬编码常量**（当前 reader.js 中为 `1E74C20E5720FBF3BB351965D7A9DFC1`），
因此跨浏览器、跨 UA、跨视口、跨账号、跨 buvid 全部相同，且随机值会被拒绝。

实现方式：运行时抓取当前 `reader.js`，从字符串表中提取 32 位大写十六进制常量，并用一次探测请求
（`ComicDetail`，校验 `bytesData` 非空）自校验；站点重新构建导致常量变化时自动更新。

### 纯 Node 端到端验证结果（2026-10-03）

```
ComicDetail(25969)     → 解密 95710 字节明文
GetImageIndex(934687)  → 19 页页面清单
ImageToken(urls, m1)   → 19 条 complete_url（自生成 m1 被接受，bytesData 10240）
下载第 1 页            → HTTP 200，image/jpeg，1963934 字节（原始 JPEG，非缩略）
下载第 2 页            → HTTP 200，1333277 字节
```

注意：Node 侧拿到的 `complete_url` 不带 `@1100w.avif` 后缀，返回的是**原图 JPEG**
（约 1.3–2.0 MB/页），比浏览器阅读器的 1100w 缩略质量更高。

## 附录 B：落地记录（2026-10-03，实现完成）

### sidecar 位置与契约

- 实现位置为仓库内 `scripts/manga-signer.cjs`（与 `scripts/tiktok-signer.cjs` / `douyin-signer.cjs` 同级）；
  运行时会被释放/复制到 `CONFIG_DIR/tools/manga/`，wasm 与 `reader.js` 缓存放在 `CONFIG_DIR/tools/manga/cache/`。
- 子命令：`bootstrap`、`comic-detail`、`image-index`、`image-token`、`chapter-pages`、`download`、`serve`。
- `serve` 为常驻模式（stdin 逐行收请求、stdout 逐行回响应），避免每页都重启 Node + 重载 wasm。
- **stdout 契约：每次只输出一行紧凑 JSON**（`ok` 字段为成功标记）。不可用 `JSON.stringify(x, null, 2)` 美化输出，
  否则多行里的裸字符串行会污染 Rust 侧按行解析。
- 环境变量：`MANGA_COOKIE`、`MANGA_BUVID3`、`MANGA_CACHE`、`MANGA_PROXY`。

### 与设计稿的差异

| 设计稿 | 实际实现 | 原因 |
|---|---|---|
| 新增 `tools/manga/` 目录 | 仓库内 `scripts/manga-signer.cjs`，运行时释放到 `CONFIG_DIR/tools/manga/` | 与既有 signer 的发布方式一致（单一源文件 + 运行时释放） |
| 页面用通用下载器 | 新增 `Client::media_request()`（`crates/bili_sync/src/bilibili/client.rs`） | `mangaup` 原图 CDN 拒绝 Range，通用分片逻辑会误判失败 |
| 直链直接用 `complete_url` | `with_cdn_code()` 统一补 `&code=DanmakuInfo` | 缺该参数直接 400 / `250617` |
| 新增独立 `/api/manga/...` 命名空间 | 只新增 `GET /api/manga/comic`（供前端解析/校验漫画 ID），其余全部复用现有源接口 | 「全部复用」 |
| 默认保留 CDN 返回格式（WebP） | 实际保存原图 JPEG | 原图链路不带缩放后缀，拿到的就是 JPEG |
| 逐页落盘 + 打包 CBZ | 一致：话级下载到 `<作品目录>/.pages/<话>/` 暂存，全部就绪后打包 CBZ 并回收空 `.pages` | — |

### 端到端实测（真实账号，2026-10-03）

运行环境：临时配置目录、端口 `12345`。流程与结果：

1. `GET /api/manga/comic?comic_id=<完整链接>` → `200`，返回《碧蓝之海》、作者、137 话。
2. `POST /api/video-sources {source_type:"manga", source_id:"25969"}` → 建源成功，`source_id = 1`。
3. 白名单 `野岛元的受难` → 扫描 → 下载 → 日志 `漫画「碧蓝之海」第0089话已打包 …（19 页，27.2 MB）`；
   数据库 `video.download_status` 为完成态、19 个 `page` 全部完成且 `path` 指向同一个 CBZ。
4. CBZ 校验：20 个条目（`0001.jpg`…`0019.jpg` + `ComicInfo.xml`），`compress_type = 0 (Stored)`，
   `ComicInfo.xml` 的 Series / Number / Title / Summary / Writer / Penciller / Web / PageCount /
   LanguageISO / Year / Month / Day / Manga=YesAndRightToLeft 全部正确，JPEG magic `ff d8 ff`。
5. 特典流程：白名单 `贺图,出版社声明` → 产出 `Specials/碧蓝之海 SP01 贺图.cbz` 与
   `Specials/碧蓝之海 SP02 出版社声明.cbz`，`ComicInfo.xml` 含 `Number=SP01/SP02`、`Format=Special`。

### 已知遗留（非阻塞）

- `reset_video_source_path` 对漫画源仍走番剧那套「移动单文件」逻辑；漫画是「作品目录 + CBZ」，重设路径后需要人工复核。
- 漫画源的 AI 重命名路径未做实测。
- 特典 `SP` 序号按 `pubtime` 排序，若之后补充更早发布的特典，已有编号可能整体漂移。
- 前端「批量添加漫画」未测试。
