//! 哔哩哔哩漫画的章节下载与 CBZ 打包。
//!
//! 漫画不走通用视频下载链路（取流 / m4s 合并 / NFO / 弹幕），而是：
//! 1. 调 sidecar 拿到一话全部页面的带 token 直链（token 短时效，必须立刻下载）；
//! 2. 逐页落盘到隐藏暂存目录 `系列目录/.pages/<话名>/`，复用 `page.download_status`
//!    实现断点续传；
//! 3. 全部页面就绪后写成 `ComicInfo.xml` 并打包成 store 模式的 CBZ；
//! 4. 更新 `page.path` / `page.image` / `video.path` 与状态位，并清理暂存目录。
//!
//! 目录布局遵循主流漫画库（Komga / Kavita / Mihon）的硬性要求：
//! ```text
//! <源 path>/
//! └── 碧蓝之海/
//!     ├── 碧蓝之海 - 0001 第1话.cbz
//!     ├── Specials/碧蓝之海 SP01 贺图.cbz
//!     ├── cover.jpg
//!     └── .nomedia
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use sea_orm::prelude::*;
use sea_orm::ActiveValue::Set;
use sea_orm::QueryOrder;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use bili_sync_entity::{page, video};

use crate::adapter::{VideoSource, VideoSourceEnum};
use crate::bilibili::manga::fetch_chapter_pages;
use crate::bilibili::{BiliClient, Client};
use crate::utils::filenamify::filenamify;
use crate::utils::status::{PageStatus, VideoStatus, STATUS_OK};

/// 同一话内页面的并发下载数（CDN 无签名校验，但保持温和的并发避免触发风控）。
const PAGE_CONCURRENCY: usize = 4;
/// 暂存目录名（以点开头，Komga / Kavita 扫描时都会跳过隐藏目录）。
const STAGING_DIR_NAME: &str = ".pages";
/// 特典目录名（Kavita 依赖它把特典归到 `Specials`）。
const SPECIALS_DIR_NAME: &str = "Specials";

