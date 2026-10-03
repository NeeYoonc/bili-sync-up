//! 哔哩哔哩漫画（manga.bilibili.com）接口封装。
//!
//! 与番剧 / 课程都不是同一套接口：
//! - 作品详情：`POST /twirp/comic.v1.Comic/ComicDetail`（响应加密）
//! - 单话页面清单：`POST /twirp/comic.v1.Comic/GetImageIndex`（响应加密）
//! - 页面下载地址：`POST /twirp/comic.v1.Comic/ImageToken`（响应加密）
//!
//! 这三个接口的请求体需要站点 wasm 生成的 `m2`，URL 需要 `ultra_sign`，响应需要
//! 用 `buvid3` 解密，另外还必须携带站点构建产物里的 `x-bili-data-sn` 常量。
//! 算法本身是 Go wasm，直接复刻成本高且会随站点构建漂移，因此这里复用项目已有的
//! 「Node 子进程签名器」模式：把 `scripts/manga-signer.cjs` 内嵌进二进制，首次使用时
//! 释放到 `CONFIG_DIR/tools/manga/`，再用 Node 现场运行站点 wasm。
//!
//! 漫画 URL 形如 <https://manga.bilibili.com/detail/mc25969>，其中的 `25969` 即 `comic_id`。

use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_stream::try_stream;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

use super::VideoInfo;
use crate::config::CONFIG_DIR;

/// 内嵌的漫画签名 sidecar（Node + 站点 wasm）。
const MANGA_SIGNER_JS: &str = include_str!("../../../../scripts/manga-signer.cjs");
/// sidecar 单次调用超时（含首次下载 wasm 的时间）。
const SIDECAR_TIMEOUT_SECS: u64 = 300;

/// 漫画签名器落盘目录（CONFIG_DIR/tools/manga/）。
fn manga_tools_dir() -> PathBuf {
    CONFIG_DIR.join("tools").join("manga")
}

/// sidecar 的 wasm / sn 缓存目录（跨进程复用，避免每次调用重新下载站点资源）。
fn manga_cache_dir() -> PathBuf {
    manga_tools_dir().join("cache")
}

/// 确保签名器已写入磁盘（缺失或长度不符时重新释放）。
async fn ensure_manga_signer() -> Result<PathBuf> {
    let dir = manga_tools_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .context("创建漫画签名器目录失败")?;
    let path = dir.join("manga-signer.cjs");
    let needs_write = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta.len() != MANGA_SIGNER_JS.len() as u64,
        Err(_) => true,
    };
    if needs_write {
        let mut file = tokio::fs::File::create(&path)
            .await
            .with_context(|| format!("创建漫画签名器失败: {}", path.display()))?;
        file.write_all(MANGA_SIGNER_JS.as_bytes())
            .await
            .context("写入漫画签名器失败")?;
        file.flush().await.ok();
    }
    Ok(path)
}

/// 查找 Node.js 运行时：优先 BILI_SYNC_MANGA_NODE，其次常见安装路径与 CONFIG_DIR/tools，最后系统 PATH。
fn find_manga_node() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("BILI_SYNC_MANGA_NODE") {
        let path = PathBuf::from(configured);
        let node_name = if cfg!(windows) { "node.exe" } else { "node" };
        if path.is_file() {
            return Some(path);
        }
        let candidate = if path.is_dir() { path.join(node_name) } else { path };
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    #[cfg(windows)]
    {
        let candidates = [
            PathBuf::from(r"C:\Program Files\nodejs\node.exe"),
            PathBuf::from(r"C:\Program Files (x86)\nodejs\node.exe"),
            CONFIG_DIR.join("bin").join("node.exe"),
            CONFIG_DIR.join("tools").join("node.exe"),
        ];
        if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
            return Some(path);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        let node_name = if cfg!(windows) { "node.exe" } else { "node" };
        std::env::split_paths(&paths)
            .map(|dir| dir.join(node_name))
            .find(|path| path.is_file())
    })
}

/// 把当前 B 站凭证拼成 Cookie 头（漫画接口按站点要求需要登录态）。
fn cookie_header(credential: &super::Credential) -> String {
    let mut parts = vec![
        format!("SESSDATA={}", credential.sessdata),
        format!("bili_jct={}", credential.bili_jct),
        format!("buvid3={}", credential.buvid3),
        format!("DedeUserID={}", credential.dedeuserid),
    ];
    if let Some(buvid4) = &credential.buvid4 {
        parts.push(format!("buvid4={}", buvid4));
    }
    parts.join("; ")
}

