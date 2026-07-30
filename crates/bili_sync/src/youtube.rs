//! YouTube 作为持续扫描的视频源接入。
//!
//! `yt-dlp` 仅处理 YouTube 的播放列表枚举和媒体直链解析；调度周期、暂停控制、
//! 并发上限、来源持久化及下载状态均由 bili-sync 管理。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::{Extension, Json, Path as AxumPath, Query};
use chrono::{Local, NaiveDate, TimeZone};
use futures::{stream, SinkExt, StreamExt};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use bili_sync_entity::{youtube_source, youtube_video};

use crate::api::wrapper::{ApiError, ApiResponse};
use crate::api::{
    request::{ResetSpecificTasksRequest, UpdateVideoStatusRequest, VideosRequest},
    response::{
        DeleteVideoResponse, PageInfo, ResetAllVideosResponse, ResetVideoResponse, SubmissionVideoInfo,
        SubmissionVideosResponse, UpdateVideoStatusResponse, VideoInfo, VideoResponse, VideoSourceTag, VideosResponse,
    },
};
use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};
use crate::config::CONFIG_DIR;
use crate::task::TASK_CONTROLLER;
use crate::unified_downloader::UnifiedDownloader;
use crate::utils::live_updates::{notify_queue_status_changed, notify_video_sources_changed, notify_videos_changed};
use crate::utils::time_format::now_standard_string;

const YTDLP_VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const YTDLP_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const YTDLP_RELEASE_BASE_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(90);
const YOUTUBE_LOGIN_DEBUG_PORT: u16 = 38491;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const MAX_DOWNLOAD_RETRIES: i32 = 4;
const YTDLP_TEST_VIDEO: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
const SUBSCRIPTIONS_URL: &str = "https://www.youtube.com/feed/subscriptions";
const LIKED_URL: &str = "https://www.youtube.com/playlist?list=LL";
const WATCH_LATER_URL: &str = "https://www.youtube.com/playlist?list=WL";
static YOUTUBE_SIDECAR_BACKFILL_DONE: AtomicBool = AtomicBool::new(false);
static YTDLP_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
struct YtDlpPackage {
    target_key: &'static str,
    asset_name: &'static str,
    binary_name: &'static str,
    archive_binary_name: Option<&'static str>,
}

#[derive(Debug, Deserialize)]
pub struct YouTubeLoginRequest {
    pub browser: String,
}

/// 由浏览器扩展导出的 Netscape `cookies.txt` 内容。主流 yt-dlp 前端以该格式
/// 作为稳定登录交接格式，避免直接读取运行中浏览器的锁定 Cookie 数据库。
#[derive(Debug, Deserialize)]
pub struct YouTubeCookieImportRequest {
    pub cookies: String,
}