/// 下载一话漫画并打包为 CBZ，返回更新后的 video ActiveModel。
pub async fn download_manga_chapter(
    video_source: &VideoSourceEnum,
    video_model: video::Model,
    pages: Vec<page::Model>,
    connection: &DatabaseConnection,
    bili_client: &BiliClient,
    token: CancellationToken,
) -> Result<video::ActiveModel> {
    let media_client = bili_client.client.clone();
    let VideoSourceEnum::MangaSource(manga_source) = video_source else {
        return Err(anyhow!("download_manga_chapter 只接受漫画源"));
    };
    let Some(ep_id) = video_model.ep_id.clone() else {
        return Err(anyhow!("漫画章节「{}」缺少 ep_id", video_model.name));
    };

    let source_name = manga_source.name.clone();
    let series_name = filenamify(&source_name);
    let series_dir = if manga_source.flat_folder {
        manga_source.path.clone()
    } else {
        manga_source.path.join(&series_name)
    };
    tokio::fs::create_dir_all(&series_dir)
        .await
        .with_context(|| format!("创建漫画目录失败: {}", series_dir.display()))?;

    // 话标题：video.name 通常是「作品名 话标题」，这里剥掉作品名前缀
    let chapter_title = chapter_display_title(&video_model, &source_name);
    let is_special = video_model.episode_number.is_none();
    let label = if is_special {
        format!("SP{:02}", resolve_special_index(video_source, &video_model, connection).await?)
    } else {
        format!("{:04}", video_model.episode_number.unwrap_or_default())
    };
    let stem = filenamify(&if is_special {
        format!("{} {} {}", series_name, label, chapter_title)
    } else {
        format!("{} - {} {}", series_name, label, chapter_title)
    });
    let output_dir = if is_special {
        series_dir.join(SPECIALS_DIR_NAME)
    } else {
        series_dir.clone()
    };
    tokio::fs::create_dir_all(&output_dir)
        .await
        .with_context(|| format!("创建漫画输出目录失败: {}", output_dir.display()))?;
    let cbz_path = output_dir.join(format!("{}.cbz", stem));
    let staging_dir = series_dir.join(STAGING_DIR_NAME).join(&stem);

    ensure_series_assets(&series_dir, &video_model, &media_client, &token).await;

    // 1) 取该话全部页面的直链（token 短时效，拿到后立刻下载）
    let fetched = fetch_chapter_pages(&ep_id)
        .await
        .with_context(|| format!("获取漫画章节 {} 的页面清单失败", ep_id))?;
    let page_total = fetched.len();

    // 2) 对齐 page 行（B 漫的 image_count 与实际页面数可能不一致）
    let page_models = reconcile_pages(&video_model, pages, page_total, connection).await?;
    let existing_images: HashMap<i32, Option<String>> = page_models
        .iter()
        .map(|model| (model.pid, model.image.clone()))
        .collect();
    let existing_status: HashMap<i32, u32> = page_models.iter().map(|model| (model.pid, model.download_status)).collect();

    tokio::fs::create_dir_all(&staging_dir)
        .await
        .with_context(|| format!("创建漫画暂存目录失败: {}", staging_dir.display()))?;

    // 3) 逐页下载（跳过已就绪的页，实现断点续传）
    let semaphore = Arc::new(Semaphore::new(PAGE_CONCURRENCY));
    let page_client = media_client.clone();
    // 先把任务摊平成「自有数据」，避免在闭包里捕获 `&MangaPage`（会引入高阶生命周期，
    // 让整个下载 future 无法证明 Send）
    let jobs: Vec<(i32, String)> = fetched
        .iter()
        .enumerate()
        .map(|(index, page_info)| (index as i32 + 1, page_info.url.clone()))
        .collect();
    let mut stream = futures::stream::iter(jobs.into_iter().map(|(pid, url)| {
        let staging_dir = staging_dir.clone();
        let semaphore = Arc::clone(&semaphore);
        let token = token.clone();
        // 每页任务各自持有一份 client 句柄（reqwest::Client 内部是 Arc）
        let client = page_client.clone();
        let cached = existing_images
            .get(&pid)
            .cloned()
            .flatten()
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .or_else(|| staged_page_path(&staging_dir, pid));
        let done = existing_status.get(&pid).copied().unwrap_or(0) >= STATUS_OK && cached.is_some();
        async move {
            if done {
                return (pid, cached.map(Ok));
            }
            let _permit = semaphore.acquire_owned().await;
            let result = download_single_page(&client, &staging_dir, pid, &url, &token).await;
            (pid, Some(result))
        }
    }))
    .buffer_unordered(PAGE_CONCURRENCY);

    let mut downloaded: HashMap<i32, PathBuf> = HashMap::new();
    let mut failures: Vec<String> = Vec::new();
    while let Some((pid, result)) = stream.next().await {
        match result {
            Some(Ok(path)) => {
                downloaded.insert(pid, path);
            }
            Some(Err(error)) => {
                failures.push(format!("第{}页: {:#}", pid, error));
            }
            None => {}
        }
    }
    if !failures.is_empty() {
        // 已成功的页写入数据库，失败页保持未完成状态，下轮直接续传
        persist_page_progress(&video_model, &page_models, &downloaded, &fetched, &cbz_path, connection, false).await?;
        return Err(anyhow!(
            "漫画「{}」第{}话有 {} 页下载失败：{}",
            source_name,
            label,
            failures.len(),
            failures.join("；")
        ));
    }

    // 4) 写 ComicInfo.xml 并打包 CBZ
    let orientation_is_webtoon = fetched
        .first()
        .and_then(|page_info| match (page_info.width, page_info.height) {
            (Some(width), Some(height)) => Some(width > height),
            _ => None,
        })
        .unwrap_or(false);
    let comic_info = build_comic_info(
        &video_model,
        &source_name,
        &series_name,
        &label,
        &chapter_title,
        page_total,
        is_special,
        orientation_is_webtoon,
        manga_source.media_id.as_deref().unwrap_or_default(),
    );
    let comic_info_path = staging_dir.join("ComicInfo.xml");
    tokio::fs::write(&comic_info_path, comic_info.as_bytes())
        .await
        .context("写入 ComicInfo.xml 失败")?;

    let mut entries: Vec<(String, PathBuf)> = Vec::with_capacity(page_total + 1);
    for (index, _page_info) in fetched.iter().enumerate() {
        let pid = index as i32 + 1;
        let path = downloaded
            .get(&pid)
            .cloned()
            .ok_or_else(|| anyhow!("第{}页缺少已下载文件", pid))?;
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("jpg")
            .to_string();
        entries.push((format!("{:04}.{}", pid, ext), path));
    }
    entries.push(("ComicInfo.xml".to_string(), comic_info_path));

    let zip_path = cbz_path.clone();
    let zip_entries = entries.clone();
    tokio::task::spawn_blocking(move || write_cbz(&zip_path, &zip_entries))
        .await
        .context("漫画打包任务失败")??;

    // 5) 落库并清理暂存目录
    persist_page_progress(&video_model, &page_models, &downloaded, &fetched, &cbz_path, connection, true).await?;

    if let Err(error) = tokio::fs::remove_dir_all(&staging_dir).await {
        debug!(
            "清理漫画暂存目录失败（不影响结果）: {} - {}",
            staging_dir.display(),
            error
        );
    }
    // 暂存根目录（`系列目录/.pages`）在这一话清理干净后如果变空，也一并移除以保持漫画库整洁
    let staging_root = series_dir.join(STAGING_DIR_NAME);
    if staging_root != staging_dir && dir_is_empty(&staging_root).await {
        let _ = tokio::fs::remove_dir(&staging_root).await;
    }

    let cbz_size = tokio::fs::metadata(&cbz_path)
        .await
        .map(|meta| meta.len() as i64)
        .unwrap_or_default();

    let completed: u32 = VideoStatus::from([STATUS_OK; 5]).into();
    info!(
        "漫画「{}」第{}话已打包: {}（{} 页，{:.1} MB）",
        source_name,
        label,
        cbz_path.display(),
        page_total,
        cbz_size as f64 / 1024.0 / 1024.0
    );

    let mut active_model: video::ActiveModel = video_model.into();
    active_model.path = Set(cbz_path.to_string_lossy().to_string());
    active_model.download_status = Set(completed);
    active_model.total_file_size_bytes = Set(Some(cbz_size));
    active_model.single_page = Set(Some(page_total <= 1));
    Ok(active_model)
}

