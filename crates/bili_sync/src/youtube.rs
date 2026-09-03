//! YouTube 作为持续扫描的视频源接入。
//!
//! `yt-dlp` 仅处理 YouTube 的播放列表枚举和媒体直链解析；调度周期、暂停控制、
//! 并发上限、来源持久化及下载状态均由 bili-sync 管理。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::{Extension, Json, Path as AxumPath, Query};
use chrono::{Local, NaiveDate, TimeZone};
use futures::{stream, StreamExt};

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use bili_sync_entity::{youtube_source, youtube_video};

use crate::api::wrapper::{ApiError, ApiResponse};
use crate::api::{
    request::{ResetSpecificTasksRequest, ResetVideoSourcePathRequest, UpdateVideoStatusRequest, VideosRequest},
    response::{
        DeleteVideoResponse, PageInfo, ResetAllVideosResponse, ResetVideoResponse, ResetVideoSourcePathResponse,
        SubmissionVideoInfo, SubmissionVideosResponse, UpdateVideoStatusResponse, VideoInfo, VideoResponse,
        VideoSourceTag, VideosResponse,
    },
};
use crate::bilibili::{
    AudioQuality, DanmakuElem, DanmakuWriter, FilterOption, PageInfo as BiliPageInfo, VideoCodecs, VideoQuality,
};
use crate::config::CONFIG_DIR;
use crate::external_media::{ExternalMediaFormat, ExternalMediaMetadata, ExternalSubtitle};
use crate::task::TASK_CONTROLLER;
use crate::unified_downloader::UnifiedDownloader;
use crate::utils::live_updates::{notify_queue_status_changed, notify_video_sources_changed, notify_videos_changed};
use crate::utils::time_format::now_standard_string;

const YTDLP_VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const YTDLP_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const YTDLP_RELEASE_BASE_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(90);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const MAX_DOWNLOAD_RETRIES: i32 = 4;
const SUBSCRIPTIONS_URL: &str = "https://www.youtube.com/feed/subscriptions";
const SUBSCRIPTION_CHANNELS_URL: &str = "https://www.youtube.com/feed/channels";
const LIKED_URL: &str = "https://www.youtube.com/playlist?list=LL";
const WATCH_LATER_URL: &str = "https://www.youtube.com/playlist?list=WL";

/// 扫描失败告警静默期：同一来源失败后 6 小时内不重复打 WARN，避免私有源
/// （稍后再看/喜欢/订阅）登录失效时每轮（约 20 分钟）刷屏告警。
const SCAN_FAILURE_WARN_COOLDOWN: Duration = Duration::from_secs(6 * 60 * 60);
static SCAN_FAILURE_WARN_LAST: LazyLock<StdMutex<HashMap<i32, Instant>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

static YTDLP_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn parse_video_id_set(value: Option<&str>) -> HashSet<String> {
    value
        .and_then(|value| serde_json::from_str::<HashSet<String>>(value).ok())
        .unwrap_or_default()
}

pub(crate) fn serialize_video_id_set(values: &HashSet<String>) -> Result<Option<String>> {
    if values.is_empty() {
        return Ok(None);
    }
    let mut values = values.iter().cloned().collect::<Vec<_>>();
    values.sort();
    Ok(Some(serde_json::to_string(&values)?))
}