#[derive(Debug, Deserialize)]
pub struct YouTubeSearchRequest {
    pub keyword: String,
    pub source_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct YouTubeSearchResult {
    pub result_type: String,
    pub title: String,
    pub author: String,
    pub youtube_url: String,
    pub channel_id: Option<String>,
    pub cover: String,
    pub description: String,
    pub follower: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct YouTubeSearchResponse {
    pub success: bool,
    pub results: Vec<YouTubeSearchResult>,
    pub total: usize,
}

#[derive(Debug, Deserialize)]
pub struct YouTubeSourceVideosRequest {
    pub url: String,
    pub source_type: String,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub keyword: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateYouTubeSourceRequest {
    pub source_type: String,
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    pub path: String,
    #[serde(default)]
    pub audio_only: bool,
    #[serde(default)]
    pub audio_only_m4a_only: bool,
    #[serde(default)]
    pub flat_folder: bool,
    pub download_danmaku: Option<bool>,
    #[serde(default)]
    pub download_subtitle: bool,
    pub ai_subtitle_language: Option<String>,
    pub filter_option: Option<FilterOption>,
    #[serde(default)]
    pub blacklist_keywords: Vec<String>,
    #[serde(default)]
    pub whitelist_keywords: Vec<String>,
    pub case_sensitive: Option<bool>,
    pub min_duration_seconds: Option<i32>,
    pub max_duration_seconds: Option<i32>,
    pub published_after: Option<String>,
    pub published_before: Option<String>,
    #[serde(default)]
    pub selected_videos: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateYouTubeSourceEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateYouTubeSourceRequest {
    pub name: Option<String>,
    pub path: Option<String>,
    pub audio_only: Option<bool>,
    pub audio_only_m4a_only: Option<bool>,
    pub flat_folder: Option<bool>,
    /// YouTube 中对应直播聊天回放文件。
    pub download_danmaku: Option<bool>,
    pub download_subtitle: Option<bool>,
    pub ai_subtitle_language: Option<String>,
    #[serde(default)]
    pub filter_option: Option<Option<FilterOption>>,
    pub inherit_filter_option: Option<bool>,
    pub blacklist_keywords: Option<Vec<String>>,
    pub whitelist_keywords: Option<Vec<String>>,
    pub case_sensitive: Option<bool>,
    pub min_duration_seconds: Option<Option<i32>>,
    pub max_duration_seconds: Option<Option<i32>>,
    pub published_after: Option<Option<String>>,
    pub published_before: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteYouTubeSourceRequest {
    #[serde(default)]
    pub delete_local_files: bool,
}

#[derive(Debug, Deserialize)]
pub struct ResetYouTubeSourcePathRequest {
    pub new_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct YouTubeQueueStatusResponse {
    pub pending: u64,
    pub downloading: u64,
    pub completed: u64,
    pub failed: u64,
    pub tasks: Vec<YouTubeVideoResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct YouTubeSourceResponse {
    pub id: i32,
    pub source_type: String,
    pub name: String,
    pub url: String,
    pub path: String,
    pub enabled: bool,
    pub audio_only: bool,
    pub audio_only_m4a_only: bool,
    pub flat_folder: bool,
    pub download_danmaku: bool,
    pub download_subtitle: bool,
    pub ai_subtitle_language: String,
    pub filter_option: Option<FilterOption>,
    pub blacklist_keywords: Vec<String>,
    pub whitelist_keywords: Vec<String>,
    pub case_sensitive: bool,
    pub min_duration_seconds: Option<i32>,
    pub max_duration_seconds: Option<i32>,
    pub published_after: Option<String>,
    pub published_before: Option<String>,
    pub selected_videos: Vec<String>,
    pub last_scan_at: Option<String>,
    pub pending_count: u64,
    pub completed_count: u64,
    pub failed_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct YouTubeVideoResponse {
    pub id: i32,
    pub source_id: i32,
    pub youtube_id: String,
    pub url: String,
    pub title: String,
    pub uploader: String,
    pub thumbnail: Option<String>,
    pub published_at: Option<String>,
    pub duration_seconds: Option<i32>,
    pub download_status: String,
    pub retry_count: i32,
    pub output_path: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct YouTubeStatusResponse {
    pub ytdlp_available: bool,
    pub ytdlp_version: Option<String>,
    pub logged_in: bool,
    pub default_output_path: String,
}

#[derive(Debug, Serialize)]
pub struct YouTubeLoginResponse {
    pub logged_in: bool,
    pub message: String,
}

pub async fn youtube_status() -> Result<ApiResponse<YouTubeStatusResponse>, ApiError> {
    // 与 aria2 一致：第一次进入设置页/添加源页时若本机没有 yt-dlp，
    // 自动下载当前系统对应的官方可执行文件。
    let _ = ensure_ytdlp_available().await;
    let version = ytdlp_version().await;
    Ok(ApiResponse::ok(YouTubeStatusResponse {
        ytdlp_available: version.is_some(),
        ytdlp_version: version,
        logged_in: has_youtube_session(&cookie_path()),
        default_output_path: default_output_path().display().to_string(),
    }))
}

pub async fn search_youtube(
    Query(request): Query<YouTubeSearchRequest>,
) -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    ensure_ytdlp_available().await?;
    let keyword = request.keyword.trim();
    if keyword.is_empty() {
        return Err(ApiError::from(anyhow!("请输入 YouTube 搜索关键词")));
    }
    let source_type = request.source_type.trim().to_ascii_lowercase();
    if !matches!(source_type.as_str(), "channel" | "playlist") {
        return Err(ApiError::from(anyhow!("YouTube 搜索仅支持频道或播放列表")));
    }

    let search_url = youtube_search_url(keyword, &source_type)?;
    let mut command = Command::new(ytdlp_executable());
    command.args([
        "--flat-playlist",
        "--dump-json",
        "--playlist-end",
        "30",
        "--ignore-errors",
        "--no-warnings",
    ]);
    append_ytdlp_runtime(&mut command);
    append_cookies(&mut command);
    command.arg(search_url.as_str());
    let output = tokio::time::timeout(Duration::from_secs(2 * 60), command.output())
        .await
        .map_err(|_| anyhow!("搜索 YouTube 来源超时"))??;
    if !output.status.success() {
        return Err(ApiError::from(anyhow!(
            "yt-dlp 搜索 YouTube 来源失败：{}",
            command_error(&output)
        )));
    }

    let mut seen_urls = HashSet::new();
    let mut results = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(item) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(url) = item
            .get("uploader_url")
            .filter(|_| source_type == "channel")
            .or_else(|| item.get("channel_url").filter(|_| source_type == "channel"))
            .or_else(|| item.get("webpage_url"))
            .or_else(|| item.get("url"))
            .and_then(|value| value.as_str())
            .filter(|value| value.starts_with("http"))
            .map(str::to_string)
        else {
            continue;
        };
        let url = if source_type == "channel" {
            canonical_channel_url(&url)
        } else {
            url
        };
        if !seen_urls.insert(url.clone()) {
            continue;
        }
        let title = item
            .get("title")
            .or_else(|| item.get("channel"))
            .or_else(|| item.get("uploader"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("未命名 YouTube 来源")
            .to_string();
        let author = item
            .get("uploader_id")
            .or_else(|| item.get("uploader"))
            .or_else(|| item.get("channel"))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let cover = item
            .get("thumbnails")
            .and_then(|value| value.as_array())
            .and_then(|items| {
                items
                    .iter()
                    .filter_map(|thumbnail| {
                        let url = thumbnail.get("url")?.as_str()?;
                        let width = thumbnail
                            .get("width")
                            .and_then(|value| value.as_i64())
                            .unwrap_or_default();
                        let height = thumbnail
                            .get("height")
                            .and_then(|value| value.as_i64())
                            .unwrap_or_default();
                        Some((width.saturating_mul(height), url))
                    })
                    .max_by_key(|(area, _)| *area)
                    .map(|(_, url)| url)
            })
            .unwrap_or("");
        let cover = if cover.starts_with("//") {
            format!("https:{cover}")
        } else {
            cover.to_string()
        };
        results.push(YouTubeSearchResult {
            result_type: format!("youtube_{source_type}"),
            title,
            author,
            youtube_url: url,
            channel_id: item
                .get("channel_id")
                .or_else(|| item.get("id").filter(|_| source_type == "channel"))
                .and_then(|value| value.as_str())
                .map(str::to_string),
            cover,
            description: item
                .get("description")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            follower: item.get("channel_follower_count").and_then(|value| value.as_i64()),
        });
    }
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

/// 枚举频道/播放列表中的视频。响应直接复用 B 站投稿选择面板的数据结构，
/// 让添加 YouTube 来源与现有“投稿”流程共用同一套列表、搜索和选择 UI。
pub async fn get_youtube_source_videos(
    Query(request): Query<YouTubeSourceVideosRequest>,
) -> Result<ApiResponse<SubmissionVideosResponse>, ApiError> {
    ensure_ytdlp_available().await?;
    let source_type = request.source_type.trim().to_ascii_lowercase();
    if !matches!(source_type.as_str(), "channel" | "playlist") {
        return Err(ApiError::from(anyhow!("仅频道和播放列表支持历史视频选择")));
    }
    let raw_url = request.url.trim();
    if raw_url.is_empty() || !is_youtube_url(raw_url) {
        return Err(ApiError::from(anyhow!("请输入有效的 YouTube 频道或播放列表链接")));
    }
    let page = request.page.unwrap_or(1).max(1);
    let page_size = request.page_size.unwrap_or(100).clamp(1, 200);
    let keyword = request
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let source_url = if source_type == "channel" {
        canonical_channel_url(raw_url)
    } else {
        raw_url.to_string()
    };

    // 多取一条用于判断是否还有下一页。普通频道通常一次即可返回完整列表；
    // 大频道继续沿用投稿面板的“加载更多”交互。
    let start = (page - 1).saturating_mul(page_size).saturating_add(1);
    let end_with_probe = start.saturating_add(page_size);
    let mut command = Command::new(ytdlp_executable());
    command.args([
        "--flat-playlist",
        "--dump-json",
        "--ignore-errors",
        "--no-warnings",
        "--playlist-start",
        &start.to_string(),
        "--playlist-end",
        &end_with_probe.to_string(),
    ]);
    append_ytdlp_runtime(&mut command);
    append_cookies(&mut command);
    command.arg(&source_url);
    let output = tokio::time::timeout(Duration::from_secs(10 * 60), command.output())
        .await
        .map_err(|_| anyhow!("加载 YouTube 历史视频超时"))??;
    if !output.status.success() {
        return Err(ApiError::from(anyhow!(
            "加载 YouTube 历史视频失败：{}",
            command_error(&output)
        )));
    }

    let mut videos = Vec::new();
    let mut seen = HashSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(item) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(id) = item
            .get("id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let title = item
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or(id)
            .to_string();
        if keyword
            .as_ref()
            .is_some_and(|keyword| !title.to_ascii_lowercase().contains(keyword))
        {
            continue;
        }
        let cover = item
            .get("thumbnail")
            .and_then(|value| value.as_str())
            .or_else(|| {
                item.get("thumbnails")
                    .and_then(|value| value.as_array())
                    .and_then(|items| items.last())
                    .and_then(|thumbnail| thumbnail.get("url"))
                    .and_then(|value| value.as_str())
            })
            .unwrap_or("")
            .to_string();
        let duration = item
            .get("duration")
            .and_then(|value| value.as_f64())
            .and_then(|value| i32::try_from(value.round() as i64).ok())
            .unwrap_or_default();
        let view = item
            .get("view_count")
            .and_then(|value| value.as_i64())
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or_default();
        videos.push(SubmissionVideoInfo {
            bvid: id.to_string(),
            title,
            cover,
            pubtime: item
                .get("upload_date")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            duration,
            view,
            danmaku: 0,
            description: item
                .get("description")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    let has_more = videos.len() > page_size as usize;
    videos.truncate(page_size as usize);
    let total = i64::from(start - 1) + videos.len() as i64 + i64::from(has_more);
    Ok(ApiResponse::ok(SubmissionVideosResponse {
        videos,
        total,
        page,
        page_size,
    }))
}

pub async fn import_youtube_login(
    Json(request): Json<YouTubeLoginRequest>,
) -> Result<ApiResponse<YouTubeLoginResponse>, ApiError> {
    let browser = normalize_browser(&request.browser)?;
    ensure_ytdlp_available().await?;
    let cookie_file = cookie_path();
    prepare_parent(&cookie_file).await?;
    let output = tokio::time::timeout(LOGIN_TIMEOUT, async {
        Command::new(ytdlp_executable())
            .args(["--cookies-from-browser", browser])
            .arg("--cookies")
            .arg(&cookie_file)
            .args(["--skip-download", "--no-playlist", "--no-warnings", YTDLP_TEST_VIDEO])
            .output()
            .await
    })
    .await
    .map_err(|_| ApiError::from(anyhow!("导入浏览器登录状态超时")))??;
    if !output.status.success() {
        let error = command_error(&output);
        if error.contains("Failed to decrypt with DPAPI") || error.contains("App-Bound Encryption") {
            return Err(ApiError::from(anyhow!(
                "浏览器默认资料目录受 Chromium App-Bound Encryption 保护，无法直接读取。请点击“打开登录窗口”，在专用窗口登录后点击“完成登录”；也可导入 cookies.txt。"
            )));
        }
        return Err(ApiError::from(anyhow!("导入浏览器 Cookie 失败：{}", error)));
    }
    if !has_youtube_session(&cookie_file) {
        return Err(ApiError::from(anyhow!(
            "未找到 YouTube 登录会话；请先在 {} 登录 YouTube 后重试",
            browser
        )));
    }
    Ok(ApiResponse::ok(YouTubeLoginResponse {
        logged_in: true,
        message: format!("已从 {} 导入 YouTube 登录状态", browser),
    }))
}

/// 启动一个由本应用管理的干净浏览器资料目录，并开启 CDP 端口。
/// 这是 Chromium 的 App-Bound Encryption 出现后，开源下载器常用的稳定替代：
/// 不尝试解密用户默认资料目录，而是让用户在该专用窗口完成一次 Google 登录，
/// 再经浏览器的官方调试协议导出本机 Cookie。
pub async fn start_interactive_youtube_login(
    Json(request): Json<YouTubeLoginRequest>,
) -> Result<ApiResponse<YouTubeLoginResponse>, ApiError> {
    let browser = normalize_browser(&request.browser)?;
    let executable = browser_executable(browser)?;
    let profile = interactive_login_profile(browser);
    tokio::fs::create_dir_all(&profile).await?;
    Command::new(executable)
        .arg(format!("--remote-debugging-port={YOUTUBE_LOGIN_DEBUG_PORT}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "https://accounts.google.com/ServiceLogin?service=youtube",
        ])
        .spawn()
        .context("启动 YouTube 登录浏览器失败")?;
    Ok(ApiResponse::ok(YouTubeLoginResponse {
        logged_in: false,
        message: "已打开专用浏览器窗口。请在窗口中登录 YouTube，完成后回到这里点击“完成登录”".to_string(),
    }))
}

pub async fn complete_interactive_youtube_login() -> Result<ApiResponse<YouTubeLoginResponse>, ApiError> {
    ensure_ytdlp_available().await?;
    let endpoint = format!("http://127.0.0.1:{YOUTUBE_LOGIN_DEBUG_PORT}/json/version");
    let version: serde_json::Value = reqwest::Client::new()
        .get(&endpoint)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .context("未找到专用登录窗口；请先点击“打开登录窗口”")?
        .error_for_status()
        .context("无法连接专用登录窗口；请保持该窗口打开")?
        .json()
        .await
        .context("读取登录窗口状态失败")?;
    let ws_url = version
        .get("webSocketDebuggerUrl")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("登录窗口未启用 Cookie 导出接口；请关闭后重新打开登录窗口"))?;
    let (mut socket, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .context("连接登录窗口失败")?;
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            serde_json::json!({"id": 1, "method": "Storage.getCookies"})
                .to_string()
                .into(),
        ))
        .await
        .context("请求浏览器 Cookie 失败")?;
    let cookies = loop {
        let Some(message) = socket.next().await else {
            return Err(ApiError::from(anyhow!("登录窗口意外关闭")));
        };
        let message = message.context("读取浏览器 Cookie 失败")?;
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            let payload: serde_json::Value = serde_json::from_str(&text).context("解析浏览器 Cookie 响应失败")?;
            if payload.get("id").and_then(|value| value.as_i64()) == Some(1) {
                break payload
                    .get("result")
                    .and_then(|result| result.get("cookies"))
                    .and_then(|value| value.as_array())
                    .ok_or_else(|| anyhow!("登录窗口没有返回 Cookie"))?
                    .clone();
            }
        }
    };
    let contents = cdp_cookies_to_netscape(&cookies);
    if !is_netscape_youtube_cookie_file(&contents) {
        return Err(ApiError::from(anyhow!(
            "未检测到 youtube.com 域的登录会话。请在专用窗口打开 YouTube 首页，确认右上角已显示账号头像，保持窗口不要关闭后再点击完成登录"
        )));
    }
    let path = cookie_path();
    prepare_parent(&path).await?;
    let temporary = path.with_extension("txt.validating");
    tokio::fs::write(&temporary, contents)
        .await
        .context("写入 YouTube 登录 Cookie 失败")?;
    if let Err(error) = validate_youtube_login_cookie(&temporary).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(ApiError::from(anyhow!(
            "YouTube 登录会话验证失败：{}。请在专用窗口确认已登录 YouTube 首页后重试",
            error
        )));
    }
    tokio::fs::rename(&temporary, &path)
        .await
        .context("保存 YouTube 登录 Cookie 失败")?;
    Ok(ApiResponse::ok(YouTubeLoginResponse {
        logged_in: true,
        message: "YouTube 登录状态已验证并导入".to_string(),
    }))
}

pub async fn import_youtube_cookie_file(
    Json(request): Json<YouTubeCookieImportRequest>,
) -> Result<ApiResponse<YouTubeLoginResponse>, ApiError> {
    ensure_ytdlp_available().await?;
    if !is_netscape_youtube_cookie_file(&request.cookies) {
        return Err(ApiError::from(anyhow!(
            "文件不是包含 YouTube 会话的 Netscape cookies.txt；请在已登录 YouTube 的浏览器中导出 cookies.txt"
        )));
    }
    let path = cookie_path();
    prepare_parent(&path).await?;
    let temporary = path.with_extension("txt.importing");
    tokio::fs::write(&temporary, request.cookies.as_bytes())
        .await
        .context("写入 YouTube cookies.txt 失败")?;
    if let Err(error) = validate_youtube_login_cookie(&temporary).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(ApiError::from(anyhow!("YouTube cookies.txt 验证失败：{}", error)));
    }
    tokio::fs::rename(&temporary, &path)
        .await
        .context("保存 YouTube cookies.txt 失败")?;

    Ok(ApiResponse::ok(YouTubeLoginResponse {
        logged_in: true,
        message: "已导入 cookies.txt；订阅、喜欢和稍后再看将在下一次扫描时使用此登录状态".to_string(),
    }))
}

pub async fn get_youtube_sources(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<Vec<YouTubeSourceResponse>>, ApiError> {
    let sources = youtube_source::Entity::find()
        .order_by_desc(youtube_source::Column::Id)
        .all(db.as_ref())
        .await?;
    let mut response = Vec::with_capacity(sources.len());
    for source in sources {
        response.push(source_response(db.as_ref(), source).await?);
    }
    Ok(ApiResponse::ok(response))
}

pub async fn create_youtube_source(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<CreateYouTubeSourceRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    let source_type = normalize_source_type(&request.source_type)?;
    let url = resolve_source_url(source_type, request.url.as_deref())?;
    if !is_youtube_url(&url) {
        return Err(ApiError::from(anyhow!("频道或播放列表必须是有效的 YouTube 链接")));
    }
    let path = request.path.trim();
    if path.is_empty() {
        return Err(ApiError::from(anyhow!("下载目录不能为空")));
    }
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::from(anyhow!("视频源名称不能为空")));
    }
    let model = youtube_source::ActiveModel {
        source_type: Set(source_type.to_string()),
        name: Set(name.to_string()),
        url: Set(url),
        path: Set(path.to_string()),
        enabled: Set(true),
        audio_only: Set(request.audio_only),
        audio_only_m4a_only: Set(request.audio_only_m4a_only),
        flat_folder: Set(request.flat_folder),
        download_danmaku: Set(request.download_danmaku.unwrap_or(true)),
        download_subtitle: Set(request.download_subtitle),
        ai_subtitle_language: Set(request
            .ai_subtitle_language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("zh-CN")
            .to_string()),
        filter_option: Set(request.filter_option.map(serde_json::to_value).transpose()?),
        blacklist_keywords: Set((!request.blacklist_keywords.is_empty())
            .then(|| serde_json::to_string(&request.blacklist_keywords))
            .transpose()?),
        whitelist_keywords: Set((!request.whitelist_keywords.is_empty())
            .then(|| serde_json::to_string(&request.whitelist_keywords))
            .transpose()?),
        keyword_case_sensitive: Set(request.case_sensitive.unwrap_or(true)),
        min_duration_seconds: Set(request.min_duration_seconds),
        max_duration_seconds: Set(request.max_duration_seconds),
        published_after: Set(request.published_after.filter(|value| !value.trim().is_empty())),
        published_before: Set(request.published_before.filter(|value| !value.trim().is_empty())),
        selected_videos: Set((!request.selected_videos.is_empty())
            .then(|| serde_json::to_string(&request.selected_videos))
            .transpose()?),
        last_scan_at: Set(None),
        created_at: Set(now_standard_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let response = source_response(db.as_ref(), model).await?;
    notify_video_sources_changed();
    Ok(ApiResponse::ok(response))
}

pub async fn update_youtube_source_enabled(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<UpdateYouTubeSourceEnabledRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    let Some(model) = youtube_source::Entity::find_by_id(id).one(db.as_ref()).await? else {
        return Err(ApiError::from(anyhow!("YouTube 视频源不存在")));
    };
    let mut active: youtube_source::ActiveModel = model.into();
    active.enabled = Set(request.enabled);
    let model = active.update(db.as_ref()).await?;
    notify_video_sources_changed();
    Ok(ApiResponse::ok(source_response(db.as_ref(), model).await?))
}

pub async fn update_youtube_source(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<UpdateYouTubeSourceRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    let Some(model) = youtube_source::Entity::find_by_id(id).one(db.as_ref()).await? else {
        return Err(ApiError::from(anyhow!("YouTube 视频源不存在")));
    };
    let mut active: youtube_source::ActiveModel = model.into();
    if let Some(name) = request.name {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::from(anyhow!("视频源名称不能为空")));
        }
        active.name = Set(name.to_string());
    }
    if let Some(path) = request.path {
        let path = path.trim();
        if path.is_empty() {
            return Err(ApiError::from(anyhow!("下载目录不能为空")));
        }
        active.path = Set(path.to_string());
    }
    if let Some(audio_only) = request.audio_only {
        active.audio_only = Set(audio_only);
    }
    if let Some(value) = request.audio_only_m4a_only {
        active.audio_only_m4a_only = Set(value);
    }
    if let Some(value) = request.flat_folder {
        active.flat_folder = Set(value);
    }
    if let Some(value) = request.download_danmaku {
        active.download_danmaku = Set(value);
    }
    if let Some(download_subtitle) = request.download_subtitle {
        active.download_subtitle = Set(download_subtitle);
    }
    if let Some(language) = request.ai_subtitle_language {
        let language = language.trim();
        active.ai_subtitle_language = Set(if language.is_empty() {
            "zh-CN".to_string()
        } else {
            language.to_string()
        });
    }
    if request.inherit_filter_option == Some(true) {
        active.filter_option = Set(None);
    } else if let Some(filter_option) = request.filter_option {
        active.filter_option = Set(filter_option.map(serde_json::to_value).transpose()?);
    }
    if let Some(keywords) = request.blacklist_keywords {
        active.blacklist_keywords = Set((!keywords.is_empty())
            .then(|| serde_json::to_string(&keywords))
            .transpose()?);
    }
    if let Some(keywords) = request.whitelist_keywords {
        active.whitelist_keywords = Set((!keywords.is_empty())
            .then(|| serde_json::to_string(&keywords))
            .transpose()?);
    }
    if let Some(value) = request.case_sensitive {
        active.keyword_case_sensitive = Set(value);
    }
    if let Some(value) = request.min_duration_seconds {
        active.min_duration_seconds = Set(value);
    }
    if let Some(value) = request.max_duration_seconds {
        active.max_duration_seconds = Set(value);
    }
    if let Some(value) = request.published_after {
        active.published_after = Set(value.filter(|value| !value.trim().is_empty()));
    }
    if let Some(value) = request.published_before {
        active.published_before = Set(value.filter(|value| !value.trim().is_empty()));
    }
    let model = active.update(db.as_ref()).await?;
    notify_video_sources_changed();
    Ok(ApiResponse::ok(source_response(db.as_ref(), model).await?))
}

pub async fn delete_youtube_source(
    AxumPath(id): AxumPath<i32>,
    Query(request): Query<DeleteYouTubeSourceRequest>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<bool>, ApiError> {
    delete_youtube_source_internal(db.as_ref(), id, request.delete_local_files).await?;
    Ok(ApiResponse::ok(true))
}

pub async fn delete_youtube_source_internal(db: &DatabaseConnection, id: i32, delete_local_files: bool) -> Result<()> {
    let Some(source) = youtube_source::Entity::find_by_id(id).one(db).await? else {
        return Err(anyhow!("YouTube 视频源不存在"));
    };
    if delete_local_files {
        let videos = youtube_video::Entity::find()
            .filter(youtube_video::Column::SourceId.eq(id))
            .all(db)
            .await?;
        for video in videos {
            if let Some(path) = video.output_path.as_deref() {
                remove_recorded_output(&source.path, path).await?;
            }
        }
    }
    youtube_source::Entity::delete_by_id(id).exec(db).await?;
    notify_video_sources_changed();
    notify_videos_changed();
    notify_queue_status_changed();
    Ok(())
}

pub async fn reset_youtube_source_path(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<ResetYouTubeSourcePathRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    let new_path = request.new_path.trim();
    if new_path.is_empty() {
        return Err(ApiError::from(anyhow!("下载目录不能为空")));
    }
    let Some(source) = youtube_source::Entity::find_by_id(id).one(db.as_ref()).await? else {
        return Err(ApiError::from(anyhow!("YouTube 视频源不存在")));
    };
    // 只搬迁已记录的媒体文件；未记录的用户文件不会被碰触。
    let old_base = PathBuf::from(&source.path);
    let new_base = PathBuf::from(new_path);
    for video in youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.eq(id))
        .all(db.as_ref())
        .await?
    {
        let Some(output_path) = video.output_path.as_deref() else {
            continue;
        };
        let old_file = PathBuf::from(output_path);
        let Ok(relative) = old_file.strip_prefix(&old_base) else {
            continue;
        };
        let target = new_base.join(relative);
        if old_file == target || !old_file.is_file() {
            continue;
        }
        for companion in recorded_output_files(&old_file).await? {
            let Ok(companion_relative) = companion.strip_prefix(&old_base) else {
                continue;
            };
            let companion_target = new_base.join(companion_relative);
            move_file_cross_volume(&companion, &companion_target).await?;
        }
        remove_empty_parent_directories(old_file.parent(), &old_base).await;
        let mut active: youtube_video::ActiveModel = video.into();
        active.output_path = Set(Some(target.display().to_string()));
        active.updated_at = Set(now_standard_string());
        active.update(db.as_ref()).await?;
    }
    let mut active: youtube_source::ActiveModel = source.into();
    active.path = Set(new_path.to_string());
    let source = active.update(db.as_ref()).await?;
    notify_video_sources_changed();
    notify_videos_changed();
    Ok(ApiResponse::ok(source_response(db.as_ref(), source).await?))
}

pub async fn retry_youtube_source(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<u64>, ApiError> {
    if youtube_source::Entity::find_by_id(id).one(db.as_ref()).await?.is_none() {
        return Err(ApiError::from(anyhow!("YouTube 视频源不存在")));
    }
    let videos = youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.eq(id))
        .all(db.as_ref())
        .await?;
    let mut retried = 0u64;
    for video in videos {
        let missing_sidecars = if video.download_status == "completed" {
            match video.output_path.as_deref().map(PathBuf::from) {
                Some(output_path) => {
                    let thumb = youtube_sidecar_path(&output_path, "-thumb.jpg")?;
                    let fanart = youtube_sidecar_path(&output_path, "-fanart.jpg")?;
                    !tokio::fs::metadata(thumb).await.is_ok_and(|meta| meta.len() >= 1024)
                        || !tokio::fs::metadata(fanart).await.is_ok_and(|meta| meta.len() >= 1024)
                }
                None => true,
            }
        } else {
            false
        };
        if video.download_status != "failed" && !missing_sidecars {
            continue;
        }
        let mut active: youtube_video::ActiveModel = video.into();
        active.download_status = Set("pending".to_string());
        active.retry_count = Set(0);
        active.error_message = Set(None);
        active.updated_at = Set(now_standard_string());
        active.update(db.as_ref()).await?;
        retried += 1;
    }
    if retried > 0 {
        notify_videos_changed();
        notify_queue_status_changed();
    }
    Ok(ApiResponse::ok(retried))
}

pub async fn retry_youtube_video(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<bool>, ApiError> {
    let Some(video) = youtube_video::Entity::find_by_id(id).one(db.as_ref()).await? else {
        return Err(ApiError::from(anyhow!("YouTube 下载任务不存在")));
    };
    let mut active: youtube_video::ActiveModel = video.into();
    active.download_status = Set("pending".to_string());
    active.retry_count = Set(0);
    active.error_message = Set(None);
    active.updated_at = Set(now_standard_string());
    active.update(db.as_ref()).await?;
    notify_videos_changed();
    notify_queue_status_changed();
    Ok(ApiResponse::ok(true))
}

pub async fn get_youtube_queue_status(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<YouTubeQueueStatusResponse>, ApiError> {
    let count = |status: &str| {
        youtube_video::Entity::find()
            .filter(youtube_video::Column::DownloadStatus.eq(status))
            .count(db.as_ref())
    };
    let tasks = youtube_video::Entity::find()
        .filter(youtube_video::Column::DownloadStatus.is_in(["pending", "downloading", "failed"]))
        .order_by_asc(youtube_video::Column::Id)
        .limit(100)
        .all(db.as_ref())
        .await?
        .into_iter()
        .map(video_response)
        .collect();
    Ok(ApiResponse::ok(YouTubeQueueStatusResponse {
        pending: count("pending").await?,
        downloading: count("downloading").await?,
        completed: count("completed").await?,
        failed: count("failed").await?,
        tasks,
    }))
}

pub async fn scan_youtube_source(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<u64>, ApiError> {
    ensure_ytdlp_available().await?;
    let Some(source) = youtube_source::Entity::find_by_id(id).one(db.as_ref()).await? else {
        return Err(ApiError::from(anyhow!("YouTube 视频源不存在")));
    };
    let added = scan_source(db.as_ref(), &source).await?;
    Ok(ApiResponse::ok(added))
}

pub async fn get_youtube_videos(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<Vec<YouTubeVideoResponse>>, ApiError> {
    let videos = youtube_video::Entity::find()
        .order_by_desc(youtube_video::Column::Id)
        .limit(200)
        .all(db.as_ref())
        .await?;
    Ok(ApiResponse::ok(videos.into_iter().map(video_response).collect()))
}

pub fn unified_youtube_id(value: &str) -> Option<i32> {
    value.strip_prefix("youtube-").and_then(|id| id.parse::<i32>().ok())
}

fn youtube_source_type_label(source_type: &str) -> &'static str {
    match source_type {
        "subscriptions" => "YouTube 订阅",
        "channel" => "YouTube 频道",
        "playlist" => "YouTube 播放列表",
        "liked" => "YouTube 喜欢",
        "watch_later" => "YouTube 稍后观看",
        _ => "YouTube",
    }
}

fn youtube_upper_paths(uploader: &str) -> (PathBuf, PathBuf) {
    let uploader = crate::utils::filenamify::filenamify(uploader);
    let bucket = uploader.chars().next().unwrap_or('_').to_string().to_lowercase();
    let upper_dir = crate::config::reload_config().upper_path.join(bucket).join(uploader);
    (upper_dir.join("folder.jpg"), upper_dir.join("person.nfo"))
}

fn youtube_failure_status(video: &youtube_video::Model) -> u32 {
    if video.download_status == "failed" {
        video.retry_count.clamp(1, 6) as u32
    } else {
        0
    }
}

async fn youtube_artifact_status(video: &youtube_video::Model, source: &youtube_source::Model) -> ([u32; 5], [u32; 5]) {
    let failure = youtube_failure_status(video);
    let completed = video.download_status == "completed";
    let output = video.output_path.as_deref().map(PathBuf::from);
    let media_ok = output
        .as_ref()
        .is_some_and(|path| path.is_file() && std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0));
    let cover_ok = output
        .as_ref()
        .and_then(|path| youtube_sidecar_path(path, "-thumb.jpg").ok())
        .is_some_and(|path| path.is_file());
    let nfo_ok = output.as_ref().is_some_and(|path| path.with_extension("nfo").is_file());
    let subtitle_ok = match output.as_ref() {
        Some(path) => youtube_subtitle_exists(path).await.unwrap_or(false),
        None => false,
    };
    let live_chat_ok = output.as_ref().is_some_and(|path| {
        path.with_extension("live_chat.json").is_file() || path.with_extension("live_chat.checked").is_file()
    });
    let (face_path, person_nfo_path) = youtube_upper_paths(&video.uploader);
    let face_ok = face_path.is_file();
    let person_ok = person_nfo_path.is_file();
    let status = |ok: bool| if ok { 7 } else { failure };
    let optional_status = |ok: bool, warning: bool| {
        if ok || (completed && !warning) {
            7
        } else {
            failure
        }
    };
    let warning = video.error_message.as_deref().unwrap_or("");

    let video_status = [
        status(cover_ok),
        status(nfo_ok),
        status(face_ok),
        status(person_ok),
        status(media_ok),
    ];
    let page_status = [
        status(cover_ok),
        status(media_ok),
        status(nfo_ok),
        optional_status(live_chat_ok, warning.contains("直播聊天")),
        if source.download_subtitle {
            optional_status(subtitle_ok, warning.contains("字幕"))
        } else {
            7
        },
    ];
    (video_status, page_status)
}

async fn unified_youtube_parts(
    video: &youtube_video::Model,
    source: &youtube_source::Model,
) -> (VideoInfo, PageInfo, VideoSourceTag) {
    let (video_status, page_status) = youtube_artifact_status(video, source).await;
    let output_path = video.output_path.clone();
    let parent_path = output_path
        .as_deref()
        .and_then(|path| Path::new(path).parent())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| source.path.clone());
    (
        VideoInfo {
            id: video.id,
            bvid: video.youtube_id.clone(),
            name: video.title.clone(),
            upper_name: video.uploader.clone(),
            path: parent_path,
            category: 0,
            download_status: video_status,
            cover: video.thumbnail.clone().unwrap_or_default(),
            valid: true,
            is_charge_video: false,
            bangumi_title: None,
        },
        PageInfo {
            id: video.id,
            pid: 1,
            name: video.title.clone(),
            download_status: page_status,
            path: output_path,
            danmaku_last_synced_at: None,
            danmaku_sync_generation: 0,
            danmaku_cid_snapshot: None,
            danmaku_last_write_count: 0,
        },
        VideoSourceTag {
            source_id: source.id,
            source_type: "youtube".to_string(),
            source_type_label: youtube_source_type_label(&source.source_type).to_string(),
            source_name: source.name.clone(),
            split_chapters_after_download: false,
            audio_only: source.audio_only,
            audio_only_m4a_only: false,
            flat_folder: false,
        },
    )
}

async fn filtered_youtube_models(
    db: &DatabaseConnection,
    params: &VideosRequest,
) -> Result<Vec<(youtube_video::Model, youtube_source::Model, u64)>> {
    let sources = youtube_source::Entity::find().all(db).await?;
    let source_map = sources
        .into_iter()
        .map(|source| (source.id, source))
        .collect::<HashMap<_, _>>();
    let mut rows = Vec::new();
    for video in youtube_video::Entity::find().all(db).await? {
        let Some(source) = source_map.get(&video.source_id).cloned() else {
            continue;
        };
        if params.youtube.is_some_and(|id| id != source.id) {
            continue;
        }
        if let Some(query) = params.query.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
            let query = query.to_lowercase();
            let output = video.output_path.as_deref().unwrap_or("").to_lowercase();
            if !video.title.to_lowercase().contains(&query)
                && !video.uploader.to_lowercase().contains(&query)
                && !output.contains(&query)
            {
                continue;
            }
        }
        if params.show_failed_only.unwrap_or(false) && video.download_status != "failed" {
            continue;
        }
        let file_size = video
            .output_path
            .as_deref()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|meta| meta.len())
            .unwrap_or_default();
        rows.push((video, source, file_size));
    }
    let ascending = params.sort_order.as_deref() == Some("asc");
    rows.sort_by(|left, right| {
        let ordering = match params.sort_by.as_deref().unwrap_or("id") {
            "name" => left.0.title.to_lowercase().cmp(&right.0.title.to_lowercase()),
            "pubtime" => left.0.published_at.cmp(&right.0.published_at),
            "file_size" => left.2.cmp(&right.2),
            _ => left.0.id.cmp(&right.0.id),
        };
        if ascending {
            ordering
        } else {
            ordering.reverse()
        }
    });
    Ok(rows)
}

pub async fn get_unified_youtube_videos(
    db: &DatabaseConnection,
    params: &VideosRequest,
) -> Result<VideosResponse, ApiError> {
    let rows = filtered_youtube_models(db, params).await?;
    let total_count = rows.len() as u64;
    let page = params.page.unwrap_or(0);
    let page_size = params.page_size.unwrap_or(10).max(1);
    let start = (page.saturating_mul(page_size)) as usize;
    let mut videos = Vec::new();
    for (video, source, _) in rows.into_iter().skip(start).take(page_size as usize) {
        videos.push(unified_youtube_parts(&video, &source).await.0);
    }
    Ok(VideosResponse {
        videos,
        total_count,
        file_size_stats_pending: false,
    })
}

pub async fn get_unified_youtube_video(db: &DatabaseConnection, id: i32) -> Result<VideoResponse, ApiError> {
    let video = youtube_video::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow!("YouTube 视频不存在: {}", id))?;
    let source = youtube_source::Entity::find_by_id(video.source_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow!("YouTube 视频源不存在: {}", video.source_id))?;
    let (video, page, source) = unified_youtube_parts(&video, &source).await;
    Ok(VideoResponse {
        video,
        pages: vec![page],
        source: Some(source),
    })
}

pub async fn reset_unified_youtube_video(
    db: &DatabaseConnection,
    id: i32,
    force: bool,
) -> Result<ResetVideoResponse, ApiError> {
    let video = youtube_video::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow!("YouTube 视频不存在: {}", id))?;
    let should_reset = force || video.download_status == "failed";
    if should_reset {
        let mut active: youtube_video::ActiveModel = video.into();
        active.download_status = Set("pending".to_string());
        active.retry_count = Set(0);
        active.error_message = Set(None);
        active.updated_at = Set(now_standard_string());
        active.update(db).await?;
        notify_videos_changed();
        notify_queue_status_changed();
        crate::task::resume_scanning();
    }
    let response = get_unified_youtube_video(db, id).await?;
    Ok(ResetVideoResponse {
        resetted: should_reset,
        video: response.video,
        pages: response.pages,
    })
}

pub async fn reset_all_unified_youtube_videos(
    db: &DatabaseConnection,
    params: &VideosRequest,
) -> Result<ResetAllVideosResponse, ApiError> {
    let rows = filtered_youtube_models(db, params).await?;
    let force = params.force.unwrap_or(false);
    let mut count = 0usize;
    for (video, _, _) in rows {
        if force || video.download_status == "failed" {
            let mut active: youtube_video::ActiveModel = video.into();
            active.download_status = Set("pending".to_string());
            active.retry_count = Set(0);
            active.error_message = Set(None);
            active.updated_at = Set(now_standard_string());
            active.update(db).await?;
            count += 1;
        }
    }
    if count > 0 {
        notify_videos_changed();
        notify_queue_status_changed();
        crate::task::resume_scanning();
    }
    Ok(ResetAllVideosResponse {
        resetted: count > 0,
        resetted_videos_count: count,
        resetted_pages_count: count,
    })
}

pub async fn reset_specific_unified_youtube_tasks(
    db: &DatabaseConnection,
    request: &ResetSpecificTasksRequest,
) -> Result<ResetAllVideosResponse, ApiError> {
    let params = VideosRequest {
        platform: Some("youtube".to_string()),
        youtube: request.youtube,
        query: request.query.clone(),
        show_failed_only: request.show_failed_only,
        force: request.force,
        ..Default::default()
    };
    reset_all_unified_youtube_videos(db, &params).await
}

pub async fn update_unified_youtube_status(
    db: &DatabaseConnection,
    id: i32,
    request: &UpdateVideoStatusRequest,
) -> Result<UpdateVideoStatusResponse, ApiError> {
    let video = youtube_video::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow!("YouTube 视频不存在: {}", id))?;
    let values = request
        .video_updates
        .iter()
        .chain(request.page_updates.iter().flat_map(|page| page.updates.iter()))
        .map(|update| update.status_value)
        .collect::<Vec<_>>();
    let status = if values.iter().any(|value| (1..=6).contains(value)) {
        "failed"
    } else if !values.is_empty() && values.iter().all(|value| *value == 7) {
        "completed"
    } else {
        "pending"
    };
    let mut active: youtube_video::ActiveModel = video.into();
    active.download_status = Set(status.to_string());
    active.retry_count = Set(if status == "failed" { 1 } else { 0 });
    active.error_message = Set(None);
    active.updated_at = Set(now_standard_string());
    active.update(db).await?;
    notify_videos_changed();
    if status == "pending" {
        crate::task::resume_scanning();
    }
    let response = get_unified_youtube_video(db, id).await?;
    Ok(UpdateVideoStatusResponse {
        success: true,
        video: response.video,
        pages: response.pages,
    })
}

pub async fn delete_unified_youtube_video(db: &DatabaseConnection, id: i32) -> Result<DeleteVideoResponse, ApiError> {
    let video = youtube_video::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow!("YouTube 视频不存在: {}", id))?;
    let source = youtube_source::Entity::find_by_id(video.source_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow!("YouTube 视频源不存在: {}", video.source_id))?;
    if let Some(output_path) = video.output_path.as_deref() {
        remove_recorded_output(&source.path, output_path).await?;
    }
    youtube_video::Entity::delete_by_id(id).exec(db).await?;
    notify_videos_changed();
    notify_queue_status_changed();
    Ok(DeleteVideoResponse {
        success: true,
        video_id: id,
        message: "YouTube 视频已成功删除".to_string(),
    })
}

pub async fn unified_youtube_cover_path(db: &DatabaseConnection, id: i32) -> Result<Option<PathBuf>, ApiError> {
    let Some(video) = youtube_video::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    Ok(video
        .output_path
        .as_deref()
        .and_then(|path| youtube_sidecar_path(Path::new(path), "-thumb.jpg").ok())
        .filter(|path| path.is_file()))
}

/// 由现有 `video_downloader` 的同一周期调用。来源扫描和待下载任务都受全局暂停、
/// 下载并发配置及相同的日志/通知周期控制。
pub async fn process_scheduled_sources(
    db: &DatabaseConnection,
    downloader: Arc<UnifiedDownloader>,
    concurrent_limit: usize,
) -> Result<()> {
    let sources = youtube_source::Entity::find()
        .filter(youtube_source::Column::Enabled.eq(true))
        .all(db)
        .await?;
    if sources.is_empty() {
        return Ok(());
    }
    if let Err(error) = ensure_ytdlp_available().await {
        warn!(error = %error, "已配置 YouTube 视频源，但 yt-dlp 自动安装失败；跳过本轮 YouTube 扫描");
        return Ok(());
    }
    recover_interrupted_downloads(db).await?;
    for source in &sources {
        if TASK_CONTROLLER.is_paused() {
            return Ok(());
        }
        if let Err(error) = scan_source(db, source).await {
            warn!(source_id = source.id, error = %error, "扫描 YouTube 视频源失败");
        }
    }
    if TASK_CONTROLLER.is_paused() {
        return Ok(());
    }
    download_pending(db, downloader.clone(), concurrent_limit.max(1)).await?;
    if !YOUTUBE_SIDECAR_BACKFILL_DONE.load(Ordering::SeqCst) {
        backfill_completed_sidecars(db, downloader.as_ref()).await?;
        if !TASK_CONTROLLER.is_paused() {
            YOUTUBE_SIDECAR_BACKFILL_DONE.store(true, Ordering::SeqCst);
        }
    }
    Ok(())
}

async fn scan_source(db: &DatabaseConnection, source: &youtube_source::Model) -> Result<u64> {
    let mut command = Command::new(ytdlp_executable());
    command.args(["--flat-playlist", "--dump-json", "--ignore-errors", "--no-warnings"]);
    append_ytdlp_runtime(&mut command);
    append_cookies(&mut command);
    // 频道的 `/videos`、`/shorts` 和 `/streams` 是三个彼此独立的标签页。
    // 扫描频道根地址时 yt-dlp 会按频道完整枚举三个标签页；若直接使用用户
    // 粘贴的 `/videos` 地址，则只会看到普通视频，漏掉 Shorts 和直播回放。
    let scan_url = if source.source_type == "channel" {
        canonical_channel_url(&source.url)
    } else {
        source.url.clone()
    };
    command.arg(&scan_url);
    let output = tokio::time::timeout(Duration::from_secs(10 * 60), command.output())
        .await
        .map_err(|_| anyhow!("扫描 YouTube 来源超时"))??;
    if !output.status.success() {
        bail!("yt-dlp 扫描失败：{}", command_error(&output));
    }
    let mut added = 0;
    let selected_history = source
        .selected_videos
        .as_deref()
        .and_then(|value| serde_json::from_str::<HashSet<String>>(value).ok())
        .unwrap_or_default();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(item) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(youtube_id) = item.get("id").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) else {
            continue;
        };
        // 与 B 站投稿源一致：创建来源时只回补勾选的历史视频；以后仅自动加入
        // 来源创建后发布的新视频，未勾选的旧视频不会在下一轮扫描中“补回来”。
        if !selected_history.is_empty()
            && !selected_history.contains(youtube_id)
            && !youtube_item_is_newer_than_source(&item, &source.created_at)
        {
            continue;
        }
        let exists = youtube_video::Entity::find()
            .filter(youtube_video::Column::SourceId.eq(source.id))
            .filter(youtube_video::Column::YoutubeId.eq(youtube_id))
            .one(db)
            .await?
            .is_some();
        if exists {
            continue;
        }
        let url = item
            .get("webpage_url")
            .or_else(|| item.get("url"))
            .and_then(|v| v.as_str())
            .filter(|v| v.starts_with("http"))
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={youtube_id}"));
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(youtube_id)
            .to_string();
        let duration_seconds = item
            .get("duration")
            .and_then(|v| v.as_f64())
            .and_then(|v| i32::try_from(v.round() as i64).ok());
        let published_at = item.get("upload_date").and_then(|v| v.as_str()).map(str::to_string);
        if crate::utils::keyword_filter::should_filter_video_dual_list(
            &title,
            &source.blacklist_keywords,
            &source.whitelist_keywords,
            source.keyword_case_sensitive,
        ) {
            continue;
        }
        if source
            .min_duration_seconds
            .zip(duration_seconds)
            .is_some_and(|(minimum, duration)| duration < minimum)
            || source
                .max_duration_seconds
                .zip(duration_seconds)
                .is_some_and(|(maximum, duration)| duration > maximum)
        {
            continue;
        }
        let compact_date = published_at.as_deref();
        if source
            .published_after
            .as_deref()
            .map(|value| value.replace('-', ""))
            .zip(compact_date)
            .is_some_and(|(minimum, actual)| actual < minimum.as_str())
            || source
                .published_before
                .as_deref()
                .map(|value| value.replace('-', ""))
                .zip(compact_date)
                .is_some_and(|(maximum, actual)| actual > maximum.as_str())
        {
            continue;
        }
        youtube_video::ActiveModel {
            source_id: Set(source.id),
            youtube_id: Set(youtube_id.to_string()),
            url: Set(url),
            title: Set(title),
            uploader: Set(item
                .get("uploader")
                .or_else(|| item.get("channel"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()),
            thumbnail: Set(item.get("thumbnail").and_then(|v| v.as_str()).map(str::to_string)),
            published_at: Set(published_at),
            duration_seconds: Set(duration_seconds),
            download_status: Set("pending".to_string()),
            output_path: Set(None),
            error_message: Set(None),
            created_at: Set(now_standard_string()),
            updated_at: Set(now_standard_string()),
            ..Default::default()
        }
        .insert(db)
        .await?;
        added += 1;
    }
    let mut active: youtube_source::ActiveModel = source.clone().into();
    active.last_scan_at = Set(Some(now_standard_string()));
    active.update(db).await?;
    notify_video_sources_changed();
    if added > 0 {
        info!(source_id = source.id, added, "YouTube 视频源发现新视频");
        notify_videos_changed();
        notify_queue_status_changed();
    }
    Ok(added)
}

fn youtube_item_is_newer_than_source(item: &serde_json::Value, created_at: &str) -> bool {
    let created = chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S").ok();
    if let (Some(created), Some(timestamp)) = (
        created,
        item.get("timestamp")
            .or_else(|| item.get("release_timestamp"))
            .and_then(|value| value.as_i64()),
    ) {
        return Local
            .from_local_datetime(&created)
            .single()
            .is_some_and(|created| timestamp > created.timestamp());
    }
    let Some(upload_date) = item.get("upload_date").and_then(|value| value.as_str()) else {
        return false;
    };
    let created_date = created_at.get(..10).unwrap_or(created_at).replace('-', "");
    upload_date > created_date.as_str()
}

async fn recover_interrupted_downloads(db: &DatabaseConnection) -> Result<()> {
    let interrupted = youtube_video::Entity::find()
        .filter(youtube_video::Column::DownloadStatus.eq("downloading"))
        .all(db)
        .await?;
    if interrupted.is_empty() {
        return Ok(());
    }
    let count = interrupted.len();
    for video in interrupted {
        let mut active: youtube_video::ActiveModel = video.into();
        active.download_status = Set("pending".to_string());
        active.error_message = Set(Some("上次进程在下载中中断，已自动恢复到待下载队列".to_string()));
        active.updated_at = Set(now_standard_string());
        active.update(db).await?;
    }
    warn!(count, "已恢复上次进程中断的 YouTube 下载任务");
    notify_videos_changed();
    notify_queue_status_changed();
    Ok(())
}

async fn download_pending(
    db: &DatabaseConnection,
    downloader: Arc<UnifiedDownloader>,
    concurrent_limit: usize,
) -> Result<()> {
    let videos = youtube_video::Entity::find()
        .filter(youtube_video::Column::DownloadStatus.eq("pending"))
        .order_by_asc(youtube_video::Column::Id)
        .all(db)
        .await?;
    if videos.is_empty() {
        return Ok(());
    }
    let db = db.clone();
    stream::iter(videos)
        .for_each_concurrent(concurrent_limit, move |video| {
            let db = db.clone();
            let downloader = downloader.clone();
            async move {
                if let Err(error) = download_video(&db, downloader.as_ref(), video).await {
                    warn!(error = %error, "下载 YouTube 视频失败");
                }
            }
        })
        .await;
    Ok(())
}

/// 为升级前已经完成的 YouTube 媒体补齐封面、NFO 和字幕。
///
/// 附属文件失败不能把已经验证完成的媒体重新标记为失败；真实错误写入
/// `error_message`，并在下次进程启动时继续尝试缺失的附属文件。
async fn backfill_completed_sidecars(db: &DatabaseConnection, downloader: &UnifiedDownloader) -> Result<()> {
    let videos = youtube_video::Entity::find()
        .filter(youtube_video::Column::DownloadStatus.eq("completed"))
        .filter(youtube_video::Column::OutputPath.is_not_null())
        .order_by_asc(youtube_video::Column::Id)
        .all(db)
        .await?;
    if videos.is_empty() {
        return Ok(());
    }

    let sources = youtube_source::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|source| (source.id, source))
        .collect::<HashMap<_, _>>();
    let total = videos.len();
    let mut refreshed = 0usize;
    let mut warned = 0usize;

    info!(total, "开始回填已完成 YouTube 视频的封面、NFO 和字幕");
    for video in videos {
        if TASK_CONTROLLER.is_paused() {
            info!(refreshed, total, "YouTube 附属文件回填因下载暂停而停止");
            break;
        }
        let Some(output_path) = video.output_path.as_deref().map(PathBuf::from) else {
            continue;
        };
        let Some(source) = sources.get(&video.source_id) else {
            let mut active: youtube_video::ActiveModel = video.into();
            active.error_message = Set(Some("媒体已完成；附属文件回填失败：视频源不存在".to_string()));
            active.updated_at = Set(now_standard_string());
            active.update(db).await?;
            warned += 1;
            continue;
        };
        if youtube_sidecars_complete(&video, source).await {
            continue;
        }
        if !is_reusable_media_file(&output_path).await {
            let mut active: youtube_video::ActiveModel = video.into();
            active.download_status = Set("pending".to_string());
            active.retry_count = Set(0);
            active.output_path = Set(None);
            active.error_message = Set(Some(format!(
                "媒体记录为已完成，但文件不存在或不可播放，已自动重新加入下载队列：{}",
                output_path.display()
            )));
            active.updated_at = Set(now_standard_string());
            active.update(db).await?;
            warned += 1;
            continue;
        }

        match extract_youtube_metadata(&video.url).await {
            Ok(metadata) => {
                let title = metadata.title.clone().unwrap_or_else(|| video.title.clone());
                let uploader = metadata
                    .uploader
                    .clone()
                    .or_else(|| metadata.channel.clone())
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| source.name.clone());
                let warning_message = ensure_youtube_sidecars(
                    downloader,
                    &metadata,
                    &output_path,
                    &video.url,
                    &title,
                    &uploader,
                    source,
                )
                .await;
                if warning_message.is_some() {
                    warned += 1;
                }

                let mut active: youtube_video::ActiveModel = video.into();
                active.title = Set(title);
                active.uploader = Set(uploader);
                active.thumbnail = Set(metadata.thumbnail);
                active.published_at = Set(metadata.upload_date);
                active.duration_seconds = Set(metadata
                    .duration
                    .and_then(|value| i32::try_from(value.round() as i64).ok()));
                active.error_message = Set(warning_message);
                active.updated_at = Set(now_standard_string());
                active.update(db).await?;
                refreshed += 1;
            }
            Err(error) => {
                warn!(
                    youtube_id = %video.youtube_id,
                    error = %error,
                    "YouTube 媒体已完成，但附属文件元数据解析失败"
                );
                let mut active: youtube_video::ActiveModel = video.into();
                active.error_message = Set(Some(format!("媒体已完成；附属文件元数据解析失败：{error:#}")));
                active.updated_at = Set(now_standard_string());
                active.update(db).await?;
                warned += 1;
            }
        }
    }