/// 话标题：去掉 `video.name` 里的「作品名 」前缀，空标题时回退到「第N话」。
fn chapter_display_title(video_model: &video::Model, source_name: &str) -> String {
    let name = video_model.name.trim();
    let stripped = name
        .strip_prefix(source_name)
        .map(str::trim)
        .filter(|rest| !rest.is_empty())
        .unwrap_or(name);
    if !stripped.is_empty() {
        return stripped.to_string();
    }
    match video_model.episode_number {
        Some(number) => format!("第{}话", number),
        None => "特典".to_string(),
    }
}

/// 计算特典在同源特典里的序号（按发布时间升序，新特典追加在末尾，序号稳定）。
async fn resolve_special_index(
    video_source: &VideoSourceEnum,
    video_model: &video::Model,
    connection: &DatabaseConnection,
) -> Result<usize> {
    let specials = video::Entity::find()
        .filter(video_source.filter_expr())
        .filter(video::Column::EpisodeNumber.is_null())
        .order_by_asc(video::Column::Pubtime)
        .order_by_asc(video::Column::Id)
        .all(connection)
        .await
        .context("查询同源特典失败")?;
    Ok(specials
        .iter()
        .position(|model| model.id == video_model.id)
        .map(|index| index + 1)
        .unwrap_or(specials.len() + 1))
}