#[derive(Debug, Clone, Copy)]
struct YtDlpPackage {
    target_key: &'static str,
    asset_name: &'static str,
    binary_name: &'static str,
    archive_binary_name: Option<&'static str>,
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
    #[serde(default)]
    pub url: Option<String>,
    pub source_type: String,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub keyword: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct YouTubeChannelPlaylistsRequest {
    pub url: String,
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
    #[serde(default)]
    pub ai_rename: bool,
    pub ai_rename_video_prompt: Option<String>,
    pub ai_rename_audio_prompt: Option<String>,
    #[serde(default)]
    pub ai_rename_enable_multi_page: bool,
    #[serde(default)]
    pub ai_rename_enable_collection: bool,
    #[serde(default)]
    pub ai_rename_enable_bangumi: bool,
    #[serde(default)]
    pub ai_rename_rename_parent_dir: bool,
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
    #[serde(default)]
    pub selected_channels: Vec<String>,
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
    pub ai_rename: Option<bool>,
    pub ai_rename_video_prompt: Option<String>,
    pub ai_rename_audio_prompt: Option<String>,
    pub ai_rename_enable_multi_page: Option<bool>,
    pub ai_rename_enable_collection: Option<bool>,
    pub ai_rename_enable_bangumi: Option<bool>,
    pub ai_rename_rename_parent_dir: Option<bool>,
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

#[derive(Debug, Clone, Serialize)]
pub struct YouTubeQueueStatusResponse {
    pub pending: u64,
    pub downloading: u64,
    pub completed: u64,
    pub failed: u64,
    pub skipped: u64,
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
    pub scan_deleted_videos: bool,
    pub scan_deleted_videos_once: bool,
    pub audio_only: bool,
    pub audio_only_m4a_only: bool,
    pub flat_folder: bool,
    pub download_danmaku: bool,
    pub download_subtitle: bool,
    pub ai_subtitle_language: String,
    pub ai_rename: bool,
    pub ai_rename_video_prompt: String,
    pub ai_rename_audio_prompt: String,
    pub ai_rename_enable_multi_page: bool,
    pub ai_rename_enable_collection: bool,
    pub ai_rename_enable_bangumi: bool,
    pub ai_rename_rename_parent_dir: bool,
    pub filter_option: Option<FilterOption>,
    pub blacklist_keywords: Vec<String>,
    pub whitelist_keywords: Vec<String>,
    pub case_sensitive: bool,
    pub min_duration_seconds: Option<i32>,
    pub max_duration_seconds: Option<i32>,
    pub published_after: Option<String>,
    pub published_before: Option<String>,
    pub selected_videos: Vec<String>,
    pub selected_channels: Vec<String>,
    pub last_scan_at: Option<String>,
    pub pending_count: u64,
    pub completed_count: u64,
    pub failed_count: u64,
    pub skipped_count: u64,
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
    pub is_charge_video: bool,
    pub charge_can_play: bool,
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
    pub container_runtime: bool,
    pub cookie_path: String,
}

#[derive(Debug, Serialize)]
pub struct YouTubeLoginResponse {
    pub logged_in: bool,
    pub message: String,
}


pub async fn youtube_status() -> Result<ApiResponse<YouTubeStatusResponse>, ApiError> {
    // 与 aria2 一致：第一次进入设置页/添加源页时若本机没有 yt-dlp，
    // 自动下载当前系统对应的官方可执行文件。
    if let Err(error) = ensure_ytdlp_available().await {
        warn!(%error, "yt-dlp 自动安装失败");
    }
    let version = ytdlp_version().await;
    let logged_in = youtube_has_session()
        && tokio::time::timeout(Duration::from_secs(15), load_youtube_subscription_channels())
            .await
            .is_ok_and(|result| result.is_ok());
    Ok(ApiResponse::ok(YouTubeStatusResponse {
        ytdlp_available: version.is_some(),
        ytdlp_version: version,
        logged_in,
        default_output_path: default_output_path().display().to_string(),
        container_runtime: is_container_runtime(),
        cookie_path: youtube_cookie_file().map(|path| path.display().to_string()).unwrap_or_default(),
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

    // “播放列表/收藏”来源先搜索 UP 主频道，选择频道后再加载其全部播放列表。
    let channel_mode = matches!(source_type.as_str(), "channel" | "playlist");
    let search_url = youtube_search_url(
        keyword,
        if channel_mode {
            "channel"
        } else {
            &source_type
        },
    )?;
    let mut command = ytdlp_command();
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
    append_youtube_proxy(&mut command);
    append_ytdlp_tab_args(&mut command);
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
            .filter(|_| channel_mode)
            .or_else(|| item.get("channel_url").filter(|_| channel_mode))
            .or_else(|| item.get("webpage_url"))
            .or_else(|| item.get("url"))
            .and_then(|value| value.as_str())
            .filter(|value| value.starts_with("http"))
            .map(str::to_string)
        else {
            continue;
        };
        let url = if channel_mode {
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
            result_type: if channel_mode {
                "youtube_channel".to_string()
            } else {
                format!("youtube_{source_type}")
            },
            title,
            author,
            youtube_url: url,
            channel_id: item
                .get("channel_id")
                .or_else(|| item.get("id").filter(|_| channel_mode))
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

/// 列出指定 YouTube 频道的全部播放列表。添加“播放列表/收藏”来源时，
/// 先按 UP 主搜索频道，选择频道后调用本接口展示该 UP 的所有播放列表。
pub async fn get_youtube_channel_playlists(
    Query(request): Query<YouTubeChannelPlaylistsRequest>,
) -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    ensure_ytdlp_available().await?;
    let channel_url = request.url.trim();
    if channel_url.is_empty() || !is_youtube_url(channel_url) {
        return Err(ApiError::from(anyhow!("请输入有效的 YouTube 频道链接")));
    }
    let channel_url = canonical_channel_url(channel_url);
    let playlists_url = format!("{channel_url}/playlists");
    let mut command = ytdlp_command();
    command.args([
        "--flat-playlist",
        "--dump-json",
        "--playlist-end",
        "100",
        "--ignore-errors",
        "--no-warnings",
    ]);
    append_ytdlp_runtime(&mut command);
    append_cookies(&mut command);
    append_youtube_proxy(&mut command);
    append_ytdlp_tab_args(&mut command);
    command.arg(playlists_url.as_str());
    let output = tokio::time::timeout(Duration::from_secs(5 * 60), command.output())
        .await
        .map_err(|_| anyhow!("加载 YouTube 频道播放列表超时"))??;
    if !output.status.success() {
        return Err(ApiError::from(anyhow!(
            "加载 YouTube 频道播放列表失败：{}",
            command_error(&output)
        )));
    }

    let mut seen_urls = HashSet::new();
    let mut results = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(item) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(playlist_url) = item
            .get("webpage_url")
            .or_else(|| item.get("url"))
            .and_then(|value| value.as_str())
            .filter(|value| {
                value.contains("playlist?list=") || value.contains("/playlist/")
            })
            .map(str::to_string)
        else {
            continue;
        };
        if !seen_urls.insert(playlist_url.clone()) {
            continue;
        }
        let title = item
            .get("title")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("未命名播放列表")
            .to_string();
        let author = item
            .get("channel")
            .or_else(|| item.get("uploader"))
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
            result_type: "youtube_playlist".to_string(),
            title,
            author,
            youtube_url: playlist_url,
            channel_id: item
                .get("channel_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            cover,
            description: item
                .get("description")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            follower: item.get("playlist_count").and_then(|value| value.as_i64()),
        });
    }
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

/// 枚举频道、播放列表、喜欢视频或已订阅频道。响应直接复用 B 站投稿
/// 选择面板的数据结构，让添加 YouTube 来源继续共用项目原有选择 UI。
pub async fn get_youtube_source_videos(
    Query(request): Query<YouTubeSourceVideosRequest>,
) -> Result<ApiResponse<SubmissionVideosResponse>, ApiError> {
    let source_type = request.source_type.trim().to_ascii_lowercase();
    if !matches!(source_type.as_str(), "subscriptions" | "channel" | "playlist" | "liked") {
        return Err(ApiError::from(anyhow!(
            "仅订阅动态、频道、播放列表和喜欢的视频支持内容选择"
        )));
    }
    let raw_url = request.url.as_deref().unwrap_or("").trim();
    let page = request.page.unwrap_or(1).max(1);
    let page_size = request.page_size.unwrap_or(100).clamp(1, 200);
    let keyword = request
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    if source_type == "subscriptions" {
        return get_youtube_subscription_channels(page, page_size, keyword.as_deref()).await;
    }

    ensure_ytdlp_available().await?;
    let source_url = match source_type.as_str() {
        "liked" => LIKED_URL.to_string(),
        "channel" | "playlist" => {
            if raw_url.is_empty() || !is_youtube_url(raw_url) {
                return Err(ApiError::from(anyhow!("请输入有效的 YouTube 频道或播放列表链接")));
            }
            if source_type == "channel" {
                canonical_channel_url(raw_url)
            } else {
                raw_url.to_string()
            }
        }
        "subscriptions" => unreachable!(),
        _ => unreachable!(),
    };

    // 多取一条用于判断是否还有下一页。普通频道通常一次即可返回完整列表；
    // 大频道继续沿用投稿面板的“加载更多”交互。
    let start = (page - 1).saturating_mul(page_size).saturating_add(1);
    let end_with_probe = start.saturating_add(page_size);
    let mut command = ytdlp_command();
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
    append_youtube_proxy(&mut command);
    append_ytdlp_tab_args(&mut command);
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
            .unwrap_or("");
        let cover = if cover.starts_with("//") {
            format!("https:{cover}")
        } else {
            cover.to_string()
        };
        let duration = item
            .get("duration")
            .and_then(|value| value.as_f64())
            .and_then(|value| i32::try_from(value.round() as i64).ok())
            .unwrap_or_default();
        let view = item
            .get("view_count")
            .or_else(|| item.get("channel_follower_count"))
            .or_else(|| item.get("playlist_count"))
            .and_then(|value| value.as_i64())
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or_default();
        videos.push(SubmissionVideoInfo {
            bvid: id.to_string(),
            title,
            author: item
                .get("uploader")
                .or_else(|| item.get("channel"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string),
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

async fn get_youtube_subscription_channels(
    page: i32,
    page_size: i32,
    keyword: Option<&str>,
) -> Result<ApiResponse<SubmissionVideosResponse>, ApiError> {
    let initial_data = load_youtube_subscription_channels().await?;
    let mut channels = Vec::new();
    let mut seen = HashSet::new();
    collect_youtube_channel_renderers(&initial_data, &mut seen, &mut channels);
    if let Some(keyword) = keyword {
        channels.retain(|channel| {
            channel.title.to_ascii_lowercase().contains(keyword)
                || channel
                    .author
                    .as_deref()
                    .is_some_and(|author| author.to_ascii_lowercase().contains(keyword))
        });
    }

    let total = channels.len() as i64;
    let start = usize::try_from((page - 1).saturating_mul(page_size)).unwrap_or_default();
    let videos = channels.into_iter().skip(start).take(page_size as usize).collect();
    Ok(ApiResponse::ok(SubmissionVideosResponse {
        videos,
        total,
        page,
        page_size,
    }))
}

async fn load_youtube_subscription_channels() -> Result<serde_json::Value> {
    let contents = youtube_cookie_text()
        .context("读取 YouTube Cookie 失败：请先在设置页导入 cookies.txt 或传输登录状态")?;
    let cookie_jar = youtube_cookie_jar_from_text(&contents)?;
    let mut client_builder = reqwest::Client::builder()
        // Netscape 文件同时包含 YouTube 和 Google 账号域 Cookie。必须让
        // Cookie Jar 按域名分别发送：直接拼成一个 Cookie 请求头会把
        // google.com 的同名 SID/PSID 错发给 youtube.com，导致有效会话也被判为退出。
        .cookie_provider(cookie_jar)
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(Duration::from_secs(30));
    let proxy = configured_external_proxy();
    if !proxy.is_empty() {
        client_builder = client_builder.no_proxy().proxy(reqwest::Proxy::all(&proxy)?);
    }
    let client = client_builder.build()?;
    let response = client
        .get(SUBSCRIPTION_CHANNELS_URL)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36",
        )
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
        .context("访问 YouTube 已订阅频道页面失败")?;
    // 浏览器式被动续约：把页面响应里的 Set-Cookie 合并写回 cookies.txt。
    let fallback_domain = response.url().host_str().unwrap_or("www.youtube.com").to_string();
    if let Some(merged) = crate::utils::netscape_cookies::renew_cookie_text(
        &contents,
        response.headers(),
        &fallback_domain,
        is_youtube_auth_cookie_domain,
    ) {
        if let Err(error) = set_youtube_cookies(&merged).await {
            warn!(error = %error, "写回 YouTube 续约后的 Cookie 失败");
        }
    }

    let final_url = response.url().to_string();
    if final_url.contains("accounts.google.com") || final_url.contains("ServiceLogin") {
        bail!("YouTube Cookie 已失效或不完整，请在登录 YouTube 的同一浏览器配置中重新传输登录状态");
    }
    if !response.status().is_success() {
        bail!("YouTube 已订阅频道页面返回 HTTP {}", response.status());
    }

    let html = response.text().await.context("读取 YouTube 已订阅频道页面失败")?;
    if youtube_page_is_logged_out(&html) {
        bail!("YouTube Cookie 已失效或不完整，请在登录 YouTube 的同一浏览器配置中重新传输登录状态");
    }
    extract_youtube_initial_data(&html)
        .ok_or_else(|| anyhow!("YouTube 已订阅频道页面缺少频道数据，请重新传输登录状态后重试"))
}

fn youtube_cookie_jar(path: &Path) -> Result<Arc<reqwest::cookie::Jar>> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("读取 YouTube Cookie 失败：{}", path.display()))?;
    youtube_cookie_jar_from_text(&contents)
}

fn youtube_cookie_jar_from_text(contents: &str) -> Result<Arc<reqwest::cookie::Jar>> {
    let now = chrono::Utc::now().timestamp();
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let mut cookie_count = 0usize;
    for raw_line in contents.lines() {
        let line = raw_line.strip_prefix("#HttpOnly_").unwrap_or(raw_line);
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() < 7 || !is_youtube_auth_cookie_domain(columns[0]) {
            continue;
        }
        let expires = columns[4].parse::<i64>().unwrap_or_default();
        if expires != 0 && expires <= now {
            continue;
        }
        let host = columns[0].trim().trim_start_matches('.');
        let url = reqwest::Url::parse(&format!("https://{host}/"))
            .with_context(|| format!("YouTube Cookie 域名无效：{}", columns[0]))?;
        let path = if columns[2].starts_with('/') { columns[2] } else { "/" };
        let mut cookie = format!("{}={}; Path={path}", columns[5], columns[6]);
        if columns[1].eq_ignore_ascii_case("TRUE") || columns[0].starts_with('.') {
            cookie.push_str(&format!("; Domain=.{}", host));
        }
        if columns[3].eq_ignore_ascii_case("TRUE") {
            cookie.push_str("; Secure");
        }
        jar.add_cookie_str(&cookie, &url);
        cookie_count += 1;
    }
    if cookie_count == 0 {
        bail!("尚未导入有效的 YouTube Cookie");
    }
    Ok(jar)
}

fn youtube_page_is_logged_out(html: &str) -> bool {
    html.contains("\"LOGGED_IN\":false")
        || html.contains("\"loggedIn\":false")
        || (html.contains("accounts.google.com/ServiceLogin") && !html.contains("\"LOGGED_IN\":true"))
}

fn is_youtube_auth_cookie_domain(domain: &str) -> bool {
    let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    domain == "youtube.com"
        || domain.ends_with(".youtube.com")
        || domain == "google.com"
        || domain.ends_with(".google.com")
}

fn youtube_cookie_diagnostic(contents: &str) -> String {
    let helper_version = contents
        .lines()
        .find_map(|line| line.strip_prefix("# Bili Sync Login Helper "))
        .unwrap_or("未知旧版");
    let mut youtube_count = 0usize;
    let mut google_count = 0usize;
    let mut google_session = false;
    for raw_line in contents.lines() {
        let line = raw_line.strip_prefix("#HttpOnly_").unwrap_or(raw_line);
        if line.starts_with('#') {
            continue;
        }
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() < 7 {
            continue;
        }
        let domain = columns[0].trim().trim_start_matches('.').to_ascii_lowercase();
        if domain == "youtube.com" || domain.ends_with(".youtube.com") {
            youtube_count += 1;
        } else if domain == "google.com" || domain.ends_with(".google.com") {
            google_count += 1;
            google_session |= matches!(
                columns[5],
                "SID" | "SAPISID" | "APISID" | "__Secure-1PSID" | "__Secure-3PSID"
            );
        }
    }
    format!(
        "助手 {helper_version}，YouTube Cookie {youtube_count} 个，Google Cookie {google_count} 个，Google 会话 {}",
        if google_session { "已捕获" } else { "未捕获" }
    )
}

fn extract_youtube_initial_data(html: &str) -> Option<serde_json::Value> {
    const MARKERS: [&str; 4] = [
        "var ytInitialData = ",
        "window[\"ytInitialData\"] = ",
        "ytInitialData = ",
        "\"ytInitialData\":",
    ];
    for marker in MARKERS {
        let Some(marker_start) = html.find(marker) else {
            continue;
        };
        let remainder = &html[marker_start + marker.len()..];
        let Some(json_start) = remainder.find('{') else {
            continue;
        };
        let Some(json) = first_json_object(&remainder[json_start..]) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str(json) {
            return Some(value);
        }
    }
    None
}

fn first_json_object(input: &str) -> Option<&str> {
    if !input.starts_with('{') {
        return None;
    }
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&input[..index + character.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

fn youtube_text(value: Option<&serde_json::Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    value
        .get("simpleText")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            value.get("runs").and_then(|value| value.as_array()).map(|runs| {
                runs.iter()
                    .filter_map(|run| run.get("text").and_then(|value| value.as_str()))
                    .collect::<String>()
            })
        })
        .unwrap_or_default()
}

fn parse_youtube_channel_renderer(renderer: &serde_json::Value) -> Option<SubmissionVideoInfo> {
    let channel_id = renderer
        .get("channelId")
        .or_else(|| renderer.pointer("/navigationEndpoint/browseEndpoint/browseId"))
        .and_then(|value| value.as_str())
        .filter(|value| value.starts_with("UC"))?;
    let title = youtube_text(renderer.get("title"));
    if title.trim().is_empty() {
        return None;
    }
    let cover = renderer
        .pointer("/thumbnail/thumbnails")
        .and_then(|value| value.as_array())
        .and_then(|items| items.last())
        .and_then(|thumbnail| thumbnail.get("url"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let cover = if cover.starts_with("//") {
        format!("https:{cover}")
    } else {
        cover.to_string()
    };
    let description = [
        youtube_text(renderer.get("subscriberCountText")),
        youtube_text(renderer.get("videoCountText")),
        youtube_text(renderer.get("descriptionSnippet")),
    ]
    .into_iter()
    .filter(|value| !value.trim().is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    Some(SubmissionVideoInfo {
        bvid: channel_id.to_string(),
        title: title.clone(),
        author: Some(title),
        cover,
        pubtime: String::new(),
        duration: 0,
        view: 0,
        danmaku: 0,
        description,
    })
}

fn collect_youtube_channel_renderers(
    value: &serde_json::Value,
    seen: &mut HashSet<String>,
    channels: &mut Vec<SubmissionVideoInfo>,
) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                if matches!(key.as_str(), "channelRenderer" | "gridChannelRenderer") {
                    if let Some(channel) = parse_youtube_channel_renderer(child) {
                        if seen.insert(channel.bvid.clone()) {
                            channels.push(channel);
                        }
                    }
                }
                collect_youtube_channel_renderers(child, seen, channels);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_youtube_channel_renderers(item, seen, channels);
            }
        }
        _ => {}
    }
}

pub async fn import_youtube_cookie_file(
    Json(request): Json<YouTubeCookieImportRequest>,
) -> Result<ApiResponse<YouTubeLoginResponse>, ApiError> {
    ensure_ytdlp_available().await?;
    if !is_netscape_youtube_cookie_file(&request.cookies) {
        return Err(ApiError::bad_request(
            "文件不是包含 YouTube 会话的 Netscape cookies.txt；请在已登录 YouTube 的浏览器中导出 cookies.txt",
        ));
    }
    // 先清理旧会话（数据库 + 历史文件），再将新会话写入数据库。
    clear_youtube_login_state().await;
    set_youtube_cookies(&request.cookies).await?;
    if let Err(error) = validate_youtube_login_cookie().await {
        clear_youtube_login_state().await;
        return Err(ApiError::bad_request(format!(
            "YouTube cookies.txt 验证失败：{}（{}）",
            error,
            youtube_cookie_diagnostic(&request.cookies)
        )));
    }

    Ok(ApiResponse::ok(YouTubeLoginResponse {
        logged_in: true,
        message: "已导入 YouTube 登录凭证；订阅、喜欢和稍后再看将在下一次扫描时使用此登录状态".to_string(),
    }))
}



pub async fn get_youtube_sources(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<Vec<YouTubeSourceResponse>>, ApiError> {
    get_platform_sources(db.as_ref(), "youtube").await
}

pub async fn get_douyin_sources(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<Vec<YouTubeSourceResponse>>, ApiError> {
    get_platform_sources(db.as_ref(), "douyin").await
}

pub(crate) async fn get_platform_sources(
    db: &DatabaseConnection,
    platform: &str,
) -> Result<ApiResponse<Vec<YouTubeSourceResponse>>, ApiError> {
    let sources = youtube_source::Entity::find()
        .order_by_desc(youtube_source::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .filter(|source| source_platform(source) == platform)
        .collect::<Vec<_>>();
    let mut response = Vec::with_capacity(sources.len());
    for source in sources {
        response.push(source_response(db, source).await?);
    }
    Ok(ApiResponse::ok(response))
}

pub(crate) async fn require_source_platform(
    db: &DatabaseConnection,
    id: i32,
    platform: &str,
) -> Result<youtube_source::Model, ApiError> {
    let platform_label = if platform == "douyin" { "抖音" } else { "YouTube" };
    let Some(source) = youtube_source::Entity::find_by_id(id).one(db).await? else {
        return Err(ApiError::from(anyhow!("{platform_label}视频源不存在")));
    };
    if source_platform(&source) != platform {
        return Err(ApiError::bad_request(format!(
            "视频源 {} 不属于 {} 平台",
            id,
            if platform == "douyin" { "抖音" } else { "YouTube" }
        )));
    }
    Ok(source)
}

async fn require_video_platform(db: &DatabaseConnection, id: i32, platform: &str) -> Result<(), ApiError> {
    let platform_label = match platform {
        "douyin" => "抖音",
        "tiktok" => "TikTok",
        _ => "YouTube",
    };
    let Some(video) = youtube_video::Entity::find_by_id(id).one(db).await? else {
        return Err(ApiError::from(anyhow!("{platform_label}下载任务不存在")));
    };
    require_source_platform(db, video.source_id, platform).await?;
    Ok(())
}

/// 校验外源（YouTube/抖音/TikTok）下载任务可重试：外源统一存在 `youtube_video` 表，
/// 重试逻辑与来源平台无关。返回 400 时给出明确平台提示。
async fn require_external_video_platform(
    db: &DatabaseConnection,
    id: i32,
) -> Result<(), ApiError> {
    let Some(video) = youtube_video::Entity::find_by_id(id).one(db).await? else {
        return Err(ApiError::from(anyhow!("外源下载任务不存在")));
    };
    let Some(source) = youtube_source::Entity::find_by_id(video.source_id).one(db).await? else {
        return Err(ApiError::from(anyhow!("外源下载任务所属视频源不存在")));
    };
    let source_type = source.source_type.as_str();
    if source_type.is_empty() {
        return Err(ApiError::from(anyhow!("外源下载任务所属视频源类型无效")));
    }
    Ok(())
}

pub async fn create_youtube_source_checked(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<CreateYouTubeSourceRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    let normalized = normalize_source_type(&request.source_type)?;
    if normalized.starts_with("douyin") {
        return Err(ApiError::bad_request("请通过抖音视频源接口创建抖音源"));
    }
    if normalized == "tiktok" {
        return Err(ApiError::bad_request("请通过 TikTok 视频源接口创建 TikTok 源"));
    }
    create_youtube_source(Extension(db), Json(request)).await
}

pub async fn create_douyin_source(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<CreateYouTubeSourceRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    if !normalize_source_type(&request.source_type)?.starts_with("douyin") {
        return Err(ApiError::bad_request("请使用有效的抖音来源类型"));
    }
    create_youtube_source(Extension(db), Json(request)).await
}



pub async fn create_youtube_source(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<CreateYouTubeSourceRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    let source_type = normalize_source_type(&request.source_type)?;
    let url = resolve_source_url(source_type, request.url.as_deref())?;
    if source_type == "douyin" {
        crate::douyin::resolve_sec_user_id(&url).await?;
    } else if source_type == "tiktok" {
        if !crate::tiktok::is_tiktok_url(&url) {
            return Err(ApiError::from(anyhow!("TikTok 来源必须是有效的 tiktok.com 链接")));
        }
    } else if source_type == "tiktok_favorite" {
        // “我的喜欢”由当前登录账号在扫描时通过官方接口拉取，无需填写链接。
    } else if source_type == "tiktok_collection" {
        if !crate::tiktok::is_tiktok_url(&url) {
            return Err(ApiError::from(anyhow!("TikTok 播放列表必须是有效的 tiktok.com 链接")));
        }
    } else if !source_type.starts_with("douyin") && !is_youtube_url(&url) {
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
        ai_rename: Set(request.ai_rename),
        ai_rename_video_prompt: Set(request.ai_rename_video_prompt.unwrap_or_default()),
        ai_rename_audio_prompt: Set(request.ai_rename_audio_prompt.unwrap_or_default()),
        ai_rename_enable_multi_page: Set(request.ai_rename_enable_multi_page),
        ai_rename_enable_collection: Set(request.ai_rename_enable_collection),
        ai_rename_enable_bangumi: Set(request.ai_rename_enable_bangumi),
        ai_rename_rename_parent_dir: Set(request.ai_rename_rename_parent_dir),
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
        selected_channels: Set((!request.selected_channels.is_empty())
            .then(|| serde_json::to_string(&request.selected_channels))
            .transpose()?),
        known_video_ids: Set(None),
        scan_deleted_videos: Set(false),
        scan_deleted_videos_once: Set(false),
        deleted_video_ids: Set(None),
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

pub async fn update_youtube_source_enabled_checked(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<UpdateYouTubeSourceEnabledRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    require_source_platform(db.as_ref(), id, "youtube").await?;
    update_youtube_source_enabled(AxumPath(id), Extension(db), Json(request)).await
}

pub async fn update_douyin_source_enabled(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<UpdateYouTubeSourceEnabledRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    require_source_platform(db.as_ref(), id, "douyin").await?;
    update_youtube_source_enabled(AxumPath(id), Extension(db), Json(request)).await
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
    if let Some(value) = request.ai_rename {
        active.ai_rename = Set(value);
    }
    if let Some(value) = request.ai_rename_video_prompt {
        active.ai_rename_video_prompt = Set(value);
    }
    if let Some(value) = request.ai_rename_audio_prompt {
        active.ai_rename_audio_prompt = Set(value);
    }
    if let Some(value) = request.ai_rename_enable_multi_page {
        active.ai_rename_enable_multi_page = Set(value);
    }
    if let Some(value) = request.ai_rename_enable_collection {
        active.ai_rename_enable_collection = Set(value);
    }
    if let Some(value) = request.ai_rename_enable_bangumi {
        active.ai_rename_enable_bangumi = Set(value);
    }
    if let Some(value) = request.ai_rename_rename_parent_dir {
        active.ai_rename_rename_parent_dir = Set(value);
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

pub async fn update_youtube_source_checked(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<UpdateYouTubeSourceRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    require_source_platform(db.as_ref(), id, "youtube").await?;
    update_youtube_source(AxumPath(id), Extension(db), Json(request)).await
}

pub async fn update_douyin_source(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<UpdateYouTubeSourceRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    require_source_platform(db.as_ref(), id, "douyin").await?;
    update_youtube_source(AxumPath(id), Extension(db), Json(request)).await
}

pub async fn delete_youtube_source(
    AxumPath(id): AxumPath<i32>,
    Query(request): Query<DeleteYouTubeSourceRequest>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<bool>, ApiError> {
    require_source_platform(db.as_ref(), id, "youtube").await?;
    crate::api::handler::delete_video_source(
        Extension(db),
        AxumPath(("youtube".to_string(), id)),
        Query(crate::api::request::DeleteVideoSourceRequest {
            delete_local_files: request.delete_local_files,
        }),
    )
    .await?;
    Ok(ApiResponse::ok(true))
}

pub async fn delete_youtube_source_checked(
    AxumPath(id): AxumPath<i32>,
    Query(request): Query<DeleteYouTubeSourceRequest>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<bool>, ApiError> {
    delete_youtube_source(AxumPath(id), Query(request), Extension(db)).await
}

pub async fn delete_douyin_source(
    AxumPath(id): AxumPath<i32>,
    Query(request): Query<DeleteYouTubeSourceRequest>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<bool>, ApiError> {
    require_source_platform(db.as_ref(), id, "douyin").await?;
    crate::api::handler::delete_video_source(
        Extension(db),
        AxumPath(("douyin".to_string(), id)),
        Query(crate::api::request::DeleteVideoSourceRequest {
            delete_local_files: request.delete_local_files,
        }),
    )
    .await?;
    Ok(ApiResponse::ok(true))
}

pub(crate) async fn reset_external_source_path_shared(
    txn: &sea_orm::DatabaseTransaction,
    source_type: &str,
    id: i32,
    request: &ResetVideoSourcePathRequest,
) -> Result<ResetVideoSourcePathResponse, anyhow::Error> {
    let new_path = request.new_path.trim();
    if new_path.is_empty() {
        return Err(anyhow!("下载目录不能为空"));
    }
    let Some(source) = youtube_source::Entity::find_by_id(id).one(txn).await? else {
        return Err(anyhow!("外部平台视频源不存在"));
    };
    // 只搬迁已记录的媒体文件；未记录的用户文件不会被碰触。
    let old_base = PathBuf::from(&source.path);
    let new_base = PathBuf::from(new_path);
    let videos = youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.eq(id))
        .all(txn)
        .await?;
    let mut moved_files_count = 0usize;
    let mut cleaned_folders_count = 0usize;
    if request.apply_rename_rules {
        for video in &videos {
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
                moved_files_count += 1;
            }
            if request.clean_empty_folders {
                cleaned_folders_count += remove_empty_parent_directories(old_file.parent(), &old_base).await;
            }
            // 仅在实际移动文件时同步更新各视频 output_path（保持相对目录结构）
            let mut active: youtube_video::ActiveModel = video.clone().into();
            active.output_path = Set(Some(new_base.join(relative).display().to_string()));
            active.updated_at = Set(now_standard_string());
            active.update(txn).await?;
        }
    }
    let mut active: youtube_source::ActiveModel = source.into();
    active.path = Set(new_path.to_string());
    active.update(txn).await?;
    Ok(ResetVideoSourcePathResponse {
        success: true,
        source_id: id,
        source_type: source_type.to_string(),
        old_path: old_base.display().to_string(),
        new_path: new_path.to_string(),
        moved_files_count,
        updated_videos_count: videos.len(),
        cleaned_folders_count,
        message: format!(
            "{}视频源路径重设完成，移动 {} 个文件，清理 {} 个空文件夹",
            external_platform_label(source_type),
            moved_files_count,
            cleaned_folders_count
        ),
    })
}

fn external_platform_label(source_type: &str) -> &str {
    match source_type {
        "youtube" => "YouTube",
        "douyin" => "抖音",
        "tiktok" => "TikTok",
        _ => source_type,
    }
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

pub async fn retry_youtube_video_checked(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<bool>, ApiError> {
    // YouTube/抖音/TikTok 外源共用 youtube_video 表与重试逻辑，全部放行；
    // 该接口被 YouTube/抖音/TikTok 视频管理页共用。
    require_external_video_platform(db.as_ref(), id).await?;
    retry_youtube_video(AxumPath(id), Extension(db)).await
}

pub async fn retry_douyin_video(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<bool>, ApiError> {
    require_video_platform(db.as_ref(), id, "douyin").await?;
    retry_youtube_video(AxumPath(id), Extension(db)).await
}

pub async fn get_youtube_queue_status(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<YouTubeQueueStatusResponse>, ApiError> {
    get_platform_queue_status(db.as_ref(), "youtube").await
}

pub async fn get_douyin_queue_status(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<YouTubeQueueStatusResponse>, ApiError> {
    get_platform_queue_status(db.as_ref(), "douyin").await
}

async fn get_platform_queue_status(
    db: &DatabaseConnection,
    platform: &str,
) -> Result<ApiResponse<YouTubeQueueStatusResponse>, ApiError> {
    let source_ids = youtube_source::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .filter(|source| source_platform(source) == platform)
        .map(|source| source.id)
        .collect::<Vec<_>>();
    if source_ids.is_empty() {
        return Ok(ApiResponse::ok(YouTubeQueueStatusResponse {
            pending: 0,
            downloading: 0,
            completed: 0,
            failed: 0,
            skipped: 0,
            tasks: Vec::new(),
        }));
    }
    let count = |status: &str| {
        youtube_video::Entity::find()
            .filter(youtube_video::Column::SourceId.is_in(source_ids.clone()))
            .filter(youtube_video::Column::DownloadStatus.eq(status))
            .count(db)
    };
    let tasks = youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.is_in(source_ids.clone()))
        .filter(youtube_video::Column::DownloadStatus.is_in(["pending", "downloading", "failed", "skipped"]))
        .order_by_asc(youtube_video::Column::Id)
        .limit(100)
        .all(db)
        .await?
        .into_iter()
        .map(video_response)
        .collect();
    Ok(ApiResponse::ok(YouTubeQueueStatusResponse {
        pending: count("pending").await?,
        downloading: count("downloading").await?,
        completed: count("completed").await?,
        failed: count("failed").await?,
        skipped: count("skipped").await?,
        tasks,
    }))
}

pub async fn get_youtube_videos(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<Vec<YouTubeVideoResponse>>, ApiError> {
    Ok(ApiResponse::ok(
        get_platform_video_models(db.as_ref(), "youtube")
            .await?
            .into_iter()
            .map(video_response)
            .collect(),
    ))
}

pub async fn get_douyin_videos(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<Vec<YouTubeVideoResponse>>, ApiError> {
    Ok(ApiResponse::ok(
        get_platform_video_models(db.as_ref(), "douyin")
            .await?
            .into_iter()
            .map(video_response)
            .collect(),
    ))
}

async fn get_platform_video_models(db: &DatabaseConnection, platform: &str) -> Result<Vec<youtube_video::Model>> {
    let source_ids = youtube_source::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .filter(|source| source_platform(source) == platform)
        .map(|source| source.id)
        .collect::<Vec<_>>();
    if source_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.is_in(source_ids))
        .order_by_desc(youtube_video::Column::Id)
        .limit(200)
        .all(db)
        .await?)
}

pub fn unified_youtube_id(value: &str) -> Option<i32> {
    value
        .strip_prefix("youtube-")
        .or_else(|| value.strip_prefix("douyin-"))
        .or_else(|| value.strip_prefix("tiktok-"))
        .and_then(|id| id.parse::<i32>().ok())
}

fn is_douyin_source(source: &youtube_source::Model) -> bool {
    source.source_type.starts_with("douyin")
}


fn source_platform(source: &youtube_source::Model) -> &'static str {
    if is_douyin_source(source) {
        "douyin"
    } else if crate::tiktok::is_tiktok_source(source) {
        "tiktok"
    } else {
        "youtube"
    }
}

fn source_platform_label(source: &youtube_source::Model) -> &'static str {
    if is_douyin_source(source) {
        "抖音"
    } else if crate::tiktok::is_tiktok_source(source) {
        "TikTok"
    } else {
        "YouTube"
    }
}

async fn fetch_platform_asset(
    downloader: &UnifiedDownloader,
    source: &youtube_source::Model,
    urls: &[&str],
    path: &Path,
) -> Result<()> {
    if is_douyin_source(source) {
        let cookie = crate::douyin::cookie_header()?;
        downloader
            .fetch_with_fallback_with_referer_and_cookie(urls, path, "https://www.douyin.com/", &cookie)
            .await
    } else if crate::tiktok::is_tiktok_source(source) {
        // TikTok 播放直链要求 Chrome TLS 指纹 + 网页会话 Cookie，否则 Akamai CDN
        // 返回 HTTP 403 Access Denied；必须走 curl-impersonate 下载（跟随外源代理）。
        crate::tiktok::fetch_tiktok_media_with_impersonation(urls, path).await
    } else {
        let proxy = configured_external_proxy();
        downloader.fetch_with_fallback_with_proxy(urls, path, &proxy).await
    }
}

fn youtube_source_type_label(source_type: &str) -> &'static str {
    match source_type {
        "subscriptions" => "YouTube 订阅",
        "channel" => "YouTube 频道",
        "playlist" => "YouTube 播放列表",
        "liked" => "YouTube 喜欢",
        "watch_later" => "YouTube 稍后观看",
        "douyin" => "抖音作者",
        "douyin_liked" => "抖音我的喜欢",
        "douyin_collection" => "抖音收藏夹",
        "douyin_watch_later" => "抖音稍后再看",
        "douyin_theater" => "抖音放映厅",
        "douyin_series" => "抖音短剧",
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
    let completed = video.download_status == "completed" || video.download_status == "skipped";
    let output = video.output_path.as_deref().map(PathBuf::from);
    let charge_locked = video.is_charge_video && !video.charge_can_play;
    let media_ok = charge_locked
        || output
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
    let danmaku_ok = output.as_ref().is_some_and(|path| {
        if is_douyin_source(source) {
            path.with_extension("ass").is_file() || path.with_extension("danmaku.checked").is_file()
        } else {
            (path.with_extension("live_chat.json").is_file() && path.with_extension("ass").is_file())
                || path.with_extension("live_chat.checked").is_file()
        }
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

    // 占位（付费/加密/明确不可下载）视频：与 B 站充电视频占位一致，任务整体视为已完成，
    // 不再对封面/NFO/头像等子任务显示“未开始”。
    if charge_locked {
        return ([7, 7, 7, 7, 7], [7, 7, 7, 7, 7]);
    }
    // 主动跳过（未达最低分辨率等）：在视频卡片中视为已完成，警告信息由队列页展示。
    if video.download_status == "skipped" {
        return ([7, 7, 7, 7, 7], [7, 7, 7, 7, 7]);
    }
    // 已完成：与 B 站一致，任务状态跟随数据库而不是磁盘文件系统。已下载的外源视频
    // 即使媒体文件被移走/清理，卡片仍保持“全部完成”，不会重新变回“进行中/未开始”。
    // 弹幕/字幕等可选子任务仍按是否存在对应告警决定是否降级展示。
    if video.download_status == "completed" {
        let warning = video.error_message.as_deref().unwrap_or("");
        let page_status = [
            7,
            7,
            7,
            optional_status(
                danmaku_ok,
                warning.contains(if is_douyin_source(source) {
                    "弹幕"
                } else {
                    "直播聊天"
                }),
            ),
            if source.download_subtitle {
                optional_status(subtitle_ok, warning.contains("字幕"))
            } else {
                7
            },
        ];
        return ([7, 7, 7, 7, 7], page_status);
    }
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
        optional_status(
            danmaku_ok,
            warning.contains(if is_douyin_source(source) {
                "弹幕"
            } else {
                "直播聊天"
            }),
        ),
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
    let image_paths = youtube_image_post_paths(video);
    let is_image_post = is_douyin_source(source) && (video.is_image_post || !image_paths.is_empty());
    let image_urls = image_paths
        .iter()
        .enumerate()
        .map(|(index, _)| format!("/api/videos/douyin-{}/images/{}", video.id, index + 1))
        .collect();
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
            is_charge_video: video.is_charge_video,
            is_image_post,
            is_story: is_douyin_source(source) && video.is_story,
            image_urls,
            bangumi_title: None,
            url: Some(video.url.clone()),
            skip_reason: (video.download_status == "skipped").then(|| {
                video
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "未达到最低下载标准".to_string())
            }),
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
            source_type: source_platform(source).to_string(),
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
        if let Some(platform) = params.platform.as_deref() {
            if source_platform(&source) != platform {
                continue;
            }
        }
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
            "is_charge_video" => left.0.is_charge_video.cmp(&right.0.is_charge_video),
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
    let should_reset = force || video.download_status == "failed" || (video.is_charge_video && !video.charge_can_play);
    if should_reset {
        let mut active: youtube_video::ActiveModel = video.into();
        active.download_status = Set("pending".to_string());
        active.retry_count = Set(0);
        active.is_charge_video = Set(false);
        active.charge_can_play = Set(false);
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
        if force || video.download_status == "failed" || (video.is_charge_video && !video.charge_can_play) {
            let mut active: youtube_video::ActiveModel = video.into();
            active.download_status = Set("pending".to_string());
            active.retry_count = Set(0);
            active.is_charge_video = Set(false);
            active.charge_can_play = Set(false);
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

/// 外源“编辑状态 / 按任务批量重置”中会当场重建的附属文件种类。
///
/// 外源视频数据库只有整条视频一个状态（不像 B 站有独立的子任务状态位），
/// “视频封面/视频信息(NFO)/UP头像/UP主信息”只是按本地文件实时推导出来的展示项。
/// 因此把这类子任务重置为“未开始”时，直接删掉对应文件并用最新元数据当场重建，
/// 不再把整条视频打成 pending —— 否则媒体文件被移走后会被重新下载，
/// 卡片也会重新显示“未下载”。
#[derive(Debug, Clone, Copy, Default)]
struct YoutubeArtifactRegen {
    cover: bool,
    nfo: bool,
    upper_face: bool,
    upper_info: bool,
    /// 请求同时要求重跑媒体本体（分P下载/视频内容）→ 走原整体重置流程。
    media: bool,
    /// 请求里出现了本实现尚不能当场重建的任务（弹幕/字幕等）→ 回退原流程。
    unsupported: bool,
}

impl YoutubeArtifactRegen {
    fn any(&self) -> bool {
        self.cover || self.nfo || self.upper_face || self.upper_info
    }
}

/// 把视频级/分页级任务索引折算成外源附属文件重跑项。
///
/// 视频级索引：封面0 / 视频信息1 / UP头像2 / UP主信息3 / 分P下载4。
/// 分页级索引：封面0 / 视频内容1 / 单集NFO2 / 弹幕(直播)3 / 字幕4。
fn youtube_regen_from_indexes(video_indexes: &[usize], page_indexes: &[usize]) -> YoutubeArtifactRegen {
    let mut regen = YoutubeArtifactRegen::default();
    for &index in video_indexes {
        match index {
            0 => regen.cover = true,
            1 => regen.nfo = true,
            2 => regen.upper_face = true,
            3 => regen.upper_info = true,
            4 => regen.media = true,
            _ => regen.unsupported = true,
        }
    }
    for &index in page_indexes {
        match index {
            0 => regen.cover = true,
            1 => regen.media = true,
            2 => regen.nfo = true,
            _ => regen.unsupported = true,
        }
    }
    regen
}

/// 把“置为未开始”的状态更新折算成要当场重建的附属文件。
fn youtube_regen_from_status_updates(request: &UpdateVideoStatusRequest) -> YoutubeArtifactRegen {
    let mut video_indexes = Vec::new();
    let mut page_indexes = Vec::new();
    for update in &request.video_updates {
        if update.status_value == 0 {
            video_indexes.push(update.status_index);
        }
    }
    for page in &request.page_updates {
        for update in &page.updates {
            if update.status_value == 0 {
                page_indexes.push(update.status_index);
            }
        }
    }
    youtube_regen_from_indexes(&video_indexes, &page_indexes)
}

/// 当场重建外源视频的附属文件（封面/NFO/UP头像/UP主信息）。
///
/// - 不动整条视频的下载状态（已完成仍保持已完成）；
/// - 重建前先重新解析平台元数据，解析失败时不动现有文件；
/// - 每个下载函数只补生成缺失的文件，因此只会重建被“重置”的那部分。
async fn regenerate_youtube_artifacts(
    db: &DatabaseConnection,
    video: youtube_video::Model,
    regen: YoutubeArtifactRegen,
) -> Result<()> {
    let Some(source) = youtube_source::Entity::find_by_id(video.source_id).one(db).await? else {
        bail!("视频源不存在（source_id={}）", video.source_id);
    };
    let platform = source_platform_label(&source);
    let Some(output_path) = video
        .output_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
    else {
        bail!("该视频还没有已下载的媒体文件，无法重建{platform}附属文件（请先完成下载）");
    };
    let metadata = if regen.cover || regen.nfo {
        let metadata = extract_youtube_metadata(&video.url, Some(&source))
            .await
            .with_context(|| format!("重新解析{platform}元数据失败"))?;
        Some(metadata)
    } else {
        None
    };
    if regen.cover || regen.nfo {
        let parent = output_path
            .parent()
            .ok_or_else(|| anyhow!("{platform}媒体路径无效：{}", output_path.display()))?;
        if !parent.is_dir() {
            bail!(
                "{platform}媒体目录不存在：{}（文件可能已被整体移走，请先恢复文件或重新下载）",
                parent.display()
            );
        }
    }
    let downloader = TASK_CONTROLLER.get_downloader().await;
    if regen.cover {
        let downloader = downloader
            .as_deref()
            .ok_or_else(|| anyhow!("下载器尚未就绪，请稍后重试"))?;
        let thumb = youtube_sidecar_path(&output_path, "-thumb.jpg")?;
        let fanart = youtube_sidecar_path(&output_path, "-fanart.jpg")?;
        remove_file_if_exists(&thumb).await?;
        remove_file_if_exists(&fanart).await?;
        download_youtube_cover(
            downloader,
            metadata.as_ref().expect("封面重建需要元数据"),
            &output_path,
            &source,
        )
        .await
        .with_context(|| format!("重新生成{platform}封面失败"))?;
    }
    if regen.nfo {
        if !crate::config::reload_config().nfo_config.enabled {
            bail!("{platform} NFO 生成已在设置中关闭，无法重新生成（原文件未删除）");
        }
        let nfo_path = output_path.with_extension("nfo");
        remove_file_if_exists(&nfo_path).await?;
        let metadata = metadata.as_ref().expect("NFO 重建需要元数据");
        generate_youtube_nfo(metadata, &output_path, &video.url, &video.title, &video.uploader, &source)
            .await
            .with_context(|| format!("重新生成{platform}视频信息/NFO失败"))?;
        // 抖音短剧/放映厅/合集是番剧结构：单集 NFO 之外还要刷新剧集 tvshow.nfo。
        if is_episodic_douyin_source(&source) {
            if let (Some(season_dir), Some(downloader)) = (output_path.parent(), downloader.as_deref()) {
                if let Some(series_dir) = season_dir.parent() {
                    let tvshow = series_dir.join("tvshow.nfo");
                    remove_file_if_exists(&tvshow).await?;
                    if let Err(error) =
                        ensure_youtube_series_sidecars(downloader, metadata, &output_path, &source).await
                    {
                        warn!(platform, %error, "{platform}剧集 tvshow.nfo 重建失败（单集 NFO 已更新）");
                    }
                }
            }
        }
    }
    if regen.upper_face || regen.upper_info {
        let downloader = downloader
            .as_deref()
            .ok_or_else(|| anyhow!("下载器尚未就绪，请稍后重试"))?;
        let (face_path, person_nfo_path) = youtube_upper_paths(&video.uploader);
        if regen.upper_face {
            remove_file_if_exists(&face_path).await?;
        }
        if regen.upper_info {
            remove_file_if_exists(&person_nfo_path).await?;
        }
        let profile_url = metadata
            .as_ref()
            .and_then(|metadata| metadata.channel_url.as_deref().or(metadata.uploader_url.as_deref()));
        download_youtube_upper_face(downloader, &video.uploader, profile_url, &source)
            .await
            .with_context(|| format!("重新生成{platform}UP头像/UP主信息失败"))?;
    }
    Ok(())
}

pub async fn reset_specific_unified_youtube_tasks(
    db: &DatabaseConnection,
    request: &ResetSpecificTasksRequest,
) -> Result<ResetAllVideosResponse, ApiError> {
    // 外源视频只有整条视频一个整体状态（不像 B 站有每个子任务的独立状态位）。
    // 当“强制重置 + 只选了封面/视频信息/UP头像/UP主信息”这类附属文件任务时，
    // 直接重建对应文件即可，不再把整条视频打成 pending —— 否则媒体被移走的
    // 已完成视频会被重新加入下载队列，卡片也会重新显示“未下载”。
    let mut video_indexes = if request.video_task_indexes.is_empty() {
        request.task_indexes.clone()
    } else {
        request.video_task_indexes.clone()
    };
    video_indexes.sort_unstable();
    video_indexes.dedup();
    let mut page_indexes = request.page_task_indexes.clone();
    page_indexes.sort_unstable();
    page_indexes.dedup();

    let regen = youtube_regen_from_indexes(&video_indexes, &page_indexes);
    let has_specific_indexes = !video_indexes.is_empty() || !page_indexes.is_empty();
    if request.force.unwrap_or(false)
        && has_specific_indexes
        && regen.any()
        && !regen.media
        && !regen.unsupported
    {
        let params = VideosRequest {
            platform: request.platform.clone().or_else(|| Some("youtube".to_string())),
            youtube: request.youtube,
            query: request.query.clone(),
            show_failed_only: request.show_failed_only,
            force: Some(true),
            ..Default::default()
        };
        let rows = filtered_youtube_models(db, &params).await?;
        let targets = rows
            .into_iter()
            .map(|(video, _, _)| video)
            .filter(|video| {
                video.download_status == "completed"
                    && video
                        .output_path
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            // 没有“已完成”的目标（例如全部是失败任务）：退回原整体重置流程。
            return reset_all_unified_youtube_videos(db, &params).await;
        }
        let db = db.clone();
        let results = stream::iter(targets)
            .map(|video| {
                let db = db.clone();
                async move {
                    match regenerate_youtube_artifacts(&db, video.clone(), regen).await {
                        Ok(()) => Ok(video.id),
                        Err(error) => Err((video.id, error)),
                    }
                }
            })
            .buffered(2)
            .collect::<Vec<_>>()
            .await;
        let mut resetted = 0usize;
        let mut first_error: Option<anyhow::Error> = None;
        for result in results {
            match result {
                Ok(_) => resetted += 1,
                Err((video_id, error)) => {
                    warn!(video_id, error = %error, "外源视频附属文件重建失败");
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if resetted == 0 {
            if let Some(error) = first_error {
                return Err(anyhow!("外源附属文件重置失败：{error:#}").into());
            }
            return Ok(ResetAllVideosResponse {
                resetted: false,
                resetted_videos_count: 0,
                resetted_pages_count: 0,
            });
        }
        if let Some(error) = first_error {
            warn!(resetted, error = %error, "部分外源视频附属文件重建失败，其余已成功");
        }
        notify_videos_changed();
        return Ok(ResetAllVideosResponse {
            resetted: true,
            resetted_videos_count: resetted,
            resetted_pages_count: 0,
        });
    }

    // 其余情况（整源重置、只重置失败任务、需要重下媒体等）沿用整体重置流程。
    let params = VideosRequest {
        platform: request.platform.clone().or_else(|| Some("youtube".to_string())),
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

    // 外源附属文件任务：把“封面/视频信息/UP头像/UP主信息”置为未开始时当场重建，
    // 不把整条视频打成 pending（避免媒体被移走的已完成视频重新下载/显示未下载）。
    let has_updates = !request.video_updates.is_empty() || !request.page_updates.is_empty();
    let all_to_zero = has_updates
        && request
            .video_updates
            .iter()
            .chain(request.page_updates.iter().flat_map(|page| page.updates.iter()))
            .all(|update| update.status_value == 0);
    let regen = youtube_regen_from_status_updates(request);
    if video.download_status == "completed"
        && all_to_zero
        && regen.any()
        && !regen.media
        && !regen.unsupported
    {
        regenerate_youtube_artifacts(db, video, regen).await?;
        notify_videos_changed();
        let response = get_unified_youtube_video(db, id).await?;
        return Ok(UpdateVideoStatusResponse {
            success: true,
            video: response.video,
            pages: response.pages,
        });
    }

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
        remove_recorded_output(&source, output_path).await?;
    }
    let txn = db.begin().await?;
    let mut deleted_video_ids = parse_video_id_set(source.deleted_video_ids.as_deref());
    deleted_video_ids.insert(video.youtube_id.clone());
    let mut active_source: youtube_source::ActiveModel = source.clone().into();
    active_source.deleted_video_ids = Set(serialize_video_id_set(&deleted_video_ids)?);
    active_source.update(&txn).await?;
    youtube_video::Entity::delete_by_id(id).exec(&txn).await?;
    txn.commit().await?;
    notify_videos_changed();
    notify_video_sources_changed();
    notify_queue_status_changed();
    Ok(DeleteVideoResponse {
        success: true,
        video_id: id,
        message: format!("{} 视频已成功删除", source_platform_label(&source)),
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

fn youtube_image_post_paths(video: &youtube_video::Model) -> Vec<PathBuf> {
    let Some(output_path) = video.output_path.as_deref().map(Path::new) else {
        return Vec::new();
    };
    let Ok(image_dir) = youtube_sidecar_path(output_path, "-images") else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(image_dir) else {
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|extension| {
                        matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp")
                    })
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

pub async fn unified_youtube_image_path(
    db: &DatabaseConnection,
    id: i32,
    image_index: usize,
) -> Result<Option<PathBuf>, ApiError> {
    if image_index == 0 {
        return Ok(None);
    }
    let Some(video) = youtube_video::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    let Some(source) = youtube_source::Entity::find_by_id(video.source_id).one(db).await? else {
        return Ok(None);
    };
    if !is_douyin_source(&source) {
        return Ok(None);
    }
    Ok(youtube_image_post_paths(&video).into_iter().nth(image_index - 1))
}

/// 由现有 `video_downloader` 的同一周期调用。来源扫描和待下载任务都受全局暂停、
/// 下载并发配置及相同的日志/通知周期控制。
/// 进程启动后的首次外源扫描宽限（秒）：避免与其它启动任务叠加触发抖音风控。
/// 抖音源启动轮另有“最近扫过就跳过”保护，因此这里只保留避开启动风暴的短宽限，
/// 不再让 YouTube 源无谓等待 60 秒。
const STARTUP_EXTERNAL_SCAN_GRACE_SECONDS: u64 = 15;
/// TikTok 对连续请求敏感（403/验证页风控），相邻视频下载间的固定间隔（秒）。
const TIKTOK_DOWNLOAD_INTERVAL_SECONDS: u64 = 6;
/// 进程启动后的首次外源扫描是否仍未执行（仅对启动轮生效）。
static EXTERNAL_STARTUP_SCAN_PENDING: AtomicBool = AtomicBool::new(true);

/// 距上次成功扫描是否仍在最近一个扫描间隔内（用于启动轮跳过，避免重启后立刻全量重扫）。
fn source_scanned_recently(last_scan_at: Option<&str>, interval_seconds: u64) -> bool {
    let Some(last_scan_at) = last_scan_at else {
        return false;
    };
    let Ok(last_scan) = chrono::NaiveDateTime::parse_from_str(last_scan_at, "%Y-%m-%d %H:%M:%S") else {
        return false;
    };
    let interval = interval_seconds.max(60) as i64;
    (chrono::Local::now().naive_local() - last_scan).num_seconds() < interval
}

/// 启动轮是否跳过该抖音源的扫描：距上次成功扫描不超过一个扫描间隔时跳过，
/// 避免进程重启后把所有源立刻重扫一遍、在启动阶段叠加触发风控。
fn skip_douyin_source_scan_at_startup(source: &youtube_source::Model) -> bool {
    is_douyin_source(source)
        && source_scanned_recently(source.last_scan_at.as_deref(), crate::config::reload_config().interval)
}

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
        warn!(error = %error, "已配置 YouTube/抖音视频源，但 yt-dlp 自动安装失败；跳过本轮扫描");
        return Ok(());
    }
    recover_interrupted_downloads(db).await?;
    let startup_round = EXTERNAL_STARTUP_SCAN_PENDING.swap(false, Ordering::SeqCst);
    if startup_round && STARTUP_EXTERNAL_SCAN_GRACE_SECONDS > 0 {
        info!(
            grace_seconds = STARTUP_EXTERNAL_SCAN_GRACE_SECONDS,
            "进程启动后延迟 {} 秒开始首次外源扫描，避免启动阶段叠加触发抖音风控",
            STARTUP_EXTERNAL_SCAN_GRACE_SECONDS
        );
        tokio::time::sleep(Duration::from_secs(STARTUP_EXTERNAL_SCAN_GRACE_SECONDS)).await;
        if TASK_CONTROLLER.is_paused() {
            return Ok(());
        }
    }
    for source in &sources {
        if TASK_CONTROLLER.is_paused() {
            return Ok(());
        }
        if startup_round && skip_douyin_source_scan_at_startup(source) {
            debug!(
                source_id = source.id,
                "{}视频源「{}」最近已扫描过，启动轮跳过以避免风控请求叠加",
                source_platform_label(source),
                source.name
            );
            continue;
        }
        let mut scan_result = scan_source(db, source).await;
        if let Err(error) = scan_result.as_ref() {
            if is_douyin_source(source) && crate::douyin::is_douyin_risk_error(error) {
                warn!(
                    source_id = source.id,
                    error = %error,
                    "扫描{}视频源「{}」失败（风控/限流），等待退避后重试一次",
                    source_platform_label(source),
                    source.name
                );
                tokio::time::sleep(crate::douyin::douyin_risk_retry_delay().await).await;
                if !TASK_CONTROLLER.is_paused() {
                    scan_result = scan_source(db, source).await;
                }
            } else if !is_douyin_source(source) && is_youtube_transient_error(error) {
                warn!(
                    source_id = source.id,
                    error = %error,
                    "扫描{}视频源「{}」失败（瞬时网络错误），等待退避后重试一次",
                    source_platform_label(source),
                    source.name
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
                if !TASK_CONTROLLER.is_paused() {
                    scan_result = scan_source(db, source).await;
                }
            }
        }
        if let Err(error) = scan_result {
            let now = Instant::now();
            let mut last_warns = SCAN_FAILURE_WARN_LAST
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if last_warns
                .get(&source.id)
                .is_some_and(|marked_at| now.duration_since(*marked_at) < SCAN_FAILURE_WARN_COOLDOWN)
            {
                drop(last_warns);
                debug!(
                    source_id = source.id,
                    error = %error,
                    "扫描{}视频源「{}」失败（处于静默期，不重复告警）",
                    source_platform_label(source),
                    source.name
                );
            } else {
                last_warns.insert(source.id, now);
                drop(last_warns);
                if is_youtube_private_source(source) && is_youtube_auth_required_error(&error) {
                    warn!(
                        source_id = source.id,
                        %error,
                        "扫描{}视频源「{}」失败：需要有效的 YouTube 登录状态（稍后再看/喜欢/订阅为私有内容）。请重新导入最新的 YouTube cookies 后重试",
                        source_platform_label(source),
                        source.name
                    );
                } else {
                    warn!(
                        source_id = source.id,
                        error = %error,
                        "扫描{}视频源「{}」失败",
                        source_platform_label(source),
                        source.name
                    );
                }
            }
        }
        let delay = crate::config::reload_config()
            .submission_risk_control
            .source_delay_seconds;
        if delay > 0 && source.id != sources.last().map(|item| item.id).unwrap_or(source.id) {
            debug!(
                source_id = source.id,
                delay_seconds = delay,
                "{}视频源「{}」扫描完成，按全局源间风控延迟等待",
                source_platform_label(source),
                source.name
            );
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
    }
    if TASK_CONTROLLER.is_paused() {
        return Ok(());
    }
    download_pending(db, downloader.clone(), concurrent_limit.max(1)).await?;
    Ok(())
}

/// 从 yt-dlp 平铺列表项提取封面地址：优先顶层 `thumbnail`，否则回退到
/// `thumbnails[]` 数组（TikTok 平铺项没有顶层 thumbnail，只提供 thumbnails）。
fn ytdlp_flat_item_cover_url(item: &serde_json::Value) -> Option<String> {
    if let Some(url) = item
        .get("thumbnail")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
    {
        return Some(url.to_string());
    }
    item.get("thumbnails")
        .and_then(|value| value.as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|thumb| thumb.get("id").and_then(|v| v.as_str()) == Some("cover"))
                .or_else(|| items.first())
        })
        .and_then(|thumb| thumb.get("url"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

async fn scan_source(db: &DatabaseConnection, source: &youtube_source::Model) -> Result<u64> {
    if is_douyin_source(source) {
        return scan_douyin_source(db, source).await;
    }
    if source.source_type == "tiktok_favorite" {
        crate::tiktok::ensure_tiktok_session()?;
        return crate::tiktok::scan_tiktok_favorite_source(db, source).await;
    }
    if source.source_type == "tiktok_collection" {
        crate::tiktok::ensure_tiktok_session()?;
        return crate::tiktok::scan_tiktok_collection_source(db, source).await;
    }
    if source.source_type == "tiktok" {
        // TikTok 作者源走官方 API 直连扫描（api/creator/item_list/），不再依赖
        // yt-dlp tiktok:user 平铺扫描——后者对部分作者会因无法解析 secondary
        // user ID 而整轮失败。公开作者无需登录。
        return crate::tiktok::scan_tiktok_user_source(db, source).await;
    }
    // 稍后再看/我的喜欢/订阅 属于登录私有内容；未导入有效 YouTube 会话时直接
    // 给出明确提示并跳过扫描，避免每轮 yt-dlp 白跑并报出误导性的“播放列表不存在”。
    if is_youtube_private_source(source) && !youtube_has_session() {
        bail!(
            "「{}」需要有效的 YouTube 登录状态（稍后再看/喜欢/订阅为私有内容）：请在设置页用电脑端登录助手重新导入 YouTube cookies",
            source.name
        );
    }
    let tiktok = crate::tiktok::is_tiktok_source(source);
    let mut command = ytdlp_command();
    command.args([
        "--flat-playlist",
        "--dump-json",
        "--ignore-errors",
        "--no-warnings",
        // 出口网络挂起/被风控时快速失败：socket 20 秒无数据即超时，重试限制为 3 次，
        // 避免 yt-dlp 默认 10 次重试把单源扫描拖到数分钟（硬超时见下方）。
        "--socket-timeout",
        "20",
        "--retries",
        "3",
    ]);
    append_ytdlp_runtime(&mut command);
    if tiktok {
        // 公开内容无需登录；仅在导入过 TikTok cookies 时携带，不混用 YouTube/抖音 Cookie。
        crate::tiktok::append_tiktok_cookies(&mut command);
    } else {
        append_cookies(&mut command);
        append_ytdlp_tab_args(&mut command);
    }
    append_youtube_proxy(&mut command);
    // 频道的 `/videos`、`/shorts` 和 `/streams` 是三个彼此独立的标签页。
    // 扫描频道根地址时 yt-dlp 会按频道完整枚举三个标签页；若直接使用用户
    // 粘贴的 `/videos` 地址，则只会看到普通视频，漏掉 Shorts 和直播回放。
    // TikTok 作者主页直接扫描即可，无需改写 URL。
    let scan_url = if !tiktok && source.source_type == "channel" {
        canonical_channel_url(&source.url)
    } else {
        source.url.clone()
    };
    command.arg(&scan_url);
    let output = tokio::time::timeout(Duration::from_secs(5 * 60), command.output())
        .await
        .map_err(|_| anyhow!("扫描 {} 来源超时", source_platform_label(source)))??;
    if !output.status.success() {
        bail!("yt-dlp 扫描失败：{}", command_error(&output));
    }
    let mut added = 0;
    let selected_history = source
        .selected_videos
        .as_deref()
        .and_then(|value| serde_json::from_str::<HashSet<String>>(value).ok())
        .unwrap_or_default();
    let selected_channels = source
        .selected_channels
        .as_deref()
        .and_then(|value| serde_json::from_str::<HashSet<String>>(value).ok())
        .unwrap_or_default();
    let known_video_ids = parse_video_id_set(source.known_video_ids.as_deref());
    let mut deleted_video_ids = parse_video_id_set(source.deleted_video_ids.as_deref());
    let scan_deleted_videos = source.scan_deleted_videos || source.scan_deleted_videos_once;
    let mut scanned_video_ids = HashSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(item) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(youtube_id) = item.get("id").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) else {
            continue;
        };
        scanned_video_ids.insert(youtube_id.to_string());
        if source.source_type == "subscriptions"
            && !selected_channels.is_empty()
            && item
                .get("channel_id")
                .or_else(|| item.get("uploader_id"))
                .and_then(|value| value.as_str())
                .is_none_or(|channel_id| !selected_channels.contains(channel_id))
        {
            continue;
        }
        // 与 B 站投稿源一致：创建来源时只回补勾选的历史视频；以后仅自动加入
        // 来源创建后新发布或新加入列表的视频。known_video_ids 用来识别“刚刚
        // 点赞了一个旧视频”这类发布时间早、但列表成员关系刚发生变化的情况。
        let was_deleted = deleted_video_ids.contains(youtube_id);
        if !selected_history.is_empty()
            && !selected_history.contains(youtube_id)
            && !(scan_deleted_videos && was_deleted)
            && (known_video_ids.contains(youtube_id)
                || (known_video_ids.is_empty() && !youtube_item_is_newer_than_source(&item, &source.created_at)))
        {
            continue;
        }
        if let Some(existing) = youtube_video::Entity::find()
            .filter(youtube_video::Column::SourceId.eq(source.id))
            .filter(youtube_video::Column::YoutubeId.eq(youtube_id))
            .one(db)
            .await?
        {
            deleted_video_ids.remove(youtube_id);
            // 回填早期扫描缺失的封面（TikTok 平铺项没有顶层 thumbnail 字段，
            // 旧数据封面为空时在此补上，视频管理卡片即可显示平台封面）。
            if existing.thumbnail.as_deref().is_none_or(str::is_empty) {
                if let Some(cover) = ytdlp_flat_item_cover_url(&item) {
                    let mut active: youtube_video::ActiveModel = existing.into();
                    active.thumbnail = Set(Some(cover));
                    active.updated_at = Set(now_standard_string());
                    active.update(db).await?;
                    notify_videos_changed();
                }
            }
            continue;
        }
        if was_deleted && !scan_deleted_videos {
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
            thumbnail: Set(ytdlp_flat_item_cover_url(&item)),
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
        deleted_video_ids.remove(youtube_id);
        added += 1;
    }
    let mut active: youtube_source::ActiveModel = source.clone().into();
    known_video_ids.into_iter().for_each(|youtube_id| {
        scanned_video_ids.insert(youtube_id);
    });
    active.known_video_ids = Set((!scanned_video_ids.is_empty())
        .then(|| serde_json::to_string(&scanned_video_ids))
        .transpose()?);
    active.deleted_video_ids = Set(serialize_video_id_set(&deleted_video_ids)?);
    active.scan_deleted_videos_once = Set(false);
    active.last_scan_at = Set(Some(now_standard_string()));
    active.update(db).await?;
    notify_video_sources_changed();
    if added > 0 {
        info!(
            platform = source_platform_label(&source),
            source_id = source.id,
            added,
            "{}视频源「{}」发现 {} 个新视频",
            source_platform_label(&source),
            source.name,
            added
        );
        notify_videos_changed();
        notify_queue_status_changed();
    }
    Ok(added)
}

async fn scan_douyin_source(db: &DatabaseConnection, source: &youtube_source::Model) -> Result<u64> {
    let selected_history = source
        .selected_videos
        .as_deref()
        .and_then(|value| serde_json::from_str::<HashSet<String>>(value).ok())
        .unwrap_or_default();
    let known_video_ids = parse_video_id_set(source.known_video_ids.as_deref());
    let mut deleted_video_ids = parse_video_id_set(source.deleted_video_ids.as_deref());
    let scan_deleted_videos = source.scan_deleted_videos || source.scan_deleted_videos_once;
    // 增量优先：作品按发布时间倒序，翻到整页都是已知视频即提前停止，
    // 避免每轮全量枚举几十页触发 aweme/post 限流；只有开启扫描删除
    //（需要全量枚举判断作品是否已消失）时才全量翻页。
    let stop_when_known = (!scan_deleted_videos).then_some(&known_video_ids);
    let posts =
        crate::douyin::fetch_source_posts(&source.source_type, &source.url, usize::MAX, stop_when_known).await?;
    let created = chrono::NaiveDateTime::parse_from_str(&source.created_at, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|value| Local.from_local_datetime(&value).single())
        .map(|value| value.timestamp());
    let mut scanned_video_ids = HashSet::new();
    let mut added = 0u64;
    for post in posts {
        scanned_video_ids.insert(post.id.clone());
        let was_deleted = deleted_video_ids.contains(&post.id);
        if !selected_history.is_empty()
            && !selected_history.contains(&post.id)
            && !(scan_deleted_videos && was_deleted)
            && (known_video_ids.contains(&post.id)
                || (known_video_ids.is_empty()
                    && !created
                        .zip(post.timestamp)
                        .is_some_and(|(created, published)| published > created)))
        {
            continue;
        }
        if let Some(existing) = youtube_video::Entity::find()
            .filter(youtube_video::Column::SourceId.eq(source.id))
            .filter(youtube_video::Column::YoutubeId.eq(&post.id))
            .one(db)
            .await?
        {
            deleted_video_ids.remove(&post.id);
            if existing.is_image_post != post.is_image_post
                || existing.is_story != post.is_story
                || existing.episode_number != post.episode_number
            {
                let mut active: youtube_video::ActiveModel = existing.into();
                active.is_image_post = Set(post.is_image_post);
                active.is_story = Set(post.is_story);
                active.episode_number = Set(post.episode_number);
                active.updated_at = Set(now_standard_string());
                active.update(db).await?;
            }
            continue;
        }
        if was_deleted && !scan_deleted_videos {
            continue;
        }
        if crate::utils::keyword_filter::should_filter_video_dual_list(
            &post.title,
            &source.blacklist_keywords,
            &source.whitelist_keywords,
            source.keyword_case_sensitive,
        ) {
            continue;
        }
        if source
            .min_duration_seconds
            .zip(post.duration_seconds)
            .is_some_and(|(minimum, duration)| duration < minimum)
            || source
                .max_duration_seconds
                .zip(post.duration_seconds)
                .is_some_and(|(maximum, duration)| duration > maximum)
        {
            continue;
        }
        let compact_date = post.published_at.as_deref();
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
        deleted_video_ids.remove(&post.id);
        youtube_video::ActiveModel {
            source_id: Set(source.id),
            youtube_id: Set(post.id),
            url: Set(post.url),
            title: Set(post.title),
            uploader: Set(post.uploader),
            thumbnail: Set(post.thumbnail),
            published_at: Set(post.published_at),
            duration_seconds: Set(post.duration_seconds),
            episode_number: Set(post.episode_number),
            is_image_post: Set(post.is_image_post),
            is_story: Set(post.is_story),
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
    known_video_ids.into_iter().for_each(|id| {
        scanned_video_ids.insert(id);
    });
    let mut active: youtube_source::ActiveModel = source.clone().into();
    active.known_video_ids = Set((!scanned_video_ids.is_empty())
        .then(|| serde_json::to_string(&scanned_video_ids))
        .transpose()?);
    active.deleted_video_ids = Set(serialize_video_id_set(&deleted_video_ids)?);
    active.scan_deleted_videos_once = Set(false);
    active.last_scan_at = Set(Some(now_standard_string()));
    active.update(db).await?;
    notify_video_sources_changed();
    if added > 0 {
        info!(source_id = source.id, added, "抖音视频源发现新作品");
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
    warn!(count, "已恢复上次进程中断的 YouTube/抖音下载任务");
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
    let platforms = Arc::new(
        youtube_source::Entity::find()
            .all(db)
            .await?
            .into_iter()
            .map(|source| (source.id, source_platform_label(&source)))
            .collect::<HashMap<_, _>>(),
    );
    // TikTok 对连续 yt-dlp 解析请求非常敏感：标记出 TikTok 来源，视频间插入
    // 固定间隔，降低被风控（403/验证页）的概率。
    let tiktok_source_ids = Arc::new(
        youtube_source::Entity::find()
            .all(db)
            .await?
            .into_iter()
            .filter(|source| crate::tiktok::is_tiktok_source(source))
            .map(|source| source.id)
            .collect::<std::collections::HashSet<_>>(),
    );
    let db = db.clone();
    stream::iter(videos)
        .for_each_concurrent(concurrent_limit, move |video| {
            let db = db.clone();
            let downloader = downloader.clone();
            let platforms = platforms.clone();
            let tiktok_source_ids = tiktok_source_ids.clone();
            async move {
                let platform = platforms.get(&video.source_id).copied().unwrap_or("媒体");
                if tiktok_source_ids.contains(&video.source_id) {
                    tokio::time::sleep(Duration::from_secs(TIKTOK_DOWNLOAD_INTERVAL_SECONDS)).await;
                }
                if let Err(error) = download_video(&db, downloader.as_ref(), video).await {
                    warn!(error = %error, "下载{}视频失败", platform);
                }
            }
        })
        .await;
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
    let platform_label = source_platform_label(&source);
    // 首页「正在下载」进度：外源无分片字节进度，先注册阶段与状态（完成/失败时自动清理）
    let progress_key = format!("youtube:{}", video.id);
    let progress_platform = if is_douyin_source(&source) {
        "douyin"
    } else if crate::tiktok::is_tiktok_source(&source) {
        "tiktok"
    } else {
        "youtube"
    };
    let _progress_guard = crate::workflow::DownloadProgressGuard::new(progress_key.clone());
    crate::download_progress::DOWNLOAD_PROGRESS
        .begin_task(&progress_key, progress_platform, &video.title, "下载中", "")
        .await;
    loop {
        info!(
            platform = platform_label,
            source_id = source.id,
            youtube_id = %video.youtube_id,
            "{}视频源「{}」开始下载视频「{}」",
            platform_label,
            source.name,
            video.title
        );
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
            Ok(mut downloaded) => {
                if downloaded.paid_content {
                    // 与 B 站充电视频一致：标记为付费/加密且当前不可播放，生成 0 字节
                    // 占位文件并把下载状态记为已完成（UI 显示充电视频徽标而非失败）。
                    let mut paid: youtube_video::ActiveModel = video.clone().into();
                    paid.download_status = Set("completed".to_string());
                    paid.retry_count = Set(0);
                    paid.is_charge_video = Set(true);
                    paid.charge_can_play = Set(false);
                    paid.output_path = Set(Some(downloaded.output_path.display().to_string()));
                    paid.error_message = Set(None);
                    paid.updated_at = Set(now_standard_string());
                    paid.update(db).await?;
                    info!(
                        platform = platform_label,
                        source_id = source.id,
                        youtube_id = %video.youtube_id,
                        path = %downloaded.output_path.display(),
                        "{}视频源「{}」视频「{}」为付费/加密内容，已按充电视频处理：生成占位文件并标记为已完成",
                        platform_label,
                        source.name,
                        video.title
                    );
                    notify_videos_changed();
                    notify_queue_status_changed();
                    return Ok(());
                }
                if downloaded.skipped {
                    // 未达到最低分辨率等主动跳过场景：标记为 skipped 并写入警告，
                    // 不进入 failed，也不保留不存在的输出路径。
                    let mut skipped: youtube_video::ActiveModel = video.clone().into();
                    skipped.download_status = Set("skipped".to_string());
                    skipped.retry_count = Set(0);
                    skipped.output_path = Set(None);
                    skipped.error_message = Set(downloaded.warning_message);
                    skipped.title = Set(downloaded.title);
                    skipped.uploader = Set(downloaded.uploader);
                    skipped.thumbnail = Set(downloaded.thumbnail);
                    skipped.published_at = Set(downloaded.published_at);
                    skipped.duration_seconds = Set(downloaded.duration_seconds);
                    skipped.updated_at = Set(now_standard_string());
                    skipped.update(db).await?;
                    info!(
                        platform = platform_label,
                        source_id = source.id,
                        youtube_id = %video.youtube_id,
                        "{}视频源「{}」视频「{}」已跳过下载（未达到最低分辨率或主动跳过）",
                        platform_label,
                        source.name,
                        video.title
                    );
                    notify_videos_changed();
                    notify_queue_status_changed();
                    return Ok(());
                }
                let downloaded_title = downloaded.title.clone();
                if source.ai_rename && crate::config::reload_config().ai_rename.enabled {
                    match ai_rename_external_file(&source, &video, &downloaded, None, None).await {
                        Ok(path) => downloaded.output_path = path,
                        Err(error) => {
                            warn!(
                                platform = platform_label,
                                source_id = source.id,
                                youtube_id = %video.youtube_id,
                                %error,
                                "{} AI 重命名失败，保留原文件名",
                                platform_label
                            );
                            let message = format!("AI 重命名失败：{error:#}");
                            downloaded.warning_message = Some(match downloaded.warning_message.take() {
                                Some(existing) => format!("{existing}；{message}"),
                                None => message,
                            });
                        }
                    }
                }
                active.download_status = Set("completed".to_string());
                active.retry_count = Set(0);
                active.output_path = Set(Some(downloaded.output_path.display().to_string()));
                active.error_message = Set(downloaded.warning_message);
                active.title = Set(downloaded.title);
                active.uploader = Set(downloaded.uploader);
                active.thumbnail = Set(downloaded.thumbnail);
                active.published_at = Set(downloaded.published_at);
                active.duration_seconds = Set(downloaded.duration_seconds);
                active.is_image_post = Set(downloaded.is_image_post);
                info!(
                    platform = platform_label,
                    source_id = source.id,
                    youtube_id = %video.youtube_id,
                    path = %downloaded.output_path.display(),
                    "{}视频源「{}」视频「{}」下载完成",
                    platform_label,
                    source.name,
                    downloaded_title
                );
                active.update(db).await?;
                notify_videos_changed();
                notify_queue_status_changed();
                return Ok(());
            }
            Err(error) => {
                let retry_count = video.retry_count.saturating_add(1);
                let error_text = error.to_string();
                let permanent_failure =
                    error_text.contains("CENC 加密 DASH") || error_text.contains("付费/加密内容");
                let exhausted = permanent_failure || retry_count >= MAX_DOWNLOAD_RETRIES;
                active.retry_count = Set(retry_count);
                active.download_status = Set(if exhausted { "failed" } else { "pending" }.to_string());
                active.error_message = Set(Some(format!("{:#}", error)));
                video = active.update(db).await?;
                // 前两次重试失败只记 debug，第三次起才报 WARN，避免每轮重试都刷屏。
                if retry_count >= 3 {
                    warn!(
                        platform = platform_label,
                        source_id = source.id,
                        retry_count,
                        max_retries = MAX_DOWNLOAD_RETRIES,
                        error = %error,
                        "{}视频源「{}」视频「{}」下载失败，真实错误已持久化",
                        platform_label,
                        source.name,
                        video.title
                    );
                } else {
                    debug!(
                        platform = platform_label,
                        source_id = source.id,
                        retry_count,
                        max_retries = MAX_DOWNLOAD_RETRIES,
                        error = %error,
                        "{}视频源「{}」视频「{}」下载失败，真实错误已持久化",
                        platform_label,
                        source.name,
                        video.title
                    );
                }
                notify_videos_changed();
                notify_queue_status_changed();

                if exhausted || TASK_CONTROLLER.is_paused() {
                    return Ok(());
                }
                // TikTok 对短间隔重复请求会升级风控（403/验证页），重试退避显著拉长；
                // 其它平台维持原有 5 秒/次退避。YouTube 命中验证墙（Sign in to
                // confirm you're not a bot）时拉长到 5 分钟级，让出口 IP 冷却，
                // 避免每 5 秒空转把风控越打越重。
                let retry_delay = if crate::tiktok::is_tiktok_source(&source) {
                    Duration::from_secs((retry_count.max(1) as u64) * 30)
                } else if !crate::tiktok::is_tiktok_source(&source) && !is_douyin_source(&source) && is_youtube_bot_wall_error(&error) {
                    Duration::from_secs(300 * (retry_count.max(1) as u64))
                } else {
                    Duration::from_secs((retry_count.max(1) as u64) * 5)
                };
                info!(
                    platform = platform_label,
                    source_id = source.id,
                    youtube_id = %video.youtube_id,
                    retry_count,
                    delay_seconds = retry_delay.as_secs(),
                    "{}视频源「{}」视频「{}」等待后刷新直链并重试",
                    platform_label,
                    source.name,
                    video.title
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

struct DownloadedYouTubeMedia {
    output_path: PathBuf,
    title: String,
    uploader: String,
    thumbnail: Option<String>,
    published_at: Option<String>,
    duration_seconds: Option<i32>,
    is_image_post: bool,
    warning_message: Option<String>,
    /// 抖音付费/加密内容（CENC 加密且无解密密钥）已转占位文件。
    paid_content: bool,
    /// 因未达到设定的最低分辨率等原因主动跳过下载，不作为错误/失败处理。
    skipped: bool,
}

struct SelectedStreams {
    video: Option<ExternalMediaFormat>,
    audio: Option<ExternalMediaFormat>,
    mixed: Option<ExternalMediaFormat>,
}

fn external_ai_file(
    source: &youtube_source::Model,
    video: &youtube_video::Model,
    downloaded: &DownloadedYouTubeMedia,
    sort_index: i32,
) -> Result<crate::utils::ai_rename::FileToRename> {
    use crate::utils::ai_rename::{AiRenameContext, FileToRename};

    let path = downloaded.output_path.clone();
    let current_stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .with_context(|| format!("{}媒体文件名无效", source_platform_label(source)))?
        .to_string();
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or(if source.audio_only { "m4a" } else { "mp4" })
        .to_string();
    let is_audio = matches!(
        ext.to_ascii_lowercase().as_str(),
        "m4a" | "mp3" | "flac" | "aac" | "ogg"
    );
    Ok(FileToRename {
        path,
        current_stem,
        ext,
        ctx: AiRenameContext {
            title: downloaded.title.clone(),
            owner: downloaded.uploader.clone(),
            duration: downloaded.duration_seconds.unwrap_or_default().max(0) as u32,
            pubdate: downloaded.published_at.clone().unwrap_or_default(),
            part_name: downloaded.title.clone(),
            source_type: source_platform(source).to_string(),
            is_audio,
            sort_index: Some(sort_index),
            bvid: video.youtube_id.clone(),
            ..Default::default()
        },
        page_id: video.id,
        video_id: video.id,
        bvid: video.youtube_id.clone(),
        single_page: true,
        flat_folder: source.flat_folder,
    })
}

fn apply_external_ai_filename(
    file: &crate::utils::ai_rename::FileToRename,
    new_stem: &str,
    rename_parent_dir: bool,
    platform: &str,
) -> Result<Option<PathBuf>> {
    use crate::utils::ai_rename;

    let new_stem = new_stem.trim();
    if new_stem.is_empty() || new_stem == file.current_stem {
        return Ok(None);
    }
    let parent = file
        .path
        .parent()
        .with_context(|| format!("{platform}媒体文件没有父目录"))?;
    let mut final_stem = new_stem.to_string();
    let mut new_path = parent.join(format!("{}.{}", final_stem, file.ext));
    let mut suffix = 1;
    while new_path.exists() && new_path != file.path {
        final_stem = format!("{}-{}", new_stem, suffix);
        new_path = parent.join(format!("{}.{}", final_stem, file.ext));
        suffix += 1;
        if suffix > 99 {
            bail!("无法为{platform}媒体生成唯一文件名");
        }
    }

    std::fs::rename(&file.path, &new_path).with_context(|| {
        format!(
            "重命名{platform}媒体失败: {} -> {}",
            file.path.display(),
            new_path.display()
        )
    })?;
    if let Err(error) = ai_rename::rename_sidecars(&file.path, &final_stem, &file.ext) {
        warn!(%error, "{} AI 重命名侧车文件失败", platform);
    }
    let old_image_dir = parent.join(format!("{}-images", file.current_stem));
    let new_image_dir = parent.join(format!("{}-images", final_stem));
    if old_image_dir.is_dir() && old_image_dir != new_image_dir {
        if let Err(error) = std::fs::rename(&old_image_dir, &new_image_dir) {
            warn!(%error, "抖音图文原图目录 AI 重命名失败");
        }
    }
    if let Err(error) = ai_rename::update_nfo_content(&new_path.with_extension("nfo"), &final_stem) {
        warn!(%error, "{} AI 重命名更新 NFO 失败", platform);
    }

    if rename_parent_dir && !file.flat_folder {
        if let Some(parent_parent) = parent.parent() {
            let mut target_dir = parent_parent.join(&final_stem);
            if target_dir.exists() && target_dir != parent {
                target_dir = parent_parent.join(format!("{}-{}", final_stem, file.bvid));
            }
            if target_dir != parent {
                match std::fs::rename(parent, &target_dir) {
                    Ok(()) => {
                        new_path = target_dir.join(new_path.file_name().context("重命名后的文件名无效")?);
                    }
                    Err(error) => warn!(%error, "{} AI 重命名上级目录失败", platform),
                }
            }
        }
    }
    Ok(Some(new_path))
}

async fn ai_rename_external_file(
    source: &youtube_source::Model,
    video: &youtube_video::Model,
    downloaded: &DownloadedYouTubeMedia,
    video_prompt: Option<&str>,
    audio_prompt: Option<&str>,
) -> Result<PathBuf> {
    // 抖音短剧/放映厅/合集按番剧结构命名（S01E01 + Season 01），文件名与目录
    // 都是媒体库识别结构的一部分，不允许 AI 重命名，避免破坏剧集组织。
    if is_episodic_douyin_source(source) {
        debug!(
            "{}剧集源「{}」跳过 AI 重命名（保持 S01E01/Season 结构）",
            source_platform_label(source),
            source.name
        );
        return Ok(downloaded.output_path.clone());
    }
    let config = crate::config::reload_config().ai_rename;
    let file = external_ai_file(source, video, downloaded, 1)?;
    let prompt = if file.ctx.is_audio {
        audio_prompt
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                if source.ai_rename_audio_prompt.trim().is_empty() {
                    &config.audio_prompt_hint
                } else {
                    &source.ai_rename_audio_prompt
                }
            })
    } else {
        video_prompt
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                if source.ai_rename_video_prompt.trim().is_empty() {
                    &config.video_prompt_hint
                } else {
                    &source.ai_rename_video_prompt
                }
            })
    };
    let names = crate::utils::ai_rename::ai_generate_filenames_batch(
        &config,
        &format!("{}_{}", source_platform(source), source.id),
        std::slice::from_ref(&file),
        prompt,
    )
    .await?;
    let Some(new_stem) = names.first() else {
        return Ok(downloaded.output_path.clone());
    };
    Ok(apply_external_ai_filename(
        &file,
        new_stem,
        source.ai_rename_rename_parent_dir,
        source_platform_label(source),
    )?
    .unwrap_or_else(|| downloaded.output_path.clone()))
}

pub async fn ai_rename_external_history(
    db: &DatabaseConnection,
    source: &youtube_source::Model,
    video_prompt: &str,
    audio_prompt: &str,
    rename_parent_dir: bool,
) -> Result<crate::utils::ai_rename::BatchRenameResult> {
    use crate::utils::ai_rename::{ai_generate_filenames_batch, FileToRename};

    let mut result = crate::utils::ai_rename::BatchRenameResult::default();
    let config = crate::config::reload_config().ai_rename;
    let platform = source_platform_label(source);
    let source_key = format!("{}_{}", source_platform(source), source.id);
    // 抖音短剧/放映厅/合集按番剧结构命名（S01E01 + Season 01），不允许 AI
    // 重命名，避免破坏剧集组织与媒体库识别。
    if is_episodic_douyin_source(&source) {
        let skipped = youtube_video::Entity::find()
            .filter(youtube_video::Column::SourceId.eq(source.id))
            .filter(youtube_video::Column::DownloadStatus.eq("completed"))
            .count(db)
            .await
            .unwrap_or(0);
        info!(
            "{}剧集源「{}」跳过历史 AI 重命名（保持 S01E01/Season 结构），跳过 {} 个已完成文件",
            platform,
            source.name,
            skipped
        );
        result.skipped_count += skipped as usize;
        return Ok(result);
    }
    let mut video_files: Vec<FileToRename> = Vec::new();
    let mut audio_files: Vec<FileToRename> = Vec::new();
    let videos = youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.eq(source.id))
        .filter(youtube_video::Column::DownloadStatus.eq("completed"))
        .order_by_asc(youtube_video::Column::Id)
        .all(db)
        .await?;
    for (index, video) in videos.into_iter().enumerate() {
        let Some(output_path) = video.output_path.as_deref().filter(|value| !value.is_empty()) else {
            result.skipped_count += 1;
            continue;
        };
        if !Path::new(output_path).is_file() {
            result.skipped_count += 1;
            continue;
        }
        let downloaded = DownloadedYouTubeMedia {
            output_path: PathBuf::from(output_path),
            title: video.title.clone(),
            uploader: video.uploader.clone(),
            thumbnail: video.thumbnail.clone(),
            published_at: video.published_at.clone(),
            duration_seconds: video.duration_seconds,
            is_image_post: video.is_image_post,
            warning_message: None,
            paid_content: false,
            skipped: false,
        };
        match external_ai_file(
            source,
            &video,
            &downloaded,
            i32::try_from(index + 1).unwrap_or(i32::MAX),
        ) {
            Ok(file) if file.ctx.is_audio => audio_files.push(file),
            Ok(file) => video_files.push(file),
            Err(error) => {
                warn!(source_id = source.id, sort_index = index + 1, %error, "{}历史文件 AI 重命名信息收集失败", platform);
                result.failed_count += 1;
            }
        }
    }

    let video_prompt = if video_prompt.trim().is_empty() {
        if source.ai_rename_video_prompt.trim().is_empty() {
            config.video_prompt_hint.as_str()
        } else {
            source.ai_rename_video_prompt.as_str()
        }
    } else {
        video_prompt
    };
    let audio_prompt = if audio_prompt.trim().is_empty() {
        if source.ai_rename_audio_prompt.trim().is_empty() {
            config.audio_prompt_hint.as_str()
        } else {
            source.ai_rename_audio_prompt.as_str()
        }
    } else {
        audio_prompt
    };
    let batch_size = 10;
    for (kind, files, prompt) in [
        ("视频", video_files.as_slice(), video_prompt),
        ("音频", audio_files.as_slice(), audio_prompt),
    ] {
        for (batch_index, batch) in files.chunks(batch_size).enumerate() {
            info!(
                source_id = source.id,
                batch = batch_index + 1,
                total_batches = files.len().div_ceil(batch_size),
                count = batch.len(),
                "{}历史{}文件 AI 重命名批次开始",
                platform,
                kind
            );
            match ai_generate_filenames_batch(&config, &source_key, batch, prompt).await {
                Ok(names) => {
                    for (file, new_stem) in batch.iter().zip(names.iter()) {
                        match apply_external_ai_filename(file, new_stem, rename_parent_dir, platform) {
                            Ok(Some(new_path)) => {
                                let Some(video) = youtube_video::Entity::find_by_id(file.video_id).one(db).await?
                                else {
                                    result.failed_count += 1;
                                    continue;
                                };
                                let mut active: youtube_video::ActiveModel = video.into();
                                active.output_path = Set(Some(new_path.display().to_string()));
                                active.updated_at = Set(now_standard_string());
                                active.update(db).await?;
                                result.renamed_count += 1;
                            }
                            Ok(None) => result.skipped_count += 1,
                            Err(error) => {
                                warn!(source_id = source.id, file = %file.current_stem, %error, "{}历史文件 AI 重命名失败", platform);
                                result.failed_count += 1;
                            }
                        }
                    }
                }
                Err(error) => {
                    warn!(source_id = source.id, batch = batch_index + 1, %error, "{}历史{}文件 AI 重命名批次失败", platform, kind);
                    result.failed_count += batch.len();
                }
            }
        }
    }
    Ok(result)
}

async fn download_youtube_media(
    downloader: &UnifiedDownloader,
    source: &youtube_source::Model,
    video: &youtube_video::Model,
) -> Result<DownloadedYouTubeMedia> {
    let platform = source_platform_label(source);
    let metadata = match extract_youtube_metadata(&video.url, Some(source)).await {
        Ok(metadata) => metadata,
        Err(error) if crate::tiktok::is_tiktok_unavailable_error(&error) => {
            // TikTok 明确不可下载（地区/内容不可用，statusCode≠0）：与付费/加密占位
            // 方案一致，生成占位文件并标记为不可下载，避免反复重试。
            warn!(
                platform,
                source_id = source.id,
                youtube_id = %video.youtube_id,
                %error,
                "{}视频「{}」明确无法下载（地区/内容不可用），生成占位并停止重试",
                platform,
                video.title
            );
            let fallback_metadata = ExternalMediaMetadata {
                id: video.youtube_id.clone(),
                title: Some(video.title.clone()),
                uploader: Some(source.name.clone()),
                uploader_url: None,
                channel: None,
                channel_id: None,
                channel_url: None,
                thumbnail: video.thumbnail.clone(),
                description: None,
                language: None,
                upload_date: video.published_at.clone(),
                duration: video.duration_seconds.map(|value| value as f64),
                formats: Vec::new(),
                subtitles: HashMap::new(),
                automatic_captions: HashMap::new(),
                images: Vec::new(),
                music_urls: Vec::new(),
                creators: None,
            };
            let placeholder_title = video.title.clone();
            let placeholder_uploader = if video.uploader.trim().is_empty() {
                source.name.clone()
            } else {
                video.uploader.clone()
            };
            let output_path =
                youtube_output_path(source, video, &fallback_metadata, &placeholder_title, &placeholder_uploader)?;
            crate::douyin::create_paid_placeholder(&output_path).await?;
            // 尽力把封面下载到本地（TikTok 封面 CDN 同样要求 Chrome 指纹，走
            // curl-impersonate），这样卡片有真实本地封面而不是只能依赖远程代理。
            if let Err(error) = download_youtube_cover(downloader, &fallback_metadata, &output_path, source).await {
                warn!(
                    platform,
                    source_id = source.id,
                    youtube_id = %video.youtube_id,
                    %error,
                    "{}视频「{}」占位封面下载失败（不影响占位标记）",
                    platform,
                    video.title
                );
            }
            return Ok(DownloadedYouTubeMedia {
                output_path,
                title: placeholder_title,
                uploader: placeholder_uploader,
                thumbnail: video.thumbnail.clone(),
                published_at: video.published_at.clone(),
                duration_seconds: video.duration_seconds,
                is_image_post: false,
                warning_message: Some("无法下载视频：你所在国家或地区无法下载此视频".to_string()),
                paid_content: true,
                skipped: false,
            });
        }
        Err(error) => return Err(error),
    };
    let title = metadata.title.clone().unwrap_or_else(|| video.title.clone());
    let uploader = metadata
        .uploader
        .clone()
        .or_else(|| metadata.channel.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| source.name.clone());
    let output_path = youtube_output_path(source, video, &metadata, &title, &uploader)?;
    info!(
        platform,
        source_id = source.id,
        youtube_id = %video.youtube_id,
        "{}视频源「{}」正在处理视频「{}」",
        platform,
        source.name,
        title
    );
    let media_exists = is_reusable_media_file(&output_path).await;
    if media_exists {
        info!(path = %output_path.display(), "{}视频源「{}」视频「{}」目标文件已存在，复用现有文件", platform, source.name, title);
    } else {
        // 上次 ffmpeg 被中断时可能留下非零但不可播放的最终文件，不能仅凭文件
        // 大小就把任务标记为完成。
        remove_file_if_exists(&output_path).await?;
    }
    let filter_option = source
        .filter_option
        .as_ref()
        .map(|value| serde_json::from_value::<FilterOption>(value.clone()))
        .transpose()
        .with_context(|| format!("{platform}视频源级流过滤设置无效"))?
        .unwrap_or_else(|| crate::config::reload_config().filter_option);
    let selected = if metadata.images.is_empty() {
        let selected = select_youtube_streams(&metadata.formats, &filter_option, source.audio_only, platform)?;
        log_selected_youtube_streams(&selected, &filter_option, source, &title);
        Some(selected)
    } else {
        None
    };

    // 未达到设定的最低分辨率时主动跳过下载，不产生失败/错误任务。
    if !media_exists && !metadata.images.is_empty() && !source.audio_only {
        if let Some(selected) = selected.as_ref() {
            let min_height = youtube_quality_height(filter_option.video_min_quality);
            if let Some(actual_height) = selected_effective_video_height(selected) {
                if actual_height > 0 && actual_height < min_height {
                    warn!(
                        platform,
                        source_id = source.id,
                        youtube_id = %video.youtube_id,
                        min_height,
                        actual_height,
                        "{}视频源「{}」视频「{}」未达到设定的最低分辨率 {}p（实际 {}p），跳过下载",
                        platform,
                        source.name,
                        title,
                        min_height,
                        actual_height
                    );
                    return Ok(DownloadedYouTubeMedia {
                        output_path,
                        title,
                        uploader,
                        thumbnail: metadata.thumbnail,
                        published_at: metadata.upload_date,
                        duration_seconds: metadata
                            .duration
                            .and_then(|value| i32::try_from(value.round() as i64).ok()),
                        is_image_post: false,
                        warning_message: Some(format!(
                            "未达到设定的最低分辨率（最低 {}p，实际 {}p），已跳过下载",
                            min_height, actual_height
                        )),
                        paid_content: false,
                        skipped: true,
                    });
                }
            }
        }
    }

    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("无法创建{platform}下载目录: {}", parent.display()))?;
    }

    if media_exists {
        // 媒体已落盘时不重复下载，但仍继续执行字幕等独立子任务。
    } else if !metadata.images.is_empty() {
        crate::douyin::download_image_post(downloader, &metadata, &output_path, &filter_option).await?;
    } else if source.audio_only {
        let selected = selected.as_ref().context("图文作品不应进入音频流选择")?;
        if let Some(audio) = selected.audio.as_ref() {
            let urls = format_urls(&audio, platform)?;
            let temporary = output_path.with_extension("download.m4a");
            if let Err(error) = fetch_platform_asset(downloader, source, &urls, &temporary)
                .await
                .with_context(|| format!("使用项目统一下载器下载{platform}音频失败"))
            {
                let _ = remove_file_if_exists(&temporary).await;
                return Err(error);
            }
            if audio.decryption_key.is_none() && crate::douyin::is_cenc_encrypted_media(&temporary).await {
                let _ = remove_file_if_exists(&temporary).await;
                return Ok(paid_placeholder_result(
                    output_path, &metadata, title, uploader, platform,
                )
                .await?);
            }
            if let Some(key) = audio.decryption_key.as_deref() {
                let decrypted = output_path.with_extension("decrypted.m4a");
                if let Err(error) = crate::douyin::decrypt_dash_stream(&temporary, key, &decrypted).await {
                    let _ = remove_file_if_exists(&temporary).await;
                    let _ = remove_file_if_exists(&decrypted).await;
                    return Err(error);
                }
                replace_file(&decrypted, &output_path).await?;
                remove_file_if_exists(&temporary).await?;
            } else {
                replace_file(&temporary, &output_path).await?;
            }
        } else {
            // 抖音只提供带音轨的 MP4。先由统一下载器取得用户选择质量的
            // 混合流，再复用项目 ffmpeg 工具提取为真正的 M4A。
            let mixed = selected
                .mixed
                .as_ref()
                .ok_or_else(|| anyhow!("{platform}未返回可用音频流"))?;
            let urls = format_urls(&mixed, platform)?;
            let source_temporary = output_path.with_extension("audio-source.mp4");
            let audio_temporary = output_path.with_extension("download.m4a");
            if let Err(error) = fetch_platform_asset(downloader, source, &urls, &source_temporary)
                .await
                .with_context(|| format!("使用项目统一下载器下载{platform}音频源失败"))
            {
                let _ = remove_file_if_exists(&source_temporary).await;
                return Err(error);
            }
            if mixed.decryption_key.is_none() && crate::douyin::is_cenc_encrypted_media(&source_temporary).await {
                let _ = remove_file_if_exists(&source_temporary).await;
                return Ok(paid_placeholder_result(
                    output_path, &metadata, title, uploader, platform,
                )
                .await?);
            }
            let decrypted_source = output_path.with_extension("audio-source-decrypted.mp4");
            let extract_source = if let Some(key) = mixed.decryption_key.as_deref() {
                if let Err(error) = crate::douyin::decrypt_dash_stream(&source_temporary, key, &decrypted_source).await
                {
                    let _ = remove_file_if_exists(&source_temporary).await;
                    let _ = remove_file_if_exists(&decrypted_source).await;
                    return Err(error);
                }
                decrypted_source.as_path()
            } else {
                source_temporary.as_path()
            };
            if let Err(error) = extract_audio_track(extract_source, &audio_temporary, platform).await {
                let _ = remove_file_if_exists(&source_temporary).await;
                let _ = remove_file_if_exists(&decrypted_source).await;
                let _ = remove_file_if_exists(&audio_temporary).await;
                return Err(error);
            }
            replace_file(&audio_temporary, &output_path).await?;
            remove_file_if_exists(&source_temporary).await?;
            remove_file_if_exists(&decrypted_source).await?;
        }
    } else if let (Some(video_stream), Some(audio_stream)) = (
        selected.as_ref().and_then(|streams| streams.video.as_ref()),
        selected.as_ref().and_then(|streams| streams.audio.as_ref()),
    ) {
        let video_urls = format_urls(&video_stream, platform)?;
        let audio_urls = format_urls(&audio_stream, platform)?;
        let video_temporary =
            output_path.with_extension(format!("video.{}", video_stream.ext.as_deref().unwrap_or("mp4")));
        let audio_temporary =
            output_path.with_extension(format!("audio.{}", audio_stream.ext.as_deref().unwrap_or("m4a")));
        let merge_temporary = output_path.with_extension("merging.mp4");
        // YouTube 的高画质通常是独立 DASH 音视频流。两条直链都交给项目
        // 原生统一下载器并行传输；音频失败时保留已下载的视频临时文件，
        // 下次重试只重新下载音频，避免大视频（数百 MB）整片重下。
        let video_ready = is_reusable_media_file(&video_temporary).await;
        let mut audio_error: Option<anyhow::Error> = None;
        if video_ready {
            info!(
                platform,
                source_id = source.id,
                youtube_id = %video.youtube_id,
                path = %video_temporary.display(),
                "{}视频流临时文件已存在，复用后仅重试音频",
                platform
            );
            if let Err(error) =
                fetch_platform_asset(downloader, source, &audio_urls, &audio_temporary).await
            {
                audio_error = Some(error.context(format!("使用项目统一下载器下载{platform}音频流失败")));
            }
        } else {
            let video_fut = fetch_platform_asset(downloader, source, &video_urls, &video_temporary);
            let audio_fut = fetch_platform_asset(downloader, source, &audio_urls, &audio_temporary);
            let (video_result, audio_result) = tokio::join!(video_fut, audio_fut);
            if let Err(error) = video_result {
                let error = error.context(format!("使用项目统一下载器下载{platform}视频流失败"));
                let _ = remove_file_if_exists(&video_temporary).await;
                let _ = remove_file_if_exists(&audio_temporary).await;
                return Err(error);
            }
            audio_error = audio_result
                .err()
                .map(|error| error.context(format!("使用项目统一下载器下载{platform}音频流失败")));
        }
        if let Some(error) = audio_error {
            // 保留视频临时文件：下次重试时仅重新下载音频后合并。
            let _ = remove_file_if_exists(&audio_temporary).await;
            return Err(error);
        }
        let merge_result = match (
            video_stream.decryption_key.as_deref(),
            audio_stream.decryption_key.as_deref(),
        ) {
            (Some(video_key), Some(audio_key)) => {
                crate::douyin::merge_encrypted_dash(
                    &video_temporary,
                    video_key,
                    &audio_temporary,
                    audio_key,
                    &merge_temporary,
                )
                .await
            }
            (None, None) => {
                downloader
                    .merge(&video_temporary, &audio_temporary, &merge_temporary)
                    .await
            }
            _ => Err(anyhow!("{platform}独立音视频流的 CENC 密钥状态不一致")),
        }
        .with_context(|| format!("使用项目现有 ffmpeg 链路合并{platform}音视频失败"));
        if let Err(error) = merge_result {
            let _ = remove_file_if_exists(&video_temporary).await;
            let _ = remove_file_if_exists(&audio_temporary).await;
            let _ = remove_file_if_exists(&merge_temporary).await;
            return Err(error);
        }
        replace_file(&merge_temporary, &output_path).await?;
        remove_file_if_exists(&video_temporary).await?;
        remove_file_if_exists(&audio_temporary).await?;
    } else {
        let mixed = selected
            .as_ref()
            .context("图文作品不应进入混合流选择")?
            .mixed
            .as_ref()
            .ok_or_else(|| anyhow!("{platform}未返回可用的音视频流"))?;
        let urls = format_urls(&mixed, platform)?;
        let temporary = output_path.with_extension(format!("download.{}", mixed.ext.as_deref().unwrap_or("mp4")));
        if let Err(error) = fetch_platform_asset(downloader, source, &urls, &temporary)
            .await
            .with_context(|| format!("使用项目统一下载器下载{platform}混合流失败"))
        {
            let _ = remove_file_if_exists(&temporary).await;
            return Err(error);
        }
        if mixed.decryption_key.is_none() && crate::douyin::is_cenc_encrypted_media(&temporary).await {
            let _ = remove_file_if_exists(&temporary).await;
            return Ok(paid_placeholder_result(
                output_path, &metadata, title, uploader, platform,
            )
            .await?);
        }
        if let Some(key) = mixed.decryption_key.as_deref() {
            let decrypted = output_path.with_extension("decrypted.mp4");
            if let Err(error) = crate::douyin::decrypt_dash_stream(&temporary, key, &decrypted).await {
                let _ = remove_file_if_exists(&temporary).await;
                let _ = remove_file_if_exists(&decrypted).await;
                return Err(error);
            }
            replace_file(&decrypted, &output_path).await?;
            remove_file_if_exists(&temporary).await?;
        } else {
            replace_file(&temporary, &output_path).await?;
        }
    }

    let warning_message = if source.audio_only && source.audio_only_m4a_only && metadata.images.is_empty() {
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
        is_image_post: !metadata.images.is_empty(),
        warning_message,
        paid_content: false,
        skipped: false,
    })
}

/// 抖音付费/加密内容统一处理：生成 0 字节占位文件并返回付费结果，
/// 由 `download_video` 持久化为失败状态并停止重试（与 B 站充电视频占位一致）。
async fn paid_placeholder_result(
    output_path: PathBuf,
    metadata: &ExternalMediaMetadata,
    title: String,
    uploader: String,
    platform: &str,
) -> Result<DownloadedYouTubeMedia> {
    crate::douyin::create_paid_placeholder(&output_path).await?;
    info!(
        path = %output_path.display(),
        "检测到{}付费/加密内容，已清理密文并生成占位文件",
        platform
    );
    Ok(DownloadedYouTubeMedia {
        output_path,
        title,
        uploader,
        thumbnail: metadata.thumbnail.clone(),
        published_at: metadata.upload_date.clone(),
        duration_seconds: metadata
            .duration
            .and_then(|value| i32::try_from(value.round() as i64).ok()),
        is_image_post: false,
        warning_message: Some(format!(
            "{platform}付费/加密内容（CENC 加密且未提供解密密钥），需购买后才能下载；已清理无效媒体并生成占位文件"
        )),
        paid_content: true,
        skipped: false,
    })
}

fn format_urls<'a>(format: &'a ExternalMediaFormat, platform: &str) -> Result<Vec<&'a str>> {
    let mut urls = Vec::with_capacity(1 + format.fallback_urls.len());
    if let Some(url) = format.url.as_deref().filter(|value| !value.is_empty()) {
        urls.push(url);
    }
    urls.extend(
        format
            .fallback_urls
            .iter()
            .map(String::as_str)
            .filter(|value| !value.is_empty()),
    );
    if urls.is_empty() {
        bail!("{platform}媒体流缺少下载地址");
    }
    Ok(urls)
}

async fn extract_audio_track(input: &Path, output: &Path, platform: &str) -> Result<()> {
    remove_file_if_exists(output).await?;
    let result = tokio::process::Command::new(crate::downloader::resolve_media_tool_path("ffmpeg"))
        .args(["-y", "-i"])
        .arg(input)
        .args(["-map", "0:a:0", "-vn", "-c:a", "copy"])
        .arg(output)
        .output()
        .await
        .with_context(|| format!("启动 ffmpeg 提取{platform}音轨失败"))?;
    if !result.status.success() {
        bail!("ffmpeg 提取{platform}音轨失败：{}", command_error(&result));
    }
    Ok(())
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

async fn extract_youtube_metadata(url: &str, source: Option<&youtube_source::Model>) -> Result<ExternalMediaMetadata> {
    if source.is_some_and(is_douyin_source) || url.contains("douyin.com") {
        let aweme_id = crate::douyin::aweme_id(url).context("抖音作品链接缺少有效作品 ID")?;
        return crate::douyin::extract_metadata(aweme_id, source).await;
    }
    if url.contains("tiktok.com") {
        match extract_ytdlp_metadata(url, "TikTok").await {
            Ok(metadata) => return Ok(metadata),
            Err(ytdlp_error) => {
                debug!(error = %ytdlp_error, url = %url, "yt-dlp 解析 TikTok 媒体直链失败，尝试 API 兜底（item/detail）");
                return match crate::tiktok::extract_tiktok_media_detail(url).await {
                    Ok(metadata) => Ok(metadata),
                    Err(api_error) if crate::tiktok::is_tiktok_unavailable_error(&api_error) => {
                        // 明确不可下载（地区/内容不可用）：直接透传，不再追加风控提示。
                        Err(api_error)
                    }
                    Err(api_error) => bail!(
                        "yt-dlp 解析 TikTok 媒体直链失败：{ytdlp_error}；API 兜底也失败：{api_error}（通常是当前出口 IP 被 TikTok 风控，请更换外源代理节点后重试）"
                    ),
                };
            }
        }
    }
    extract_ytdlp_metadata(url, "YouTube").await
}

pub(crate) async fn extract_ytdlp_metadata(url: &str, platform: &str) -> Result<ExternalMediaMetadata> {
    let is_youtube = is_youtube_url(url);
    // YouTube 自 2025 年底起，默认 web 客户端要么报 “The page needs to be reloaded”，
    // 要么只返回最高 1080p 的格式（高分辨率走 SABR 协议、响应里没有签名直链）。
    // web_embedded/web_safari 对多数视频可返回完整格式；visionos 客户端（yt-dlp
    // ≥2026.08.19 内置）对走 SABR 协议的高码率源返回最高 4K 的签名直链，是目前
    // 不依赖 PO Token/JS 运行时的 4K 兜底。三个客户端格式会合并，全局视频质量
    // 设置可据此选到 1440p/4K。
    let client_args = if is_youtube {
        Some("youtube:player_client=web_embedded,web_safari,visionos")
    } else {
        None
    };
    let metadata = run_ytdlp_metadata(url, platform, client_args).await?;
    // web_embedded 对部分视频（尤其需要 PO Token/登录会话的高码率源）只返回
    // ≤1080p 格式。此时用备用客户端再解析一次（tv_embedded/tv 为传统兜底，
    // visionos 提供 4K 直链），取分辨率上限更高的一份，避免“设置 4K 却只能
    // 下到 1080p”。仅对最高不足 4K 的情况触发。
    if is_youtube {
        let mut best = metadata;
        let best_max = youtube_max_format_height(&best.formats);
        // web_embedded 对部分视频（尤其需要 PO Token/登录会话的高码率源）只返回
        // ≤1080p 格式。用 tv_embedded/tv/visionos 再解析一次取分辨率更高的一份。
        // 为避免高频多客户端请求把出口 IP 打标（YouTube 会直接进入 “Sign in to
        // confirm you're not a bot” 风控墙），每个视频仅在进程内尝试一次。
        if best_max < 2160 {
            // 锁只用于判断是否首次尝试，随后立即释放，避免跨 await 持有非 Send 的
            // MutexGuard。
            let first_attempt = {
                let mut tried = YOUTUBE_FALLBACK_TRIED
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                tried.insert(url.to_string())
            };
            if first_attempt {
                debug!(
                    url = %url,
                    first_max_height = best_max,
                    "web_embedded 仅返回最高 {}p 格式，尝试 tv_embedded/tv/visionos 获取更高码率（每视频仅一次）",
                    best_max
                );
                if let Ok(alt) = run_ytdlp_metadata(
                    url,
                    platform,
                    Some("youtube:player_client=tv_embedded,tv,visionos"),
                )
                .await
                {
                    let alt_max = youtube_max_format_height(&alt.formats);
                    if alt_max > best_max {
                        info!(
                            url = %url,
                            previous_max_height = best_max,
                            alt_max_height = alt_max,
                            "YouTube 媒体解析改用 tv_embedded/tv/visionos 客户端（{}p -> {}p）",
                            best_max,
                            alt_max
                        );
                        return Ok(alt);
                    }
                }
            }
        }
        return Ok(best);
    }
    Ok(metadata)
}

/// 记录已尝试过 tv_embedded 兜底解析的视频 URL，避免每次重试都多发一个请求。
static YOUTUBE_FALLBACK_TRIED: LazyLock<StdMutex<HashSet<String>>> =
    LazyLock::new(|| StdMutex::new(HashSet::new()));

async fn run_ytdlp_metadata(
    url: &str,
    platform: &str,
    client_args: Option<&str>,
) -> Result<ExternalMediaMetadata> {
    let mut command = ytdlp_command();
    command.args([
        "--dump-single-json",
        "--skip-download",
        "--no-playlist",
        "--no-warnings",
    ]);
    if let Some(client_args) = client_args {
        command.args(["--extractor-args", client_args]);
    }
    append_ytdlp_runtime(&mut command);
    append_cookies_for_url(&mut command, url);
    append_youtube_proxy_for_url(&mut command, url);
    command.arg(url);
    let output = tokio::time::timeout(DOWNLOAD_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow!("解析 {platform} 媒体直链超时"))??;
    if !output.status.success() {
        bail!("yt-dlp 解析 {platform} 媒体直链失败：{}", command_error(&output));
    }
    serde_json::from_slice(&output.stdout).with_context(|| format!("解析 yt-dlp {platform} 媒体元数据失败"))
}

/// 格式列表中分辨率上限（最高高度，无则 0）。
fn youtube_max_format_height(formats: &[ExternalMediaFormat]) -> i32 {
    formats
        .iter()
        .filter_map(|format| format.height)
        .max()
        .unwrap_or(0)
}

/// YouTube 验证墙/客户端不可用错误：命中后应拉长退避，避免高频请求加重风控。
fn is_youtube_bot_wall_error(error: &anyhow::Error) -> bool {
    let text = format!("{:#}", error).to_ascii_lowercase();
    [
        "sign in to confirm you're not a bot",
        "sign in to confirm",
        "requested format is not available",
        "the page needs to be reloaded",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// 需要 YouTube 登录状态的私有来源类型。
fn is_youtube_private_source(source: &youtube_source::Model) -> bool {
    matches!(
        source.source_type.as_str(),
        "watch_later" | "liked" | "subscriptions"
    )
}

/// yt-dlp 对“需要登录的私有列表（如稍后再看 WL）”在未登录/登录失效时的典型报错。
fn is_youtube_auth_required_error(error: &anyhow::Error) -> bool {
    let text = format!("{:#}", error).to_ascii_lowercase();
    [
        "playlist does not exist",
        "this playlist is private",
        "private video",
        "sign in to confirm",
        "requires authentication",
        "login required",
        "log in",
        "must be logged in",
        "you are not authorized",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// 在格式列表中选择最佳独立视频流（视频/音频分离时使用）。
/// `allow_vp9` 为 true 时允许 VP9 编码作为最高画质兜底。
fn select_best_youtube_video_stream(
    formats: &[ExternalMediaFormat],
    filter: &FilterOption,
    min_height: i32,
    max_height: i32,
    allow_vp9: bool,
) -> Option<ExternalMediaFormat> {
    formats
        .iter()
        .filter(|format| {
            if !is_http_format(format) || !has_video(format) || has_audio(format) {
                return false;
            }
            if allow_vp9 {
                youtube_video_allowed_with_vp9_fallback(format, filter)
            } else {
                youtube_video_allowed(format, filter)
            }
        })
        .cloned()
        .max_by_key(|format| {
            let height = format_quality_height(format);
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
}

/// 在格式列表中选择最佳混合流（音视频一体时使用），同样支持 VP9 兜底。
fn select_best_youtube_mixed_stream(
    formats: &[ExternalMediaFormat],
    filter: &FilterOption,
    min_height: i32,
    max_height: i32,
    min_audio_bitrate: i32,
    max_audio_bitrate: i32,
    allow_vp9: bool,
) -> Option<ExternalMediaFormat> {
    formats
        .iter()
        .filter(|format| {
            if !is_http_format(format) || !has_video(format) || !has_audio(format) {
                return false;
            }
            if !youtube_audio_allowed(format, filter) {
                return false;
            }
            if allow_vp9 {
                youtube_video_allowed_with_vp9_fallback(format, filter)
            } else {
                youtube_video_allowed(format, filter)
            }
        })
        .cloned()
        .max_by_key(|format| {
            let height = format_quality_height(format);
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
}

/// VP9 兜底升级：strict 为偏好编码下的选择，relaxed 为允许 VP9 后的选择；
/// 当 relaxed 能提供更高分辨率时升级并记录日志（保持设备兼容：同分辨率仍优先偏好编码）。
fn upgrade_with_vp9_fallback(
    strict: Option<ExternalMediaFormat>,
    relaxed: Option<ExternalMediaFormat>,
    platform: &str,
) -> Option<ExternalMediaFormat> {
    match (strict, relaxed) {
        (Some(strict), Some(relaxed)) if format_quality_height(&relaxed) > format_quality_height(&strict) => {
            info!(
                platform,
                strict_height = format_quality_height(&strict),
                vp9_height = format_quality_height(&relaxed),
                "{}视频最高画质仅有 VP9 编码（偏好编码最高 {}p，VP9 可达 {}p），已自动放行以保证最高画质",
                platform,
                format_quality_height(&strict),
                format_quality_height(&relaxed)
            );
            Some(relaxed)
        }
        (None, Some(relaxed)) => {
            info!(
                platform,
                vp9_height = format_quality_height(&relaxed),
                "{}视频可用格式均为 VP9 编码（偏好编码无可用流），已自动放行最高画质 {}p",
                platform,
                format_quality_height(&relaxed)
            );
            Some(relaxed)
        }
        (strict, _) => strict,
    }
}

fn select_youtube_streams(
    formats: &[ExternalMediaFormat],
    filter: &FilterOption,
    audio_only: bool,
    platform: &str,
) -> Result<SelectedStreams> {
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
    let min_height = youtube_quality_height(filter.video_min_quality);
    let max_height = youtube_quality_height(filter.video_max_quality);
    if audio_only {
        let strict_mixed = select_best_youtube_mixed_stream(
            formats,
            filter,
            min_height,
            max_height,
            min_audio_bitrate,
            max_audio_bitrate,
            false,
        );
        let relaxed_mixed = select_best_youtube_mixed_stream(
            formats,
            filter,
            min_height,
            max_height,
            min_audio_bitrate,
            max_audio_bitrate,
            true,
        );
        let mixed = upgrade_with_vp9_fallback(strict_mixed, relaxed_mixed, platform);
        return Ok(SelectedStreams {
            video: None,
            audio,
            mixed,
        });
    }

    // 先按用户编码偏好选择；若偏好编码无法达到更高分辨率，自动放行 VP9 保证最高画质
    let strict_video = select_best_youtube_video_stream(formats, filter, min_height, max_height, false);
    let relaxed_video = select_best_youtube_video_stream(formats, filter, min_height, max_height, true);
    let video = upgrade_with_vp9_fallback(strict_video, relaxed_video, platform);

    let strict_mixed = select_best_youtube_mixed_stream(
        formats,
        filter,
        min_height,
        max_height,
        min_audio_bitrate,
        max_audio_bitrate,
        false,
    );
    let relaxed_mixed = select_best_youtube_mixed_stream(
        formats,
        filter,
        min_height,
        max_height,
        min_audio_bitrate,
        max_audio_bitrate,
        true,
    );
    let mixed = upgrade_with_vp9_fallback(strict_mixed, relaxed_mixed, platform);
    if video.is_none() && mixed.is_none() {
        let ids = formats
            .iter()
            .filter_map(|format| format.format_id.as_deref())
            .collect::<Vec<_>>()
            .join(",");
        bail!("{platform}没有可下载的视频格式；解析到的格式 ID: {}", ids);
    }
    Ok(SelectedStreams { video, audio, mixed })
}

fn format_quality_height(format: &ExternalMediaFormat) -> i32 {
    match (format.width, format.height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => width.min(height),
        (_, Some(height)) => height,
        (Some(width), None) => width,
        _ => 0,
    }
}

/// 返回实际下载时会使用的视频流高度。
///
/// 与 `download_youtube_media` 的分支保持一致：
/// - 独立视频流 + 独立音频流同时存在时，使用视频流高度；
/// - 否则使用混合流（音视频一体）高度。
fn selected_effective_video_height(selected: &SelectedStreams) -> Option<i32> {
    if selected.video.is_some() && selected.audio.is_some() {
        selected.video.as_ref().map(format_quality_height)
    } else {
        selected.mixed.as_ref().map(format_quality_height)
    }
}

/// 编码是否命中用户的编码偏好列表。
fn youtube_codec_allowed(codec: Option<&str>, filter: &FilterOption) -> bool {
    youtube_codec(codec).is_some_and(|codec| filter.codecs.contains(&codec))
}

/// 格式的 HDR/Dolby 元数据过滤（与编码偏好无关）。
fn youtube_video_metadata_allowed(format: &ExternalMediaFormat, filter: &FilterOption) -> bool {
    let dynamic_range = format.dynamic_range.as_deref().unwrap_or("SDR").to_ascii_uppercase();
    if filter.no_hdr && dynamic_range != "SDR" {
        return false;
    }
    if filter.no_dolby_video && (dynamic_range.contains("DV") || dynamic_range.contains("DOLBY")) {
        return false;
    }
    true
}

fn youtube_video_allowed(format: &ExternalMediaFormat, filter: &FilterOption) -> bool {
    youtube_codec_allowed(format.vcodec.as_deref(), filter) && youtube_video_metadata_allowed(format, filter)
}

/// 是否为 VP9 编码（YouTube 的 1440p/4K/8K 常见编码，偏好列表里没有时作为最高画质兜底）。
fn is_youtube_vp9(codec: Option<&str>) -> bool {
    let codec = codec.unwrap_or_default().to_ascii_lowercase();
    codec.starts_with("vp9") || codec.starts_with("vp09")
}

/// 编码偏好 + VP9 兜底：命中偏好列表，或为 VP9（仅在偏好编码无法达到更高分辨率时使用）。
fn youtube_video_allowed_with_vp9_fallback(format: &ExternalMediaFormat, filter: &FilterOption) -> bool {
    (youtube_codec_allowed(format.vcodec.as_deref(), filter) || is_youtube_vp9(format.vcodec.as_deref()))
        && youtube_video_metadata_allowed(format, filter)
}

fn youtube_audio_allowed(format: &ExternalMediaFormat, filter: &FilterOption) -> bool {
    let codec = format.acodec.as_deref().unwrap_or_default().to_ascii_lowercase();
    if filter.no_dolby_audio && (codec.contains("ac-3") || codec.contains("ec-3") || codec.contains("dolby")) {
        return false;
    }
    !filter.no_hires || youtube_audio_bitrate(format) <= 256
}

fn youtube_audio_bitrate(format: &ExternalMediaFormat) -> i32 {
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

fn log_selected_youtube_streams(
    selected: &SelectedStreams,
    filter: &FilterOption,
    source: &youtube_source::Model,
    title: &str,
) {
    let platform = source_platform_label(source);
    let describe = |format: &ExternalMediaFormat| {
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
        platform,
        video_min = ?filter.video_min_quality,
        video_max = ?filter.video_max_quality,
        audio_min = ?filter.audio_min_quality,
        audio_max = ?filter.audio_max_quality,
        codecs = ?filter.codecs,
        video = selected.video.as_ref().map(&describe),
        audio = selected.audio.as_ref().map(&describe),
        mixed = selected.mixed.as_ref().map(&describe),
        "{}视频源「{}」视频「{}」已按项目流过滤设置选择下载格式",
        platform,
        source.name,
        title
    );
}

fn youtube_output_path(
    source: &youtube_source::Model,
    video: &youtube_video::Model,
    metadata: &ExternalMediaMetadata,
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
    // 抖音短剧/放映厅/合集与 B 站合集 up_seasonal 模式保持一致：
    // 下载根目录/剧集名/S01E001 - 每集标题.mp4。一个剧集一个目录，
    // 文件名带三位集号保证文件系统排序稳定；无集号时回退到普通单 P 命名。
    let episodic_source = matches!(
        source.source_type.as_str(),
        "douyin_theater" | "douyin_series" | "douyin_collection"
    );
    let (video_folder, page_name) = if episodic_source {
        if let Some(episode) = video.episode_number.filter(|value| *value >= 1) {
            // 与 B 站番剧标准结构保持一致：
            // 下载根目录/剧集名/Season 01/S01E01.mp4（文件名不带标题，避免超长）。
            let series_folder = crate::utils::filenamify::filenamify(&source.name);
            let series_folder = if series_folder.is_empty() {
                format!("剧集_{}", source.id)
            } else {
                series_folder
            };
            let video_folder = format!("{series_folder}/Season 01");
            (video_folder, format!("S01E{:02}", episode))
        } else {
            crate::config::with_config(|bundle| {
                Ok::<_, anyhow::Error>((
                    bundle.render_video_template(&args)?,
                    bundle.render_page_template(&args)?,
                ))
            })?
        }
    } else {
        crate::config::with_config(|bundle| {
            Ok::<_, anyhow::Error>((
                bundle.render_video_template(&args)?,
                bundle.render_page_template(&args)?,
            ))
        })?
    };

    // 图文作品必须生成可在现有视频管理页播放的 MP4，
    // 同时原图和配乐仍保留在同目录。
    let extension = if source.audio_only && metadata.images.is_empty() {
        "m4a"
    } else {
        "mp4"
    };
    let root = PathBuf::from(&source.path);
    if source.flat_folder {
        Ok(root.join(format!("{page_name}.{extension}")))
    } else {
        Ok(root.join(video_folder).join(format!("{page_name}.{extension}")))
    }
}

async fn ensure_youtube_sidecars(
    downloader: &UnifiedDownloader,
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    video_url: &str,
    title: &str,
    uploader: &str,
    source: &youtube_source::Model,
) -> Option<String> {
    let mut warnings = Vec::new();
    let platform = source_platform_label(source);
    let profile_url = metadata.channel_url.as_deref().or(metadata.uploader_url.as_deref());
    if let Err(error) = download_youtube_upper_face(downloader, uploader, profile_url, source).await {
        warn!(platform, youtube_id = %metadata.id, error = %error, "{}视频源「{}」视频「{}」媒体已下载，但 UP 头像子任务失败", platform, source.name, title);
        warnings.push(format!("UP头像下载失败：{error:#}"));
    }
    if let Err(error) = download_youtube_cover(downloader, metadata, output_path, source).await {
        warn!(platform, youtube_id = %metadata.id, error = %error, "{}视频源「{}」视频「{}」媒体已下载，但封面子任务失败", platform, source.name, title);
        warnings.push(format!("封面下载失败：{error:#}"));
    }
    if let Err(error) = generate_youtube_nfo(metadata, output_path, video_url, title, uploader, source).await {
        warn!(platform, youtube_id = %metadata.id, error = %error, "{}视频源「{}」视频「{}」媒体已下载，但 NFO 子任务失败", platform, source.name, title);
        warnings.push(format!("NFO 生成失败：{error:#}"));
    }
    if source.download_subtitle {
        if let Err(error) =
            download_youtube_subtitle(downloader, metadata, output_path, &source.ai_subtitle_language, source).await
        {
            warn!(platform, youtube_id = %metadata.id, error = %error, "{}视频源「{}」视频「{}」媒体已下载，但字幕子任务失败", platform, source.name, title);
            warnings.push(format!("字幕下载失败：{error:#}"));
        }
    }
    if source.download_danmaku {
        let result = if is_douyin_source(source) {
            crate::douyin::download_danmaku(metadata, output_path, title).await
        } else {
            download_youtube_live_chat(metadata, output_path, video_url, title).await
        };
        if let Err(error) = result {
            let task = if is_douyin_source(source) {
                "弹幕"
            } else {
                "直播聊天"
            };
            warn!(
                platform,
                youtube_id = %metadata.id,
                error = %error,
                "{}视频源「{}」视频「{}」媒体已下载，但{}子任务失败",
                platform,
                source.name,
                title,
                task
            );
            warnings.push(format!("{task}下载失败：{error:#}"));
        } else {
            let task = if is_douyin_source(source) {
                "弹幕"
            } else {
                "直播聊天"
            };
            info!(platform, youtube_id = %metadata.id, "{}视频源「{}」视频「{}」{} 子任务完成", platform, source.name, title, task);
        }
    }
    // 短剧/放映厅/合集按番剧结构补充剧集级附属文件（tvshow.nfo、season.nfo、folder.jpg、poster.jpg）。
    if is_episodic_douyin_source(source) {
        if let Err(error) = ensure_youtube_series_sidecars(downloader, metadata, output_path, source).await {
            warn!(platform, youtube_id = %metadata.id, error = %error, "{}视频源「{}」媒体已下载，但剧集附属文件子任务失败", platform, source.name);
            warnings.push(format!("剧集附属文件生成失败：{error:#}"));
        }
    }
    (!warnings.is_empty()).then(|| format!("媒体已完成；{}", warnings.join("；")))
}

fn is_episodic_douyin_source(source: &youtube_source::Model) -> bool {
    matches!(
        source.source_type.as_str(),
        "douyin_theater" | "douyin_series" | "douyin_collection"
    )
}

/// 抖音短剧/放映厅/合集按番剧结构生成剧集级附属文件：
/// 剧集根目录 tvshow.nfo、folder.jpg、poster.jpg，以及 Season 01/season.nfo。
/// 幂等：已存在的文件跳过，不重复下载。
async fn ensure_youtube_series_sidecars(
    downloader: &UnifiedDownloader,
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    source: &youtube_source::Model,
) -> Result<()> {
    let platform = source_platform_label(source);
    let Some(season_dir) = output_path.parent() else {
        return Ok(());
    };
    let Some(series_dir) = season_dir.parent() else {
        return Ok(());
    };
    tokio::fs::create_dir_all(&series_dir).await.ok();

    // folder.jpg / poster.jpg：优先复用已下载的单集封面，避免额外请求。
    let thumb_path = youtube_sidecar_path(output_path, "-thumb.jpg")?;
    let thumb_exists = tokio::fs::metadata(&thumb_path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024);
    for name in ["folder.jpg", "poster.jpg"] {
        let target = series_dir.join(name);
        if tokio::fs::metadata(&target)
            .await
            .is_ok_and(|metadata| metadata.len() >= 1024)
        {
            continue;
        }
        if thumb_exists {
            tokio::fs::copy(&thumb_path, &target)
                .await
                .with_context(|| format!("生成{platform}剧集封面失败: {}", target.display()))?;
        } else if let Some(url) = metadata.thumbnail.as_deref() {
            let temporary = target.with_extension("download");
            if let Err(error) = fetch_platform_asset(downloader, source, &[url], &temporary)
                .await
                .with_context(|| format!("使用项目统一下载器下载{platform}剧集封面失败"))
            {
                let _ = remove_file_if_exists(&temporary).await;
                return Err(error);
            }
            replace_file(&temporary, &target).await?;
        }
        info!(platform, path = %target.display(), "{}视频源「{}」剧集封面生成完成", platform, source.name);
    }

    // 剧集级 NFO 仅在启用 NFO 时生成
    if !crate::config::reload_config().nfo_config.enabled {
        return Ok(());
    }
    let tvshow_path = series_dir.join("tvshow.nfo");
    if !tokio::fs::metadata(&tvshow_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        let xml = generate_youtube_tvshow_nfo(source, metadata);
        let temporary = series_dir.join("tvshow.nfo.download");
        tokio::fs::write(&temporary, xml.as_bytes()).await.with_context(|| {
            format!("写入{platform}剧集 NFO 失败: {}", temporary.display())
        })?;
        replace_file(&temporary, &tvshow_path).await?;
        info!(platform, path = %tvshow_path.display(), "{}视频源「{}」剧集 tvshow.nfo 生成完成", platform, source.name);
    }

    let season_path = season_dir.join("season.nfo");
    if !tokio::fs::metadata(&season_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        let xml = generate_youtube_season_nfo();
        let temporary = season_dir.join("season.nfo.download");
        tokio::fs::write(&temporary, xml.as_bytes()).await.with_context(|| {
            format!("写入{platform}季度 NFO 失败: {}", temporary.display())
        })?;
        replace_file(&temporary, &season_path).await?;
        info!(platform, path = %season_path.display(), "{}视频源「{}」季度 season.nfo 生成完成", platform, source.name);
    }
    Ok(())
}

fn generate_youtube_tvshow_nfo(source: &youtube_source::Model, metadata: &ExternalMediaMetadata) -> String {
    let escape = |value: &str| quick_xml::escape::escape(value).into_owned();
    let platform = source_platform(source);
    let studio = source_platform_label(source);
    let description = metadata.description.as_deref().unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<tvshow>
    <title>{}</title>
    <originaltitle>{}</originaltitle>
    <sorttitle>{}</sorttitle>
    <plot>{}</plot>
    <studio>{}</studio>
    <uniqueid type="{}" default="true">{}</uniqueid>
    <season>-1</season>
    <episode>-1</episode>
</tvshow>
"#,
        escape(&source.name),
        escape(&source.name),
        escape(&source.name),
        escape(description),
        escape(studio),
        escape(platform),
        escape(&source.url),
    )
}

fn generate_youtube_season_nfo() -> String {
    r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<season>
    <seasonnumber>1</seasonnumber>
    <title>Season 01</title>
    <plot></plot>
    <locked>false</locked>
</season>
"#
        .to_string()
}

async fn download_youtube_upper_face(
    downloader: &UnifiedDownloader,
    uploader: &str,
    profile_url: Option<&str>,
    source: &youtube_source::Model,
) -> Result<()> {
    let uploader = crate::utils::filenamify::filenamify(uploader);
    let platform = source_platform_label(source);
    if crate::tiktok::is_tiktok_source(source) {
        // TikTok 头像通过 `api/creator/item_list/` Web API 获取（带 Cookie + 随机
        // verifyFp + 进程稳定 device_id 即可服务端直连）；人物 NFO 一并生成。
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
        tokio::fs::create_dir_all(&upper_dir)
            .await
            .with_context(|| format!("创建{platform} UP头像目录失败: {}", upper_dir.display()))?;

        if !face_exists {
            let sec_uid = profile_url.and_then(crate::tiktok::tiktok_handle_from_url).ok_or_else(|| {
                anyhow!("TikTok 元数据没有频道主页地址，无法获取 UP 头像")
            })?;
            let avatar_url = crate::tiktok::fetch_tiktok_author_avatar_url(&sec_uid)
                .await?
                .ok_or_else(|| anyhow!("TikTok 作者作品接口没有返回头像地址"))?;
            let temporary = upper_dir.join("folder.download");
            if let Err(error) = fetch_platform_asset(downloader, source, &[avatar_url.as_str()], &temporary)
                .await
                .with_context(|| format!("使用项目统一下载器下载{platform} UP头像失败"))
            {
                let _ = remove_file_if_exists(&temporary).await;
                return Err(error);
            }
            replace_file(&temporary, &face_path).await?;
            info!(platform = source_platform_label(source), uploader, path = %face_path.display(), "{}视频源「{}」 UP头像「{}」下载完成", source_platform_label(source), source.name, uploader);
        }

        if !person_nfo_exists {
            let channel_id = profile_url
                .and_then(crate::tiktok::tiktok_handle_from_url)
                .unwrap_or_else(|| uploader.clone());
            let xml = generate_youtube_person_nfo(&uploader, &channel_id, "tiktok");
            let temporary = upper_dir.join("person.nfo.download");
            if let Err(error) = tokio::fs::write(&temporary, xml.as_bytes())
                .await
                .with_context(|| format!("写入{platform} UP主 NFO 临时文件失败: {}", temporary.display()))
            {
                let _ = remove_file_if_exists(&temporary).await;
                return Err(error);
            }
            replace_file(&temporary, &person_nfo_path).await?;
            info!(platform = source_platform_label(source), uploader, path = %person_nfo_path.display(), "{}视频源「{}」 UP主 person.nfo「{}」生成完成", source_platform_label(source), source.name, uploader);
        }
        return Ok(());
    }
    if uploader.is_empty() {
        bail!("{platform} UP主名称为空");
    }
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

    tokio::fs::create_dir_all(&upper_dir)
        .await
        .with_context(|| format!("创建{platform} UP头像目录失败: {}", upper_dir.display()))?;

    if !face_exists {
        let avatar_url = if is_douyin_source(source) {
            let author_profile_url = profile_url
                .filter(|url| url.contains("douyin.com/user/"))
                .unwrap_or(&source.url);
            crate::douyin::fetch_profile(author_profile_url)
                .await?
                .avatar_url
                .context("抖音作者资料没有返回头像")?
        } else {
            let profile_url = profile_url
                .filter(|url| url.starts_with("http"))
                .context("YouTube 元数据没有频道主页地址")?;
            extract_youtube_source_metadata(profile_url)
                .await?
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
                .map(|thumbnail| thumbnail.url.clone())
                .context("YouTube 频道主页没有返回 UP 头像")?
        };
        let temporary = upper_dir.join("folder.download");
        if let Err(error) = fetch_platform_asset(downloader, source, &[avatar_url.as_str()], &temporary)
            .await
            .with_context(|| format!("使用项目统一下载器下载{platform} UP头像失败"))
        {
            let _ = remove_file_if_exists(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &face_path).await?;
        info!(platform = source_platform_label(source), uploader, path = %face_path.display(), "{}视频源「{}」 UP头像「{}」下载完成", source_platform_label(source), source.name, uploader);
    }

    if !person_nfo_exists {
        let channel_id = if is_douyin_source(source) {
            let author_profile_url = profile_url
                .filter(|url| url.contains("douyin.com/user/"))
                .unwrap_or(&source.url);
            crate::douyin::resolve_sec_user_id(author_profile_url).await?
        } else {
            let profile_url = profile_url
                .filter(|url| url.starts_with("http"))
                .context("YouTube 元数据没有频道主页地址")?;
            let profile = extract_youtube_source_metadata(profile_url).await?;
            profile
                .channel_id
                .or(profile.uploader_id)
                .or(profile.id)
                .filter(|id| !id.trim().is_empty())
                .context("YouTube 频道主页没有返回频道 ID")?
        };
        let xml = generate_youtube_person_nfo(&uploader, &channel_id, source_platform(source));
        let temporary = upper_dir.join("person.nfo.download");
        if let Err(error) = tokio::fs::write(&temporary, xml.as_bytes())
            .await
            .with_context(|| format!("写入{platform} UP主 NFO 临时文件失败: {}", temporary.display()))
        {
            let _ = remove_file_if_exists(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &person_nfo_path).await?;
        info!(platform = source_platform_label(source), uploader, path = %person_nfo_path.display(), "{}视频源「{}」 UP主「{}」 person.nfo 生成完成", source_platform_label(source), source.name, uploader);
    }
    Ok(())
}

fn generate_youtube_person_nfo(uploader: &str, channel_id: &str, platform: &str) -> String {
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
    <uniqueid type="{}_channel" default="true">{}</uniqueid>
</person>"#,
        now_standard_string(),
        escape(uploader),
        escape(uploader),
        escape(platform),
        escape(channel_id),
    )
}

async fn extract_youtube_source_metadata(url: &str) -> Result<YtDlpSourceMetadata> {
    let mut command = ytdlp_command();
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
    append_youtube_proxy(&mut command);
    append_ytdlp_tab_args(&mut command);
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
    let parent = output_path.parent().context("媒体输出文件没有父目录")?;
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .context("媒体输出文件名无效")?;
    Ok(parent.join(format!("{stem}{suffix}")))
}

async fn download_youtube_cover(
    downloader: &UnifiedDownloader,
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    source: &youtube_source::Model,
) -> Result<()> {
    let platform = source_platform_label(source);
    let url = metadata
        .thumbnail
        .as_deref()
        .with_context(|| format!("{platform}元数据没有返回视频封面"))?;
    let thumb_path = youtube_sidecar_path(output_path, "-thumb.jpg")?;
    if !tokio::fs::metadata(&thumb_path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024)
    {
        let temporary = youtube_sidecar_path(output_path, "-thumb.download")?;
        if let Err(error) = fetch_platform_asset(downloader, source, &[url], &temporary)
            .await
            .with_context(|| format!("使用项目统一下载器下载{platform}封面失败"))
        {
            let _ = remove_file_if_exists(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &thumb_path).await?;
        info!(platform = source_platform_label(source), youtube_id = %metadata.id, path = %thumb_path.display(), "{}视频源「{}」视频「{}」封面下载完成", source_platform_label(source), source.name, metadata.title.as_deref().unwrap_or(&metadata.id));
    }
    let fanart_path = youtube_sidecar_path(output_path, "-fanart.jpg")?;
    if !tokio::fs::metadata(&fanart_path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024)
    {
        tokio::fs::copy(&thumb_path, &fanart_path)
            .await
            .with_context(|| format!("生成{platform} fanart 失败: {}", fanart_path.display()))?;
        info!(platform = source_platform_label(source), youtube_id = %metadata.id, path = %fanart_path.display(), "{}视频源「{}」视频「{}」 fanart 生成完成", source_platform_label(source), source.name, metadata.title.as_deref().unwrap_or(&metadata.id));
    }
    Ok(())
}

/// 从 yt-dlp `creators` 中提取「联合创作者」频道名（主上传频道之外的名字）。
///
/// yt-dlp 在联合投稿视频上会返回「主频道 + 合作频道」的完整列表；但音乐视频的
/// `creators` 也可能是表演者名单。仅当列表至少两位时才视为联合投稿，再去掉与
/// 主频道相同的名字并去重，避免把单作者音乐视频的表演者当合作频道重复列出。
fn youtube_co_creators(metadata: &ExternalMediaMetadata, uploader: &str) -> Vec<String> {
    let Some(creators) = metadata.creators.as_deref() else {
        return Vec::new();
    };
    if creators.len() < 2 {
        return Vec::new();
    }
    let main = uploader.trim();
    let mut seen = HashSet::new();
    let mut extras = Vec::new();
    for name in creators {
        let name = name.trim();
        if name.is_empty() || name.eq_ignore_ascii_case(main) {
            continue;
        }
        if seen.insert(name.to_lowercase()) {
            extras.push(name.to_string());
        }
    }
    extras
}

/// 构建单视频（movie）NFO XML。
///
/// - `studio`：平台名（如 YouTube/抖音），写入 `<studio>`；
/// - `aired`：上传时间，分别写入 `<year>/<premiered>/<aired>`。
/// - `uploader`：主频道，写入 `<director>` 并作为第一个 `<actor>`；
/// - `co_creators`：联合投稿的合作频道名，逐个补写 `<actor>`。
/// 注意 `<aired>` 与 `<studio>` 两个标签的参数顺序不能写反，
/// 否则会出现"工作室=日期、首播=平台名"的错位。
fn build_youtube_movie_nfo_xml(
    title: &str,
    description: &str,
    platform: &str,
    studio: &str,
    id: &str,
    aired: chrono::NaiveDateTime,
    uploader: &str,
    co_creators: &[String],
    thumbnail: &str,
    video_url: &str,
) -> String {
    let escape = |value: &str| quick_xml::escape::escape(value).into_owned();
    let actor_lines = std::iter::once(uploader)
        .chain(co_creators.iter().map(String::as_str))
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("    <actor><name>{}</name><role>频道</role></actor>", escape(name)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?>\n\
<movie>\n\
    <title>{}</title>\n\
    <originaltitle>{}</originaltitle>\n\
    <sorttitle>{}</sorttitle>\n\
    <plot>{}</plot>\n\
    <uniqueid type=\"{}\" default=\"true\">{}</uniqueid>\n\
    <year>{}</year>\n\
    <premiered>{}</premiered>\n\
    <aired>{}</aired>\n\
    <studio>{}</studio>\n\
    <director>{}</director>\n\
{actor_lines}\n\
    <thumb aspect=\"poster\">{}</thumb>\n\
    <fanart><thumb>{}</thumb></fanart>\n\
    <website>{}</website>\n\
</movie>\n",
        escape(title),
        escape(title),
        escape(title),
        escape(description),
        escape(platform),
        escape(id),
        aired.format("%Y"),
        aired.format("%Y-%m-%d"),
        aired.format("%Y-%m-%d"),
        escape(studio),
        escape(uploader),
        escape(thumbnail),
        escape(thumbnail),
        escape(video_url),
    )
}

async fn generate_youtube_nfo(
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    video_url: &str,
    title: &str,
    uploader: &str,
    source: &youtube_source::Model,
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
    let thumbnail = metadata.thumbnail.as_deref().unwrap_or_default();
    let description = metadata.description.as_deref().unwrap_or_default();
    let platform = source_platform(source);
    let studio = source_platform_label(source);
    let co_creators = youtube_co_creators(metadata, uploader);
    let xml = build_youtube_movie_nfo_xml(
        title,
        description,
        platform,
        studio,
        &metadata.id,
        aired,
        uploader,
        &co_creators,
        thumbnail,
        video_url,
    );
    let temporary = nfo_path.with_extension("nfo.download");
    tokio::fs::write(&temporary, xml.as_bytes()).await.with_context(|| {
        format!(
            "写入{} NFO 失败: {}",
            source_platform_label(source),
            temporary.display()
        )
    })?;
    replace_file(&temporary, &nfo_path).await?;
    info!(platform = source_platform_label(source), youtube_id = %metadata.id, path = %nfo_path.display(), "{}视频源「{}」视频「{}」 NFO 生成完成", source_platform_label(source), source.name, title);
    Ok(())
}

async fn download_youtube_subtitle(
    downloader: &UnifiedDownloader,
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    preferred_language: &str,
    source: &youtube_source::Model,
) -> Result<()> {
    let platform = source_platform_label(source);
    if youtube_subtitle_exists(output_path).await? {
        return Ok(());
    }
    let checked_path = output_path.with_extension("subtitle.checked");
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
        tokio::fs::write(&checked_path, b"no matching subtitle\n")
            .await
            .with_context(|| format!("写入字幕检查标记失败: {}", checked_path.display()))?;
        info!(youtube_id = %metadata.id, "{}视频源「{}」视频「{}」没有匹配的字幕，跳过字幕子任务", platform, source.name, metadata.title.as_deref().unwrap_or(&metadata.id));
        return Ok(());
    };
    let extension = subtitle.ext.as_deref().unwrap_or("vtt");
    let subtitle_path = output_path.with_extension(format!("{language}.{extension}"));
    if let Err(error) = fetch_platform_asset(downloader, source, &[url], &subtitle_path)
        .await
        .with_context(|| format!("使用项目统一下载器下载{platform}字幕失败"))
    {
        let _ = remove_file_if_exists(&subtitle_path).await;
        return Err(error);
    }
    let _ = remove_file_if_exists(&checked_path).await;
    info!(platform, youtube_id = %metadata.id, path = %subtitle_path.display(), "{}视频源「{}」视频「{}」字幕下载完成", platform, source.name, metadata.title.as_deref().unwrap_or(&metadata.id));
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

async fn write_external_danmaku_ass(
    output_path: &Path,
    title: &str,
    duration_seconds: i32,
    danmaku: Vec<DanmakuElem>,
) -> Result<()> {
    let ass_path = output_path.with_extension("ass");
    let temporary = output_path.with_extension("ass.download");
    let page = BiliPageInfo {
        cid: 0,
        page: 1,
        name: title.to_string(),
        duration: u32::try_from(duration_seconds.max(0)).unwrap_or_default(),
        first_frame: None,
        dimension: None,
    };
    let writer = DanmakuWriter::new(&page, danmaku.into_iter().map(Into::into).collect());
    if let Err(error) = writer.write(temporary.clone()).await {
        let _ = remove_file_if_exists(&temporary).await;
        return Err(error).context("生成媒体 ASS 弹幕失败");
    }
    replace_file(&temporary, &ass_path).await
}

fn youtube_runs_text(value: Option<&serde_json::Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if let Some(text) = value.get("simpleText").and_then(serde_json::Value::as_str) {
        return text.to_string();
    }
    value
        .get("runs")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|run| {
            run.get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    run.pointer("/emoji/shortcuts/0")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| run.pointer("/emoji/emojiId").and_then(serde_json::Value::as_str))
                        .map(str::to_string)
                })
        })
        .collect()
}

fn parse_youtube_live_chat(contents: &str) -> Vec<DanmakuElem> {
    const RENDERERS: &[&str] = &[
        "liveChatTextMessageRenderer",
        "liveChatPaidMessageRenderer",
        "liveChatMembershipItemRenderer",
        "liveChatPaidStickerRenderer",
    ];
    let mut seen = HashSet::new();
    let mut elems = Vec::new();
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(replay) = value.get("replayChatItemAction") else {
            continue;
        };
        let progress = replay
            .get("videoOffsetTimeMsec")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or_default()
            .max(0);
        for action in replay
            .get("actions")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(item) = action.pointer("/addChatItemAction/item") else {
                continue;
            };
            let Some(renderer) = RENDERERS.iter().find_map(|key| item.get(*key)) else {
                continue;
            };
            let id = renderer
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !id.is_empty() && !seen.insert(id.clone()) {
                continue;
            }
            let mut content = youtube_runs_text(renderer.get("message"));
            if content.is_empty() {
                content = youtube_runs_text(renderer.get("headerSubtext"));
            }
            if content.is_empty() {
                content = youtube_runs_text(renderer.get("purchaseAmountText"));
            }
            if content.trim().is_empty() {
                continue;
            }
            elems.push(DanmakuElem {
                id: 0,
                progress,
                mode: 1,
                fontsize: 25,
                color: 0xFFFFFF,
                mid_hash: renderer
                    .get("authorExternalChannelId")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                content,
                ctime: renderer
                    .get("timestampUsec")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| value.parse::<i64>().ok())
                    .map(|value| value / 1_000_000)
                    .unwrap_or_default(),
                weight: 0,
                action: String::new(),
                pool: 0,
                dmid_str: id,
                attr: 0,
            });
        }
    }
    elems.sort_by_key(|item| item.progress);
    elems
}

async fn convert_youtube_live_chat_to_ass(
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    title: &str,
) -> Result<usize> {
    let live_chat_path = output_path.with_extension("live_chat.json");
    let contents = tokio::fs::read_to_string(&live_chat_path)
        .await
        .with_context(|| format!("读取 YouTube 直播聊天 JSON 失败: {}", live_chat_path.display()))?;
    let elems = parse_youtube_live_chat(&contents);
    let count = elems.len();
    let duration = metadata.duration.map(|value| value.ceil() as i32).unwrap_or_default();
    write_external_danmaku_ass(output_path, title, duration, elems).await?;
    Ok(count)
}

async fn download_youtube_live_chat(
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    video_url: &str,
    title: &str,
) -> Result<()> {
    let live_chat_path = output_path.with_extension("live_chat.json");
    let live_chat_ass_path = output_path.with_extension("ass");
    let checked_path = output_path.with_extension("live_chat.checked");
    if tokio::fs::metadata(&live_chat_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
        && tokio::fs::metadata(&live_chat_ass_path)
            .await
            .is_ok_and(|metadata| metadata.len() > 0)
    {
        return Ok(());
    }
    if tokio::fs::metadata(&checked_path).await.is_ok() {
        return Ok(());
    }
    if tokio::fs::metadata(&live_chat_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        let count = convert_youtube_live_chat_to_ass(metadata, output_path, title).await?;
        info!(youtube_id = %metadata.id, count, path = %live_chat_ass_path.display(), "YouTube 视频「{}」直播聊天 ASS 生成完成", title);
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
    let _ = url;
    let output_template = output_path.with_extension("%(ext)s");
    let mut command = ytdlp_command();
    command.args([
        "--skip-download",
        "--write-subs",
        "--sub-langs",
        "live_chat",
        "--sub-format",
        "json",
        "--no-warnings",
        "--no-progress",
        "-o",
    ]);
    command.arg(&output_template);
    append_ytdlp_runtime(&mut command);
    append_cookies_for_url(&mut command, video_url);
    append_youtube_proxy_for_url(&mut command, video_url);
    command.arg(video_url);
    let output = tokio::time::timeout(DOWNLOAD_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow!("YouTube 直播聊天下载超时"))??;
    if !output.status.success() {
        bail!("yt-dlp 下载 YouTube 直播聊天失败：{}", command_error(&output));
    }
    if !tokio::fs::metadata(&live_chat_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        bail!("yt-dlp 未生成 YouTube 直播聊天文件");
    }
    let count = convert_youtube_live_chat_to_ass(metadata, output_path, title).await?;
    let _ = remove_file_if_exists(&checked_path).await;
    info!(youtube_id = %metadata.id, count, json = %live_chat_path.display(), ass = %live_chat_ass_path.display(), "YouTube 直播聊天 JSON 和 ASS 下载完成");
    Ok(())
}

fn select_subtitle_item<'a>(
    subtitles: &'a HashMap<String, Vec<ExternalSubtitle>>,
    language: &str,
) -> Option<(&'a ExternalSubtitle, &'a str)> {
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
    if tokio::fs::metadata(output_path.with_extension("subtitle.checked"))
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        return Ok(true);
    }
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
    } else if codec.starts_with("hev") || codec.starts_with("hvc") || codec.starts_with("h265") {
        Some(VideoCodecs::HEV)
    } else if codec.starts_with("av01") || codec.starts_with("av1") {
        Some(VideoCodecs::AV1)
    } else {
        None
    }
}

fn is_http_format(format: &ExternalMediaFormat) -> bool {
    format.url.as_deref().is_some_and(|url| url.starts_with("http"))
        && format
            .protocol
            .as_deref()
            .is_none_or(|protocol| protocol == "http" || protocol == "https")
}

fn has_video(format: &ExternalMediaFormat) -> bool {
    format.vcodec.as_deref().is_some_and(|codec| codec != "none")
}

fn has_audio(format: &ExternalMediaFormat) -> bool {
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

pub(crate) async fn source_response(db: &DatabaseConnection, source: youtube_source::Model) -> Result<YouTubeSourceResponse> {
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
        scan_deleted_videos: source.scan_deleted_videos,
        scan_deleted_videos_once: source.scan_deleted_videos_once,
        audio_only: source.audio_only,
        audio_only_m4a_only: source.audio_only_m4a_only,
        flat_folder: source.flat_folder,
        download_danmaku: source.download_danmaku,
        download_subtitle: source.download_subtitle,
        ai_subtitle_language: source.ai_subtitle_language,
        ai_rename: source.ai_rename,
        ai_rename_video_prompt: source.ai_rename_video_prompt,
        ai_rename_audio_prompt: source.ai_rename_audio_prompt,
        ai_rename_enable_multi_page: source.ai_rename_enable_multi_page,
        ai_rename_enable_collection: source.ai_rename_enable_collection,
        ai_rename_enable_bangumi: source.ai_rename_enable_bangumi,
        ai_rename_rename_parent_dir: source.ai_rename_rename_parent_dir,
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
        selected_channels: source
            .selected_channels
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default(),
        last_scan_at: source.last_scan_at,
        pending_count: count("pending").await?,
        completed_count: count("completed").await?,
        failed_count: count("failed").await?,
        skipped_count: count("skipped").await?,
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
        is_charge_video: video.is_charge_video,
        charge_can_play: video.charge_can_play,
        download_status: video.download_status,
        retry_count: video.retry_count,
        output_path: video.output_path,
        error_message: video.error_message,
    }
}

pub(crate) fn normalize_source_type(value: &str) -> Result<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "subscriptions" => Ok("subscriptions"),
        "channel" => Ok("channel"),
        "playlist" => Ok("playlist"),
        "liked" => Ok("liked"),
        "watch_later" => Ok("watch_later"),
        "douyin" => Ok("douyin"),
        "tiktok" => Ok("tiktok"),
        "tiktok_favorite" => Ok("tiktok_favorite"),
        "tiktok_collection" => Ok("tiktok_collection"),
        "douyin_liked" => Ok("douyin_liked"),
        "douyin_collection" => Ok("douyin_collection"),
        "douyin_watch_later" => Ok("douyin_watch_later"),
        "douyin_theater" => Ok("douyin_theater"),
        "douyin_series" => Ok("douyin_series"),
        _ => bail!("来源类型必须是 subscriptions、channel、playlist、liked、watch_later 或有效的抖音来源类型"),
    }
}
fn resolve_source_url(kind: &str, supplied: Option<&str>) -> Result<String> {
    match kind {
        "subscriptions" => Ok(SUBSCRIPTIONS_URL.to_string()),
        "liked" => Ok(LIKED_URL.to_string()),
        "watch_later" => Ok(WATCH_LATER_URL.to_string()),
        "douyin" | "douyin_collection" | "douyin_theater" | "douyin_series" | "tiktok" | "tiktok_collection" => supplied
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("该来源必须选择或填写详情链接")),
        "douyin_liked" => Ok("https://www.douyin.com/user/self?tab=like".to_string()),
        "douyin_watch_later" => Ok("https://www.douyin.com/?watch_later=1".to_string()),
        "tiktok_favorite" => Ok("https://www.tiktok.com/self?tab=like".to_string()),
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

/// yt-dlp 自 2026.08.19 起内置 `visionos` 客户端：对 YouTube 走 SABR 协议的
/// 高码率源（web 客户端响应无签名直链）返回最高 4K 的签名直链，是目前不依赖
/// PO Token/JS 运行时的 4K 兜底。低于该版本的 yt-dlp 不认识该客户端。
const YTDLP_VISIONOS_MIN_VERSION: (u32, u32, u32) = (2026, 8, 19);

/// 解析 yt-dlp `--version` 输出（如 `2026.08.19`，开发版可能带 `.devN` 后缀），
/// 只取前三个数字段比较。
fn ytdlp_version_at_least(version: &str, want: (u32, u32, u32)) -> bool {
    let mut parts = version.split('.').filter_map(|part| part.parse::<u32>().ok());
    let got = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    got >= want
}

/// yt-dlp 自动升级失败后的重试冷却：启动阶段多个入口（设置页/添加源/扫描）都会调用
/// `ensure_ytdlp_available`，失败后 1 小时内不再重试，避免反复触发一次最长 3 分钟的下载。
const YTDLP_UPGRADE_RETRY_COOLDOWN: Duration = Duration::from_secs(60 * 60);

/// 距下次允许自动升级 yt-dlp 的 Unix 秒（0 表示未冷却）。
static YTDLP_UPGRADE_COOLDOWN_UNTIL: AtomicI64 = AtomicI64::new(0);

fn ytdlp_upgrade_on_cooldown() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    now < YTDLP_UPGRADE_COOLDOWN_UNTIL.load(Ordering::Relaxed)
}

fn set_ytdlp_upgrade_cooldown() {
    let until = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
        + YTDLP_UPGRADE_RETRY_COOLDOWN.as_secs() as i64;
    YTDLP_UPGRADE_COOLDOWN_UNTIL.store(until, Ordering::Relaxed);
}

pub(crate) async fn ensure_ytdlp_available() -> Result<()> {
    if let Ok(configured) = std::env::var("BILI_SYNC_YTDLP_PATH") {
        let configured = PathBuf::from(configured);
        let Some(version) = ytdlp_version_at(&configured).await else {
            bail!("BILI_SYNC_YTDLP_PATH 指向的 yt-dlp 不可用：{}", configured.display());
        };
        if !ytdlp_version_at_least(&version, YTDLP_VISIONOS_MIN_VERSION) {
            warn!(
                version,
                path = %configured.display(),
                "自定义 yt-dlp 版本过旧（缺少 visionos 客户端），YouTube 4K 视频可能只能下载到 1080p；请升级到 2026.08.19 及以上版本"
            );
        }
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

    // 已有可用的 yt-dlp：版本达标直接使用；过旧则自动升级（优先走外源代理）。
    // 升级失败不阻断流程，继续用旧版（4K 可能退化为 1080p），并进入 1 小时重试冷却。
    if let Some(version) = ytdlp_version().await {
        if ytdlp_version_at_least(&version, YTDLP_VISIONOS_MIN_VERSION) {
            return Ok(());
        }
        if ytdlp_upgrade_on_cooldown() {
            return Ok(());
        }
        let binary_path = managed_ytdlp_path(package);
        match download_and_install_ytdlp(package, &binary_path, Some(&version)).await {
            Ok(()) => {
                if let Some(new_version) = ytdlp_version_at(&binary_path).await {
                    info!(version = new_version, "yt-dlp 已自动升级并可用");
                }
                return Ok(());
            }
            Err(error) => {
                set_ytdlp_upgrade_cooldown();
                warn!(
                    version,
                    error = %error,
                    "yt-dlp 自动升级失败，继续使用现有版本（YouTube 4K 可能退化为 1080p，1 小时内不再自动重试；也可手动把新版 yt-dlp 放到 {}）",
                    managed_ytdlp_path(package).display()
                );
                return Ok(());
            }
        }
    }

    // 完全没有 yt-dlp：全新安装（失败即报错，因为无旧版可回退）。
    let binary_path = managed_ytdlp_path(package);
    download_and_install_ytdlp(package, &binary_path, None).await?;
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
    // 自定义镜像可能让 musl 编译的主程序运行在 glibc 层中；允许显式指定
    // 运行层 libc，避免下载与实际系统加载器不兼容的 yt-dlp 构建。
    let configured_libc = std::env::var("BILI_SYNC_YTDLP_RUNTIME_LIBC").ok();
    let target_env = ytdlp_runtime_target_env(configured_libc.as_deref(), cfg!(target_env = "musl"));
    ytdlp_package_for(std::env::consts::OS, std::env::consts::ARCH, target_env)
}

fn ytdlp_runtime_target_env(configured_libc: Option<&str>, compiled_with_musl: bool) -> &'static str {
    match configured_libc.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("glibc" | "gnu") => "",
        Some("musl") => "musl",
        _ if compiled_with_musl => "musl",
        _ => "",
    }
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

async fn download_and_install_ytdlp(
    package: YtDlpPackage,
    binary_path: &Path,
    old_version: Option<&str>,
) -> Result<()> {
    let parent = binary_path
        .parent()
        .ok_or_else(|| anyhow!("yt-dlp 安装路径无效：{}", binary_path.display()))?;
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("创建 yt-dlp 安装目录失败：{}", parent.display()))?;

    let asset_url = format!("{YTDLP_RELEASE_BASE_URL}/{}", package.asset_name);
    let checksums_url = format!("{YTDLP_RELEASE_BASE_URL}/SHA2-256SUMS");
    match old_version {
        Some(version) => info!(
            version,
            url = asset_url,
            "yt-dlp 版本过旧，自动下载最新版替换"
        ),
        None => info!(
            target = package.target_key,
            asset = package.asset_name,
            url = asset_url,
            "本机未检测到 yt-dlp，开始下载对应系统版本"
        ),
    }

    // 外源代理：配置了代理时优先走代理下载（部分网络下 GitHub 仅代理可达），
    // 代理失败再回退直连；未配置代理则只走直连。
    let configured_proxy = configured_external_proxy();
    let proxy = if configured_proxy.trim().is_empty() {
        None
    } else {
        Some(configured_proxy.trim().to_string())
    };
    let build_client = |use_proxy: bool| -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(YTDLP_DOWNLOAD_TIMEOUT)
            .user_agent(concat!("bili-sync-up/", env!("CARGO_PKG_VERSION")));
        if use_proxy {
            if let Some(proxy) = proxy.as_deref() {
                builder = builder.proxy(
                    reqwest::Proxy::all(proxy)
                        .with_context(|| format!("解析 yt-dlp 下载代理失败：{proxy}"))?,
                );
            }
        }
        builder.build().context("创建 yt-dlp 下载客户端失败")
    };
    let attempts: Vec<bool> = if proxy.is_some() { vec![true, false] } else { vec![false] };
    let total_attempts = attempts.len();

    let mut last_error = None;
    for (index, use_proxy) in attempts.into_iter().enumerate() {
        let client = build_client(use_proxy)?;
        let fetched = async {
            let (asset_response, checksums_response) = tokio::try_join!(
                client.get(&asset_url).send(),
                client.get(&checksums_url).send(),
            )
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
            Ok::<_, anyhow::Error>((asset_bytes, checksums))
        }
        .await;
        let (asset_bytes, checksums) = match fetched {
            Ok(value) => value,
            Err(error) => {
                last_error = Some(error);
                if index + 1 < total_attempts {
                    warn!(
                        "yt-dlp {}下载失败，回退直连重试",
                        if use_proxy { "代理" } else { "直连" }
                    );
                }
                continue;
            }
        };

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
            .with_context(|| format!("安装 yt-dlp 失败：{}", binary_path.display()))?;
        return Ok(());
    }

    Err(last_error.unwrap_or_else(|| anyhow!("yt-dlp 下载失败")))
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

/// YouTube 凭证当前内容：优先数据库，回退旧版 cookies.txt 文件（启动迁移会搬进数据库）。
fn youtube_cookie_text() -> Option<String> {
    if let Some(value) = crate::credential_store::get(crate::credential_store::keys::YOUTUBE_COOKIES) {
        return Some(value);
    }
    let legacy = CONFIG_DIR.join("youtube-cookies.txt");
    std::fs::read_to_string(legacy).ok()
}

/// 返回可直接传给 yt-dlp `--cookies` 的路径（数据库凭证的影子文件或旧版文件）。
fn youtube_cookie_file() -> Option<PathBuf> {
    let contents = youtube_cookie_text()?;
    if contents.trim().is_empty() {
        return None;
    }
    match crate::credential_store::sync_shadow(crate::credential_store::keys::YOUTUBE_COOKIES, &contents) {
        Ok(path) => Some(path),
        Err(error) => {
            warn!(error = %error, "写入 YouTube 影子 Cookie 文件失败");
            None
        }
    }
}

/// 把 YouTube 凭证写入数据库（并同步影子文件）。
async fn set_youtube_cookies(contents: &str) -> Result<()> {
    crate::credential_store::set(crate::credential_store::keys::YOUTUBE_COOKIES, contents).await
}

/// 清理 YouTube 登录状态：数据库凭证 + 旧版/历史残留文件。
async fn clear_youtube_login_state() {
    if crate::credential_store::has(crate::credential_store::keys::YOUTUBE_COOKIES) {
        if let Err(error) = crate::credential_store::delete(crate::credential_store::keys::YOUTUBE_COOKIES).await {
            warn!(error = %error, "清理数据库中的 YouTube 登录凭证失败");
        }
    }
    clear_youtube_login_state_files_except(None).await;
}

/// 是否已导入有效的 YouTube 登录会话（数据库优先，兼容旧文件）。
fn youtube_has_session() -> bool {
    youtube_cookie_text().is_some_and(|contents| has_youtube_session_value(&contents))
}

fn has_youtube_session_value(contents: &str) -> bool {
    contents.lines().any(|line| {
        if line.trim_start().starts_with('#') {
            return false;
        }
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() < 7 || !is_youtube_auth_cookie_domain(columns[0]) {
            return false;
        }
        matches!(
            columns[5],
            "SID" | "SAPISID" | "APISID" | "__Secure-1PSID" | "__Secure-3PSID"
        )
    })
}

/// 清理旧版或历史导入残留的 YouTube 登录状态文件（主 Cookie 及其备份/临时快照）。
/// `exclude` 用于跳过当前正在写入的临时文件。
async fn clear_youtube_login_state_files_except(exclude: Option<&Path>) {
    let mut removed = 0usize;
    let Ok(mut entries) = tokio::fs::read_dir(&*CONFIG_DIR).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !entry.file_type().await.map(|kind| kind.is_file()).unwrap_or(false) {
            continue;
        }
        if exclude.is_some_and(|excluded| entry.path() == excluded) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "youtube-cookies.txt" || name.starts_with("youtube-cookies.txt.") {
            if let Err(error) = tokio::fs::remove_file(entry.path()).await {
                warn!(path = %entry.path().display(), error = %error, "清理旧 YouTube 登录状态文件失败");
            } else {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        info!(removed, "已清理旧版 YouTube 登录状态文件，重新导入新会话");
    }
}

/// 外源（YouTube/抖音）登录状态守护任务的启动延迟与检查间隔。
const EXTERNAL_LOGIN_GUARD_STARTUP_DELAY: Duration = Duration::from_secs(5 * 60);
const EXTERNAL_LOGIN_GUARD_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// 外源登录状态守护调度器。
///
/// 定期探测 YouTube/抖音登录状态：有效则顺带完成一次浏览器式会话续约；明确
/// 过期则清理残留登录状态文件并发送通知，避免“看似已登录、实际已失效”导致
/// 扫描持续报错。风控/网络原因导致的探测失败不会清理会话。
pub async fn external_login_guard_scheduler() {
    info!(
        "外源登录状态守护任务已启动：{} 分钟后首次检查，之后每 {} 小时检查并续约一次",
        EXTERNAL_LOGIN_GUARD_STARTUP_DELAY.as_secs() / 60,
        EXTERNAL_LOGIN_GUARD_INTERVAL.as_secs() / 3600
    );
    tokio::time::sleep(EXTERNAL_LOGIN_GUARD_STARTUP_DELAY).await;
    loop {
        let started = std::time::Instant::now();
        if let Err(error) = guard_external_login_states().await {
            warn!(error = %error, "外源登录状态守护检查失败");
        }
        let elapsed = started.elapsed();
        if elapsed < EXTERNAL_LOGIN_GUARD_INTERVAL {
            tokio::time::sleep(EXTERNAL_LOGIN_GUARD_INTERVAL - elapsed).await;
        }
    }
}

async fn guard_external_login_states() -> Result<()> {
    // 抖音：探测即续约（signed_get 内部会刷新 msToken/webid/verifyFp 并合并 Set-Cookie）。
    match crate::douyin::probe_douyin_login().await {
        crate::douyin::DouyinLoginProbe::Valid => {
            info!(target: "bili_sync_rs::douyin", "抖音登录状态有效，已完成会话续约");
        }
        crate::douyin::DouyinLoginProbe::Expired => {
            crate::douyin::clear_douyin_login_state().await;
            record_external_login_expired(
                "抖音",
                "抖音登录状态已过期或未登录，已清理残留会话；请重新在设置页导入电脑浏览器导出的 douyin.com cookies.txt",
            )
            .await;
        }
        crate::douyin::DouyinLoginProbe::Unclear => {
            debug!(target: "bili_sync_rs::douyin", "抖音登录状态检查结果不确定（网络或风控），保留现有会话");
        }
        crate::douyin::DouyinLoginProbe::NotConfigured => {
            debug!(target: "bili_sync_rs::douyin", "尚未导入抖音登录状态，跳过守护检查");
        }
    }

    // YouTube：订阅频道页面探测即续约。
    if youtube_has_session() {
        match load_youtube_subscription_channels().await {
            Ok(_) => info!("YouTube 登录状态有效，已完成会话续约"),
            Err(error) if format!("{:#}", error).contains("Cookie 已失效") => {
                clear_youtube_login_state().await;
                record_external_login_expired(
                    "YouTube",
                    "YouTube 登录状态已过期或未登录，已清理残留会话；请重新在设置页导入 cookies.txt",
                )
                .await;
            }
            Err(error) => {
                // 代理未开/上游抖动等网络原因：会话无法续约但保留现状。从 debug
                // 提升为 warn，避免用户看到“会话一直失效”却不知道是代理/网络导致
                // 续约根本没发生。
                warn!(
                    error = %error,
                    "YouTube 登录状态检查失败（代理/网络不可用），本次未能续约会话，保留现有会话。请确认外源代理（proxy/youtube_proxy）可用"
                );
            }
        }
    }
    Ok(())
}

async fn record_external_login_expired(platform: &str, message: &str) {
    info!(platform, "{}登录状态过期，已清理残留会话并提示重新导入", platform);
    if let Err(error) = crate::utils::notification::send_error_notification(
        &format!("{platform}登录状态过期"),
        message,
        None,
    )
    .await
    {
        warn!(platform, error = %error, "发送外源登录过期通知失败");
    }
}
fn default_output_path() -> PathBuf {
    CONFIG_DIR.join("youtube-downloads")
}
async fn remove_recorded_output(source: &youtube_source::Model, output_path: &str) -> Result<()> {
    let base = PathBuf::from(&source.path);
    let output = PathBuf::from(output_path);
    if !output.starts_with(&base) {
        if is_douyin_source(source) {
            warn!(target: "bili_sync_rs::douyin", path = %output.display(), "跳过不在抖音来源目录中的记录文件");
        } else {
            warn!(target: "bili_sync_rs::youtube", path = %output.display(), "跳过不在 YouTube 来源目录中的记录文件");
        }
        return Ok(());
    }
    for path in recorded_output_files(&output).await? {
        match tokio::fs::remove_file(&path).await {
            Ok(()) if is_douyin_source(source) => {
                info!(target: "bili_sync_rs::douyin", path = %path.display(), "已删除抖音已记录媒体/附属文件")
            }
            Ok(()) => {
                info!(target: "bili_sync_rs::youtube", path = %path.display(), "已删除 YouTube 已记录媒体/附属文件")
            }
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

async fn remove_empty_parent_directories(mut current: Option<&Path>, stop_at: &Path) -> usize {
    let mut removed = 0usize;
    while let Some(directory) = current {
        if !directory.starts_with(stop_at) {
            break;
        }
        match tokio::fs::remove_dir(directory).await {
            Ok(()) => {
                removed += 1;
                // 旧根目录移空后也一并删除（空目录删除成功，非空则失败退出）
                if directory == stop_at {
                    break;
                }
                current = directory.parent();
            }
            Err(_) => break,
        }
    }
    removed
}

async fn validate_youtube_login_cookie() -> Result<()> {
    // 不再仅依据 yt-dlp 输出中的 `Found YouTube account cookies` 判断登录。
    // 该提示只说明文件里存在账号 Cookie 名称，过期或不完整的会话也可能出现。
    // 真实访问账号专属的“已订阅频道”页面，才能确认 Cookie 当前确实可用。
    tokio::time::timeout(LOGIN_TIMEOUT, load_youtube_subscription_channels())
        .await
        .map_err(|_| anyhow!("验证 YouTube 账号登录状态超时"))??;
    Ok(())
}

async fn prepare_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("无效的 Cookie 文件路径"))?;
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("无法创建配置目录: {}", parent.display()))
}
fn append_cookies(command: &mut Command) {
    if let Some(path) = youtube_cookie_file() {
        command.arg("--cookies").arg(path);
    }
}

fn append_cookies_for_url(command: &mut Command, url: &str) {
    if url.contains("douyin.com") {
        crate::douyin::append_cookies(command);
    } else if url.contains("tiktok.com") {
        // TikTok 公开内容无需登录；仅在导入过 TikTok cookies 时使用，避免误用 YouTube/抖音 Cookie。
        crate::tiktok::append_tiktok_cookies(command);
    } else {
        append_cookies(command);
    }
}

/// 外源网络代理：YouTube/TikTok 等平台共用的 yt-dlp 与直链下载代理。
/// 新配置写入 `proxy`；`youtube_proxy` 保留为旧配置兼容回退。
pub(crate) fn configured_external_proxy() -> String {
    crate::config::with_config(|bundle| {
        let proxy = bundle.config.proxy.trim();
        if !proxy.is_empty() {
            proxy.to_string()
        } else {
            bundle.config.youtube_proxy.trim().to_string()
        }
    })
}

pub(crate) fn append_youtube_proxy(command: &mut Command) {
    let proxy = configured_external_proxy();
    if !proxy.is_empty() {
        command.arg("--proxy").arg(proxy);
    }
}

fn append_youtube_proxy_for_url(command: &mut Command, url: &str) {
    if should_proxy_ytdlp_url(url) {
        append_youtube_proxy(command);
    }
}

fn should_proxy_ytdlp_url(url: &str) -> bool {
    is_youtube_url(url) || crate::tiktok::is_tiktok_url(url)
}


/// 频道/播放列表 tab 扫描时跳过 yt-dlp 的 authcheck。
///
/// yt-dlp 新版在“网页下载不成功”（例如 YouTube 返回登录墙/验证页）时会误判
/// “播放列表需要登录”并拒绝提取公开内容；对公开频道/播放列表跳过该检查是
/// 官方推荐做法（--extractor-args youtubetab:skip=authcheck）。cookies 仍会
/// 正常携带，私有内容在有 cookies 时依旧可以提取。
fn append_ytdlp_tab_args(command: &mut Command) {
    command.args(["--extractor-args", "youtubetab:skip=authcheck"]);
}

/// 创建 yt-dlp 子进程命令。
///
/// 顺手尝试让 Python 以 UTF-8 输出错误消息；打包版 yt-dlp（PyInstaller）可能
/// 忽略该环境变量，因此 `command_error` 里还有 GBK 回退解码兜底，保证 Windows
/// 本地化错误文本（如“远程主机强迫关闭了一个现有的连接”）不乱码。
pub(crate) fn ytdlp_command() -> Command {
    let mut command = Command::new(ytdlp_executable());
    command.env("PYTHONUTF8", "1");
    command.env("PYTHONIOENCODING", "utf-8");
    command
}

/// yt-dlp 扫描失败是否为瞬时网络错误（连接被重置/中断/超时）。
///
/// 这类错误通常是 YouTube 对突发请求做的瞬时限流或线路抖动，退避后重试
/// 一次即可恢复，不应视为来源配置或登录状态问题。
fn is_youtube_transient_error(error: &anyhow::Error) -> bool {
    let text = format!("{:#}", error).to_ascii_lowercase();
    [
        "connection aborted",
        "connectionreseterror",
        "connection reset",
        "remote end closed connection",
        "broken pipe",
        "connectionerror",
        "transporterror",
        "read timed out",
        "timed out",
        "10054",
        "10053",
        "10060",
        "temporary failure in name resolution",
        "name or service not known",
        "could not connect",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub(crate) fn append_ytdlp_runtime(command: &mut Command) {
    if let Some((name, path)) = ytdlp_js_runtime() {
        command.arg("--js-runtimes").arg(format!("{name}:{}", path.display()));
    }
}

fn ytdlp_js_runtime() -> Option<(&'static str, PathBuf)> {
    if let Ok(configured) = std::env::var("BILI_SYNC_YTDLP_JS_RUNTIME") {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Some((ytdlp_js_runtime_name(&path), path));
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
fn ytdlp_js_runtime_name(path: &Path) -> &'static str {
    match path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("bun") => "bun",
        Some("deno") => "deno",
        Some("qjs" | "quickjs") => "quickjs",
        _ => "node",
    }
}
fn is_container_runtime() -> bool {
    std::env::var("BILI_SYNC_CONTAINER")
        .ok()
        .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        || Path::new("/.dockerenv").exists()
}

async fn replace_cookie_file(temporary: &Path, target: &Path) -> Result<()> {
    let backup = target.with_extension("txt.backup");
    let had_target = tokio::fs::try_exists(target).await?;
    if tokio::fs::try_exists(&backup).await? {
        tokio::fs::remove_file(&backup).await?;
    }
    if had_target {
        tokio::fs::rename(target, &backup)
            .await
            .context("备份旧 YouTube Cookie 失败")?;
    }
    match tokio::fs::rename(temporary, target).await {
        Ok(()) => {
            if had_target {
                let _ = tokio::fs::remove_file(&backup).await;
            }
            Ok(())
        }
        Err(error) => {
            if had_target {
                let _ = tokio::fs::rename(&backup, target).await;
            }
            Err(error).context("保存 YouTube Cookie 失败")
        }
    }
}

fn has_youtube_session(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|contents| has_youtube_session_value(&contents))
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
                && is_youtube_auth_cookie_domain(columns[0])
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
pub(crate) fn command_error(output: &std::process::Output) -> String {
    let stderr = decode_command_bytes(&output.stderr);
    let stdout = decode_command_bytes(&output.stdout);
    trim_output(if stderr.trim().is_empty() { &stdout } else { &stderr })
}

/// 把子进程输出字节解码为可读文本。
///
/// 优先按 UTF-8 解码；失败时按 GBK（Windows 简体中文系统代码页）解码。
/// Windows 上 yt-dlp 会把 socket 错误文本（如“远程主机强迫关闭了一个
/// 现有的连接”）按本地代码页编码输出，直接用 from_utf8_lossy 会变成乱码。
fn decode_command_bytes(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let (text, _, _) = encoding_rs::GBK.decode(bytes);
    text.into_owned()
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
    #[test]
    fn decodes_gbk_command_error_text() {
        // GBK 编码的“远程主机强迫关闭了一个现有的连接”（Windows 10054 错误文本）
        let gbk: &[u8] = &[
            0xd4, 0xb6, 0xb3, 0xcc, 0xd6, 0xf7, 0xbb, 0xfa, 0xc7, 0xbf, 0xc6, 0xc8,
            0xb9, 0xd8, 0xb1, 0xd5, 0xc1, 0xcb, 0xd2, 0xbb, 0xb8, 0xf6, 0xcf, 0xd6,
            0xd3, 0xd0, 0xb5, 0xc4, 0xc1, 0xac, 0xbd, 0xd3,
        ];
        assert!(std::str::from_utf8(gbk).is_err());
        let decoded = super::decode_command_bytes(gbk);
        assert!(decoded.contains("远程主机"), "decoded: {decoded}");
        assert!(decoded.contains("连接"), "decoded: {decoded}");
    }

    #[test]
    fn startup_skips_recently_scanned_sources() {
        let now = chrono::Local::now().naive_local();
        let recent = now.format("%Y-%m-%d %H:%M:%S").to_string();
        assert!(source_scanned_recently(Some(&recent), 1200));
        let old = (now - chrono::Duration::hours(2)).format("%Y-%m-%d %H:%M:%S").to_string();
        assert!(!source_scanned_recently(Some(&old), 1200));
        assert!(!source_scanned_recently(None, 1200));
        assert!(!source_scanned_recently(Some("not-a-date"), 1200));
    }

    use super::{
        canonical_channel_url, checksum_for_release_asset, collect_youtube_channel_renderers, current_ytdlp_package,
        extract_youtube_initial_data, generate_youtube_person_nfo, is_netscape_youtube_cookie_file, is_youtube_url,
        normalize_source_type, parse_youtube_live_chat, resolve_source_url, should_proxy_ytdlp_url, source_scanned_recently, youtube_cookie_jar,
        youtube_page_is_logged_out, youtube_search_url, ytdlp_js_runtime_name, ytdlp_package_for,
        ytdlp_runtime_target_env, SUBSCRIPTIONS_URL,
    };
    use std::collections::HashSet;
    use std::path::Path;

    fn sample_external_source(source_type: &str) -> super::youtube_source::Model {
        super::youtube_source::Model {
            id: 1,
            source_type: source_type.to_string(),
            name: "测试源".to_string(),
            url: "https://www.douyin.com/video/123".to_string(),
            path: "Z:/__not_exists__".to_string(),
            enabled: true,
            audio_only: false,
            audio_only_m4a_only: false,
            flat_folder: false,
            download_danmaku: false,
            download_subtitle: false,
            ai_subtitle_language: String::new(),
            ai_rename: false,
            ai_rename_video_prompt: String::new(),
            ai_rename_audio_prompt: String::new(),
            ai_rename_enable_multi_page: false,
            ai_rename_enable_collection: false,
            ai_rename_enable_bangumi: false,
            ai_rename_rename_parent_dir: false,
            filter_option: None,
            blacklist_keywords: None,
            whitelist_keywords: None,
            keyword_case_sensitive: false,
            min_duration_seconds: None,
            max_duration_seconds: None,
            published_after: None,
            published_before: None,
            selected_videos: None,
            selected_channels: None,
            known_video_ids: None,
            scan_deleted_videos: false,
            scan_deleted_videos_once: false,
            deleted_video_ids: None,
            last_scan_at: None,
            created_at: "2026-01-01 00:00:00".to_string(),
        }
    }

    #[tokio::test]
    async fn completed_external_video_stays_completed_when_file_moved() {
        // 已下载完成的外源视频：即使媒体文件被移走（output_path 不存在），
        // 卡片状态也应保持“全部完成”，与 B 站行为一致，而不是变成“进行中”。
        let video = super::youtube_video::Model {
            id: 1,
            source_id: 1,
            youtube_id: "abc123".to_string(),
            url: "https://www.douyin.com/video/123".to_string(),
            title: "测试视频".to_string(),
            uploader: "测试UP".to_string(),
            thumbnail: None,
            published_at: Some("20260101".to_string()),
            duration_seconds: Some(60),
            episode_number: None,
            is_image_post: false,
            is_story: false,
            is_charge_video: false,
            charge_can_play: false,
            download_status: "completed".to_string(),
            retry_count: 0,
            output_path: Some("Z:/__not_exists__/abc123.mp4".to_string()),
            error_message: None,
            created_at: "2026-01-01 00:00:00".to_string(),
            updated_at: "2026-01-01 00:00:00".to_string(),
        };
        let source = sample_external_source("douyin");
        let (video_status, page_status) = super::youtube_artifact_status(&video, &source).await;
        assert_eq!(video_status, [7, 7, 7, 7, 7], "已完成的视频文件被移走后应保持全部完成");
        assert_eq!(page_status, [7, 7, 7, 7, 7], "分页状态同样应保持全部完成");
    }

    #[test]
    fn episodic_douyin_source_is_detected() {
        assert!(super::is_episodic_douyin_source(&sample_external_source("douyin_theater")));
        assert!(super::is_episodic_douyin_source(&sample_external_source("douyin_series")));
        assert!(super::is_episodic_douyin_source(&sample_external_source("douyin_collection")));
        assert!(!super::is_episodic_douyin_source(&sample_external_source("douyin")));
    }
    #[test]
    fn validates_types_and_urls() {
        assert_eq!(normalize_source_type("playlist").unwrap(), "playlist");
        assert_eq!(resolve_source_url("subscriptions", None).unwrap(), SUBSCRIPTIONS_URL);
        assert!(is_youtube_url("https://www.youtube.com/watch?v=abc"));
        assert!(!is_youtube_url("https://youtube.example.com"));
    }

    #[test]
    fn youtube_stream_selection_falls_back_to_vp9_for_highest_quality() {
        use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};
        use super::ExternalMediaFormat;

        let filter = FilterOption {
            video_max_quality: VideoQuality::Quality8k,
            video_min_quality: VideoQuality::Quality720p,
            audio_max_quality: AudioQuality::QualityHiRES,
            audio_min_quality: AudioQuality::Quality64k,
            codecs: vec![VideoCodecs::AV1, VideoCodecs::AVC, VideoCodecs::HEV],
            ..Default::default()
        };
        let vfmt = |id: &str, h: i32, vcodec: &str| ExternalMediaFormat {
            format_id: Some(id.to_string()),
            url: Some(format!("https://example.com/{id}")),
            protocol: Some("https".to_string()),
            ext: Some("webm".to_string()),
            vcodec: Some(vcodec.to_string()),
            acodec: Some("none".to_string()),
            width: Some(if h > 0 { h * 16 / 9 } else { 0 }),
            height: Some(h),
            fps: Some(30.0),
            tbr: Some(5000.0),
            vbr: Some(4000.0),
            abr: None,
            dynamic_range: Some("SDR".to_string()),
            decryption_key: None,
            fallback_urls: Vec::new(),
        };
        let afmt = |id: &str, abr: i32| ExternalMediaFormat {
            format_id: Some(id.to_string()),
            url: Some(format!("https://example.com/{id}")),
            protocol: Some("https".to_string()),
            ext: Some("m4a".to_string()),
            vcodec: Some("none".to_string()),
            acodec: Some("mp4a.40.2".to_string()),
            width: None,
            height: None,
            fps: None,
            tbr: Some(abr as f64),
            vbr: None,
            abr: Some(abr as f64),
            dynamic_range: Some("SDR".to_string()),
            decryption_key: None,
            fallback_urls: Vec::new(),
        };
        // 4K 只有 VP9（无 AV1/HEVC 4K），偏好编码最高只有 1440p AV1 / 1080p AVC
        let formats = vec![
            vfmt("401", 2160, "vp9.2"),
            vfmt("400", 2160, "vp9"),
            vfmt("399", 1440, "vp9"),
            vfmt("308", 1440, "av01.0.05M.08"),
            vfmt("303", 1080, "vp9"),
            vfmt("136", 1080, "avc1.64001f"),
            afmt("140", 128),
        ];
        let selected = super::select_youtube_streams(&formats, &filter, false, "YouTube").expect("选择应成功");
        let v = selected.video.expect("应选出视频流");
        assert_eq!(v.height, Some(2160), "应自动放行 VP9 4K: vcodec={:?}", v.vcodec);
        assert!(v.vcodec.as_deref().unwrap_or_default().starts_with("vp9"));
        assert!(selected.audio.is_some(), "音频流应正常选择");
    }

    #[test]
    fn youtube_stream_selection_prefers_allowed_codec_at_same_height() {
        use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};
        use super::ExternalMediaFormat;

        let filter = FilterOption {
            video_max_quality: VideoQuality::Quality8k,
            video_min_quality: VideoQuality::Quality720p,
            audio_max_quality: AudioQuality::QualityHiRES,
            audio_min_quality: AudioQuality::Quality64k,
            codecs: vec![VideoCodecs::AV1, VideoCodecs::AVC, VideoCodecs::HEV],
            ..Default::default()
        };
        let vfmt = |id: &str, h: i32, vcodec: &str| ExternalMediaFormat {
            format_id: Some(id.to_string()),
            url: Some(format!("https://example.com/{id}")),
            protocol: Some("https".to_string()),
            ext: Some("webm".to_string()),
            vcodec: Some(vcodec.to_string()),
            acodec: Some("none".to_string()),
            width: Some(h * 16 / 9),
            height: Some(h),
            fps: Some(30.0),
            tbr: Some(5000.0),
            vbr: Some(4000.0),
            abr: None,
            dynamic_range: Some("SDR".to_string()),
            decryption_key: None,
            fallback_urls: Vec::new(),
        };
        let afmt = || ExternalMediaFormat {
            format_id: Some("140".to_string()),
            url: Some("https://example.com/a".to_string()),
            protocol: Some("https".to_string()),
            ext: Some("m4a".to_string()),
            vcodec: Some("none".to_string()),
            acodec: Some("mp4a.40.2".to_string()),
            width: None,
            height: None,
            fps: None,
            tbr: Some(128.0),
            vbr: None,
            abr: Some(128.0),
            dynamic_range: Some("SDR".to_string()),
            decryption_key: None,
            fallback_urls: Vec::new(),
        };
        // 4K 同时有 AV1 与 VP9：同分辨率应保持用户偏好编码 AV1，而非 VP9
        let formats = vec![
            vfmt("401", 2160, "vp9.2"),
            vfmt("400", 2160, "vp9"),
            vfmt("400-av1", 2160, "av01.0.08M.08"),
            vfmt("136", 1080, "avc1.64001f"),
            afmt(),
        ];
        let selected = super::select_youtube_streams(&formats, &filter, false, "YouTube").expect("选择应成功");
        let v = selected.video.expect("应选出视频流");
        assert_eq!(v.height, Some(2160));
        assert!(
            v.vcodec.as_deref().unwrap_or_default().starts_with("av01"),
            "同分辨率应优先偏好编码 AV1，实际: {:?}",
            v.vcodec
        );
    }

    #[test]
    fn youtube_stream_selection_vp9_fallback_respects_no_hdr_and_max_height() {
        use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};
        use super::ExternalMediaFormat;

        let vfmt = |id: &str, h: i32, vcodec: &str, dr: &str| ExternalMediaFormat {
            format_id: Some(id.to_string()),
            url: Some(format!("https://example.com/{id}")),
            protocol: Some("https".to_string()),
            ext: Some("webm".to_string()),
            vcodec: Some(vcodec.to_string()),
            acodec: Some("none".to_string()),
            width: Some(h * 16 / 9),
            height: Some(h),
            fps: Some(30.0),
            tbr: Some(5000.0),
            vbr: Some(4000.0),
            abr: None,
            dynamic_range: Some(dr.to_string()),
            decryption_key: None,
            fallback_urls: Vec::new(),
        };
        let afmt = || ExternalMediaFormat {
            format_id: Some("140".to_string()),
            url: Some("https://example.com/a".to_string()),
            protocol: Some("https".to_string()),
            ext: Some("m4a".to_string()),
            vcodec: Some("none".to_string()),
            acodec: Some("mp4a.40.2".to_string()),
            width: None,
            height: None,
            fps: None,
            tbr: Some(128.0),
            vbr: None,
            abr: Some(128.0),
            dynamic_range: Some("SDR".to_string()),
            decryption_key: None,
            fallback_urls: Vec::new(),
        };

        // 开启 no_hdr：4K 仅有 HDR VP9 时不应放行，应选 SDR 1080p AVC
        let filter = FilterOption {
            video_max_quality: VideoQuality::Quality8k,
            video_min_quality: VideoQuality::Quality720p,
            audio_max_quality: AudioQuality::QualityHiRES,
            audio_min_quality: AudioQuality::Quality64k,
            codecs: vec![VideoCodecs::AV1, VideoCodecs::AVC, VideoCodecs::HEV],
            no_hdr: true,
            ..Default::default()
        };
        let formats = vec![
            vfmt("401", 2160, "vp9.2", "HDR"),
            vfmt("136", 1080, "avc1.64001f", "SDR"),
            afmt(),
        ];
        let selected = super::select_youtube_streams(&formats, &filter, false, "YouTube").expect("选择应成功");
        let v = selected.video.expect("应选出视频流");
        assert_eq!(v.height, Some(1080), "no_hdr 时不应放行 HDR VP9");

        // 8K VP9 超出现有偏好编码高度时，应升级到 8K
        let filter = FilterOption {
            video_max_quality: VideoQuality::Quality8k,
            video_min_quality: VideoQuality::Quality720p,
            audio_max_quality: AudioQuality::QualityHiRES,
            audio_min_quality: AudioQuality::Quality64k,
            codecs: vec![VideoCodecs::AV1, VideoCodecs::AVC, VideoCodecs::HEV],
            ..Default::default()
        };
        let formats = vec![
            vfmt("402", 4320, "vp9.2", "SDR"),
            vfmt("401", 2160, "av01.0.08M.08", "SDR"),
            afmt(),
        ];
        let selected = super::select_youtube_streams(&formats, &filter, false, "YouTube").expect("选择应成功");
        let v = selected.video.expect("应选出视频流");
        assert_eq!(v.height, Some(4320), "8K VP9 应作为最高画质放行");
    }

    #[test]
    fn youtube_movie_nfo_studio_uses_platform_not_date() {
        // 回归测试：<aired> 填日期、<studio> 填平台名，二者不能写反。
        let aired = chrono::NaiveDate::from_ymd_opt(2026, 9, 1)
            .expect("有效日期")
            .and_hms_opt(0, 0, 0)
            .expect("有效时间");
        let xml = super::build_youtube_movie_nfo_xml(
            "测试视频",
            "简介",
            "YouTube",
            "YouTube",
            "dQw4w9WgXcQ",
            aired,
            "测试频道",
            &[],
            "https://example.com/thumb.jpg",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        );
        assert!(xml.contains("<studio>YouTube</studio>"), "studio 应为平台名: {xml}");
        assert!(xml.contains("<aired>2026-09-01</aired>"), "aired 应为日期: {xml}");
        assert!(xml.contains("<premiered>2026-09-01</premiered>"), "premiered 应为日期: {xml}");
        assert!(xml.contains("<year>2026</year>"), "year 应为年份: {xml}");
        assert!(!xml.contains("<studio>2026-09-01</studio>"), "studio 不应是日期: {xml}");
        assert!(!xml.contains("<aired>YouTube</aired>"), "aired 不应是平台名: {xml}");
        assert!(xml.contains("<director>测试频道</director>"), "director 应为频道: {xml}");
    }

    #[test]
    fn youtube_movie_nfo_includes_co_creators_as_actors() {
        // 回归测试：联合投稿视频的合作频道应逐个补写为 <actor>，主频道仅出现一次。
        let aired = chrono::NaiveDate::from_ymd_opt(2026, 9, 1)
            .expect("有效日期")
            .and_hms_opt(0, 0, 0)
            .expect("有效时间");
        let xml = super::build_youtube_movie_nfo_xml(
            "联合投稿测试",
            "简介",
            "YouTube",
            "YouTube",
            "vid123",
            aired,
            "主频道",
            &["合作频道A".to_string(), "合作频道B".to_string()],
            "https://example.com/thumb.jpg",
            "https://www.youtube.com/watch?v=vid123",
        );
        assert_eq!(xml.matches("<actor>").count(), 3, "主频道+2个合作频道共3个 actor: {xml}");
        assert!(xml.contains("<actor><name>主频道</name><role>频道</role></actor>"), "{xml}");
        assert!(xml.contains("<actor><name>合作频道A</name><role>频道</role></actor>"), "{xml}");
        assert!(xml.contains("<actor><name>合作频道B</name><role>频道</role></actor>"), "{xml}");
        assert!(xml.contains("<director>主频道</director>"), "director 应为主频道: {xml}");
        assert_eq!(xml.matches("主频道").count(), 2, "主频道只在 director 与第一个 actor 出现: {xml}");
    }

    #[test]
    fn youtube_co_creators_filters_main_channel_and_deduplicates() {
        use crate::external_media::ExternalMediaMetadata;
        // 单作者视频（creators 只有主频道，或音乐视频只有表演者）不补加 actor。
        let metadata: ExternalMediaMetadata =
            serde_json::from_str(r#"{"id":"v1","creators":["主频道"]}"#).expect("metadata 应可解析");
        assert_eq!(super::youtube_co_creators(&metadata, "主频道"), Vec::<String>::new());
        let metadata: ExternalMediaMetadata =
            serde_json::from_str(r#"{"id":"v1","creators":["音乐人"]}"#).expect("metadata 应可解析");
        assert_eq!(super::youtube_co_creators(&metadata, "主频道VEVO"), Vec::<String>::new());
        // 联合投稿：保留合作频道，去掉主频道与重复项（忽略大小写与首尾空格）。
        let metadata: ExternalMediaMetadata = serde_json::from_str(
            r#"{"id":"v1","creators":["主频道","合作频道A","合作频道B","合作频道A"," 合作频道B  "]}"#,
        )
        .expect("metadata 应可解析");
        assert_eq!(
            super::youtube_co_creators(&metadata, "主频道"),
            vec!["合作频道A".to_string(), "合作频道B".to_string()]
        );
        // 未返回 creators 字段时为空。
        let metadata: ExternalMediaMetadata =
            serde_json::from_str(r#"{"id":"v1"}"#).expect("metadata 应可解析");
        assert_eq!(super::youtube_co_creators(&metadata, "主频道"), Vec::<String>::new());
    }

    #[test]
    fn youtube_regen_maps_artifact_indexes() {
        // 视频级：封面0 / 视频信息1 / UP头像2 / UP主信息3；分P下载4 属媒体。
        let regen = super::youtube_regen_from_indexes(&[0], &[]);
        assert!(regen.cover && !regen.nfo && !regen.media && !regen.unsupported);
        let regen = super::youtube_regen_from_indexes(&[1], &[2]);
        assert!(regen.nfo && !regen.cover && !regen.media && !regen.unsupported, "视频信息与分页NFO都应映射为 nfo");
        let regen = super::youtube_regen_from_indexes(&[2, 3], &[]);
        assert!(regen.upper_face && regen.upper_info);
        let regen = super::youtube_regen_from_indexes(&[4], &[1]);
        assert!(regen.media && !regen.nfo, "分P下载/视频内容属媒体，回退整体重置");
        // 弹幕/字幕目前不当作可当场重建的附属文件 → 标记 unsupported 回退原流程。
        let regen = super::youtube_regen_from_indexes(&[], &[3, 4]);
        assert!(regen.unsupported && !regen.any());
    }

    #[test]
    fn youtube_regen_from_status_updates_ignores_completed_flags() {
        use crate::api::request::{PageStatusUpdate, StatusUpdate, UpdateVideoStatusRequest};
        let request = UpdateVideoStatusRequest {
            video_updates: vec![StatusUpdate { status_index: 1, status_value: 0 }],
            page_updates: vec![],
        };
        let regen = super::youtube_regen_from_status_updates(&request);
        assert!(regen.nfo && regen.any() && !regen.media && !regen.unsupported);
        // 标记为已完成(7) 不折算成重建请求。
        let request = UpdateVideoStatusRequest {
            video_updates: vec![StatusUpdate { status_index: 1, status_value: 7 }],
            page_updates: vec![PageStatusUpdate {
                page_id: 1,
                updates: vec![StatusUpdate { status_index: 0, status_value: 0 }],
            }],
        };
        let regen = super::youtube_regen_from_status_updates(&request);
        assert!(regen.cover && !regen.nfo);
    }

    #[test]
    fn youtube_tvshow_nfo_studio_uses_platform_label_for_tiktok() {
        // TikTok 剧集 NFO：工作室应为 "TikTok"、uniqueid type 应为 "tiktok"，
        // 而不是被写成 "YouTube"。
        let source = sample_external_source("tiktok_collection");
        let metadata: crate::external_media::ExternalMediaMetadata =
            serde_json::from_str(r#"{"id":"vid1"}"#).expect("metadata 应可构造");
        let xml = super::generate_youtube_tvshow_nfo(&source, &metadata);
        assert!(xml.contains("<studio>TikTok</studio>"), "TikTok tvshow studio 应为 TikTok: {xml}");
        assert!(xml.contains(r#"<uniqueid type="tiktok""#), "TikTok tvshow uniqueid type 应为 tiktok: {xml}");
        assert!(!xml.contains("<studio>YouTube</studio>"), "TikTok tvshow studio 不应是 YouTube: {xml}");
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
    fn login_cookie_accepts_youtube_and_google_account_domains() {
        let google_only = "# Netscape HTTP Cookie File\n.google.com\tTRUE\t/\tTRUE\t0\tSID\tvalue\n";
        let youtube_session = "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\t__Secure-3PSID\tvalue\n";
        assert!(is_netscape_youtube_cookie_file(google_only));
        assert!(is_netscape_youtube_cookie_file(youtube_session));
    }

    #[test]
    fn ytdlp_proxy_is_scoped_to_youtube_urls() {
        assert!(should_proxy_ytdlp_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(should_proxy_ytdlp_url("https://youtu.be/dQw4w9WgXcQ"));
        assert!(!should_proxy_ytdlp_url("https://www.douyin.com/video/123456789"));
        assert!(!should_proxy_ytdlp_url("https://www.bilibili.com/video/BV1xx411c7mD"));
    }

    #[test]
    fn login_cookie_jar_keeps_youtube_and_google_domains_separate() {
        use reqwest::cookie::CookieStore;

        let path = std::env::temp_dir().join(format!("bili-sync-youtube-cookie-{}.txt", std::process::id()));
        std::fs::write(
            &path,
            concat!(
                "# Netscape HTTP Cookie File\n",
                ".youtube.com\tTRUE\t/\tTRUE\t0\tYT_SESSION\tyoutube-value\n",
                ".google.com\tTRUE\t/\tTRUE\t0\tGOOGLE_SESSION\tgoogle-value\n"
            ),
        )
        .unwrap();
        let jar = youtube_cookie_jar(&path).unwrap();
        let _ = std::fs::remove_file(path);
        let youtube = jar
            .cookies(&reqwest::Url::parse("https://www.youtube.com/feed/channels").unwrap())
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let google = jar
            .cookies(&reqwest::Url::parse("https://accounts.google.com/ServiceLogin").unwrap())
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(youtube.contains("YT_SESSION=youtube-value"));
        assert!(!youtube.contains("GOOGLE_SESSION"));
        assert!(google.contains("GOOGLE_SESSION=google-value"));
        assert!(!google.contains("YT_SESSION"));
    }

    #[test]
    fn parses_subscribed_channels_from_youtube_initial_data() {
        let data = serde_json::json!({
            "contents": { "items": [
                { "channelRenderer": {
                    "channelId": "UC-one",
                    "title": { "simpleText": "频道一" },
                    "thumbnail": { "thumbnails": [{ "url": "//img.example/one.jpg" }] },
                    "subscriberCountText": { "simpleText": "1万位订阅者" }
                } },
                { "gridChannelRenderer": {
                    "navigationEndpoint": { "browseEndpoint": { "browseId": "UC-two" } },
                    "title": { "runs": [{ "text": "频道二" }] },
                    "thumbnail": { "thumbnails": [{ "url": "https://img.example/two.jpg" }] }
                } }
            ] }
        });
        let html = format!("<script>var ytInitialData = {data};</script>");
        let initial_data = extract_youtube_initial_data(&html).expect("应解析 ytInitialData");
        let mut channels = Vec::new();
        let mut seen = HashSet::new();
        collect_youtube_channel_renderers(&initial_data, &mut seen, &mut channels);
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].bvid, "UC-one");
        assert_eq!(channels[0].title, "频道一");
        assert_eq!(channels[0].cover, "https://img.example/one.jpg");
        assert_eq!(channels[1].bvid, "UC-two");
        assert_eq!(channels[1].title, "频道二");
    }

    #[test]
    fn detects_logged_out_youtube_page() {
        assert!(youtube_page_is_logged_out(
            r#"{"LOGGED_IN":false,"signInUrl":"https://accounts.google.com/ServiceLogin"}"#
        ));
        assert!(!youtube_page_is_logged_out(r#"{"LOGGED_IN":true}"#));
    }

    #[test]
    fn youtube_person_nfo_matches_existing_library_shape() {
        let nfo = generate_youtube_person_nfo("A&B <频道>", "UC-A&B", "youtube");
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

    #[test]
    fn runtime_can_override_compiled_musl_with_glibc() {
        assert_eq!(ytdlp_runtime_target_env(Some("glibc"), true), "");
        assert_eq!(ytdlp_runtime_target_env(Some("GNU"), true), "");
        assert_eq!(ytdlp_runtime_target_env(Some("musl"), false), "musl");
        assert_eq!(ytdlp_runtime_target_env(None, true), "musl");
        assert_eq!(ytdlp_runtime_target_env(None, false), "");
        assert_eq!(ytdlp_runtime_target_env(Some("invalid"), true), "musl");
    }

    #[test]
    fn detects_supported_ytdlp_js_runtime_names() {
        assert_eq!(ytdlp_js_runtime_name(Path::new("/usr/bin/qjs")), "quickjs");
        assert_eq!(ytdlp_js_runtime_name(Path::new("/usr/bin/quickjs")), "quickjs");
        assert_eq!(ytdlp_js_runtime_name(Path::new("/usr/bin/deno")), "deno");
        assert_eq!(ytdlp_js_runtime_name(Path::new("/usr/bin/bun")), "bun");
        assert_eq!(ytdlp_js_runtime_name(Path::new("/usr/bin/node")), "node");
    }

    #[test]
    fn converts_ytdlp_live_chat_json_lines_to_timed_danmaku() {
        let input = r#"{"replayChatItemAction":{"videoOffsetTimeMsec":"1234","actions":[{"addChatItemAction":{"item":{"liveChatTextMessageRenderer":{"id":"chat-1","timestampUsec":"1700000000000000","authorExternalChannelId":"UC1","message":{"runs":[{"text":"hello"},{"emoji":{"shortcuts":[":smile:"]}}]}}}}}]}}"#;
        let elems = parse_youtube_live_chat(input);
        assert_eq!(elems.len(), 1);
        assert_eq!(elems[0].progress, 1234);
        assert_eq!(elems[0].content, "hello:smile:");
        assert_eq!(elems[0].dmid_str, "chat-1");
        assert_eq!(elems[0].ctime, 1_700_000_000);
    }

    #[test]
    fn builds_episodic_douyin_output_path_like_bilibili_collection() {
        use bili_sync_entity::{youtube_source, youtube_video};
        use crate::external_media::ExternalMediaMetadata;
        use std::collections::HashMap;

        let mut source = youtube_source::Model::default();
        source.id = 42;
        source.source_type = "douyin_series".to_string();
        source.name = "测试短剧/带斜杠".to_string();
        source.path = "F:/Downloads/测试".to_string();
        let video = youtube_video::Model {
            episode_number: Some(3),
            ..Default::default()
        };
        let metadata = ExternalMediaMetadata {
            id: "1234567890".to_string(),
            title: Some("第三集".to_string()),
            uploader: Some("作者".to_string()),
            uploader_url: None,
            channel: None,
            channel_id: None,
            channel_url: None,
            thumbnail: None,
            description: None,
            language: None,
            upload_date: None,
            duration: None,
            formats: Vec::new(),
            subtitles: HashMap::new(),
            automatic_captions: HashMap::new(),
            images: Vec::new(),
            music_urls: Vec::new(),
            creators: None,
        };
        let path = super::youtube_output_path(&source, &video, &metadata, "第三集标题", "作者").unwrap();
        let expected = Path::new("F:/Downloads/测试")
            .join("测试短剧_带斜杠")
            .join("Season 01")
            .join("S01E03.mp4");
        assert_eq!(path, expected, "短剧应输出 下载根/剧集名/Season 01/S01E03.mp4");
    }

}

/// 迁移旧版 YouTube 凭证文件到数据库（升级兼容；成功后删除旧文件）。
pub(crate) async fn migrate_legacy_youtube_credentials() -> Result<()> {
    let _ = crate::credential_store::migrate_file_to_db(
        crate::credential_store::keys::YOUTUBE_COOKIES,
        &CONFIG_DIR.join("youtube-cookies.txt"),
        is_netscape_youtube_cookie_file,
    )
    .await?;
    Ok(())
}