    info!(refreshed, warned, total, "YouTube 已完成媒体附属文件回填结束");
    notify_videos_changed();
    notify_queue_status_changed();
    Ok(())
}

async fn download_video(
    db: &DatabaseConnection,
    downloader: &UnifiedDownloader,
    mut video: youtube_video::Model,
) -> Result<()> {
    if TASK_CONTROLLER.is_paused() {
        return Ok(());
    }
    let Some(source) = youtube_source::Entity::find_by_id(video.source_id).one(db).await? else {
        return Ok(());
    };
    if !source.enabled {
        return Ok(());
    }
    loop {
        let mut downloading: youtube_video::ActiveModel = video.clone().into();
        downloading.download_status = Set("downloading".to_string());
        downloading.updated_at = Set(now_standard_string());
        video = downloading.update(db).await?;
        notify_videos_changed();
        notify_queue_status_changed();

        // 每次重试都重新让 yt-dlp 解析签名直链，避免复用已经限速或失效的
        // GoogleVideo URL；媒体仍由 UnifiedDownloader 和原 ffmpeg 链路处理。
        let result = download_youtube_media(downloader, &source, &video).await;
        let mut active: youtube_video::ActiveModel = video.clone().into();
        active.updated_at = Set(now_standard_string());
        match result {
            Ok(downloaded) => {
                active.download_status = Set("completed".to_string());
                active.retry_count = Set(0);
                active.output_path = Set(Some(downloaded.output_path.display().to_string()));
                active.error_message = Set(downloaded.warning_message);
                active.title = Set(downloaded.title);
                active.uploader = Set(downloaded.uploader);
                active.thumbnail = Set(downloaded.thumbnail);
                active.published_at = Set(downloaded.published_at);
                active.duration_seconds = Set(downloaded.duration_seconds);
                info!(
                    source_id = source.id,
                    youtube_id = %video.youtube_id,
                    path = %downloaded.output_path.display(),
                    "YouTube 视频下载完成"
                );
                active.update(db).await?;
                notify_videos_changed();
                notify_queue_status_changed();
                return Ok(());
            }
            Err(error) => {
                let retry_count = video.retry_count.saturating_add(1);
                let exhausted = retry_count >= MAX_DOWNLOAD_RETRIES;
                active.retry_count = Set(retry_count);
                active.download_status = Set(if exhausted { "failed" } else { "pending" }.to_string());
                active.error_message = Set(Some(format!("{:#}", error)));
                video = active.update(db).await?;
                warn!(
                    source_id = source.id,
                    retry_count,
                    max_retries = MAX_DOWNLOAD_RETRIES,
                    error = %error,
                    "YouTube 视频下载失败，真实错误已持久化"
                );
                notify_videos_changed();
                notify_queue_status_changed();

                if exhausted || TASK_CONTROLLER.is_paused() {
                    return Ok(());
                }
                let retry_delay = Duration::from_secs((retry_count.max(1) as u64) * 5);
                info!(
                    source_id = source.id,
                    youtube_id = %video.youtube_id,
                    retry_count,
                    delay_seconds = retry_delay.as_secs(),
                    "等待后刷新 YouTube 直链并重试"
                );
                tokio::time::sleep(retry_delay).await;
                if TASK_CONTROLLER.is_paused() {
                    return Ok(());
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct YtDlpMetadata {
    id: String,
    title: Option<String>,
    uploader: Option<String>,
    uploader_url: Option<String>,
    channel: Option<String>,
    channel_id: Option<String>,
    channel_url: Option<String>,
    thumbnail: Option<String>,
    description: Option<String>,
    language: Option<String>,
    upload_date: Option<String>,
    duration: Option<f64>,
    #[serde(default)]
    formats: Vec<YtDlpFormat>,
    #[serde(default)]
    subtitles: HashMap<String, Vec<YtDlpSubtitle>>,
    #[serde(default)]
    automatic_captions: HashMap<String, Vec<YtDlpSubtitle>>,
}

#[derive(Debug, Deserialize)]
struct YtDlpSourceMetadata {
    id: Option<String>,
    channel_id: Option<String>,
    uploader_id: Option<String>,
    #[serde(default)]
    thumbnails: Vec<YtDlpThumbnail>,
}

#[derive(Debug, Deserialize)]
struct YtDlpThumbnail {
    url: String,
    id: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
}

#[derive(Clone, Debug, Deserialize)]
struct YtDlpFormat {
    format_id: Option<String>,
    url: Option<String>,
    protocol: Option<String>,
    ext: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    fps: Option<f64>,
    tbr: Option<f64>,
    vbr: Option<f64>,
    abr: Option<f64>,
    dynamic_range: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct YtDlpSubtitle {
    url: Option<String>,
    ext: Option<String>,
}

struct DownloadedYouTubeMedia {
    output_path: PathBuf,
    title: String,
    uploader: String,
    thumbnail: Option<String>,
    published_at: Option<String>,
    duration_seconds: Option<i32>,
    warning_message: Option<String>,
}

struct SelectedStreams {
    video: Option<YtDlpFormat>,
    audio: Option<YtDlpFormat>,
    mixed: Option<YtDlpFormat>,
}

async fn download_youtube_media(
    downloader: &UnifiedDownloader,
    source: &youtube_source::Model,
    video: &youtube_video::Model,
) -> Result<DownloadedYouTubeMedia> {
    let metadata = extract_youtube_metadata(&video.url).await?;
    let title = metadata.title.clone().unwrap_or_else(|| video.title.clone());
    let uploader = metadata
        .uploader
        .clone()
        .or_else(|| metadata.channel.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| source.name.clone());
    let output_path = youtube_output_path(source, video, &metadata, &title, &uploader)?;
    let media_exists = is_reusable_media_file(&output_path).await;
    if media_exists {
        info!(path = %output_path.display(), "YouTube 目标文件已存在，复用现有文件");
    } else {
        // 上次 ffmpeg 被中断时可能留下非零但不可播放的最终文件，不能仅凭文件
        // 大小就把任务标记为完成。
        remove_file_if_exists(&output_path).await?;
    }
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("无法创建 YouTube 下载目录: {}", parent.display()))?;
    }

    let filter_option = source
        .filter_option
        .as_ref()
        .map(|value| serde_json::from_value::<FilterOption>(value.clone()))
        .transpose()
        .context("YouTube 视频源级流过滤设置无效")?
        .unwrap_or_else(|| crate::config::reload_config().filter_option);
    let selected = select_youtube_streams(&metadata.formats, &filter_option, source.audio_only)?;
    log_selected_youtube_streams(&selected, &filter_option);
    if media_exists {
        // 媒体已落盘时不重复下载，但仍继续执行字幕等独立子任务。
    } else if source.audio_only {
        let audio = selected
            .audio
            .or(selected.mixed)
            .ok_or_else(|| anyhow!("YouTube 未返回可用音频流"))?;
        let url = audio.url.as_deref().context("YouTube 音频流缺少下载地址")?;
        let temporary = output_path.with_extension("download.m4a");
        if let Err(error) = downloader
            .fetch_with_fallback(&[url], &temporary)
            .await
            .context("使用项目统一下载器下载 YouTube 音频失败")
        {
            let _ = remove_file_if_exists(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &output_path).await?;
    } else if let (Some(video_stream), Some(audio_stream)) = (selected.video, selected.audio) {
        let video_url = video_stream.url.as_deref().context("YouTube 视频流缺少下载地址")?;
        let audio_url = audio_stream.url.as_deref().context("YouTube 音频流缺少下载地址")?;
        let video_temporary =
            output_path.with_extension(format!("video.{}", video_stream.ext.as_deref().unwrap_or("mp4")));
        let audio_temporary =
            output_path.with_extension(format!("audio.{}", audio_stream.ext.as_deref().unwrap_or("m4a")));
        let merge_temporary = output_path.with_extension("merging.mp4");
        // YouTube 的高画质通常是独立 DASH 音视频流。两条直链都交给项目
        // 原生统一下载器并行传输，避免大视频完成后才开始等待音频。
        let video_download = async {
            downloader
                .fetch_with_fallback(&[video_url], &video_temporary)
                .await
                .context("使用项目统一下载器下载 YouTube 视频流失败")
        };
        let audio_download = async {
            downloader
                .fetch_with_fallback(&[audio_url], &audio_temporary)
                .await
                .context("使用项目统一下载器下载 YouTube 音频流失败")
        };
        if let Err(error) = tokio::try_join!(video_download, audio_download) {
            let _ = remove_file_if_exists(&video_temporary).await;
            let _ = remove_file_if_exists(&audio_temporary).await;
            return Err(error);
        }
        if let Err(error) = downloader
            .merge(&video_temporary, &audio_temporary, &merge_temporary)
            .await
            .context("使用项目现有 ffmpeg 链路合并 YouTube 音视频失败")
        {
            let _ = remove_file_if_exists(&video_temporary).await;
            let _ = remove_file_if_exists(&audio_temporary).await;
            let _ = remove_file_if_exists(&merge_temporary).await;
            return Err(error);
        }
        replace_file(&merge_temporary, &output_path).await?;
        remove_file_if_exists(&video_temporary).await?;
        remove_file_if_exists(&audio_temporary).await?;
    } else {
        let mixed = selected.mixed.ok_or_else(|| anyhow!("YouTube 未返回可用的音视频流"))?;
        let url = mixed.url.as_deref().context("YouTube 混合流缺少下载地址")?;
        let temporary = output_path.with_extension(format!("download.{}", mixed.ext.as_deref().unwrap_or("mp4")));
        if let Err(error) = downloader
            .fetch_with_fallback(&[url], &temporary)
            .await
            .context("使用项目统一下载器下载 YouTube 混合流失败")
        {
            let _ = remove_file_if_exists(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &output_path).await?;
    }

    let warning_message = if source.audio_only && source.audio_only_m4a_only {
        None
    } else {
        ensure_youtube_sidecars(
            downloader,
            &metadata,
            &output_path,
            &video.url,
            &title,
            &uploader,
            source,
        )
        .await
    };

    Ok(DownloadedYouTubeMedia {
        output_path,
        title,
        uploader,
        thumbnail: metadata.thumbnail,
        published_at: metadata.upload_date,
        duration_seconds: metadata
            .duration
            .and_then(|value| i32::try_from(value.round() as i64).ok()),
        warning_message,
    })
}

async fn is_reusable_media_file(path: &Path) -> bool {
    if !tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024)
    {
        return false;
    }
    tokio::process::Command::new(crate::downloader::resolve_media_tool_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            &path.to_string_lossy(),
        ])
        .output()
        .await
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

async fn extract_youtube_metadata(url: &str) -> Result<YtDlpMetadata> {
    let mut command = Command::new(ytdlp_executable());
    command.args([
        "--dump-single-json",
        "--skip-download",
        "--no-playlist",
        "--no-warnings",
    ]);
    append_ytdlp_runtime(&mut command);
    append_cookies(&mut command);
    command.arg(url);
    let output = tokio::time::timeout(DOWNLOAD_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow!("解析 YouTube 媒体直链超时"))??;
    if !output.status.success() {
        bail!("yt-dlp 解析 YouTube 媒体直链失败：{}", command_error(&output));
    }
    serde_json::from_slice(&output.stdout).context("解析 yt-dlp 媒体元数据失败")
}

fn select_youtube_streams(formats: &[YtDlpFormat], filter: &FilterOption, audio_only: bool) -> Result<SelectedStreams> {
    let min_audio_bitrate = youtube_audio_min_bitrate(filter.audio_min_quality);
    let max_audio_bitrate = youtube_audio_max_bitrate(filter.audio_max_quality);
    let audio = formats
        .iter()
        .filter(|format| {
            is_http_format(format) && has_audio(format) && !has_video(format) && youtube_audio_allowed(format, filter)
        })
        .max_by_key(|format| {
            let bitrate = youtube_audio_bitrate(format);
            let in_range = bitrate >= min_audio_bitrate && bitrate <= max_audio_bitrate;
            let under_max = bitrate <= max_audio_bitrate;
            (
                i32::from(in_range),
                i32::from(under_max),
                if under_max { bitrate } else { -bitrate },
                i32::from(format.ext.as_deref() == Some("m4a")),
            )
        })
        .cloned();
    if audio_only {
        return Ok(SelectedStreams {
            video: None,
            audio,
            mixed: None,
        });
    }

    let min_height = youtube_quality_height(filter.video_min_quality);
    let max_height = youtube_quality_height(filter.video_max_quality);
    let video = formats
        .iter()
        .filter(|format| {
            is_http_format(format) && has_video(format) && !has_audio(format) && youtube_video_allowed(format, filter)
        })
        .max_by_key(|format| {
            let height = format.height.unwrap_or_default();
            let in_range = height >= min_height && height <= max_height;
            let under_max = height <= max_height;
            (
                i32::from(in_range),
                i32::from(under_max),
                if under_max { height } else { -height },
                -(youtube_codec_rank(format.vcodec.as_deref(), &filter.codecs) as i32),
                format.fps.unwrap_or_default() as i32,
                format.tbr.unwrap_or_default() as i32,
            )
        })
        .cloned();
    let mixed = formats
        .iter()
        .filter(|format| {
            is_http_format(format)
                && has_video(format)
                && has_audio(format)
                && youtube_video_allowed(format, filter)
                && youtube_audio_allowed(format, filter)
        })
        .max_by_key(|format| {
            let height = format.height.unwrap_or_default();
            let in_range = height >= min_height && height <= max_height;
            let under_max = height <= max_height;
            let audio_bitrate = youtube_audio_bitrate(format);
            let audio_in_range = audio_bitrate >= min_audio_bitrate && audio_bitrate <= max_audio_bitrate;
            (
                i32::from(in_range),
                i32::from(audio_in_range),
                i32::from(under_max),
                if under_max { height } else { -height },
                -(youtube_codec_rank(format.vcodec.as_deref(), &filter.codecs) as i32),
                format.tbr.unwrap_or_default() as i32,
            )
        })
        .cloned();
    if video.is_none() && mixed.is_none() {
        let ids = formats
            .iter()
            .filter_map(|format| format.format_id.as_deref())
            .collect::<Vec<_>>()
            .join(",");
        bail!("YouTube 没有可下载的视频格式；解析到的格式 ID: {}", ids);
    }
    Ok(SelectedStreams { video, audio, mixed })
}

fn youtube_video_allowed(format: &YtDlpFormat, filter: &FilterOption) -> bool {
    let codec_allowed = youtube_codec(format.vcodec.as_deref()).is_some_and(|codec| filter.codecs.contains(&codec));
    if !codec_allowed {
        return false;
    }
    let dynamic_range = format.dynamic_range.as_deref().unwrap_or("SDR").to_ascii_uppercase();
    if filter.no_hdr && dynamic_range != "SDR" {
        return false;
    }
    if filter.no_dolby_video && (dynamic_range.contains("DV") || dynamic_range.contains("DOLBY")) {
        return false;
    }
    true
}

fn youtube_audio_allowed(format: &YtDlpFormat, filter: &FilterOption) -> bool {
    let codec = format.acodec.as_deref().unwrap_or_default().to_ascii_lowercase();
    if filter.no_dolby_audio && (codec.contains("ac-3") || codec.contains("ec-3") || codec.contains("dolby")) {
        return false;
    }
    !filter.no_hires || youtube_audio_bitrate(format) <= 256
}

fn youtube_audio_bitrate(format: &YtDlpFormat) -> i32 {
    format.abr.or(format.tbr).unwrap_or_default().round() as i32
}

fn youtube_audio_min_bitrate(quality: AudioQuality) -> i32 {
    match quality {
        AudioQuality::Quality64k => 0,
        AudioQuality::Quality132k => 81,
        AudioQuality::Quality192k => 161,
        AudioQuality::QualityDolby | AudioQuality::QualityDolbyBangumi | AudioQuality::QualityHiRES => 257,
    }
}

fn youtube_audio_max_bitrate(quality: AudioQuality) -> i32 {
    match quality {
        AudioQuality::Quality64k => 80,
        AudioQuality::Quality132k => 160,
        AudioQuality::Quality192k => 256,
        AudioQuality::QualityDolby | AudioQuality::QualityDolbyBangumi => 640,
        AudioQuality::QualityHiRES => i32::MAX,
    }
}

fn log_selected_youtube_streams(selected: &SelectedStreams, filter: &FilterOption) {
    let describe = |format: &YtDlpFormat| {
        format!(
            "id={} {}x{} fps={} vcodec={} acodec={} tbr={} vbr={} abr={}",
            format.format_id.as_deref().unwrap_or("-"),
            format.width.unwrap_or_default(),
            format.height.unwrap_or_default(),
            format.fps.unwrap_or_default(),
            format.vcodec.as_deref().unwrap_or("-"),
            format.acodec.as_deref().unwrap_or("-"),
            format.tbr.unwrap_or_default(),
            format.vbr.unwrap_or_default(),
            format.abr.unwrap_or_default(),
        )
    };
    info!(
        video_min = ?filter.video_min_quality,
        video_max = ?filter.video_max_quality,
        audio_min = ?filter.audio_min_quality,
        audio_max = ?filter.audio_max_quality,
        codecs = ?filter.codecs,
        video = selected.video.as_ref().map(&describe),
        audio = selected.audio.as_ref().map(&describe),
        mixed = selected.mixed.as_ref().map(&describe),
        "YouTube 已按项目流过滤设置选择下载格式"
    );
}

fn youtube_output_path(
    source: &youtube_source::Model,
    video: &youtube_video::Model,
    metadata: &YtDlpMetadata,
    title: &str,
    uploader: &str,
) -> Result<PathBuf> {
    let published_at = metadata
        .upload_date
        .as_deref()
        .or(video.published_at.as_deref())
        .and_then(parse_youtube_upload_date)
        .unwrap_or_else(crate::utils::time_format::now_naive);
    let time_format = crate::config::reload_config().time_format;
    let formatted_time = published_at.format(&time_format).to_string();
    let args = serde_json::json!({
        "bvid": metadata.id,
        "title": title,
        "upper_name": uploader,
        "upper_mid": metadata.channel_id.as_deref().unwrap_or(""),
        "pubtime": formatted_time,
        "fav_time": formatted_time,
        "show_title": title,
        "ptitle": title,
        "long_title": title,
        "pid": 1,
        "pid_pad": "01",
    });
    let (video_folder, page_name) = crate::config::with_config(|bundle| {
        Ok::<_, anyhow::Error>((
            bundle.render_video_template(&args)?,
            bundle.render_page_template(&args)?,
        ))
    })?;
    let extension = if source.audio_only { "m4a" } else { "mp4" };
    let root = PathBuf::from(&source.path);
    if source.flat_folder {
        Ok(root.join(format!("{page_name}.{extension}")))
    } else {
        Ok(root.join(video_folder).join(format!("{page_name}.{extension}")))
    }
}

async fn ensure_youtube_sidecars(
    downloader: &UnifiedDownloader,
    metadata: &YtDlpMetadata,
    output_path: &Path,
    video_url: &str,
    title: &str,
    uploader: &str,
    source: &youtube_source::Model,
) -> Option<String> {
    let mut warnings = Vec::new();
    let profile_url = metadata.channel_url.as_deref().or(metadata.uploader_url.as_deref());
    if let Err(error) = download_youtube_upper_face(downloader, uploader, profile_url).await {
        warn!(youtube_id = %metadata.id, error = %error, "YouTube 媒体已下载，但 UP 头像子任务失败");
        warnings.push(format!("UP头像下载失败：{error:#}"));
    }
    if let Err(error) = download_youtube_cover(downloader, metadata, output_path).await {
        warn!(youtube_id = %metadata.id, error = %error, "YouTube 媒体已下载，但封面子任务失败");
        warnings.push(format!("封面下载失败：{error:#}"));
    }
    if let Err(error) = generate_youtube_nfo(metadata, output_path, video_url, title, uploader).await {
        warn!(youtube_id = %metadata.id, error = %error, "YouTube 媒体已下载，但 NFO 子任务失败");
        warnings.push(format!("NFO 生成失败：{error:#}"));
    }
    if source.download_subtitle {
        if let Err(error) =
            download_youtube_subtitle(downloader, metadata, output_path, &source.ai_subtitle_language).await
        {
            warn!(youtube_id = %metadata.id, error = %error, "YouTube 媒体已下载，但字幕子任务失败");
            warnings.push(format!("字幕下载失败：{error:#}"));
        }
    }
    if source.download_danmaku {
        if let Err(error) = download_youtube_live_chat(downloader, metadata, output_path).await {
            warn!(youtube_id = %metadata.id, error = %error, "YouTube 媒体已下载，但直播聊天子任务失败");
            warnings.push(format!("直播聊天下载失败：{error:#}"));
        }
    }
    (!warnings.is_empty()).then(|| format!("媒体已完成；{}", warnings.join("；")))
}

async fn youtube_sidecars_complete(video: &youtube_video::Model, source: &youtube_source::Model) -> bool {
    if source.audio_only && source.audio_only_m4a_only {
        return true;
    }
    let Some(output_path) = video.output_path.as_deref().map(PathBuf::from) else {
        return false;
    };
    let media_ok = std::fs::metadata(&output_path).is_ok_and(|metadata| metadata.len() >= 1024);
    let cover_ok = youtube_sidecar_path(&output_path, "-thumb.jpg").is_ok_and(|path| path.is_file());
    let fanart_ok = youtube_sidecar_path(&output_path, "-fanart.jpg").is_ok_and(|path| path.is_file());
    let nfo_ok = output_path.with_extension("nfo").is_file();
    let (face_path, person_nfo_path) = youtube_upper_paths(&video.uploader);
    let upper_ok = face_path.is_file() && person_nfo_path.is_file();
    let subtitle_ok = !source.download_subtitle || youtube_subtitle_exists(&output_path).await.unwrap_or(false);
    let core_complete = media_ok && cover_ok && fanart_ok && nfo_ok && upper_ok && subtitle_ok;
    if !core_complete || !source.download_danmaku {
        return core_complete;
    }
    let live_chat_path = output_path.with_extension("live_chat.json");
    let checked_path = output_path.with_extension("live_chat.checked");
    if live_chat_path.is_file() || checked_path.is_file() {
        return true;
    }

    // 旧版本下载完成时已经执行过直播聊天探测，只是没有持久化“无聊天”
    // 结果。核心附属文件齐全即可一次性补写标记，避免升级后把整个媒体库
    // 逐条重新交给 yt-dlp 扫描。
    if tokio::fs::write(&checked_path, b"checked by completed-media migration\n")
        .await
        .is_ok()
    {
        debug!(youtube_id = %video.youtube_id, "已补记 YouTube 直播聊天检查状态");
        return true;
    }
    false
}

async fn download_youtube_upper_face(
    downloader: &UnifiedDownloader,
    uploader: &str,
    profile_url: Option<&str>,
) -> Result<()> {
    let uploader = crate::utils::filenamify::filenamify(uploader);
    if uploader.is_empty() {
        bail!("YouTube UP主名称为空");
    }
    let Some(profile_url) = profile_url.filter(|url| url.starts_with("http")) else {
        bail!("YouTube 元数据没有频道主页地址");
    };
    let upper_root = crate::config::reload_config().upper_path;
    let bucket = uploader.chars().next().unwrap_or('_').to_string().to_lowercase();
    let upper_dir = upper_root.join(bucket).join(&uploader);
    let face_path = upper_dir.join("folder.jpg");
    let person_nfo_path = upper_dir.join("person.nfo");
    let face_exists = tokio::fs::metadata(&face_path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024);
    let person_nfo_exists = tokio::fs::metadata(&person_nfo_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0);
    if face_exists && person_nfo_exists {
        return Ok(());
    }

    let profile = extract_youtube_source_metadata(profile_url).await?;

    tokio::fs::create_dir_all(&upper_dir)
        .await
        .with_context(|| format!("创建 YouTube UP头像目录失败: {}", upper_dir.display()))?;

    if !face_exists {
        let avatar_url = profile
            .thumbnails
            .iter()
            .filter(|thumbnail| {
                thumbnail
                    .id
                    .as_deref()
                    .is_some_and(|id| id.to_ascii_lowercase().contains("avatar"))
                    || thumbnail
                        .width
                        .zip(thumbnail.height)
                        .is_some_and(|(width, height)| width == height)
            })
            .max_by_key(|thumbnail| {
                let avatar = thumbnail
                    .id
                    .as_deref()
                    .is_some_and(|id| id.to_ascii_lowercase().contains("avatar"));
                (
                    i32::from(avatar),
                    thumbnail
                        .width
                        .unwrap_or_default()
                        .saturating_mul(thumbnail.height.unwrap_or_default()),
                )
            })
            .map(|thumbnail| thumbnail.url.as_str())
            .context("YouTube 频道主页没有返回 UP 头像")?;
        let temporary = upper_dir.join("folder.download");
        if let Err(error) = downloader
            .fetch_with_fallback(&[avatar_url], &temporary)
            .await
            .context("使用项目统一下载器下载 YouTube UP头像失败")
        {
            let _ = remove_file_if_exists(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &face_path).await?;
        info!(uploader, path = %face_path.display(), "YouTube UP头像下载完成");
    }

    if !person_nfo_exists {
        let channel_id = profile
            .channel_id
            .as_deref()
            .or(profile.uploader_id.as_deref())
            .or(profile.id.as_deref())
            .filter(|id| !id.trim().is_empty())
            .context("YouTube 频道主页没有返回频道 ID")?;
        let xml = generate_youtube_person_nfo(&uploader, channel_id);
        let temporary = upper_dir.join("person.nfo.download");
        if let Err(error) = tokio::fs::write(&temporary, xml.as_bytes())
            .await
            .with_context(|| format!("写入 YouTube UP主 NFO 临时文件失败: {}", temporary.display()))
        {
            let _ = remove_file_if_exists(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &person_nfo_path).await?;
        info!(uploader, path = %person_nfo_path.display(), "YouTube UP主 person.nfo 生成完成");
    }
    Ok(())
}

fn generate_youtube_person_nfo(uploader: &str, channel_id: &str) -> String {
    let escape = |value: &str| quick_xml::escape::escape(value).into_owned();
    format!(
        r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<person>
    <plot/>
    <outline/>
    <lockdata>false</lockdata>
    <dateadded>{}</dateadded>
    <title>{}</title>
    <sorttitle>{}</sorttitle>
    <uniqueid type="youtube_channel" default="true">{}</uniqueid>
</person>"#,
        now_standard_string(),
        escape(uploader),
        escape(uploader),
        escape(channel_id),
    )
}

async fn extract_youtube_source_metadata(url: &str) -> Result<YtDlpSourceMetadata> {
    let mut command = Command::new(ytdlp_executable());
    command.args([
        "--dump-single-json",
        "--flat-playlist",
        "--playlist-items",
        "0",
        "--skip-download",
        "--no-warnings",
    ]);
    append_ytdlp_runtime(&mut command);
    append_cookies(&mut command);
    command.arg(url);
    let output = tokio::time::timeout(DOWNLOAD_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow!("解析 YouTube 频道头像超时"))??;
    if !output.status.success() {
        bail!("yt-dlp 解析 YouTube 频道头像失败：{}", command_error(&output));
    }
    serde_json::from_slice(&output.stdout).context("解析 yt-dlp YouTube 频道元数据失败")
}

fn youtube_sidecar_path(output_path: &Path, suffix: &str) -> Result<PathBuf> {
    let parent = output_path.parent().context("YouTube 输出文件没有父目录")?;
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .context("YouTube 输出文件名无效")?;
    Ok(parent.join(format!("{stem}{suffix}")))
}

async fn download_youtube_cover(
    downloader: &UnifiedDownloader,
    metadata: &YtDlpMetadata,
    output_path: &Path,
) -> Result<()> {
    let url = metadata
        .thumbnail
        .as_deref()
        .context("YouTube 元数据没有返回视频封面")?;
    let thumb_path = youtube_sidecar_path(output_path, "-thumb.jpg")?;
    if !tokio::fs::metadata(&thumb_path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024)
    {
        let temporary = youtube_sidecar_path(output_path, "-thumb.download")?;
        if let Err(error) = downloader
            .fetch_with_fallback(&[url], &temporary)
            .await
            .context("使用项目统一下载器下载 YouTube 封面失败")
        {
            let _ = remove_file_if_exists(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &thumb_path).await?;
        info!(youtube_id = %metadata.id, path = %thumb_path.display(), "YouTube 视频封面下载完成");
    }
    let fanart_path = youtube_sidecar_path(output_path, "-fanart.jpg")?;
    if !tokio::fs::metadata(&fanart_path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024)
    {
        tokio::fs::copy(&thumb_path, &fanart_path)
            .await
            .with_context(|| format!("生成 YouTube fanart 失败: {}", fanart_path.display()))?;
        info!(youtube_id = %metadata.id, path = %fanart_path.display(), "YouTube fanart 生成完成");
    }
    Ok(())
}

async fn generate_youtube_nfo(
    metadata: &YtDlpMetadata,
    output_path: &Path,
    video_url: &str,
    title: &str,
    uploader: &str,
) -> Result<()> {
    if !crate::config::reload_config().nfo_config.enabled {
        return Ok(());
    }
    let nfo_path = output_path.with_extension("nfo");
    if tokio::fs::metadata(&nfo_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        return Ok(());
    }
    let aired = metadata
        .upload_date
        .as_deref()
        .and_then(parse_youtube_upload_date)
        .unwrap_or_else(crate::utils::time_format::now_naive);
    let escape = |value: &str| quick_xml::escape::escape(value).into_owned();
    let thumbnail = metadata.thumbnail.as_deref().unwrap_or_default();
    let description = metadata.description.as_deref().unwrap_or_default();
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?>\n\
<movie>\n\
    <title>{}</title>\n\
    <originaltitle>{}</originaltitle>\n\
    <sorttitle>{}</sorttitle>\n\
    <plot>{}</plot>\n\
    <uniqueid type=\"youtube\" default=\"true\">{}</uniqueid>\n\
    <year>{}</year>\n\
    <premiered>{}</premiered>\n\
    <aired>{}</aired>\n\
    <studio>YouTube</studio>\n\
    <director>{}</director>\n\
    <actor><name>{}</name><role>频道</role></actor>\n\
    <thumb aspect=\"poster\">{}</thumb>\n\
    <fanart><thumb>{}</thumb></fanart>\n\
    <website>{}</website>\n\
</movie>\n",
        escape(title),
        escape(title),
        escape(title),
        escape(description),
        escape(&metadata.id),
        aired.format("%Y"),
        aired.format("%Y-%m-%d"),
        aired.format("%Y-%m-%d"),
        escape(uploader),
        escape(uploader),
        escape(thumbnail),
        escape(thumbnail),
        escape(video_url),
    );
    let temporary = nfo_path.with_extension("nfo.download");
    tokio::fs::write(&temporary, xml.as_bytes())
        .await
        .with_context(|| format!("写入 YouTube NFO 失败: {}", temporary.display()))?;
    replace_file(&temporary, &nfo_path).await
}

async fn download_youtube_subtitle(
    downloader: &UnifiedDownloader,
    metadata: &YtDlpMetadata,
    output_path: &Path,
    preferred_language: &str,
) -> Result<()> {
    if youtube_subtitle_exists(output_path).await? {
        return Ok(());
    }
    let mut requested_order = youtube_subtitle_language_candidates(preferred_language);
    requested_order.extend(["zh-Hans", "zh-CN", "zh", "en", "ja"].map(str::to_string));
    requested_order.dedup();
    let manual = requested_order.iter().find_map(|language| {
        select_subtitle_item(&metadata.subtitles, language).map(|(item, url)| (language.clone(), item, url))
    });
    // 自动字幕优先原始语言，避免先请求 YouTube 的自动翻译字幕而快速触发 429。
    let mut automatic_order = Vec::new();
    if let Some(language) = metadata.language.as_deref() {
        automatic_order.push(language);
    }
    automatic_order.extend(requested_order.iter().map(String::as_str));
    automatic_order.dedup();
    let automatic = automatic_order.into_iter().find_map(|language| {
        select_subtitle_item(&metadata.automatic_captions, language)
            .and_then(|(item, url)| Some((language.to_string(), item, url)))
    });
    let selected = manual.or(automatic);
    let Some((language, subtitle, url)) = selected else {
        info!(youtube_id = %metadata.id, "YouTube 视频没有匹配的字幕，跳过字幕子任务");
        return Ok(());
    };
    let extension = subtitle.ext.as_deref().unwrap_or("vtt");
    let subtitle_path = output_path.with_extension(format!("{language}.{extension}"));
    if let Err(error) = downloader
        .fetch_with_fallback(&[url], &subtitle_path)
        .await
        .context("使用项目统一下载器下载 YouTube 字幕失败")
    {
        let _ = remove_file_if_exists(&subtitle_path).await;
        return Err(error);
    }
    Ok(())
}

fn youtube_subtitle_language_candidates(language: &str) -> Vec<String> {
    match language.trim() {
        "zh-CN" | "zh" | "zh-Hans" => vec!["zh-Hans".into(), "zh-CN".into(), "zh".into()],
        "en-US" | "en" => vec!["en-US".into(), "en".into()],
        "ja-JP" | "ja" => vec!["ja-JP".into(), "ja".into()],
        "ko-KR" | "ko" => vec!["ko-KR".into(), "ko".into()],
        value if !value.is_empty() => vec![value.to_string()],
        _ => vec!["zh-Hans".into(), "zh-CN".into(), "zh".into()],
    }
}

async fn download_youtube_live_chat(
    downloader: &UnifiedDownloader,
    metadata: &YtDlpMetadata,
    output_path: &Path,
) -> Result<()> {
    let live_chat_path = output_path.with_extension("live_chat.json");
    let checked_path = output_path.with_extension("live_chat.checked");
    if tokio::fs::metadata(&live_chat_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
        || tokio::fs::metadata(&checked_path).await.is_ok()
    {
        return Ok(());
    }
    let live_chat = metadata
        .subtitles
        .get("live_chat")
        .or_else(|| metadata.automatic_captions.get("live_chat"))
        .and_then(|items| items.iter().find(|item| item.url.as_deref().is_some()));
    let Some(url) = live_chat.and_then(|item| item.url.as_deref()) else {
        // 直播聊天回放在视频结束后是静态资源。为“没有直播聊天”的视频写入
        // 持久标记，确保每个视频只检查一次，后续扫描/重启不再反复解析。
        tokio::fs::write(&checked_path, b"no live chat replay\n")
            .await
            .with_context(|| format!("写入 YouTube 直播聊天检查标记失败: {}", checked_path.display()))?;
        debug!(youtube_id = %metadata.id, "YouTube 视频没有直播聊天，已记录检查结果");
        return Ok(());
    };
    let temporary = output_path.with_extension("live_chat.download");
    if let Err(error) = downloader
        .fetch_with_fallback(&[url], &temporary)
        .await
        .context("使用项目统一下载器下载 YouTube 直播聊天失败")
    {
        let _ = remove_file_if_exists(&temporary).await;
        return Err(error);
    }
    replace_file(&temporary, &live_chat_path).await
}

fn select_subtitle_item<'a>(
    subtitles: &'a HashMap<String, Vec<YtDlpSubtitle>>,
    language: &str,
) -> Option<(&'a YtDlpSubtitle, &'a str)> {
    subtitles.get(language).and_then(|items| {
        // 同一语言下 yt-dlp 可能同时返回原始字幕与带 `tlang=` 的自动翻译
        // 地址。优先原始 VTT，既符合来源语言，也避免自动翻译端点更容易触发
        // YouTube 429 限流。
        items
            .iter()
            .find(|item| {
                item.ext.as_deref() == Some("vtt") && item.url.as_deref().is_some_and(|url| !url.contains("tlang="))
            })
            .or_else(|| {
                items
                    .iter()
                    .find(|item| item.url.as_deref().is_some_and(|url| !url.contains("tlang=")))
            })
            .or_else(|| items.iter().find(|item| item.ext.as_deref() == Some("vtt")))
            .or_else(|| items.first())
            .and_then(|item| item.url.as_deref().map(|url| (item, url)))
    })
}

async fn youtube_subtitle_exists(output_path: &Path) -> Result<bool> {
    let Some(parent) = output_path.parent() else {
        return Ok(false);
    };
    let stem = output_path.file_stem().and_then(|value| value.to_str()).unwrap_or("");
    let mut entries = match tokio::fs::read_dir(parent).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("");
        if name.starts_with(&format!("{stem}.")) && matches!(extension, "vtt" | "srt" | "ass") {
            if tokio::fs::metadata(path).await.is_ok_and(|metadata| metadata.len() > 0) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn parse_youtube_upload_date(value: &str) -> Option<chrono::NaiveDateTime> {
    NaiveDate::parse_from_str(value, "%Y%m%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
}

fn youtube_quality_height(quality: VideoQuality) -> i32 {
    match quality {
        VideoQuality::Quality360p => 360,
        VideoQuality::Quality480p => 480,
        VideoQuality::Quality720p => 720,
        VideoQuality::Quality1080p | VideoQuality::Quality1080pPLUS | VideoQuality::Quality1080p60 => 1080,
        VideoQuality::Quality4k | VideoQuality::QualityHdr | VideoQuality::QualityDolby => 2160,
        VideoQuality::Quality8k => 4320,
    }
}

fn youtube_codec_rank(codec: Option<&str>, preferences: &[VideoCodecs]) -> usize {
    youtube_codec(codec)
        .and_then(|codec| preferences.iter().position(|preference| *preference == codec))
        .unwrap_or(preferences.len())
}

fn youtube_codec(codec: Option<&str>) -> Option<VideoCodecs> {
    let codec = codec.unwrap_or_default().to_ascii_lowercase();
    if codec.starts_with("avc") || codec.starts_with("h264") {
        Some(VideoCodecs::AVC)
    } else if codec.starts_with("hev") || codec.starts_with("hvc") {
        Some(VideoCodecs::HEV)
    } else if codec.starts_with("av01") || codec.starts_with("av1") {
        Some(VideoCodecs::AV1)
    } else {
        None
    }
}

fn is_http_format(format: &YtDlpFormat) -> bool {
    format.url.as_deref().is_some_and(|url| url.starts_with("http"))
        && format
            .protocol
            .as_deref()
            .is_none_or(|protocol| protocol == "http" || protocol == "https")
}

fn has_video(format: &YtDlpFormat) -> bool {
    format.vcodec.as_deref().is_some_and(|codec| codec != "none")
}

fn has_audio(format: &YtDlpFormat) -> bool {
    format.acodec.as_deref().is_some_and(|codec| codec != "none")
}

async fn replace_file(source: &Path, target: &Path) -> Result<()> {
    if tokio::fs::try_exists(target).await? {
        tokio::fs::remove_file(target).await?;
    }
    tokio::fs::rename(source, target)
        .await
        .with_context(|| format!("保存下载文件失败: {}", target.display()))
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn source_response(db: &DatabaseConnection, source: youtube_source::Model) -> Result<YouTubeSourceResponse> {
    let count = |status: &str| {
        youtube_video::Entity::find()
            .filter(youtube_video::Column::SourceId.eq(source.id))
            .filter(youtube_video::Column::DownloadStatus.eq(status))
            .count(db)
    };
    Ok(YouTubeSourceResponse {
        id: source.id,
        source_type: source.source_type,
        name: source.name,
        url: source.url,
        path: source.path,
        enabled: source.enabled,
        audio_only: source.audio_only,
        audio_only_m4a_only: source.audio_only_m4a_only,
        flat_folder: source.flat_folder,
        download_danmaku: source.download_danmaku,
        download_subtitle: source.download_subtitle,
        ai_subtitle_language: source.ai_subtitle_language,
        filter_option: source
            .filter_option
            .and_then(|value| serde_json::from_value::<FilterOption>(value).ok()),
        blacklist_keywords: source
            .blacklist_keywords
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default(),
        whitelist_keywords: source
            .whitelist_keywords
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default(),
        case_sensitive: source.keyword_case_sensitive,
        min_duration_seconds: source.min_duration_seconds,
        max_duration_seconds: source.max_duration_seconds,
        published_after: source.published_after,
        published_before: source.published_before,
        selected_videos: source
            .selected_videos
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default(),
        last_scan_at: source.last_scan_at,
        pending_count: count("pending").await?,
        completed_count: count("completed").await?,
        failed_count: count("failed").await?,
    })
}

fn video_response(video: youtube_video::Model) -> YouTubeVideoResponse {
    YouTubeVideoResponse {
        id: video.id,
        source_id: video.source_id,
        youtube_id: video.youtube_id,
        url: video.url,
        title: video.title,
        uploader: video.uploader,
        thumbnail: video.thumbnail,
        published_at: video.published_at,
        duration_seconds: video.duration_seconds,
        download_status: video.download_status,
        retry_count: video.retry_count,
        output_path: video.output_path,
        error_message: video.error_message,
    }
}

fn normalize_source_type(value: &str) -> Result<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "subscriptions" => Ok("subscriptions"),
        "channel" => Ok("channel"),
        "playlist" => Ok("playlist"),
        "liked" => Ok("liked"),
        "watch_later" => Ok("watch_later"),
        _ => bail!("来源类型必须是 subscriptions、channel、playlist、liked 或 watch_later"),
    }
}
fn resolve_source_url(kind: &str, supplied: Option<&str>) -> Result<String> {
    match kind {
        "subscriptions" => Ok(SUBSCRIPTIONS_URL.to_string()),
        "liked" => Ok(LIKED_URL.to_string()),
        "watch_later" => Ok(WATCH_LATER_URL.to_string()),
        _ => {
            let url = supplied
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("频道和播放列表来源必须填写链接"))?;
            Ok(if kind == "channel" {
                canonical_channel_url(&url)
            } else {
                url
            })
        }
    }
}

fn youtube_search_url(keyword: &str, source_type: &str) -> Result<reqwest::Url> {
    let filter = match source_type {
        "channel" => "EgIQAg%3D%3D",
        "playlist" => "EgIQAw%3D%3D",
        _ => bail!("YouTube 搜索仅支持频道或播放列表"),
    };
    let mut url = reqwest::Url::parse("https://www.youtube.com/results")?;
    url.query_pairs_mut()
        .append_pair("search_query", keyword)
        .append_pair("sp", filter);
    Ok(url)
}

/// 将频道任意标签页地址归一化为频道根地址，使 yt-dlp 能完整枚举普通视频、
/// Shorts 和直播回放。播放列表地址不会调用此函数。
fn canonical_channel_url(value: &str) -> String {
    let trimmed = value.trim();
    let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    let without_trailing_slash = without_query.trim_end_matches('/');
    let Some((scheme, rest)) = without_trailing_slash.split_once("://") else {
        return without_trailing_slash.to_string();
    };
    let mut parts = rest.split('/').filter(|part| !part.is_empty());
    let Some(host) = parts.next() else {
        return without_trailing_slash.to_string();
    };
    let Some(first) = parts.next() else {
        return without_trailing_slash.to_string();
    };
    let path = if first.starts_with('@') {
        first.to_string()
    } else if matches!(first.to_ascii_lowercase().as_str(), "channel" | "c" | "user") {
        let Some(second) = parts.next() else {
            return without_trailing_slash.to_string();
        };
        format!("{first}/{second}")
    } else {
        return without_trailing_slash.to_string();
    };
    format!("{scheme}://{host}/{path}")
}
async fn ytdlp_version() -> Option<String> {
    ytdlp_version_at(&ytdlp_executable()).await
}

async fn ytdlp_version_at(executable: &Path) -> Option<String> {
    let output = tokio::time::timeout(
        YTDLP_VERSION_TIMEOUT,
        Command::new(executable).arg("--version").output(),
    )
    .await
    .ok()?
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

async fn ensure_ytdlp_available() -> Result<()> {
    if let Ok(configured) = std::env::var("BILI_SYNC_YTDLP_PATH") {
        let configured = PathBuf::from(configured);
        if ytdlp_version_at(&configured).await.is_some() {
            return Ok(());
        }
        bail!("BILI_SYNC_YTDLP_PATH 指向的 yt-dlp 不可用：{}", configured.display());
    }

    if ytdlp_version().await.is_some() {
        return Ok(());
    }

    let package = current_ytdlp_package().ok_or_else(|| {
        anyhow!(
            "当前系统架构暂不支持自动下载 yt-dlp（{}-{}），请设置 BILI_SYNC_YTDLP_PATH",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let _guard = ytdlp_install_lock().lock().await;

    if ytdlp_version().await.is_some() {
        return Ok(());
    }

    let binary_path = managed_ytdlp_path(package);
    download_and_install_ytdlp(package, &binary_path).await?;
    let version = ytdlp_version_at(&binary_path)
        .await
        .ok_or_else(|| anyhow!("yt-dlp 安装完成后执行校验失败：{}", binary_path.display()))?;
    info!(
        version,
        target = package.target_key,
        path = %binary_path.display(),
        "yt-dlp 已自动安装并可用"
    );
    Ok(())
}

fn ytdlp_install_lock() -> &'static Mutex<()> {
    YTDLP_INSTALL_LOCK.get_or_init(|| Mutex::new(()))
}

fn current_ytdlp_package() -> Option<YtDlpPackage> {
    ytdlp_package_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(target_env = "musl") { "musl" } else { "" },
    )
}

fn ytdlp_package_for(os: &str, arch: &str, target_env: &str) -> Option<YtDlpPackage> {
    let raw = |target_key, asset_name, binary_name| YtDlpPackage {
        target_key,
        asset_name,
        binary_name,
        archive_binary_name: None,
    };
    match (os, arch, target_env) {
        ("windows", "x86_64", _) => Some(raw("windows-x86_64", "yt-dlp.exe", "yt-dlp.exe")),
        ("windows", "x86", _) => Some(raw("windows-x86", "yt-dlp_x86.exe", "yt-dlp.exe")),
        ("windows", "aarch64", _) => Some(raw("windows-aarch64", "yt-dlp_arm64.exe", "yt-dlp.exe")),
        ("linux", "x86_64", "musl") => Some(raw("linux-x86_64-musl", "yt-dlp_musllinux", "yt-dlp")),
        ("linux", "aarch64", "musl") => Some(raw("linux-aarch64-musl", "yt-dlp_musllinux_aarch64", "yt-dlp")),
        ("linux", "x86_64", _) => Some(raw("linux-x86_64", "yt-dlp_linux", "yt-dlp")),
        ("linux", "aarch64", _) => Some(raw("linux-aarch64", "yt-dlp_linux_aarch64", "yt-dlp")),
        // 官方 ARMv7 构建是 one-dir zip，主程序运行时还需要同包的 `_internal` 目录。
        ("linux", "arm", _) => Some(YtDlpPackage {
            target_key: "linux-armv7l",
            asset_name: "yt-dlp_linux_armv7l.zip",
            binary_name: "yt-dlp_linux_armv7l",
            archive_binary_name: Some("yt-dlp_linux_armv7l"),
        }),
        ("macos", "x86_64" | "aarch64", _) => Some(raw("macos-universal", "yt-dlp_macos", "yt-dlp")),
        _ => None,
    }
}

fn managed_ytdlp_path(package: YtDlpPackage) -> PathBuf {
    CONFIG_DIR
        .join("tools")
        .join("yt-dlp")
        .join(package.target_key)
        .join(package.binary_name)
}

async fn download_and_install_ytdlp(package: YtDlpPackage, binary_path: &Path) -> Result<()> {
    let parent = binary_path
        .parent()
        .ok_or_else(|| anyhow!("yt-dlp 安装路径无效：{}", binary_path.display()))?;
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("创建 yt-dlp 安装目录失败：{}", parent.display()))?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(YTDLP_DOWNLOAD_TIMEOUT)
        .user_agent(concat!("bili-sync-up/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("创建 yt-dlp 下载客户端失败")?;
    let asset_url = format!("{YTDLP_RELEASE_BASE_URL}/{}", package.asset_name);
    let checksums_url = format!("{YTDLP_RELEASE_BASE_URL}/SHA2-256SUMS");
    info!(
        target = package.target_key,
        asset = package.asset_name,
        url = asset_url,
        "本机未检测到 yt-dlp，开始下载对应系统版本"
    );

    let (asset_response, checksums_response) =
        tokio::try_join!(client.get(&asset_url).send(), client.get(&checksums_url).send())
            .context("请求 yt-dlp 官方发布文件失败")?;
    let asset_bytes = asset_response
        .error_for_status()
        .with_context(|| format!("下载 yt-dlp 返回错误状态：{asset_url}"))?
        .bytes()
        .await
        .context("读取 yt-dlp 下载内容失败")?;
    let checksums = checksums_response
        .error_for_status()
        .with_context(|| format!("下载 yt-dlp 校验文件返回错误状态：{checksums_url}"))?
        .text()
        .await
        .context("读取 yt-dlp 校验文件失败")?;
    let expected = checksum_for_release_asset(&checksums, package.asset_name)
        .ok_or_else(|| anyhow!("yt-dlp 官方校验文件中缺少 {}", package.asset_name))?;
    let actual = hex::encode(Sha256::digest(asset_bytes.as_ref()));
    if !actual.eq_ignore_ascii_case(expected) {
        bail!(
            "yt-dlp 文件校验失败：asset={}, expected={}, actual={}",
            package.asset_name,
            expected,
            actual
        );
    }

    if let Some(archive_binary_name) = package.archive_binary_name {
        install_ytdlp_zip(asset_bytes.as_ref(), binary_path, archive_binary_name).await?;
        return Ok(());
    }

    let temporary = binary_path.with_extension(if cfg!(windows) { "download.exe" } else { "download" });
    if let Err(error) = tokio::fs::write(&temporary, asset_bytes.as_ref()).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error).with_context(|| format!("写入 yt-dlp 临时文件失败：{}", temporary.display()));
    }
    set_ytdlp_executable_permissions(&temporary).await?;
    if ytdlp_version_at(&temporary).await.is_none() {
        let _ = tokio::fs::remove_file(&temporary).await;
        bail!("下载的 yt-dlp 无法执行：{}", temporary.display());
    }
    replace_file(&temporary, binary_path)
        .await
        .with_context(|| format!("安装 yt-dlp 失败：{}", binary_path.display()))
}

async fn install_ytdlp_zip(bytes: &[u8], binary_path: &Path, archive_binary_name: &str) -> Result<()> {
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    let install_dir = binary_path
        .parent()
        .ok_or_else(|| anyhow!("yt-dlp 安装路径无效：{}", binary_path.display()))?
        .to_path_buf();
    let staging_dir = install_dir.with_extension("download");
    if tokio::fs::try_exists(&staging_dir).await? {
        tokio::fs::remove_dir_all(&staging_dir)
            .await
            .with_context(|| format!("清理 yt-dlp 临时目录失败：{}", staging_dir.display()))?;
    }
    tokio::fs::create_dir_all(&staging_dir)
        .await
        .with_context(|| format!("创建 yt-dlp 临时目录失败：{}", staging_dir.display()))?;

    let bytes = bytes.to_vec();
    let staging_for_extract = staging_dir.clone();
    let archive_binary_name = archive_binary_name.to_owned();
    let archive_binary_for_extract = archive_binary_name.clone();
    let extract_result = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).context("解析 yt-dlp zip 文件失败")?;
        let mut found_binary = false;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).context("读取 yt-dlp zip 条目失败")?;
            let relative = entry
                .enclosed_name()
                .ok_or_else(|| anyhow!("yt-dlp zip 包含不安全路径：{}", entry.name()))?;
            let output = staging_for_extract.join(&relative);
            if entry.is_dir() {
                std::fs::create_dir_all(&output)?;
                continue;
            }
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut contents = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut contents)?;
            std::fs::write(&output, contents)?;
            if relative == Path::new(&archive_binary_for_extract) {
                found_binary = true;
            }
        }
        if !found_binary {
            bail!("yt-dlp zip 中未找到主程序：{archive_binary_for_extract}");
        }
        Ok(())
    })
    .await
    .context("解压 yt-dlp zip 子任务失败")?;
    if let Err(error) = extract_result {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        return Err(error);
    }

    let staged_binary = staging_dir.join(archive_binary_name);
    set_ytdlp_executable_permissions(&staged_binary).await?;
    if ytdlp_version_at(&staged_binary).await.is_none() {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        bail!("下载的 yt-dlp 无法执行：{}", staged_binary.display());
    }
    if tokio::fs::try_exists(&install_dir).await? {
        tokio::fs::remove_dir_all(&install_dir)
            .await
            .with_context(|| format!("替换 yt-dlp 安装目录失败：{}", install_dir.display()))?;
    }
    tokio::fs::rename(&staging_dir, &install_dir)
        .await
        .with_context(|| format!("安装 yt-dlp 失败：{}", install_dir.display()))
}

fn checksum_for_release_asset<'a>(manifest: &'a str, asset_name: &str) -> Option<&'a str> {
    manifest.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let checksum = parts.next()?;
        let filename = parts.next()?.trim_start_matches('*');
        (filename == asset_name && checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(checksum)
    })
}