/// 保证系列目录存在封面与 `.nomedia`（Komga 本地封面只认 jpg/jpeg/png/webp/tbn）。
async fn ensure_series_assets(
    series_dir: &Path,
    video_model: &video::Model,
    client: &Client,
    token: &CancellationToken,
) {
    let nomedia = series_dir.join(".nomedia");
    if !nomedia.exists() {
        let _ = tokio::fs::write(&nomedia, b"").await;
    }
    let cover = series_dir.join("cover.jpg");
    if cover.exists() || video_model.cover.trim().is_empty() {
        return;
    }
    if token.is_cancelled() {
        return;
    }
    let bytes = match fetch_image_bytes(client, &video_model.cover).await {
        Ok(bytes) => bytes,
        Err(error) => {
            debug!("下载漫画封面失败（不影响下载）: {:#}", error);
            return;
        }
    };
    if let Err(error) = tokio::fs::write(&cover, bytes).await {
        debug!("写入漫画封面失败（不影响下载）: {}", error);
    }
}

/// 把 `page` 行对齐到该话的真实页面数。
async fn reconcile_pages(
    video_model: &video::Model,
    pages: Vec<page::Model>,
    page_total: usize,
    connection: &DatabaseConnection,
) -> Result<Vec<page::Model>> {
    let mut by_pid: HashMap<i32, page::Model> = pages.into_iter().map(|model| (model.pid, model)).collect();
    let mut result = Vec::with_capacity(page_total);
    for index in 0..page_total {
        let pid = index as i32 + 1;
        if let Some(model) = by_pid.remove(&pid) {
            result.push(model);
            continue;
        }
        let inserted = page::ActiveModel {
            video_id: Set(video_model.id),
            cid: Set(0),
            pid: Set(pid),
            name: Set(format!("第{}页", pid)),
            duration: Set(0),
            download_status: Set(0),
            created_at: Set(crate::utils::time_format::now_standard_string()),
            ..Default::default()
        }
        .insert(connection)
        .await
        .with_context(|| format!("插入漫画分页失败: pid={}", pid))?;
        result.push(inserted);
    }
    // 多余的旧分页（B 漫返回的 image_count 偏大时）直接删除，避免出现空页
    if !by_pid.is_empty() {
        let stale: Vec<i32> = by_pid.keys().copied().collect();
        page::Entity::delete_many()
            .filter(page::Column::VideoId.eq(video_model.id))
            .filter(page::Column::Pid.is_in(stale))
            .exec(connection)
            .await
            .context("清理多余漫画分页失败")?;
    }
    Ok(result)
}

/// 暂存目录里已存在的页面文件（不关心扩展名）。
fn staged_page_path(staging_dir: &Path, pid: i32) -> Option<PathBuf> {
    let prefix = format!("{:04}.", pid);
    let entries = std::fs::read_dir(staging_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&prefix) {
            let path = entry.path();
            if std::fs::metadata(&path).map(|meta| meta.len() > 0).unwrap_or(false) {
                return Some(path);
            }
        }
    }
    None
}

/// 漫画图片 CDN 不接受 Range 分片（对 Range 请求直接回 200 + 完整图片），
/// 因此这里不走 UnifiedDownloader 的分片逻辑，直接发一次普通 GET。
async fn fetch_image_bytes(client: &Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .media_request(reqwest::Method::GET, url)
        .header(reqwest::header::REFERER, "https://manga.bilibili.com/")
        .send()
        .await
        .with_context(|| format!("请求漫画图片失败: {}", short_url(url)))?;
    let status = response.status();
    if !status.is_success() {
        bail!(
            "漫画图片 CDN 返回 {}（{}）",
            status.as_u16(),
            short_url(url)
        );
    }
    let bytes = response.bytes().await.context("读取漫画图片响应失败")?;
    if bytes.is_empty() {
        bail!("漫画图片响应为空（{}）", short_url(url));
    }
    Ok(bytes.to_vec())
}

/// 目录不存在或为空时返回 true（用于清理空的暂存目录）。
async fn dir_is_empty(path: &Path) -> bool {
    match tokio::fs::read_dir(path).await {
        Ok(mut entries) => entries.next_entry().await.ok().flatten().is_none(),
        Err(_) => false,
    }
}