/// 调用 sidecar 并解析其 JSON 输出。
///
/// sidecar 约定：stdout 只输出一行 `{"ok":true,"result":...}` 或 `{"ok":false,"error":"..."}`，
/// 日志走 stderr。
async fn run_sidecar(command: &str, args: serde_json::Value) -> Result<serde_json::Value> {
    let signer = ensure_manga_signer().await?;
    let node = find_manga_node().ok_or_else(|| {
        anyhow!(
            "未找到 Node.js 运行时：哔哩哔哩漫画接口的签名/解密依赖站点 wasm，需要 Node.js 执行签名器。请安装 Node.js，或通过环境变量 BILI_SYNC_MANGA_NODE 指定 node 路径"
        )
    })?;

    let config = crate::config::reload_config();
    let credential = config.credential.load_full();

    let mut cmd = tokio::process::Command::new(&node);
    cmd.arg(&signer).arg(command);
    if !args.is_null() {
        cmd.arg(args.to_string());
    }
    cmd.env("MANGA_CACHE", manga_cache_dir());
    if let Some(credential) = credential.as_deref() {
        cmd.env("MANGA_COOKIE", cookie_header(credential));
        cmd.env("MANGA_BUVID3", credential.buvid3.as_str());
    }
    let proxy = config.proxy.trim();
    if !proxy.is_empty() {
        cmd.env("MANGA_PROXY", proxy);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = tokio::time::timeout(Duration::from_secs(SIDECAR_TIMEOUT_SECS), cmd.output())
        .await
        .map_err(|_| anyhow!("漫画签名器执行超时（{}s）", SIDECAR_TIMEOUT_SECS))?
        .map_err(|error| anyhow!("启动漫画签名器失败（Node 运行时不完整？）：{error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    // sidecar 契约是「stdout 只有一行紧凑 JSON」。这里额外要求解析结果必须是带 `ok`
    // 字段的对象，避免误命中多行输出里的裸字符串（例如某个字段值恰好是 `""`）。
    let parsed = stdout
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .find(|value| value.get("ok").is_some())
        .ok_or_else(|| {
            anyhow!(
                "漫画签名器未输出有效结果（退出码 {}）：{}",
                output.status.code().unwrap_or(-1),
                sidecar_output_text(&stderr, &stdout)
            )
        })?;
    if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        bail!(
            "漫画签名器执行失败：{}",
            parsed
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("未知错误")
        );
    }
    if !output.status.success() {
        debug!(
            "漫画签名器退出码非 0 但已返回结果：{}",
            sidecar_output_text(&stderr, &stdout)
        );
    }
    Ok(parsed
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// 把签名器子进程输出截断为可读文本（UTF-8）。
fn sidecar_output_text(stderr: &str, stdout: &str) -> String {
    let text = if stderr.trim().is_empty() { stdout } else { stderr };
    let text = text.trim();
    if text.chars().count() > 400 {
        let tail: String = text.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    } else {
        text.to_string()
    }
}

/// B 漫一话（章节）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MangaEpisode {
    pub ep_id: String,
    pub ord: f64,
    pub title: String,
    pub short_title: String,
    pub image_count: u32,
    pub is_locked: bool,
    pub pubtime: DateTime<Utc>,
    pub cover: String,
    pub size: i64,
    /// `extra != 0` 或 `type != 0` 时视为特典
    pub extra: i64,
    #[serde(rename = "type")]
    pub ep_type: i64,
}

impl MangaEpisode {
    /// 特典 / 番外 / 公告这类不属于正篇的话。
    ///
    /// B 漫里正篇的 `ord` 从 1 开始，贺图/公告这类内容 `ord` 是 0 或 0.4，
    /// 因此 `ord < 1` 本身就是最可靠的判定。
    pub fn is_special(&self) -> bool {
        self.ord < 1.0 || self.extra != 0 || self.ep_type != 0
    }

    /// 正篇集序（特典返回 None）。
    pub fn episode_number(&self) -> Option<i32> {
        if self.is_special() {
            return None;
        }
        let ord = self.ord.round();
        if !ord.is_finite() || ord < 1.0 || ord > i32::MAX as f64 {
            return None;
        }
        Some(ord as i32)
    }

    /// 展示用标题：优先 `title`，为空时回退 `short_title`。
    pub fn display_title(&self) -> String {
        let title = self.title.trim();
        if !title.is_empty() {
            return title.to_string();
        }
        let short = self.short_title.trim();
        if !short.is_empty() {
            return short.to_string();
        }
        format!("第{}话", self.episode_number().unwrap_or_default())
    }
}

/// B 漫作品详情（只保留同步需要的字段）。
#[derive(Debug, Clone)]
pub struct MangaComic {
    pub comic_id: String,
    pub title: String,
    pub author: String,
    pub intro: String,
    pub cover: String,
    pub is_finish: bool,
    pub episodes: Vec<MangaEpisode>,
}

/// B 漫一页。
#[derive(Debug, Clone)]
pub struct MangaPage {
    pub index: u32,
    pub path: String,
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// 从链接 / 分享文案 / 纯数字里解析 `comic_id`。
///
/// 支持 `https://manga.bilibili.com/detail/mc25969`、`mc25969`、`25969` 等形式。
pub fn normalize_comic_id(raw: &str) -> String {
    let text = raw.trim();
    if text.is_empty() {
        return String::new();
    }
    if text.chars().all(|ch| ch.is_ascii_digit()) {
        return text.to_string();
    }
    let lower = text.to_ascii_lowercase();
    if let Some(index) = lower.find("mc") {
        let digits: String = lower[index + 2..]
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            return digits;
        }
    }
    String::new()
}

fn parse_pubtime(raw: &str) -> DateTime<Utc> {
    // 站点返回 "2026-09-07 12:01:01"（北京时间，无时区）
    let naive = NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%d %H:%M:%S").ok();
    match naive {
        Some(naive) => crate::utils::time_format::beijing_timezone()
            .from_local_datetime(&naive)
            .single()
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now),
        None => Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_comic_id;

    #[test]
    fn normalize_comic_id_accepts_url_and_plain_id() {
        assert_eq!(normalize_comic_id("25969"), "25969");
        assert_eq!(normalize_comic_id(" mc25969 "), "25969");
        assert_eq!(
            normalize_comic_id("https://manga.bilibili.com/detail/mc25969?from=share"),
            "25969"
        );
        assert_eq!(normalize_comic_id("https://manga.bilibili.com/detail/mc25969"), "25969");
        assert_eq!(normalize_comic_id("碧蓝之海 mc25969"), "25969");
        assert_eq!(normalize_comic_id(""), "");
        assert_eq!(normalize_comic_id("not-a-comic"), "");
    }
}

fn first_non_empty(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// 拉取作品详情（含完整章节列表）。
pub async fn fetch_comic_detail(comic_id: &str) -> Result<MangaComic> {
    let result = run_sidecar("comic-detail", serde_json::json!({ "comic_id": comic_id })).await?;
    let data = result.get("data").cloned().unwrap_or(serde_json::Value::Null);
    if data.is_null() {
        bail!("漫画 {} 详情为空", comic_id);
    }
    let title = data["title"].as_str().unwrap_or_default().trim().to_string();
    if title.is_empty() {
        bail!("漫画 {} 详情缺少标题（可能是 comic_id 不存在或无权限）", comic_id);
    }
    let author = data["author_name"]
        .as_array()
        .map(|authors| {
            authors
                .iter()
                .filter_map(|author| author.as_str())
                .collect::<Vec<_>>()
                .join("、")
        })
        .unwrap_or_default();
    let cover = first_non_empty(&[
        data["vertical_cover"].as_str().unwrap_or_default(),
        data["horizontal_cover"].as_str().unwrap_or_default(),
        data["square_cover"].as_str().unwrap_or_default(),
    ]);
    let intro = first_non_empty(&[
        data["evaluate"].as_str().unwrap_or_default(),
        data["classic_lines"].as_str().unwrap_or_default(),
    ]);

    let mut episodes = Vec::new();
    for item in data["ep_list"].as_array().cloned().unwrap_or_default() {
        let Some(ep_id) = item["id"].as_i64() else {
            continue;
        };
        episodes.push(MangaEpisode {
            ep_id: ep_id.to_string(),
            ord: item["ord"].as_f64().unwrap_or_default(),
            title: item["title"].as_str().unwrap_or_default().to_string(),
            short_title: item["short_title"].as_str().unwrap_or_default().to_string(),
            image_count: item["image_count"].as_i64().unwrap_or_default().max(0) as u32,
            is_locked: item["is_locked"].as_bool().unwrap_or(false),
            pubtime: parse_pubtime(item["pub_time"].as_str().unwrap_or_default()),
            cover: item["cover"].as_str().unwrap_or_default().to_string(),
            size: item["size"].as_i64().unwrap_or_default(),
            extra: item["extra"].as_i64().unwrap_or_default(),
            ep_type: item["type"].as_i64().unwrap_or_default(),
        });
    }

    Ok(MangaComic {
        comic_id: comic_id.to_string(),
        title,
        author,
        intro,
        cover,
        is_finish: data["is_finish"].as_i64().unwrap_or_default() != 0,
        episodes,
    })
}

/// 给 CDN 直链补上 `code=DanmakuInfo`。
///
/// 实测：`mangaup.hdslb.com`（原图 CDN）对不带该参数的请求直接返回
/// `400 {"code":250617}`；浏览器阅读器请求图片时也带了这个参数，补上后同样一条直链立即 200。
/// sidecar 已经会补，这里再兜一层，避免 sidecar 版本不一致时整话下载失败。
fn with_cdn_code(url: String) -> String {
    if url.is_empty() || url.contains("code=") {
        return url;
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}code=DanmakuInfo")
}

/// 拉取一话的完整页面清单（含带 token 的下载地址）。
///
/// 注意：`complete_url` 与 token 绑定且有较短时效，必须拿到后立刻下载。
pub async fn fetch_chapter_pages(ep_id: &str) -> Result<Vec<MangaPage>> {
    let result = run_sidecar("chapter-pages", serde_json::json!({ "ep_id": ep_id })).await?;
    let mut pages = Vec::new();
    for (index, item) in result["pages"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        let raw_url = item["url"].as_str().unwrap_or_default().to_string();
        if raw_url.is_empty() {
            continue;
        }
        let url = with_cdn_code(raw_url);
        pages.push(MangaPage {
            index: item["index"].as_u64().map(|v| v as u32).unwrap_or(index as u32),
            path: item["path"].as_str().unwrap_or_default().to_string(),
            url,
            width: item["x"].as_u64().map(|v| v as u32).filter(|v| *v > 0),
            height: item["y"].as_u64().map(|v| v as u32).filter(|v| *v > 0),
        });
    }
    if pages.is_empty() {
        bail!("漫画章节 {} 未取到任何页面", ep_id);
    }
    Ok(pages)
}

/// 哔哩哔哩漫画源。
pub struct Manga {
    pub media_id: String,
}

impl Manga {
    pub fn new(media_id: String) -> Self {
        Self { media_id }
    }

    /// 生成漫画章节流。
    ///
    /// `latest_row_at` 为 None 时表示首次全量扫描（特典 / 老章节都需要入库）。
    pub fn to_video_stream_incremental(
        &self,
        latest_row_at: Option<NaiveDateTime>,
    ) -> Pin<Box<dyn Stream<Item = Result<VideoInfo>> + Send>> {
        let media_id = self.media_id.clone();
        Box::pin(try_stream! {
            let comic = fetch_comic_detail(&media_id).await?;
            info!(
                "漫画「{}」共 {} 话，开始解析",
                comic.title,
                comic.episodes.len()
            );

            let mut emitted = 0usize;
            let mut skipped_locked = 0usize;
            let mut skipped_by_time = 0usize;

            for episode in &comic.episodes {
                if episode.is_locked {
                    // 付费章节不做绕过，直接跳过并记数（日志里汇总，避免刷屏）
                    skipped_locked += 1;
                    continue;
                }
                if let Some(latest_time) = latest_row_at {
                    let pubtime_beijing = episode
                        .pubtime
                        .with_timezone(&crate::utils::time_format::beijing_timezone())
                        .naive_local();
                    if pubtime_beijing <= latest_time {
                        skipped_by_time += 1;
                        continue;
                    }
                }

                let display_title = episode.display_title();
                let share_copy = Some(format!("{} {}", comic.title, display_title));
                emitted += 1;
                yield VideoInfo::Manga {
                    title: comic.title.clone(),
                    media_id: comic.comic_id.clone(),
                    ep_id: episode.ep_id.clone(),
                    bvid: episode.ep_id.clone(),
                    cover: first_non_empty(&[episode.cover.as_str(), comic.cover.as_str()]),
                    intro: comic.intro.clone(),
                    pubtime: episode.pubtime,
                    show_title: Some(display_title),
                    episode_number: episode.episode_number(),
                    share_copy,
                    author: Some(comic.author.clone()).filter(|value| !value.is_empty()),
                    is_special: episode.is_special(),
                    page_count: Some(episode.image_count as i32),
                    is_finish: Some(comic.is_finish),
                };
            }

            info!(
                "漫画「{}」解析完成：本次产出 {} 话，跳过付费 {} 话，跳过旧章节 {} 话",
                comic.title, emitted, skipped_locked, skipped_by_time
            );
        })
    }
}