async fn set_ytdlp_executable_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("读取 yt-dlp 文件权限失败：{}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        tokio::fs::set_permissions(path, permissions)
            .await
            .with_context(|| format!("设置 yt-dlp 可执行权限失败：{}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn ytdlp_executable() -> PathBuf {
    if let Ok(path) = std::env::var("BILI_SYNC_YTDLP_PATH") {
        return PathBuf::from(path);
    }
    if let Some(package) = current_ytdlp_package() {
        let managed = managed_ytdlp_path(package);
        if managed.is_file() {
            return managed;
        }
    }
    // 兼容早期 YouTube 测试版使用的缓存位置。
    let legacy_managed = CONFIG_DIR
        .join("bin")
        .join(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" });
    if legacy_managed.is_file() {
        return legacy_managed;
    }
    PathBuf::from("yt-dlp")
}
fn cookie_path() -> PathBuf {
    CONFIG_DIR.join("youtube-cookies.txt")
}
fn default_output_path() -> PathBuf {
    CONFIG_DIR.join("youtube-downloads")
}
async fn remove_recorded_output(source_path: &str, output_path: &str) -> Result<()> {
    let base = PathBuf::from(source_path);
    let output = PathBuf::from(output_path);
    if !output.starts_with(&base) {
        warn!(path = %output.display(), "跳过不在 YouTube 来源目录中的记录文件");
        return Ok(());
    }
    for path in recorded_output_files(&output).await? {
        match tokio::fs::remove_file(&path).await {
            Ok(()) => info!(path = %path.display(), "已删除 YouTube 已记录媒体/附属文件"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    remove_empty_parent_directories(output.parent(), &base).await;
    Ok(())
}

async fn recorded_output_files(output: &Path) -> Result<Vec<PathBuf>> {
    let Some(parent) = output.parent() else {
        return Ok(vec![output.to_path_buf()]);
    };
    let output_stem = output.file_stem().and_then(|value| value.to_str()).unwrap_or("");
    let mut files = Vec::new();
    let mut entries = match tokio::fs::read_dir(parent).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
        let is_sidecar = !output_stem.is_empty()
            && (name.starts_with(&format!("{output_stem}."))
                || name.starts_with(&format!("{output_stem}-thumb."))
                || name.starts_with(&format!("{output_stem}-fanart.")));
        if path == output || is_sidecar {
            files.push(path);
        }
    }
    Ok(files)
}

async fn move_file_cross_volume(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if tokio::fs::try_exists(target).await? {
        tokio::fs::remove_file(target).await?;
    }
    match tokio::fs::rename(source, target).await {
        Ok(()) => Ok(()),
        Err(_) => {
            tokio::fs::copy(source, target)
                .await
                .with_context(|| format!("跨磁盘复制 YouTube 文件失败: {}", source.display()))?;
            tokio::fs::remove_file(source).await?;
            Ok(())
        }
    }
}

async fn remove_empty_parent_directories(mut current: Option<&Path>, stop_at: &Path) {
    while let Some(directory) = current {
        if directory == stop_at || !directory.starts_with(stop_at) {
            break;
        }
        match tokio::fs::remove_dir(directory).await {
            Ok(()) => current = directory.parent(),
            Err(_) => break,
        }
    }
}

async fn validate_youtube_login_cookie(path: &Path) -> Result<()> {
    let output = tokio::time::timeout(LOGIN_TIMEOUT, async {
        let mut command = Command::new(ytdlp_executable());
        command.arg("--cookies").arg(path).args([
            "--skip-download",
            "--no-playlist",
            "--no-warnings",
            "--format",
            "bv*+ba/b",
        ]);
        append_ytdlp_runtime(&mut command);
        command.arg(YTDLP_TEST_VIDEO).output().await
    })
    .await
    .map_err(|_| anyhow!("验证 YouTube 登录会话超时"))??;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    Ok(())
}

async fn prepare_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("无效的 Cookie 文件路径"))?;
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("无法创建配置目录: {}", parent.display()))
}
fn append_cookies(command: &mut Command) {
    let path = cookie_path();
    if has_youtube_session(&path) {
        command.arg("--cookies").arg(path);
    }
}

fn append_ytdlp_runtime(command: &mut Command) {
    if let Some((name, path)) = ytdlp_js_runtime() {
        command.arg("--js-runtimes").arg(format!("{name}:{}", path.display()));
    }
}

fn ytdlp_js_runtime() -> Option<(&'static str, PathBuf)> {
    if let Ok(configured) = std::env::var("BILI_SYNC_YTDLP_JS_RUNTIME") {
        let path = PathBuf::from(configured);
        if path.is_file() {
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .filter(|value| value.eq_ignore_ascii_case("bun"))
                .map(|_| "bun")
                .unwrap_or("node");
            return Some((name, path));
        }
    }
    #[cfg(windows)]
    {
        let node_candidates = [
            PathBuf::from(r"C:\Program Files\nodejs\node.exe"),
            PathBuf::from(r"C:\Program Files (x86)\nodejs\node.exe"),
            CONFIG_DIR.join("bin").join("node.exe"),
        ];
        if let Some(path) = node_candidates.into_iter().find(|path| path.is_file()) {
            return Some(("node", path));
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let bun = PathBuf::from(profile).join(".bun").join("bin").join("bun.exe");
            if bun.is_file() {
                return Some(("bun", bun));
            }
        }
    }
    None
}
fn interactive_login_profile(browser: &str) -> PathBuf {
    CONFIG_DIR.join("youtube-login-browser").join(browser)
}

fn browser_executable(browser: &str) -> Result<PathBuf> {
    let candidates: Vec<PathBuf> = match browser {
        "edge" => vec![
            PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
            PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
            std::env::var_os("LOCALAPPDATA")
                .map(|root| PathBuf::from(root).join(r"Microsoft\Edge\Application\msedge.exe"))
                .unwrap_or_default(),
        ],
        "chrome" => vec![
            PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
            std::env::var_os("LOCALAPPDATA")
                .map(|root| PathBuf::from(root).join(r"Google\Chrome\Application\chrome.exe"))
                .unwrap_or_default(),
        ],
        "brave" => vec![
            PathBuf::from(r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe"),
            std::env::var_os("LOCALAPPDATA")
                .map(|root| PathBuf::from(root).join(r"BraveSoftware\Brave-Browser\Application\brave.exe"))
                .unwrap_or_default(),
        ],
        "firefox" => vec![PathBuf::from(r"C:\Program Files\Mozilla Firefox\firefox.exe")],
        "chromium" => vec![PathBuf::from("chromium")],
        _ => Vec::new(),
    };
    candidates
        .into_iter()
        .find(|path| path.is_file() || path == &PathBuf::from("chromium"))
        .ok_or_else(|| anyhow!("未找到 {} 浏览器，请改用 Edge、Chrome 或导入 cookies.txt", browser))
}

fn cdp_cookies_to_netscape(cookies: &[serde_json::Value]) -> String {
    let mut lines = vec![
        "# Netscape HTTP Cookie File".to_string(),
        "# Generated from the Bili Sync YouTube login window".to_string(),
    ];
    for cookie in cookies {
        let domain = cookie.get("domain").and_then(|value| value.as_str()).unwrap_or("");
        if !domain.contains("youtube.com") && !domain.contains("google.com") {
            continue;
        }
        let name = cookie.get("name").and_then(|value| value.as_str()).unwrap_or("");
        let value = cookie.get("value").and_then(|value| value.as_str()).unwrap_or("");
        if domain.is_empty() || name.is_empty() {
            continue;
        }
        let include_subdomains = if domain.starts_with('.') { "TRUE" } else { "FALSE" };
        let path = cookie.get("path").and_then(|value| value.as_str()).unwrap_or("/");
        let secure = if cookie.get("secure").and_then(|value| value.as_bool()).unwrap_or(false) {
            "TRUE"
        } else {
            "FALSE"
        };
        let expiry = cookie
            .get("expires")
            .and_then(|value| value.as_f64())
            .filter(|value| *value > 0.0)
            .unwrap_or(0.0) as i64;
        lines.push(format!(
            "{domain}\t{include_subdomains}\t{path}\t{secure}\t{expiry}\t{name}\t{value}"
        ));
    }
    format!("{}\n", lines.join("\n"))
}

fn normalize_browser(browser: &str) -> Result<&'static str> {
    match browser.trim().to_ascii_lowercase().as_str() {
        "chrome" => Ok("chrome"),
        "edge" => Ok("edge"),
        "firefox" => Ok("firefox"),
        "brave" => Ok("brave"),
        "chromium" => Ok("chromium"),
        _ => bail!("不支持的浏览器；请选择 Chrome、Edge、Firefox、Brave 或 Chromium"),
    }
}
fn has_youtube_session(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|contents| {
        contents.lines().any(|line| {
            if line.trim_start().starts_with('#') {
                return false;
            }
            let columns = line.split('\t').collect::<Vec<_>>();
            if columns.len() < 7 || !columns[0].contains("youtube.com") {
                return false;
            }
            matches!(
                columns[5],
                "SID" | "SAPISID" | "APISID" | "__Secure-1PSID" | "__Secure-3PSID"
            )
        })
    })
}

fn is_netscape_youtube_cookie_file(contents: &str) -> bool {
    let has_header = contents
        .lines()
        .take(4)
        .any(|line| line.contains("HTTP Cookie File") || line.contains("Netscape"));
    has_header
        && contents.lines().any(|line| {
            if line.trim_start().starts_with('#') {
                return false;
            }
            let columns = line.split('\t').collect::<Vec<_>>();
            columns.len() >= 7
                && columns[0].contains("youtube.com")
                && matches!(
                    columns[5],
                    "SID" | "SAPISID" | "APISID" | "__Secure-1PSID" | "__Secure-3PSID"
                )
        })
}

fn is_youtube_url(value: &str) -> bool {
    let Some(rest) = value
        .trim()
        .strip_prefix("https://")
        .or_else(|| value.trim().strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        host.as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be" | "www.youtu.be"
    )
}
fn command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    trim_output(if stderr.trim().is_empty() { &stdout } else { &stderr })
}
fn trim_output(text: &str) -> String {
    const MAX: usize = 6_000;
    let text = text.trim();
    if text.len() <= MAX {
        text.to_string()
    } else {
        format!("…{}", &text[text.len() - MAX..])
    }
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_channel_url, checksum_for_release_asset, current_ytdlp_package, generate_youtube_person_nfo,
        is_netscape_youtube_cookie_file, is_youtube_url, normalize_source_type, resolve_source_url, youtube_search_url,
        ytdlp_package_for, SUBSCRIPTIONS_URL,
    };
    #[test]
    fn validates_types_and_urls() {
        assert_eq!(normalize_source_type("playlist").unwrap(), "playlist");
        assert_eq!(resolve_source_url("subscriptions", None).unwrap(), SUBSCRIPTIONS_URL);
        assert!(is_youtube_url("https://www.youtube.com/watch?v=abc"));
        assert!(!is_youtube_url("https://youtube.example.com"));
    }

    #[test]
    fn channel_tabs_are_normalized_for_full_channel_scan() {
        assert_eq!(
            canonical_channel_url("https://www.youtube.com/@ayu_photo_18/videos"),
            "https://www.youtube.com/@ayu_photo_18"
        );
        assert_eq!(
            canonical_channel_url("https://www.youtube.com/channel/UC123/shorts?view=0"),
            "https://www.youtube.com/channel/UC123"
        );
        assert_eq!(
            resolve_source_url("channel", Some("https://www.youtube.com/c/example/streams")).unwrap(),
            "https://www.youtube.com/c/example"
        );
    }

    #[test]
    fn builds_filtered_youtube_source_search_urls() {
        let channel = youtube_search_url("波崎天結", "channel").unwrap();
        assert_eq!(
            channel.query_pairs().find(|(key, _)| key == "search_query").unwrap().1,
            "波崎天結"
        );
        assert_eq!(
            channel.query_pairs().find(|(key, _)| key == "sp").unwrap().1,
            "EgIQAg%3D%3D"
        );

        let playlist = youtube_search_url("音乐", "playlist").unwrap();
        assert_eq!(
            playlist.query_pairs().find(|(key, _)| key == "sp").unwrap().1,
            "EgIQAw%3D%3D"
        );
        assert!(youtube_search_url("test", "subscriptions").is_err());
    }

    #[test]
    fn login_cookie_must_belong_to_youtube_domain() {
        let google_only = "# Netscape HTTP Cookie File\n.google.com\tTRUE\t/\tTRUE\t0\tSID\tvalue\n";
        let youtube_session = "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\t__Secure-3PSID\tvalue\n";
        assert!(!is_netscape_youtube_cookie_file(google_only));
        assert!(is_netscape_youtube_cookie_file(youtube_session));
    }

    #[test]
    fn youtube_person_nfo_matches_existing_library_shape() {
        let nfo = generate_youtube_person_nfo("A&B <频道>", "UC-A&B");
        assert!(nfo.contains("<person>"));
        assert!(nfo.contains("<title>A&amp;B &lt;频道&gt;</title>"));
        assert!(nfo.contains("<sorttitle>A&amp;B &lt;频道&gt;</sorttitle>"));
        assert!(nfo.contains("<uniqueid type=\"youtube_channel\" default=\"true\">UC-A&amp;B</uniqueid>"));
    }

    #[test]
    fn parses_official_ytdlp_checksum_manifest() {
        let manifest = concat!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  yt-dlp\n",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB *yt-dlp.exe\n",
        );
        assert_eq!(
            checksum_for_release_asset(manifest, "yt-dlp.exe"),
            Some("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB")
        );
        assert_eq!(checksum_for_release_asset(manifest, "missing"), None);
        assert_eq!(
            checksum_for_release_asset("not-a-checksum  yt-dlp.exe", "yt-dlp.exe"),
            None
        );
    }

    #[test]
    fn current_platform_has_ytdlp_download_asset() {
        let package = current_ytdlp_package().expect("受支持的构建平台必须提供 yt-dlp 下载文件");
        assert!(!package.target_key.is_empty());
        assert!(package.asset_name.starts_with("yt-dlp"));
        assert!(package.binary_name.starts_with("yt-dlp"));
    }

    #[test]
    fn selects_official_ytdlp_asset_for_every_release_target() {
        let windows = ytdlp_package_for("windows", "x86_64", "").unwrap();
        assert_eq!(windows.asset_name, "yt-dlp.exe");
        assert_eq!(windows.archive_binary_name, None);

        let linux_musl = ytdlp_package_for("linux", "x86_64", "musl").unwrap();
        assert_eq!(linux_musl.asset_name, "yt-dlp_musllinux");
        let linux_gnu = ytdlp_package_for("linux", "x86_64", "gnu").unwrap();
        assert_eq!(linux_gnu.asset_name, "yt-dlp_linux");

        let linux_arm64_musl = ytdlp_package_for("linux", "aarch64", "musl").unwrap();
        assert_eq!(linux_arm64_musl.asset_name, "yt-dlp_musllinux_aarch64");
        let linux_armv7 = ytdlp_package_for("linux", "arm", "musl").unwrap();
        assert_eq!(linux_armv7.asset_name, "yt-dlp_linux_armv7l.zip");
        assert_eq!(linux_armv7.archive_binary_name, Some("yt-dlp_linux_armv7l"));

        let macos = ytdlp_package_for("macos", "aarch64", "").unwrap();
        assert_eq!(macos.asset_name, "yt-dlp_macos");
        assert!(ytdlp_package_for("freebsd", "x86_64", "").is_none());
    }
}