/// 日志里只展示域名与路径，避免把带 token 的完整直链写进日志。
fn short_url(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => base.to_string(),
        None => url.to_string(),
    }
}

/// 下载单页到暂存目录，按真实文件头确定扩展名。
async fn download_single_page(
    client: &Client,
    staging_dir: &Path,
    pid: i32,
    url: &str,
    token: &CancellationToken,
) -> Result<PathBuf> {
    if token.is_cancelled() {
        return Err(anyhow!("任务已取消"));
    }
    let bytes = fetch_image_bytes(client, url)
        .await
        .with_context(|| format!("下载第{}页失败", pid))?;
    let Some(ext) = sniff_image_ext(&bytes) else {
        bail!(
            "第{}页拿到的不是图片（{} 字节，首 4 字节 {}）：B 站加密原图（mangaup + cpx）当前本地无法解码",
            pid,
            bytes.len(),
            payload_head_hex(&bytes)
        );
    };
    let target = staging_dir.join(format!("{:04}.{}", pid, ext));
    tokio::fs::write(&target, &bytes)
        .await
        .with_context(|| format!("写入分页失败: {}", target.display()))?;
    Ok(target)
}

/// 按文件头判断图片格式（B 漫 CDN 会按客户端返回 webp / avif / jpeg）。
///
/// 返回 `None` 表示载荷根本不是可识别的图片：B 漫把「加密原图」放在 `mangaup.hdslb.com`，
/// 直链带 `cpx` 参数、ImageToken 里 `hit_encrpyt = true`，落地的是边缘节点逐响应改写的
/// DRM 载荷（首字节固定 `0x08`，尾部仍是明文 JPEG 尾字节）。这种数据本地解不开，
/// 必须当作下载失败——否则 CBZ 里会被塞进一堆非图片字节，分页状态还显示「已完成」。
fn sniff_image_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some("jpg");
    }
    if bytes.len() >= 12 && &bytes[4..12] == b"ftypavif" {
        return Some("avif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    if bytes.len() >= 8 && bytes[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some("png");
    }
    None
}

/// 载荷首几字节的十六进制，用于下载失败时说明到底拿到了什么。
fn payload_head_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(4)
        .map(|byte| format!("{:02X}", byte))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 写入分页进度（打包完成后 `packaged = true`，此时 `page.image` 指向 CBZ 内的页）。
async fn persist_page_progress(
    video_model: &video::Model,
    page_models: &[page::Model],
    downloaded: &HashMap<i32, PathBuf>,
    fetched: &[crate::bilibili::manga::MangaPage],
    cbz_path: &Path,
    connection: &DatabaseConnection,
    packaged: bool,
) -> Result<()> {
    let complete: u32 = PageStatus::from([STATUS_OK; 5]).into();
    let cbz_string = cbz_path.to_string_lossy().to_string();
    for (index, page_info) in fetched.iter().enumerate() {
        let pid = index as i32 + 1;
        let Some(model) = page_models.iter().find(|model| model.pid == pid) else {
            continue;
        };
        let Some(path) = downloaded.get(&pid) else {
            continue;
        };
        let file_size = std::fs::metadata(path).ok().map(|meta| meta.len() as i64);
        let mut active_model: page::ActiveModel = model.clone().into();
        active_model.width = Set(page_info.width);
        active_model.height = Set(page_info.height);
        active_model.download_status = Set(complete);
        active_model.file_size_bytes = Set(file_size);
        active_model.path = Set(Some(cbz_string.clone()));
        if packaged {
            // 打包后暂存目录会被清理，这里改成 CBZ 内的相对路径，避免留下失效路径
            let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("jpg");
            active_model.image = Set(Some(format!("{:04}.{}", pid, ext)));
        } else {
            active_model.image = Set(Some(path.to_string_lossy().to_string()));
        }
        active_model
            .update(connection)
            .await
            .with_context(|| format!("更新漫画分页状态失败: video_id={}, pid={}", video_model.id, pid))?;
    }
    Ok(())
}

/// 生成 ComicInfo.xml（字段全部来自现有列，不新增漫画专属数据列）。
#[allow(clippy::too_many_arguments)]
fn build_comic_info(
    video_model: &video::Model,
    source_name: &str,
    series_name: &str,
    label: &str,
    chapter_title: &str,
    page_total: usize,
    is_special: bool,
    is_webtoon: bool,
    media_id: &str,
) -> String {
    let date = video_model.pubtime.date();
    let mut xml = String::with_capacity(1024);
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str(
        "<ComicInfo xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\n",
    );
    let mut push = |tag: &str, value: &str| {
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        xml.push_str(&format!("  <{tag}>{}</{tag}>\n", escape_xml(value)));
    };
    push("Series", source_name);
    push("Number", label);
    push("Title", chapter_title);
    push("Summary", &video_model.intro);
    push("Writer", &video_model.upper_name);
    push("Penciller", &video_model.upper_name);
    push("Web", &format!("https://manga.bilibili.com/detail/mc{}", media_id));
    push("PageCount", &page_total.to_string());
    push("LanguageISO", "zh");
    push("Year", &date.format("%Y").to_string());
    push("Month", &date.format("%m").to_string());
    push("Day", &date.format("%d").to_string());
    // 页漫为从右往左阅读；条漫（竖切图）没有翻页方向
    push("Manga", if is_webtoon { "Yes" } else { "YesAndRightToLeft" });
    // Kavita 只认 ComicInfo 的 Format=Special（仅文件名含「番外」不生效）
    if is_special {
        push("Format", "Special");
    }
    xml.push_str(&format!("  <Notes>{}</Notes>\n", escape_xml(series_name)));
    xml.push_str("</ComicInfo>\n");
    xml
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 以 store（不压缩）方式打包 CBZ，与 Komga 自动转换 CBR→CBZ 的行为一致，便于流式读取。
fn write_cbz(path: &Path, entries: &[(String, PathBuf)]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("创建目录失败: {}", parent.display()))?;
    }
    // 先写临时文件再改名，避免中途失败留下半截 CBZ 被媒体库扫描到
    let temp = path.with_extension("cbz.part");
    let file = std::fs::File::create(&temp).with_context(|| format!("创建 CBZ 失败: {}", temp.display()))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o644);
    for (name, source) in entries {
        let mut input = std::fs::File::open(source).with_context(|| format!("读取分页失败: {}", source.display()))?;
        zip.start_file(name.as_str(), options)
            .with_context(|| format!("写入 CBZ 条目失败: {}", name))?;
        std::io::copy(&mut input, &mut zip).with_context(|| format!("写入 CBZ 数据失败: {}", name))?;
    }
    zip.finish().context("完成 CBZ 写入失败")?;
    std::fs::rename(&temp, path).with_context(|| format!("重命名 CBZ 失败: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{payload_head_hex, sniff_image_ext};

    #[test]
    fn sniff_accepts_real_image_headers() {
        assert_eq!(sniff_image_ext(b"\xff\xd8\xff\xdb\x00\x84"), Some("jpg"));
        assert_eq!(sniff_image_ext(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("webp"));
        assert_eq!(
            sniff_image_ext(b"\x00\x00\x00\x20ftypavif\x00\x00\x00\x00"),
            Some("avif")
        );
        assert_eq!(sniff_image_ext(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x00"), Some("png"));
    }

    /// B 漫加密原图（mangaup + cpx）的载荷首字节固定 0x08，不能被当成 jpg 收下。
    #[test]
    fn sniff_rejects_bilibili_encrypted_payload() {
        let payload = [0x08u8, 0x71, 0x4B, 0xCD, 0x65, 0xFB, 0x66, 0x36, 0x6E, 0xCB];
        assert_eq!(sniff_image_ext(&payload), None);
        assert_eq!(sniff_image_ext(b""), None);
        assert_eq!(sniff_image_ext(b"<html>403"), None);
        assert_eq!(payload_head_hex(&payload), "08 71 4B CD");
    }
}
