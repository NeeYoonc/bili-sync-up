//! YouTube 作为持续扫描的视频源接入。
//!
//! `yt-dlp` 仅处理 YouTube 的播放列表枚举和媒体直链解析；调度周期、暂停控制、
//! 并发上限、来源持久化及下载状态均由 bili-sync 管理。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
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
use crate::utils::notification::{NewVideoInfo, SourceScanResult};
use crate::utils::status::{PageStatus, VideoStatus, STATUS_OK};
use crate::utils::time_format::now_standard_string;

const YTDLP_VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const YTDLP_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const YTDLP_RELEASE_BASE_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
/// 镜像目录环境变量：非空时替换 yt-dlp 官方发布基址（需同时提供同名资产与 SHA2-256SUMS）。
const YTDLP_MIRROR_BASE_ENV: &str = "BILI_SYNC_YTDLP_MIRROR_BASE";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(90);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// 单次外源视频下载尝试的硬上限。
///
/// 下载器本身有首字节/空闲超时，但「取平台元数据 → 签名直链 → 下载 → 解密/合并」
/// 是一条长链路，任一环节偶发卡住都会让任务永远停在「下载中」：首页进度不会消失、
/// 队列被堵死、重启后数据库里还留着 `downloading` 状态，只能手动强制重置。
/// 到点直接放弃本次尝试，走正常的失败/重试逻辑。
const EXTERNAL_MEDIA_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(45 * 60);
const MAX_DOWNLOAD_RETRIES: i32 = 4;
/// TikTok「只回元数据、媒体字段全空」这类失败连续重试到此轮数后，才落「无法下载视频」
/// 占位并停止自动重试。保留前几轮正常重试，是为了给用户换出口节点留出恢复机会。
const TIKTOK_MEDIA_MISSING_PLACEHOLDER_RETRY: i32 = 2;
const SUBSCRIPTIONS_URL: &str = "https://www.youtube.com/feed/subscriptions";
const SUBSCRIPTION_CHANNELS_URL: &str = "https://www.youtube.com/feed/channels";
const MY_PLAYLISTS_URL: &str = "https://www.youtube.com/feed/playlists";
const LIKED_URL: &str = "https://www.youtube.com/playlist?list=LL";
const WATCH_LATER_URL: &str = "https://www.youtube.com/playlist?list=WL";
const PLAYLIST_CATALOG_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const SOURCE_VIDEO_PREVIEW_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// YouTube 频道扫描的硬超时。
///
/// 超大频道（例如 1.6 万条：videos 2,283 + live 4,616 + shorts 9,392）在代理下
/// 用 yt-dlp 完整枚举约需 9 分钟，此前写死的 5 分钟会让这类频道每轮都超时、
/// 永远同步不到视频。这里放宽到 20 分钟；已扫描过的频道还会走增量枚举
/// （见 [`YOUTUBE_INCREMENTAL_PLAYLIST_END`]），通常几秒即可完成。
const YOUTUBE_SCAN_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// 增量扫描时每个标签页最多枚举的条数（videos/live/shorts 各自生效）。
const YOUTUBE_INCREMENTAL_PLAYLIST_END: u32 = 500;

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
    /// 图文作品是否额外合成幻灯片 MP4；关闭时只保留原图与配乐。
    #[serde(default)]
    pub generate_image_post_video: Option<bool>,
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
    /// 图文作品是否额外合成幻灯片 MP4。
    pub generate_image_post_video: Option<bool>,
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
    pub generate_image_post_video: bool,
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
pub struct YouTubeSessionCheckResponse {
    /// `valid` / `expired` / `unclear`
    pub state: String,
    pub message: String,
    pub checked_at: String,
}

#[derive(Debug, Serialize)]
pub struct YouTubeStatusResponse {
    pub ytdlp_available: bool,
    pub ytdlp_version: Option<String>,
    pub logged_in: bool,
    /// 最近一次联网登录探测的结论（由守护任务刷新）。`logged_in` 只表示「导入过
    /// 凭证」，联网结论单独展示，避免设置页把代理/风控导致的误判当成「Cookie 丢了」。
    pub session_check: Option<YouTubeSessionCheckResponse>,
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
    // 状态接口只读本地凭证，不做联网探测：
    // 1) 每打开一次设置页/添加源页就发一次 /feed/channels，还会把该响应的 Set-Cookie
    //    写回 cookies.txt，等于让「看一眼状态」变成一次静默的凭证写入；
    // 2) 代理慢的时候 15s 超时会让状态显示成「未导入」，看起来就像 Cookie 掉了。
    // 真实联网结论由守护任务刷新后从 `session_check` 返回。
    let logged_in = youtube_has_session();
    let session_check = last_youtube_session_check().map(|check| YouTubeSessionCheckResponse {
        state: check.state.as_str().to_string(),
        message: check.message,
        checked_at: check.checked_at,
    });
    Ok(ApiResponse::ok(YouTubeStatusResponse {
        ytdlp_available: version.is_some(),
        ytdlp_version: version,
        logged_in,
        session_check,
        default_output_path: default_output_path().display().to_string(),
        container_runtime: is_container_runtime(),
        cookie_path: youtube_cookie_file()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
    }))
}

/// 手动触发一次 YouTube 登录联网验证。
///
/// 只做探测：有效时顺带按响应续约，失效/不确定时**不会**删除任何凭证，结果回给
/// 前端展示，方便确认「到底是被 YouTube 挡了还是会话真过期」。
pub async fn check_youtube_session() -> Result<ApiResponse<YouTubeSessionCheckResponse>, ApiError> {
    if !youtube_has_session() {
        return Err(ApiError::bad_request(
            "尚未导入 YouTube 登录凭证，请先在设置页导入 cookies.txt 或传输登录状态",
        ));
    }
    let (state, message) = match tokio::time::timeout(Duration::from_secs(60), probe_youtube_subscription_channels())
        .await
    {
        Ok(Ok(_)) => (
            YoutubeSessionState::Valid,
            "登录状态有效（已顺带完成一次会话续约）".to_string(),
        ),
        Ok(Err(YoutubeSessionError::Expired(error))) => {
            (YoutubeSessionState::Expired, format!("探测返回未登录：{error:#}"))
        }
        Ok(Err(YoutubeSessionError::Unclear(error))) => (YoutubeSessionState::Unclear, format!("无法判断：{error:#}")),
        Err(_) => (
            YoutubeSessionState::Unclear,
            "联网验证超时（代理过慢或不可用），凭证已保留".to_string(),
        ),
    };
    record_youtube_session_check(state, message.clone());
    Ok(ApiResponse::ok(YouTubeSessionCheckResponse {
        state: state.as_str().to_string(),
        message,
        checked_at: last_youtube_session_check()
            .map(|check| check.checked_at)
            .unwrap_or_default(),
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

    let channel_mode = source_type == "channel";
    let search_url = youtube_search_url(keyword, &source_type)?;
    let mut command = ytdlp_command();
    command.args([
        "--flat-playlist",
        "--dump-json",
        "--playlist-end",
        "30",
        "--ignore-errors",
        "--no-warnings",
    ]);
    append_ytdlp_runtime(&mut command).await;
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
        let url = if channel_mode { canonical_channel_url(&url) } else { url };
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
    let results = fetch_youtube_playlist_catalog(&playlists_url, "加载 YouTube 频道播放列表")
        .await
        .map_err(ApiError::from)?;
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

/// 列出当前登录账号自己的全部播放列表。对应 B 站「我的收藏夹」入口，
/// 使用 cookies 访问 YouTube 的 `/feed/playlists`。
pub async fn get_youtube_my_playlists() -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    ensure_ytdlp_available().await?;
    if !youtube_has_session() {
        return Err(ApiError::from(anyhow!(
            "读取自己的 YouTube 播放列表需要有效登录状态，请先在设置页导入 YouTube cookies"
        )));
    }
    let results = fetch_youtube_playlist_catalog(MY_PLAYLISTS_URL, "加载我的 YouTube 播放列表")
        .await
        .map_err(ApiError::from)?;
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

async fn fetch_youtube_playlist_catalog(playlists_url: &str, operation: &str) -> Result<Vec<YouTubeSearchResult>> {
    let mut command = ytdlp_command();
    command.args([
        "--flat-playlist",
        "--dump-json",
        "--playlist-end",
        "200",
        "--ignore-errors",
        "--no-warnings",
        "--socket-timeout",
        "20",
        "--retries",
        "3",
    ]);
    append_ytdlp_runtime(&mut command).await;
    append_cookies(&mut command);
    append_youtube_proxy(&mut command);
    append_ytdlp_tab_args(&mut command);
    command.arg(playlists_url);
    let output = tokio::time::timeout(PLAYLIST_CATALOG_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow!("{operation}超时"))??;
    if !output.status.success() {
        bail!("{operation}失败：{}", command_error(&output));
    }

    let mut seen_urls = HashSet::new();
    let mut results = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(item) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(playlist_url) = youtube_playlist_url_from_item(&item) else {
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
    Ok(results)
}

fn youtube_playlist_url_from_item(item: &serde_json::Value) -> Option<String> {
    let explicit = item
        .get("webpage_url")
        .or_else(|| item.get("url"))
        .and_then(|value| value.as_str());
    if let Some(value) = explicit {
        if value.starts_with("http") && (value.contains("playlist?list=") || value.contains("/playlist/")) {
            return Some(value.to_string());
        }
    }
    let id = item
        .get("id")
        .or_else(|| item.get("playlist_id"))
        .and_then(|value| value.as_str())
        .filter(|value| {
            ["PL", "UU", "OL", "LL", "WL", "FL"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
        })?;
    Some(format!("https://www.youtube.com/playlist?list={id}"))
}

/// 枚举频道、播放列表、喜欢视频、稍后再看或已订阅频道。响应直接复用 B 站投稿
/// 选择面板的数据结构，让添加 YouTube 来源继续共用项目原有选择 UI。
pub async fn get_youtube_source_videos(
    Query(request): Query<YouTubeSourceVideosRequest>,
) -> Result<ApiResponse<SubmissionVideosResponse>, ApiError> {
    let source_type = request.source_type.trim().to_ascii_lowercase();
    if !youtube_source_video_selection_supported(&source_type) {
        return Err(ApiError::from(anyhow!(
            "仅订阅动态、频道、播放列表、喜欢的视频和稍后再看支持内容选择"
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
    if matches!(source_type.as_str(), "liked" | "watch_later") && !youtube_has_session() {
        let label = if source_type == "watch_later" {
            "稍后再看"
        } else {
            "喜欢的视频"
        };
        return Err(ApiError::from(anyhow!(
            "「{label}」需要有效的 YouTube 登录状态，请先在设置页重新导入 YouTube cookies"
        )));
    }

    ensure_ytdlp_available().await?;
    let source_url = match source_type.as_str() {
        "liked" => LIKED_URL.to_string(),
        "watch_later" => WATCH_LATER_URL.to_string(),
        "channel" => {
            if raw_url.is_empty() || !is_youtube_url(raw_url) {
                return Err(ApiError::from(anyhow!("请输入有效的 YouTube 频道或播放列表链接")));
            }
            canonical_channel_url(raw_url)
        }
        "playlist" => {
            if raw_url.is_empty() || !is_youtube_url(raw_url) {
                return Err(ApiError::from(anyhow!("请输入有效的 YouTube 播放列表链接")));
            }
            normalize_youtube_playlist_url(raw_url).map_err(ApiError::from)?
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
        "--yes-playlist",
        "--socket-timeout",
        "20",
        "--retries",
        "3",
    ]);
    append_ytdlp_runtime(&mut command).await;
    append_cookies(&mut command);
    append_youtube_proxy(&mut command);
    append_ytdlp_tab_args(&mut command);
    command.arg(&source_url);
    let output = tokio::time::timeout(SOURCE_VIDEO_PREVIEW_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow!("加载 YouTube 历史视频超时"))??;
    if !output.status.success() {
        let raw_error = command_error(&output);
        let command_failure = anyhow!("加载 YouTube 历史视频失败：{raw_error}");
        if matches!(source_type.as_str(), "liked" | "watch_later") && is_youtube_auth_required_error(&command_failure) {
            let label = if source_type == "watch_later" {
                "稍后再看"
            } else {
                "喜欢的视频"
            };
            return Err(ApiError::from(anyhow!(
                "「{label}」读取失败：YouTube 登录状态已失效或不完整，请在设置页重新导入 YouTube cookies"
            )));
        }
        return Err(ApiError::from(command_failure));
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
        if ytdlp_flat_item_is_unavailable(&item) {
            debug!(youtube_id = id, "跳过 YouTube 列表中的失效/私有视频");
            continue;
        }
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

fn youtube_source_video_selection_supported(source_type: &str) -> bool {
    matches!(
        source_type,
        "subscriptions" | "channel" | "playlist" | "liked" | "watch_later"
    )
}

fn ytdlp_flat_item_is_unavailable(item: &serde_json::Value) -> bool {
    if item
        .get("is_unavailable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    let availability = item
        .get("availability")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        availability.as_str(),
        "private" | "premium_only" | "subscriber_only" | "needs_auth" | "unavailable"
    ) {
        return true;
    }
    let title = item
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(
        title.as_str(),
        "[private video]"
            | "private video"
            | "[deleted video]"
            | "deleted video"
            | "[unavailable video]"
            | "unavailable video"
            | "[视频不可用]"
            | "[私有视频]"
            | "[已删除视频]"
    )
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
    probe_youtube_subscription_channels()
        .await
        .map_err(YoutubeSessionError::into_anyhow)
}

/// YouTube 登录探测的失败分类。
///
/// 只有 `Expired` 表示「有明确证据说明会话已经失效」，调用方在这种情况之外一律
/// 保留现有凭证。之前只要探测报错就带着「Cookie 已失效」的字样删除会话，一次
/// 代理抖动或风控页命中的误判就会把用户还能用的登录状态清掉，表现为「下载正常，
/// 过一会 Cookie 突然掉了」。
enum YoutubeSessionError {
    Expired(anyhow::Error),
    Unclear(anyhow::Error),
}

impl YoutubeSessionError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Expired(error) | Self::Unclear(error) => error,
        }
    }
}

fn youtube_cookie_expired_error() -> anyhow::Error {
    anyhow!("YouTube Cookie 已失效或不完整，请在登录 YouTube 的同一浏览器配置中重新传输登录状态")
}

async fn probe_youtube_subscription_channels() -> std::result::Result<serde_json::Value, YoutubeSessionError> {
    let contents = youtube_cookie_text().ok_or_else(|| {
        YoutubeSessionError::Unclear(anyhow!(
            "读取 YouTube Cookie 失败：请先在设置页导入 cookies.txt 或传输登录状态"
        ))
    })?;
    if !has_youtube_session_value(&contents) {
        return Err(YoutubeSessionError::Expired(youtube_cookie_expired_error()));
    }
    let cookie_jar = youtube_cookie_jar_from_text(&contents).map_err(YoutubeSessionError::Unclear)?;
    let mut client_builder = reqwest::Client::builder()
        // Netscape 文件同时包含 YouTube 和 Google 账号域 Cookie。必须让
        // Cookie Jar 按域名分别发送：直接拼成一个 Cookie 请求头会把
        // google.com 的同名 SID/PSID 错发给 youtube.com，导致有效会话也被判为退出。
        .cookie_provider(cookie_jar)
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(Duration::from_secs(30));
    let proxy = configured_external_proxy();
    if !proxy.is_empty() {
        let proxy = reqwest::Proxy::all(&proxy)
            .map_err(|error| YoutubeSessionError::Unclear(anyhow::Error::new(error).context("外源代理地址无效")))?;
        client_builder = client_builder.no_proxy().proxy(proxy);
    }
    let client = client_builder.build().map_err(|error| {
        YoutubeSessionError::Unclear(anyhow::Error::new(error).context("构建 YouTube 请求客户端失败"))
    })?;
    let response = client
        .get(SUBSCRIPTION_CHANNELS_URL)
        .header(reqwest::header::USER_AGENT, YOUTUBE_WEB_USER_AGENT)
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
        .map_err(|error| {
            YoutubeSessionError::Unclear(anyhow::Error::new(error).context("访问 YouTube 已订阅频道页面失败"))
        })?;
    let status = response.status();
    let final_url = response.url().to_string();
    let fallback_domain = response.url().host_str().unwrap_or("www.youtube.com").to_string();
    // 浏览器式被动续约：先把响应里的 Set-Cookie 合并好，但**确认本次确实处于登录
    // 态之后**才写回 cookies.txt。否则登录墙/风控页/同意条款页返回的 Set-Cookie 也
    // 会盖掉本来能用的会话。
    let renewed = crate::utils::netscape_cookies::renew_cookie_text(
        &contents,
        response.headers(),
        &fallback_domain,
        is_youtube_auth_cookie_domain,
    );
    let html = response.text().await.map_err(|error| {
        YoutubeSessionError::Unclear(anyhow::Error::new(error).context("读取 YouTube 已订阅频道页面失败"))
    })?;

    let verdict = youtube_login_verdict(&final_url, status, &html);
    debug!(
        final_url = %final_url,
        status = status.as_u16(),
        verdict = ?verdict,
        "YouTube 登录状态探测结果"
    );
    match verdict {
        YoutubeLoginVerdict::LoggedIn => {}
        YoutubeLoginVerdict::LoggedOut => {
            return Err(YoutubeSessionError::Expired(youtube_cookie_expired_error()));
        }
        YoutubeLoginVerdict::Unclear(reason) => {
            return Err(YoutubeSessionError::Unclear(anyhow!(
                "YouTube 登录状态探测不确定：{reason}（HTTP {}，最终地址 {}）",
                status.as_u16(),
                final_url
            )));
        }
    }
    let initial_data = extract_youtube_initial_data(&html).ok_or_else(|| {
        YoutubeSessionError::Unclear(anyhow!("YouTube 已订阅频道页面缺少频道数据，请重新传输登录状态后重试"))
    })?;
    if let Some(merged) = renewed {
        // 续约只允许「补齐/替换」Cookie，绝不允许把会话字段弄没：一旦合并结果里
        // 已经没有 SID/SAPISID 之类的会话 Cookie，就说明这次合并有问题，宁可不写回。
        if !has_youtube_session_value(&merged) {
            warn!("续约后的 YouTube Cookie 丢失了会话字段，已放弃写回，保留原凭证");
        } else if let Err(error) = set_youtube_cookies(&merged).await {
            warn!(error = %error, "写回 YouTube 续约后的 Cookie 失败");
        } else {
            debug!("YouTube 会话已按响应 Set-Cookie 续约");
        }
    }
    Ok(initial_data)
}

#[cfg(test)]
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

/// `/feed/channels` 响应的登录判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum YoutubeLoginVerdict {
    /// 明确处于登录态。
    LoggedIn,
    /// 明确已退出登录（有正面证据）。
    LoggedOut,
    /// 无法判断：同意条款页、风控/验证页、HTTP 错误、非预期页面等。
    Unclear(&'static str),
}

/// 判断 `/feed/channels` 的响应是否处于登录态。
///
/// 只在拿到**正面证据**时才判定退出：被重定向到 Google 登录页，或页面里明确写着
/// `"LOGGED_IN":false`。其余情况（同意条款页、人机验证页、非 2xx、页面结构变化）
/// 一律判为不确定，由调用方保留现有凭证 —— 判错成「已退出」的代价是删掉用户还能
/// 用的登录状态，比漏判一次严重得多。
fn youtube_login_verdict(final_url: &str, status: reqwest::StatusCode, html: &str) -> YoutubeLoginVerdict {
    if final_url.contains("accounts.google.com") || final_url.contains("ServiceLogin") {
        return YoutubeLoginVerdict::LoggedOut;
    }
    if html.contains("\"LOGGED_IN\":true") || html.contains("\"loggedIn\":true") {
        return YoutubeLoginVerdict::LoggedIn;
    }
    if youtube_page_is_consent_wall(final_url, html) {
        return YoutubeLoginVerdict::Unclear("命中 YouTube 同意条款/前置确认页");
    }
    if youtube_page_is_challenge(final_url, html) {
        return YoutubeLoginVerdict::Unclear("命中 YouTube 风控/人机验证页");
    }
    if !status.is_success() {
        return YoutubeLoginVerdict::Unclear("页面返回非 2xx");
    }
    if html.contains("accounts.google.com/ServiceLogin") {
        return YoutubeLoginVerdict::LoggedOut;
    }
    if html.contains("\"LOGGED_IN\":false") || html.contains("\"loggedIn\":false") {
        return YoutubeLoginVerdict::LoggedOut;
    }
    YoutubeLoginVerdict::Unclear("页面既没有登录标记也没有退出标记")
}

/// 代理出口在部分区域会先弹「同意条款 / before you continue」，这类页面同样带着
/// 未登录标记，但会话本身是好的。
fn youtube_page_is_consent_wall(final_url: &str, html: &str) -> bool {
    final_url.contains("consent.youtube.com")
        || final_url.contains("consent.google.com")
        || html.contains("consent.youtube.com")
        || html.contains("Before you continue to YouTube")
        || html.contains("before-you-continue")
}

/// 风控/「异常流量」验证页。
fn youtube_page_is_challenge(final_url: &str, html: &str) -> bool {
    final_url.contains("/sorry/")
        || html.contains("/sorry/index")
        || html.contains("unusual traffic")
        || html.contains("unusual-traffic")
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
    // 记下导入前的凭证：一次失败的导入不能把原本还能用的登录状态一起删掉。
    let previous = youtube_cookie_text();
    // 先清理旧会话（数据库 + 历史文件），再将新会话写入数据库。
    clear_youtube_login_state("导入新凭证前清理旧会话").await;
    if let Err(error) = set_youtube_cookies(&request.cookies).await {
        // 写库失败（磁盘只读、数据库被占用等）也要把旧凭证放回去，否则用户
        // 只是导入失败，却把原本能用的登录状态弄丢了。
        if let Some(previous) = previous.as_deref() {
            if let Err(restore) = set_youtube_cookies(previous).await {
                warn!(error = %restore, "恢复导入前的 YouTube 登录凭证失败");
            }
        }
        return Err(ApiError::from(error));
    }
    if let Err(error) = validate_youtube_login_cookie().await {
        match previous {
            Some(previous) => {
                if let Err(restore) = set_youtube_cookies(&previous).await {
                    warn!(error = %restore, "恢复导入前的 YouTube 登录凭证失败");
                } else {
                    info!("新导入的 YouTube 凭证验证失败，已回滚到导入前的登录状态");
                }
            }
            None => clear_youtube_login_state("新导入的凭证验证失败，且没有可回滚的旧凭证").await,
        }
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
async fn require_external_video_platform(db: &DatabaseConnection, id: i32) -> Result<(), ApiError> {
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
    let mut url = resolve_source_url(source_type, request.url.as_deref())?;
    if source_type == "douyin" {
        // 统一存成作者主页链接：搜索页、分享短链、主页链接都归一到 sec_uid，
        // 同一个作者才不会因为链接写法不同被当成两个源重复添加。
        let sec_uid = crate::douyin::resolve_sec_user_id(&url).await?;
        url = format!("https://www.douyin.com/user/{sec_uid}");
    } else if source_type == "douyin_mix" {
        // 合集统一归一成网页合集页链接（`/collection/{mix_id}`），
        // 并确认该 id 确实是合集：抖音的收藏夹与合集共用 `/collection/` 路由，
        // 只看链接分不出来，选错类型建出来的源会永远扫不到作品。
        let mix_id = crate::douyin::resolve_mix_id(&url).await?;
        let detail = crate::douyin::fetch_mix_detail(&mix_id).await?;
        let Some(detail) = detail else {
            return Err(ApiError::bad_request(format!(
                "这个链接不是抖音合集（ID {mix_id}）：合集请先搜索作者再从右侧选择；若要同步收藏夹请改用「收藏夹」来源类型"
            )));
        };
        debug!(
            mix_id = %detail.id,
            mix_name = %detail.name,
            episode_count = ?detail.episode_count,
            "抖音合集来源已确认"
        );
        url = format!("https://www.douyin.com/collection/{mix_id}");
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
    // 外源互斥：同一个源只能有一条记录，重复添加会重复扫描、重复下载。
    ensure_external_source_not_duplicated(db.as_ref(), source_type, &url).await?;
    let model = youtube_source::ActiveModel {
        source_type: Set(source_type.to_string()),
        name: Set(name.to_string()),
        url: Set(url),
        path: Set(path.to_string()),
        enabled: Set(true),
        audio_only: Set(request.audio_only),
        audio_only_m4a_only: Set(request.audio_only_m4a_only),
        flat_folder: Set(request.flat_folder),
        generate_image_post_video: Set(request.generate_image_post_video.unwrap_or(true)),
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
    if let Some(value) = request.generate_image_post_video {
        active.generate_image_post_video = Set(value);
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
            if old_file != target && old_file.is_file() {
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
            }
            // 无论媒体文件是否还在磁盘上，都要同步数据库里的 output_path：
            // 否则「媒体已被删除 / 还没下完」的分集仍指向旧目录，之后每轮扫描回填
            // 封面时都会按旧路径计算，旧目录被反复重建（用户看到的
            // 「重设路径后旧目录还在不停生成 fanart/poster」）。
            let mut active: youtube_video::ActiveModel = video.clone().into();
            active.output_path = Set(Some(target.display().to_string()));
            active.updated_at = Set(now_standard_string());
            active.update(txn).await?;
        }
    }
    let mut active: youtube_source::ActiveModel = source.into();
    active.path = Set(new_path.to_string());
    active.update(txn).await?;
    // 旧目录里没下完的半成品（`.download.mp4` / `.merging.mp4` / `.resume` 等）
    // 不属于任何已记录视频，上面按 output_path 搬迁时根本不会碰到，会在旧目录里
    // 越堆越多。换目录时按下载器自己的命名规则扫一遍删掉。
    let cleaned_partial_files_count = if old_base != new_base {
        remove_partial_download_artifacts(&old_base).await
    } else {
        0
    };
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
            "{}视频源路径重设完成，移动 {} 个文件，清理 {} 个空文件夹、{} 个未完成的临时文件",
            external_platform_label(source_type),
            moved_files_count,
            cleaned_folders_count,
            cleaned_partial_files_count
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

pub(crate) fn source_platform_label(source: &youtube_source::Model) -> &'static str {
    if is_douyin_source(source) {
        "抖音"
    } else if crate::tiktok::is_tiktok_source(source) {
        "TikTok"
    } else {
        "YouTube"
    }
}

/// 访问 YouTube 网页时使用的浏览器标识：`timedtext` 字幕端点对非浏览器 UA
/// 更早触发风控，登录探测与字幕下载共用同一个值，避免行为不一致。
const YOUTUBE_WEB_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36";

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

/// 下载单条 YouTube 字幕轨。
///
/// `timedtext` 端点对**不带 Cookie** 的请求直接返回 429（Google 侧判定为
/// automated queries），即使签名 URL 完全有效；同一 URL 带上已导入的登录
/// Cookie（按域匹配）会返回 200。因此字幕不能复用通用媒体下载链路——
/// 那条链路为了下载 googlevideo 直链并不携带会话 Cookie。
///
/// 字幕是纯文本、体积小，这里用带 Cookie Jar + 外源代理的 reqwest 直取；
/// 未导入 Cookie 或直取失败时回退到通用资源下载，保持旧行为可用。
async fn fetch_youtube_subtitle_track(
    downloader: &UnifiedDownloader,
    source: &youtube_source::Model,
    url: &str,
    path: &Path,
) -> Result<()> {
    if !is_douyin_source(source) && !crate::tiktok::is_tiktok_source(source) {
        let cookie_text = youtube_cookie_text().filter(|text| has_youtube_session_value(text));
        if let Some(cookie_text) = cookie_text {
            match download_youtube_subtitle_http(&cookie_text, url, path).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    let _ = remove_file_if_exists(path).await;
                    debug!(
                        reason = %format!("{error:#}"),
                        "带登录 Cookie 下载 YouTube 字幕轨失败，回退通用资源下载"
                    );
                }
            }
        }
    }
    fetch_platform_asset(downloader, source, &[url], path).await
}

async fn download_youtube_subtitle_http(cookie_text: &str, url: &str, path: &Path) -> Result<()> {
    let jar = youtube_cookie_jar_from_text(cookie_text)?;
    let mut builder = reqwest::Client::builder()
        .cookie_provider(jar)
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(Duration::from_secs(60));
    let proxy = configured_external_proxy();
    if !proxy.is_empty() {
        builder = builder
            .no_proxy()
            .proxy(reqwest::Proxy::all(&proxy).context("外源代理地址无效")?);
    }
    let client = builder.build().context("构建 YouTube 字幕请求客户端失败")?;
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, YOUTUBE_WEB_USER_AGENT)
        .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .header(reqwest::header::REFERER, "https://www.youtube.com/")
        .send()
        .await
        .context("请求 YouTube 字幕端点失败")?;
    let status = response.status();
    if !status.is_success() {
        bail!("下载 YouTube 字幕失败：HTTP {}", status.as_u16());
    }
    let bytes = response.bytes().await.context("读取 YouTube 字幕响应失败")?;
    if bytes.is_empty() {
        bail!("YouTube 字幕响应为空");
    }
    tokio::fs::write(path, &bytes).await.context("写入字幕临时文件失败")
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
        "douyin_mix" => "抖音合集",
        _ => "YouTube",
    }
}

fn youtube_upper_paths(uploader: &str) -> (PathBuf, PathBuf) {
    let uploader = crate::utils::filenamify::filenamify(uploader);
    let bucket = uploader.chars().next().unwrap_or('_').to_string().to_lowercase();
    let upper_dir = crate::config::reload_config().upper_path.join(bucket).join(uploader);
    (upper_dir.join("folder.jpg"), upper_dir.join("person.nfo"))
}

/// 外源视频的“任务状态位”与 B 站完全同构（u32，从低位起每 3bit 一个子任务，
/// bit31 为整条完成标记）：
/// 视频级：[视频封面, 视频信息(NFO), UP头像, UP主信息, 分P下载]；
/// 分页级：[视频封面, 视频内容, 单集NFO, 弹幕/直播, 字幕]。
/// 数值 0=未开始，1..=4=失败次数，7(STATUS_OK)=已完成。
fn youtube_video_status(video: &youtube_video::Model) -> VideoStatus {
    VideoStatus::from(video.video_task_status)
}

fn youtube_page_status(video: &youtube_video::Model) -> PageStatus {
    PageStatus::from(video.page_task_status)
}

/// 由状态位派生出整条视频的文本状态（兼容既有扫描/筛选/队列逻辑）。
/// skipped 等特殊文本由调用方在写完状态位后单独保留。
fn youtube_text_from_task_status(video_task_status: u32, page_task_status: u32) -> String {
    let video_status = VideoStatus::from(video_task_status);
    let page_status = PageStatus::from(page_task_status);
    let mut any_pending = false;
    let mut any_failed = false;
    for index in 0..5 {
        for status in [video_status.get(index), page_status.get(index)] {
            if status == 0 {
                any_pending = true;
            } else if status < STATUS_OK {
                any_failed = true;
            }
        }
    }
    if any_failed {
        "failed".to_string()
    } else if any_pending {
        "pending".to_string()
    } else {
        "completed".to_string()
    }
}

/// 全部子任务已完成的状态位编码（视频级与分页级同为 5 个子任务，编码值相同）。
fn youtube_all_tasks_ok_bits() -> u32 {
    let mut status = VideoStatus::default();
    for index in 0..5 {
        status.set(index, STATUS_OK);
    }
    status.into()
}

/// 外源视频任务状态直接从数据库状态位读取（与 B 站一致），不再由磁盘文件反推。
/// 已完成视频即使媒体文件被移走/清理，任务位仍是完成，卡片不会退回“未下载”。
async fn youtube_artifact_status(video: &youtube_video::Model, source: &youtube_source::Model) -> ([u32; 5], [u32; 5]) {
    let _ = source;
    (youtube_video_status(video).into(), youtube_page_status(video).into())
}

async fn unified_youtube_parts(
    video: &youtube_video::Model,
    source: &youtube_source::Model,
) -> (VideoInfo, Vec<PageInfo>, VideoSourceTag) {
    let (video_status, page_status) = youtube_artifact_status(video, source).await;
    let output_path = video.output_path.clone();
    let local_files = youtube_image_post_files(video);
    let image_paths = local_files.images;
    // 图集作品的视频段各自成页；本地记录指向第一个分页，这里按分页编号还原整篇作品。
    let page_paths = output_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(Path::new)
        .map(crate::douyin::image_post_page_paths)
        .unwrap_or_default();
    // 抖音与 TikTok 都可能有图文作品（TikTok 侧为 photo post / 幻灯片），
    // 两者的图片目录约定一致，这里一起标记，供视频管理页与详情页显示图片。
    let is_image_post = (is_douyin_source(source) || crate::tiktok::is_tiktok_source(source))
        && (video.is_image_post || !image_paths.is_empty() || local_files.has_video_segments || page_paths.len() > 1);
    // 图集作品里的视频段是独立分页：有多个分页（或本地还留着早前版本的视频片段）
    // 就按「多段动态」处理——卡片用紫色「动态」徽标区分，详情页不再提示「图片尚未下载」。
    let image_post_video_only =
        is_image_post && image_paths.is_empty() && (page_paths.len() > 1 || local_files.has_video_segments);
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
            image_post_video_only,
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
        {
            let multi_page = page_paths.len() > 1;
            if page_paths.is_empty() {
                // 还没下载的作品：保留一个占位分页，前端继续显示「尚未下载」。
                vec![PageInfo {
                    id: video.id,
                    pid: 1,
                    name: video.title.clone(),
                    download_status: page_status,
                    path: output_path,
                    danmaku_last_synced_at: None,
                    danmaku_sync_generation: 0,
                    danmaku_cid_snapshot: None,
                    danmaku_last_write_count: 0,
                }]
            } else {
                page_paths
                    .iter()
                    .enumerate()
                    .map(|(index, path)| PageInfo {
                        id: video.id,
                        pid: (index + 1) as i32,
                        name: if multi_page {
                            format!("第{}段", index + 1)
                        } else {
                            video.title.clone()
                        },
                        download_status: page_status,
                        path: Some(path.display().to_string()),
                        danmaku_last_synced_at: None,
                        danmaku_sync_generation: 0,
                        danmaku_cid_snapshot: None,
                        danmaku_last_write_count: 0,
                    })
                    .collect()
            }
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
            generate_image_post_video: source.generate_image_post_video,
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
    let (video, pages, source) = unified_youtube_parts(&video, &source).await;
    Ok(VideoResponse {
        video,
        pages,
        source: Some(source),
    })
}

/// 需要当场重建的外源附属文件类型（由 worker 在“只重跑附属文件”时使用）。
#[derive(Debug, Clone, Copy, Default)]
struct YoutubeArtifactRegen {
    cover: bool,
    nfo: bool,
    upper_face: bool,
    upper_info: bool,
}

impl YoutubeArtifactRegen {
    fn any(&self) -> bool {
        self.cover || self.nfo || self.upper_face || self.upper_info
    }
}

/// 删除某个外源任务对应的本地产物。重置时把“已完成(7)”的任务打回“未开始(0)”后，
/// 需要删掉已存在的文件，worker 下一轮才会真正重新生成（与 B 站“重置即重跑”一致）。
async fn remove_youtube_task_artifact(
    video: &youtube_video::Model,
    source: &youtube_source::Model,
    video_indexes: &[usize],
    page_indexes: &[usize],
) -> Result<()> {
    let mut cover = false;
    let mut nfo = false;
    let mut upper_face = false;
    let mut upper_info = false;
    let mut media = false;
    let mut danmaku = false;
    let mut subtitle = false;
    for &index in video_indexes {
        match index {
            0 => cover = true,
            1 => nfo = true,
            2 => upper_face = true,
            3 => upper_info = true,
            4 => media = true,
            _ => {}
        }
    }
    for &index in page_indexes {
        match index {
            0 => cover = true,
            1 => media = true,
            2 => nfo = true,
            3 => danmaku = true,
            4 => subtitle = true,
            _ => {}
        }
    }
    if let Some(output_path) = video.output_path.as_deref().filter(|value| !value.trim().is_empty()) {
        let output_path = PathBuf::from(output_path);
        if (cover || nfo) && source.source_type == "douyin_collection" {
            cleanup_legacy_collection_series_sidecars(source, &output_path).await?;
        }
        if cover {
            if let Ok(thumb) = youtube_sidecar_path(&output_path, "-thumb.jpg") {
                remove_file_if_exists(&thumb).await?;
            }
            if let Ok(fanart) = youtube_sidecar_path(&output_path, "-fanart.jpg") {
                remove_file_if_exists(&fanart).await?;
            }
        }
        if nfo {
            let nfo_path = output_path.with_extension("nfo");
            remove_file_if_exists(&nfo_path).await?;
            // 抖音短剧/放映厅/合集确有集号、实际使用 Season 结构时，
            // 剧集根目录还会有 tvshow.nfo。
            if uses_episodic_layout(source, video.episode_number) {
                if let Some(series_dir) = episodic_series_dir(&output_path, &source.path) {
                    let tvshow = series_dir.join("tvshow.nfo");
                    remove_file_if_exists(&tvshow).await?;
                }
            }
        }
        if media {
            // 图集作品的分页文件同属一篇作品，只删主文件会留下孤儿分页。
            for page in crate::douyin::image_post_page_paths(&output_path) {
                remove_file_if_exists(&page).await?;
            }
        }
        if danmaku {
            remove_file_if_exists(&external_ass_path(&output_path)).await?;
            remove_file_if_exists(&output_path.with_extension("ass")).await?;
            for suffix in [
                "danmaku.json",
                "danmaku.json.download",
                "danmaku.checked",
                "live_chat.json",
            ] {
                remove_file_if_exists(&output_path.with_extension(suffix)).await?;
            }
        }
        if subtitle {
            remove_external_subtitle_files(&output_path).await?;
        }
        for suffix in ["subtitle.checked", "danmaku.checked", "live_chat.checked"] {
            remove_file_if_exists(&output_path.with_extension(suffix)).await?;
        }
    }
    if upper_face || upper_info {
        let (face_path, person_nfo_path) = youtube_upper_paths(&video.uploader);
        if upper_face {
            remove_file_if_exists(&face_path).await?;
        }
        if upper_info {
            remove_file_if_exists(&person_nfo_path).await?;
        }
    }
    if let Some(output_path) = video.output_path.as_deref().filter(|value| !value.trim().is_empty()) {
        prune_empty_external_dirs(source, Path::new(output_path)).await;
    }
    Ok(())
}

async fn remove_external_subtitle_files(output_path: &Path) -> Result<()> {
    let Some(parent) = output_path.parent() else {
        return Ok(());
    };
    let stem = output_path.file_stem().and_then(|value| value.to_str()).unwrap_or("");
    if stem.is_empty() {
        return Ok(());
    }
    let prefix = format!("{stem}.");
    let danmaku_ass = format!("{stem}.zh-CN.default.ass");
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return Ok(());
    };
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("");
        if name.starts_with(&prefix) && matches!(extension, "vtt" | "srt" | "ass" | "ssa") && name != danmaku_ass {
            remove_file_if_exists(&path).await?;
        }
    }
    Ok(())
}

async fn prune_empty_external_dirs(source: &youtube_source::Model, output_path: &Path) {
    let root = PathBuf::from(source.path.trim());
    let mut current = output_path.parent().map(Path::to_path_buf);
    while let Some(dir) = current {
        if dir == root || !dir.starts_with(&root) {
            break;
        }
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            break;
        };
        if entries.next_entry().await.ok().flatten().is_some() {
            break;
        }
        if let Err(error) = tokio::fs::remove_dir(&dir).await {
            debug!(path = %dir.display(), %error, "清理空外源目录失败");
            break;
        }
        debug!(path = %dir.display(), "已清理空外源目录");
        current = dir.parent().map(Path::to_path_buf);
    }
}

/// 按 B 站语义整体重置一条外源视频的任务状态位并落库。
///
/// - force=true：重置所有非 0 状态（含已完成）；
/// - force=false：只重置失败状态；文本 failed/付费不可播的旧任务也视为需要重置。
///
/// 返回是否发生了重置。
async fn reset_youtube_video_task_bits(
    db: &DatabaseConnection,
    video: youtube_video::Model,
    force: bool,
) -> Result<bool> {
    let source = youtube_source::Entity::find_by_id(video.source_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow!("视频源不存在（source_id={}）", video.source_id))?;
    let was_failed = video.download_status == "failed";
    let charge_blocked = video.is_charge_video && !video.charge_can_play;

    let mut video_status = VideoStatus::from(video.video_task_status);
    let mut page_status = PageStatus::from(video.page_task_status);
    let old_video = video_status;
    let old_page = page_status;
    let bit_changed = if force || charge_blocked {
        video_status.reset_all() | page_status.reset_all()
    } else {
        video_status.reset_failed() | page_status.reset_failed()
    };
    if !bit_changed && !was_failed && !charge_blocked {
        return Ok(false);
    }

    // 被从“已完成”打回“未开始”的任务索引 → 删除旧产物，下一轮 worker 重新生成。
    let mut video_indexes = Vec::new();
    let mut page_indexes = Vec::new();
    for index in 0..5 {
        if old_video.get(index) == STATUS_OK && video_status.get(index) == 0 {
            video_indexes.push(index);
        }
        if old_page.get(index) == STATUS_OK && page_status.get(index) == 0 {
            page_indexes.push(index);
        }
    }
    if !(video_indexes.is_empty() && page_indexes.is_empty()) {
        remove_youtube_task_artifact(&video, &source, &video_indexes, &page_indexes).await?;
    }

    let video_bits: u32 = video_status.into();
    let page_bits: u32 = page_status.into();
    let mut active: youtube_video::ActiveModel = video.into();
    active.video_task_status = Set(video_bits);
    active.page_task_status = Set(page_bits);
    active.download_status = Set(youtube_text_from_task_status(video_bits, page_bits));
    active.retry_count = Set(0);
    active.error_message = Set(None);
    active.is_charge_video = Set(false);
    active.charge_can_play = Set(false);
    active.updated_at = Set(now_standard_string());
    active.update(db).await?;
    Ok(true)
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
    let resetted = reset_youtube_video_task_bits(db, video, force).await?;
    if resetted {
        notify_videos_changed();
        notify_queue_status_changed();
        crate::task::resume_scanning();
    }
    let response = get_unified_youtube_video(db, id).await?;
    Ok(ResetVideoResponse {
        resetted,
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
        if reset_youtube_video_task_bits(db, video, force).await? {
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
    if video_indexes.is_empty() && page_indexes.is_empty() {
        return Err(anyhow!("至少需要选择一个要重置的任务").into());
    }
    let force = request.force.unwrap_or(false);

    let params = VideosRequest {
        platform: request.platform.clone().or_else(|| Some("youtube".to_string())),
        youtube: request.youtube,
        query: request.query.clone(),
        show_failed_only: request.show_failed_only,
        force: Some(force),
        ..Default::default()
    };
    let rows = filtered_youtube_models(db, &params).await?;
    let mut count = 0usize;
    for (video, source, _) in rows {
        let mut video_status = VideoStatus::from(video.video_task_status);
        let mut page_status = PageStatus::from(video.page_task_status);
        let mut changed = false;
        let mut cleared_video = Vec::new();
        let mut cleared_page = Vec::new();
        for &index in &video_indexes {
            if index < 5 {
                let current = video_status.get(index);
                let should_reset = if force { current != 0 } else { (1..6).contains(&current) };
                if should_reset {
                    if current == STATUS_OK {
                        cleared_video.push(index);
                    }
                    video_status.set(index, 0);
                    changed = true;
                }
            }
        }
        for &index in &page_indexes {
            if index < 5 {
                let current = page_status.get(index);
                let should_reset = if force { current != 0 } else { (1..6).contains(&current) };
                if should_reset {
                    if current == STATUS_OK {
                        cleared_page.push(index);
                    }
                    page_status.set(index, 0);
                    changed = true;
                }
            }
        }
        if !changed {
            continue;
        }
        // 从“已完成”打回“未开始”的任务删掉旧产物，worker 下一轮重新生成。
        if !(cleared_video.is_empty() && cleared_page.is_empty()) {
            remove_youtube_task_artifact(&video, &source, &cleared_video, &cleared_page).await?;
        }
        let video_bits: u32 = video_status.into();
        let page_bits: u32 = page_status.into();
        let mut active: youtube_video::ActiveModel = video.into();
        active.video_task_status = Set(video_bits);
        active.page_task_status = Set(page_bits);
        active.download_status = Set(youtube_text_from_task_status(video_bits, page_bits));
        active.retry_count = Set(0);
        active.error_message = Set(None);
        active.updated_at = Set(now_standard_string());
        active.update(db).await?;
        count += 1;
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

pub async fn update_unified_youtube_status(
    db: &DatabaseConnection,
    id: i32,
    request: &UpdateVideoStatusRequest,
) -> Result<UpdateVideoStatusResponse, ApiError> {
    let video = youtube_video::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow!("YouTube 视频不存在: {}", id))?;
    let mut video_status = VideoStatus::from(video.video_task_status);
    let mut page_status = PageStatus::from(video.page_task_status);
    let mut changed = false;
    // 「已完成」被打回「未开始」的子任务：用户明确要求重跑，需要先删掉旧产物。
    let mut cleared_video = Vec::new();
    let mut cleared_page = Vec::new();

    // 与 B 站 update_video_status 一致：直接覆盖对应子任务状态位。
    for update in &request.video_updates {
        if update.status_index < 5 {
            let current = video_status.get(update.status_index);
            if current != update.status_value {
                if current == STATUS_OK && update.status_value == 0 {
                    cleared_video.push(update.status_index);
                }
                video_status.set(update.status_index, update.status_value);
                changed = true;
            }
        }
    }
    for page in &request.page_updates {
        // 外源视频只有一条合成页面，page_id 即视频 id。
        for update in &page.updates {
            if update.status_index < 5 {
                let current = page_status.get(update.status_index);
                if current != update.status_value {
                    if current == STATUS_OK && update.status_value == 0 {
                        cleared_page.push(update.status_index);
                    }
                    page_status.set(update.status_index, update.status_value);
                    changed = true;
                }
            }
        }
    }

    if changed {
        // 删掉被打回的子任务产物，否则 worker 下一轮会因为文件还在而直接跳过：
        // 媒体会被「目标文件已存在，复用现有文件」复用、UP头像/UP主信息会被缓存命中，
        // 只有先把产物删掉，重置为「未开始」才是真的重跑（与批量重置一致）。
        if !(cleared_video.is_empty() && cleared_page.is_empty()) {
            if let Some(source) = youtube_source::Entity::find_by_id(video.source_id).one(db).await? {
                remove_youtube_task_artifact(&video, &source, &cleared_video, &cleared_page).await?;
            }
        }
        let video_bits: u32 = video_status.into();
        let page_bits: u32 = page_status.into();
        let mut active: youtube_video::ActiveModel = video.into();
        active.video_task_status = Set(video_bits);
        active.page_task_status = Set(page_bits);
        active.download_status = Set(youtube_text_from_task_status(video_bits, page_bits));
        active.retry_count = Set(0);
        active.error_message = Set(None);
        active.updated_at = Set(now_standard_string());
        active.update(db).await?;
        notify_videos_changed();
        notify_queue_status_changed();
        // 有任务回到未开始/失败 → 让 worker 下一轮处理。
        crate::task::resume_scanning();
    }
    let response = get_unified_youtube_video(db, id).await?;
    Ok(UpdateVideoStatusResponse {
        success: true,
        video: response.video,
        pages: response.pages,
    })
}

/// 当场重建外源视频的附属文件（封面/NFO/UP头像/UP主信息）——供 worker 的
/// “只重跑附属文件”路径使用。不动媒体本体。
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
    // UP头像/UP主信息也要元数据：抖音合集/放映厅/收藏源的 source.url 是作品列表
    // 链接，只有作品元数据里的频道主页地址能解析出作者头像，拿源链接会直接失败。
    let metadata = if regen.cover || regen.nfo || regen.upper_face || regen.upper_info {
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
        if source.source_type == "douyin_collection" {
            cleanup_legacy_collection_series_sidecars(&source, &output_path).await?;
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
        let profile_url = metadata.channel_url.as_deref().or(metadata.uploader_url.as_deref());
        let avatar_url = avatar_url_for_nfo(&source, profile_url, None, &nfo_path).await;
        generate_youtube_nfo(
            metadata,
            &output_path,
            &video.url,
            &video.title,
            &video.uploader,
            avatar_url.as_deref(),
            &source,
        )
        .await
        .with_context(|| format!("重新生成{platform}视频信息/NFO失败"))?;
        // 抖音短剧/放映厅/合集确有集号、实际使用 Season 结构时，
        // 单集 NFO 之外还要刷新剧集 tvshow.nfo。
        if uses_episodic_layout(&source, video.episode_number) {
            if let Some(downloader) = downloader.as_deref() {
                if let Some(series_dir) = episodic_series_dir(&output_path, &source.path) {
                    let tvshow = series_dir.join("tvshow.nfo");
                    remove_file_if_exists(&tvshow).await?;
                    if let Err(error) = ensure_youtube_series_sidecars(
                        downloader,
                        metadata,
                        &output_path,
                        &source,
                        avatar_url.as_deref(),
                    )
                    .await
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
        let _ = download_youtube_upper_face(downloader, &video.uploader, profile_url, &source)
            .await
            .with_context(|| format!("重新生成{platform}UP头像/UP主信息失败"))?;
    }
    Ok(())
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

/// 图集作品在磁盘上的落地情况：可查看的原图（不含视频段）与是否含视频段。
#[derive(Default)]
pub(crate) struct ImagePostLocalFiles {
    pub(crate) images: Vec<PathBuf>,
    /// 是否有视频段（抖音 live photo / 多段视频图集落成 `NN.mp4`）。
    pub(crate) has_video_segments: bool,
}

/// 扫描图集作品的 `{stem}-images` 目录：图片段按序号排序，视频段只记标志位。
/// 纯视频段图集（多段视频/动态）没有原图，卡片与详情页要靠这个标志位区分
/// 「作品本来就没有原图」和「原图还没下载完」。
pub(crate) fn image_post_local_files(output_path: &Path) -> ImagePostLocalFiles {
    // 视频段各自成页时主文件是 `<作品>-P01.mp4`，原图目录挂在作品名上，
    // 先剥掉分页号再找 `{作品}-images`，否则分页作品会读不到原图。
    let base_path = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .and_then(crate::douyin::split_image_post_page_stem)
        .map(|(base, _)| output_path.with_file_name(base))
        .unwrap_or_else(|| output_path.to_path_buf());
    let Ok(image_dir) = youtube_sidecar_path(&base_path, "-images") else {
        return ImagePostLocalFiles::default();
    };
    let Ok(entries) = std::fs::read_dir(image_dir) else {
        return ImagePostLocalFiles::default();
    };
    let mut files = ImagePostLocalFiles::default();
    for path in entries.filter_map(|entry| entry.ok()).map(|entry| entry.path()) {
        if !path.is_file() {
            continue;
        }
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        match extension.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" | "png" | "webp" => files.images.push(path),
            // 合成中转片段（`NN.segment.mp4`）用完即清，不会被当成视频段；
            // 万一残留也按视频段处理，最多只是标记为多段视频。
            "mp4" | "mov" | "webm" => files.has_video_segments = true,
            _ => {}
        }
    }
    files.images.sort();
    files
}

fn youtube_image_post_files(video: &youtube_video::Model) -> ImagePostLocalFiles {
    match video.output_path.as_deref().map(Path::new) {
        Some(output_path) => image_post_local_files(output_path),
        None => ImagePostLocalFiles::default(),
    }
}

fn youtube_image_post_paths(video: &youtube_video::Model) -> Vec<PathBuf> {
    youtube_image_post_files(video).images
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
    // 抖音与 TikTok 的图集原图都存在同一个 `-images` 目录里，两边都要放行，
    // 否则 TikTok 图片贴在前端会显示「查看图片」但每张图都 404。
    if !is_douyin_source(&source) && !crate::tiktok::is_tiktok_source(&source) {
        return Ok(None);
    }
    Ok(youtube_image_post_paths(&video).into_iter().nth(image_index - 1))
}

/// 由现有 `video_downloader` 的同一周期调用。来源扫描和待下载任务都受全局暂停、
/// 下载并发配置及相同的日志/通知周期控制。
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

/// 扫描并下载所有已启用的外源（YouTube / 抖音 / TikTok）。
///
/// 返回本轮每个外源的扫描结果，由调用方与 B 站源合并后一起发「扫描完成」推送，
/// 保证外源与 B 站源的通知行为一致（只挂外源的部署也能收到推送）。
pub async fn process_scheduled_sources(
    db: &DatabaseConnection,
    downloader: Arc<UnifiedDownloader>,
    concurrent_limit: usize,
) -> Result<Vec<SourceScanResult>> {
    let sources = youtube_source::Entity::find()
        .filter(youtube_source::Column::Enabled.eq(true))
        .all(db)
        .await?;
    let mut results: Vec<SourceScanResult> = Vec::new();
    if sources.is_empty() {
        return Ok(results);
    }
    recover_interrupted_downloads(db).await?;
    if let Err(error) = ensure_ytdlp_available().await {
        warn!(error = %error, "已配置 YouTube/抖音视频源，但 yt-dlp 自动安装失败；跳过本轮扫描");
        return Ok(results);
    }
    let startup_round = EXTERNAL_STARTUP_SCAN_PENDING.swap(false, Ordering::SeqCst);
    for source in &sources {
        if TASK_CONTROLLER.is_paused() {
            return Ok(results);
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
        let scan_started_at = now_standard_string();
        // 每个外源都打一条「开始扫描」：以前只有失败/有新视频时才出日志，YouTube
        // 源在「没有新视频」的轮次里完全静默，看起来像没扫。日志沿用项目里其它
        // 外源日志的纯文本风格，不额外挂字段，免得行尾跟一长串 kv 显得乱。
        info!("开始扫描{}视频源「{}」", source_platform_label(source), source.name);
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
        // 外源新增视频并入「扫描完成」推送：与 B 站源共用同一份扫描摘要。
        if let Ok(added) = scan_result.as_ref() {
            info!(
                "{}视频源「{}」扫描完成，新增 {} 个视频",
                source_platform_label(source),
                source.name,
                added
            );
            results.push(collect_external_scan_result(db, source, &scan_started_at, *added).await);
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
        return Ok(results);
    }
    download_pending(db, downloader.clone(), concurrent_limit.max(1)).await?;
    Ok(results)
}

/// 汇总单个外源本轮新增的作品，供「扫描完成」推送使用。
///
/// 外源扫描函数只返回新增数量，这里按 `created_at >= 本轮扫描开始时间` 取回本轮
/// 真正新入库的行，标题/发布时间/链接与 B 站源保持一致的结构。
async fn collect_external_scan_result(
    db: &DatabaseConnection,
    source: &youtube_source::Model,
    scan_started_at: &str,
    added: u64,
) -> SourceScanResult {
    let platform = source_platform_label(source);
    let mut new_videos = Vec::new();
    if added > 0 {
        match youtube_video::Entity::find()
            .filter(youtube_video::Column::SourceId.eq(source.id))
            .filter(youtube_video::Column::CreatedAt.gte(scan_started_at))
            .all(db)
            .await
        {
            Ok(rows) => {
                new_videos = rows
                    .into_iter()
                    .map(|row| NewVideoInfo {
                        title: row.title,
                        bvid: row.youtube_id,
                        pubtime: row.published_at,
                        episode_number: row.episode_number,
                        video_id: None,
                        url: Some(row.url),
                    })
                    .collect();
            }
            Err(error) => {
                warn!(
                    source_id = source.id,
                    error = %error,
                    "读取{}视频源「{}」本轮新增作品失败，推送中仅保留数量",
                    platform,
                    source.name
                );
            }
        }
    }
    SourceScanResult {
        source_type: platform.to_string(),
        source_name: source.name.clone(),
        new_videos,
    }
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

/// 判断本次 YouTube 频道扫描是否只枚举每个标签页最近若干条（增量扫描）。
///
/// 只有「YouTube 频道 + 已有历史记录 + 未开启扫描已删除视频」才返回上限：
/// 新视频总在各标签页顶部，限 500 条足够；用户开启「扫描已删除视频」时必须
/// 全量枚举才能发现被删条目。TikTok / 抖音 / 播放列表等来源行为不变。
fn youtube_incremental_playlist_end(
    source_type: &str,
    known_video_ids_empty: bool,
    scan_deleted_videos: bool,
    is_tiktok: bool,
    is_douyin: bool,
) -> Option<u32> {
    if is_tiktok || is_douyin {
        return None;
    }
    if source_type != "channel" || known_video_ids_empty || scan_deleted_videos {
        return None;
    }
    Some(YOUTUBE_INCREMENTAL_PLAYLIST_END)
}

async fn scan_source(db: &DatabaseConnection, source: &youtube_source::Model) -> Result<u64> {
    if is_douyin_source(source) {
        return scan_douyin_source(db, source).await;
    }
    if let Err(error) = cleanup_external_checked_markers_for_source(db, source).await {
        warn!(source_id = source.id, error = %error, "清理外部平台检查标记失败，继续扫描");
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
    let known_video_ids = parse_video_id_set(source.known_video_ids.as_deref());
    let scan_deleted_videos = source.scan_deleted_videos || source.scan_deleted_videos_once;
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
    append_ytdlp_runtime(&mut command).await;
    if tiktok {
        // 公开内容无需登录；仅在导入过 TikTok cookies 时携带，不混用 YouTube/抖音 Cookie。
        crate::tiktok::append_tiktok_cookies(&mut command);
    } else {
        append_cookies(&mut command);
        append_ytdlp_tab_args(&mut command);
    }
    append_youtube_proxy(&mut command);
    if let Some(playlist_end) = youtube_incremental_playlist_end(
        &source.source_type,
        known_video_ids.is_empty(),
        scan_deleted_videos,
        tiktok,
        is_douyin_source(source),
    ) {
        // `--playlist-end` 对频道根地址是「每个标签页各自生效」：三个 tab 各取最近 N 条。
        command.arg("--playlist-end").arg(playlist_end.to_string());
        info!(
            "YouTube 视频源「{}」已有历史记录，本次仅枚举每个标签页最近 {} 条",
            source.name, playlist_end
        );
    }
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
    let output = tokio::time::timeout(YOUTUBE_SCAN_TIMEOUT, command.output())
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
    let mut deleted_video_ids = parse_video_id_set(source.deleted_video_ids.as_deref());
    let mut scanned_video_ids = HashSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(item) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(youtube_id) = item.get("id").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) else {
            continue;
        };
        if ytdlp_flat_item_is_unavailable(&item) {
            debug!(youtube_id, "跳过 YouTube 列表中的失效/私有视频");
            continue;
        }
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
            platform = source_platform_label(source),
            source_id = source.id,
            added,
            "{}视频源「{}」发现 {} 个新视频",
            source_platform_label(source),
            source.name,
            added
        );
        notify_videos_changed();
        notify_queue_status_changed();
    }
    Ok(added)
}

async fn cleanup_external_checked_markers_for_source(
    db: &DatabaseConnection,
    source: &youtube_source::Model,
) -> Result<()> {
    let videos = youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.eq(source.id))
        .filter(youtube_video::Column::OutputPath.is_not_null())
        .all(db)
        .await?;
    for video in videos {
        let Some(output_path) = video.output_path.as_deref().filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let output_path = Path::new(output_path);
        for suffix in ["subtitle.checked", "danmaku.checked", "live_chat.checked"] {
            let path = output_path.with_extension(suffix);
            if let Err(error) = remove_file_if_exists(&path).await {
                warn!(path = %path.display(), error = %error, "清理外部平台检查标记失败");
            }
        }
    }
    Ok(())
}

async fn scan_douyin_source(db: &DatabaseConnection, source: &youtube_source::Model) -> Result<u64> {
    if let Err(error) = migrate_douyin_sidecars_for_source(db, source).await {
        warn!(
            source_id = source.id,
            error = %error,
            "抖音旧弹幕附属文件迁移失败，继续扫描"
        );
    }
    if is_episodic_douyin_source(source) {
        if let Err(error) = backfill_episodic_sidecars_from_first_video(db, source).await {
            warn!(
                source_id = source.id,
                error = %error,
                "抖音剧集根目录封面/演员信息补全失败，继续扫描"
            );
        }
    }
    if source.source_type == "douyin_collection" {
        if let Err(error) = cleanup_legacy_collection_sidecars_for_source(db, source).await {
            warn!(
                source_id = source.id,
                error = %error,
                "抖音收藏夹旧剧集附属文件批量清理失败，继续扫描"
            );
        }
    }
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
        // 与 YouTube/TikTok 口径保持一致：写清是哪个源、发现了几个作品，
        // 否则日志里只有一句「发现新作品」，看不出归属与数量。
        info!(
            platform = source_platform_label(source),
            source_id = source.id,
            added,
            "{}视频源「{}」发现 {} 个新视频",
            source_platform_label(source),
            source.name,
            added
        );
        notify_videos_changed();
        notify_queue_status_changed();
    }
    Ok(added)
}

async fn migrate_douyin_sidecars_for_source(db: &DatabaseConnection, source: &youtube_source::Model) -> Result<()> {
    if !is_douyin_source(source) {
        return Ok(());
    }
    let videos = youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.eq(source.id))
        .filter(youtube_video::Column::OutputPath.is_not_null())
        .filter(youtube_video::Column::EpisodeNumber.is_not_null())
        .all(db)
        .await?;
    for video in videos {
        let Some(output_path) = video.output_path.as_deref().filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        crate::douyin::migrate_douyin_sidecar_names(Path::new(output_path)).await?;
    }
    Ok(())
}

async fn backfill_episodic_sidecars_from_first_video(
    db: &DatabaseConnection,
    source: &youtube_source::Model,
) -> Result<()> {
    if !is_episodic_douyin_source(source) {
        return Ok(());
    }
    let videos = youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.eq(source.id))
        .filter(youtube_video::Column::OutputPath.is_not_null())
        .order_by_asc(youtube_video::Column::EpisodeNumber)
        .order_by_asc(youtube_video::Column::Id)
        .all(db)
        .await?;
    // 只按「媒体确实还在磁盘上」的分集回填：媒体已删除 / 未下完的分集如果仍留着
    // 旧的 output_path，就会把封面写到已经不用的目录里 —— 重设路径后旧目录被
    // 反复重建、每轮扫描都冒出 fanart/poster 就是这么来的。
    let Some(video) = videos.into_iter().find(|video| {
        video
            .output_path
            .as_deref()
            .is_some_and(|path| !path.trim().is_empty() && Path::new(path).is_file())
    }) else {
        return Ok(());
    };
    let Some(output_path) = video.output_path.as_deref().filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    let output_path = PathBuf::from(output_path);
    let Some(series_dir) = episodic_series_dir(&output_path, &source.path) else {
        return Ok(());
    };
    let thumb = youtube_sidecar_path(&output_path, "-thumb.jpg")?;
    let fanart = youtube_sidecar_path(&output_path, "-fanart.jpg")?;
    for (name, source_path) in [
        ("folder.jpg", &thumb),
        ("poster.jpg", &thumb),
        // 竖图给 poster/folder，横图给 landscape/fanart，避免把竖版封面拉成背景墙
        ("landscape.jpg", &fanart),
        ("fanart.jpg", &fanart),
    ] {
        let target = series_dir.join(name);
        if tokio::fs::metadata(&target)
            .await
            .is_ok_and(|metadata| metadata.len() >= 1024)
            || !tokio::fs::metadata(source_path)
                .await
                .is_ok_and(|metadata| metadata.len() >= 1024)
        {
            // 合集封面已存在，但单集封面更新过（换了封面 / 改了竖图横图映射）时刷新一次，
            // 否则老库里的 poster.jpg 会一直是旧图。
            if let Ok(target_meta) = tokio::fs::metadata(&target).await {
                if target_meta.len() >= 1024 && cover_source_is_newer(source_path, &target_meta).await {
                    let _ = tokio::fs::copy(source_path, &target).await;
                    let _ = ensure_jpeg_image(&target).await;
                }
            }
            continue;
        }
        tokio::fs::copy(source_path, &target)
            .await
            .with_context(|| format!("补全剧集根目录封面失败: {}", target.display()))?;
        if let Err(error) = ensure_jpeg_image(&target).await {
            warn!(path = %target.display(), %error, "剧集根目录封面转 JPEG 失败（文件仍是原始格式）");
        }
    }

    let tvshow_path = series_dir.join("tvshow.nfo");
    let Ok(tvshow) = tokio::fs::read_to_string(&tvshow_path).await else {
        return Ok(());
    };
    if tvshow.contains("<actor>") || video.uploader.trim().is_empty() {
        return Ok(());
    }
    let avatar = tokio::fs::read_to_string(output_path.with_extension("nfo"))
        .await
        .ok()
        .and_then(|nfo| extract_xml_tag_text(&nfo, "thumb"));
    let escape = |value: &str| quick_xml::escape::escape(value).into_owned();
    let thumb = avatar
        .map(|value| format!("<thumb>{}</thumb>", escape(&value)))
        .unwrap_or_default();
    let actor = format!(
        "    <actor><name>{}</name><role>UP主</role>{thumb}</actor>\n",
        escape(video.uploader.trim())
    );
    let Some(season_tag) = tvshow.find("    <season>") else {
        return Ok(());
    };
    let refreshed = format!("{}{}{}", &tvshow[..season_tag], actor, &tvshow[season_tag..]);
    let temporary = unique_download_path(&tvshow_path);
    tokio::fs::write(&temporary, refreshed).await?;
    replace_file(&temporary, &tvshow_path).await?;
    info!(
        source_id = source.id,
        path = %tvshow_path.display(),
        "已为抖音剧集 tvshow.nfo 补全 UP 主演员信息"
    );
    Ok(())
}

fn extract_xml_tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let value = quick_xml::escape::unescape(xml[start..end].trim()).ok()?.into_owned();
    (!value.is_empty()).then_some(value)
}

async fn cleanup_legacy_collection_sidecars_for_source(
    db: &DatabaseConnection,
    source: &youtube_source::Model,
) -> Result<()> {
    if source.source_type != "douyin_collection" {
        return Ok(());
    }
    let videos = youtube_video::Entity::find()
        .filter(youtube_video::Column::SourceId.eq(source.id))
        .filter(youtube_video::Column::OutputPath.is_not_null())
        .all(db)
        .await?;
    for video in videos {
        let Some(output_path) = video.output_path.as_deref().filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        cleanup_legacy_collection_series_sidecars(source, Path::new(output_path)).await?;
    }
    Ok(())
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
    // 只看「来源仍启用」的视频：禁用来源的待下载任务以前也会进循环，而且 TikTok
    // 会先白等 6 秒/条节流再发现来源被禁用（130 条 TikTok 待下载 = 十几分钟静默），
    // 表现就是「扫描完最后一个源之后卡住不动」。
    let enabled_source_ids: HashSet<i32> = youtube_source::Entity::find()
        .filter(youtube_source::Column::Enabled.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|source| source.id)
        .collect();
    let videos = videos
        .into_iter()
        .filter(|video| enabled_source_ids.contains(&video.source_id))
        .collect::<Vec<_>>();
    if videos.is_empty() {
        return Ok(());
    }
    // 下载阶段以前全程无日志：一旦有待下载任务（尤其 TikTok 每条固定 6 秒节流），
    // 日志里就停在最后一个源的「扫描完成」，看起来像卡死。补上开始/结束两条，
    // 下一轮再出问题能直接看出卡在扫描还是下载。
    info!(
        "开始下载外源待处理视频，共 {} 个（并发 {}）",
        videos.len(),
        concurrent_limit
    );
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
            .filter(crate::tiktok::is_tiktok_source)
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
    info!("外源待处理视频下载阶段结束");
    Ok(())
}

/// worker 的“只重跑附属文件”路径（不下载媒体）。
/// 触发条件：媒体位（视频4/分页1）已完成，封面/NFO/UP头像/UP主信息等子任务位需要重跑。
async fn run_youtube_sidecar_tasks(db: &DatabaseConnection, video: youtube_video::Model) -> Result<()> {
    let Some(source) = youtube_source::Entity::find_by_id(video.source_id).one(db).await? else {
        return Ok(());
    };
    let platform = source_platform_label(&source);
    let video_run = VideoStatus::from(video.video_task_status).should_run();
    let page_run = PageStatus::from(video.page_task_status).should_run();
    let mut regen = YoutubeArtifactRegen::default();
    if video_run[0] || page_run[0] {
        regen.cover = true;
    }
    if video_run[1] || page_run[2] {
        regen.nfo = true;
    }
    if video_run[2] {
        regen.upper_face = true;
    }
    if video_run[3] {
        regen.upper_info = true;
    }
    if regen.any() {
        if let Err(error) = regenerate_youtube_artifacts(db, video.clone(), regen).await {
            // 失败落库为 failed，不进入无限重试；用户可再次重置触发。
            let mut active: youtube_video::ActiveModel = video.clone().into();
            active.retry_count = Set(video.retry_count.saturating_add(1));
            active.download_status = Set("failed".to_string());
            active.error_message = Set(Some(format!("{error:#}")));
            active.updated_at = Set(now_standard_string());
            active.update(db).await?;
            notify_videos_changed();
            return Err(error);
        }
    }
    // 成功后：本次需要重建的任务 + 禁用/未实现的可选任务全部置为已完成。
    let mut video_status = VideoStatus::from(video.video_task_status);
    let mut page_status = PageStatus::from(video.page_task_status);
    for index in 0..5 {
        if video_status.get(index) < STATUS_OK {
            video_status.set(index, STATUS_OK);
        }
        if page_status.get(index) < STATUS_OK {
            page_status.set(index, STATUS_OK);
        }
    }
    let video_bits: u32 = video_status.into();
    let page_bits: u32 = page_status.into();
    info!(
        platform,
        source_id = source.id,
        youtube_id = %video.youtube_id,
        "{}视频源「{}」视频「{}」附属文件重建完成",
        platform,
        source.name,
        video.title
    );
    let mut active: youtube_video::ActiveModel = video.clone().into();
    active.video_task_status = Set(video_bits);
    active.page_task_status = Set(page_bits);
    active.download_status = Set(youtube_text_from_task_status(video_bits, page_bits));
    active.retry_count = Set(0);
    active.error_message = Set(None);
    active.updated_at = Set(now_standard_string());
    active.update(db).await?;
    notify_videos_changed();
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

    // B 站式按位调度：媒体（视频4/分页1）已完成、只有封面/NFO/头像等附属子任务
    // 需要重跑时，只重建附属文件，绝不重新下载媒体 —— 媒体被移走的已完成视频
    // 也不会被重新拉回下载队列。
    let video_status = VideoStatus::from(video.video_task_status);
    let page_status = PageStatus::from(video.page_task_status);
    let video_run = video_status.should_run();
    let page_run = page_status.should_run();
    let media_pending = video_run[4] || page_run[1];
    let sidecar_pending = video_run[0]
        || video_run[1]
        || video_run[2]
        || video_run[3]
        || page_run[0]
        || page_run[2]
        || page_run[3]
        || page_run[4];
    if !media_pending && sidecar_pending {
        return run_youtube_sidecar_tasks(db, video).await;
    }
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
        // 整次尝试套一层硬超时：任何环节卡死都不能让任务永久停在「下载中」。
        // 计时用于「下载完成」日志里的耗时/平均速度，与 B 站保持同一模板。
        let attempt_started = Instant::now();
        let result = match tokio::time::timeout(
            EXTERNAL_MEDIA_ATTEMPT_TIMEOUT,
            download_youtube_media(downloader, &source, &video),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow!(
                "{}视频下载超时：{} 分钟内没有任何结果，已中止本次尝试（请检查网络/代理，或稍后重试）",
                platform_label,
                EXTERNAL_MEDIA_ATTEMPT_TIMEOUT.as_secs() / 60
            )),
        };
        let mut active: youtube_video::ActiveModel = video.clone().into();
        active.updated_at = Set(now_standard_string());
        match result {
            Ok(mut downloaded) => {
                if downloaded.paid_content {
                    // 与 B 站充电视频一致：标记为付费/加密且当前不可播放，生成 0 字节
                    // 占位文件并把下载状态记为已完成（UI 显示充电视频徽标而非失败）。
                    let mut paid: youtube_video::ActiveModel = video.clone().into();
                    paid.video_task_status = Set(youtube_all_tasks_ok_bits());
                    paid.page_task_status = Set(youtube_all_tasks_ok_bits());
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
                    skipped.video_task_status = Set(youtube_all_tasks_ok_bits());
                    skipped.page_task_status = Set(youtube_all_tasks_ok_bits());
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
                // 关闭图文视频合成后没有主媒体文件，只保留原图目录；此时跳过
                // 单文件 AI 重命名，避免对不存在的 MP4 做改名。
                if source.ai_rename
                    && crate::config::reload_config().ai_rename.enabled
                    && downloaded.output_path.is_file()
                {
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
                active.video_task_status = Set(youtube_all_tasks_ok_bits());
                active.page_task_status = Set(youtube_all_tasks_ok_bits());
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
                // 与 B 站「视频下载完成，总大小: x MB，耗时: y 秒，平均速度: z MB/s」
                // 使用同一套模板；外源多平台并发，前面保留来源/标题便于定位。
                let download_type = if source.audio_only { "音频" } else { "视频" };
                let total_bytes = external_output_total_bytes(&downloaded.output_path).await;
                let elapsed_secs = attempt_started.elapsed().as_secs_f64();
                if total_bytes > 0 && elapsed_secs > 0.0 {
                    let (speed, unit) = format_transfer_speed(total_bytes as f64 / elapsed_secs);
                    info!(
                        platform = platform_label,
                        source_id = source.id,
                        youtube_id = %video.youtube_id,
                        path = %downloaded.output_path.display(),
                        "{}视频源「{}」{}「{}」下载完成，总大小: {:.2} MB，耗时: {:.2} 秒，平均速度: {:.2} {}",
                        platform_label,
                        source.name,
                        download_type,
                        downloaded_title,
                        total_bytes as f64 / 1_048_576.0,
                        elapsed_secs,
                        speed,
                        unit
                    );
                } else {
                    info!(
                        platform = platform_label,
                        source_id = source.id,
                        youtube_id = %video.youtube_id,
                        path = %downloaded.output_path.display(),
                        "{}视频源「{}」{}「{}」下载完成",
                        platform_label,
                        source.name,
                        download_type,
                        downloaded_title
                    );
                }
                active.update(db).await?;
                notify_videos_changed();
                notify_queue_status_changed();
                return Ok(());
            }
            Err(error) => {
                let retry_count = video.retry_count.saturating_add(1);
                let error_text = error.to_string();
                let permanent_failure = error_text.contains("CENC 加密 DASH") || error_text.contains("付费/加密内容");
                let exhausted = permanent_failure || retry_count >= MAX_DOWNLOAD_RETRIES;
                // 与 B 站一致：把失败次数写进「媒体」子任务的状态位（视频级 index 4、
                // 分页级 index 1）。只改 download_status 字符串的话，卡片右上角读到的
                // 状态位仍是 0，外源重试失败后会一直显示灰色的「进行中」，看不出已经失败。
                // 状态位写满 MAX_DOWNLOAD_RETRIES 后 check_continue 自然为 false，重试节奏
                // 仍由 retry_count / exhausted 决定，重置逻辑也能正确识别「失败」项。
                let failure = if exhausted {
                    MAX_DOWNLOAD_RETRIES as u32
                } else {
                    retry_count.clamp(1, MAX_DOWNLOAD_RETRIES) as u32
                };
                let mut video_status = VideoStatus::from(video.video_task_status);
                let mut page_status = PageStatus::from(video.page_task_status);
                video_status.set(4, failure);
                page_status.set(1, failure);
                active.video_task_status = Set(video_status.into());
                active.page_task_status = Set(page_status.into());
                active.retry_count = Set(retry_count);
                active.download_status = Set(if exhausted { "failed" } else { "pending" }.to_string());
                active.error_message = Set(Some(format!("{:#}", error)));
                video = active.update(db).await?;
                // 前两次重试失败只记 debug，第三次起才报 WARN，避免每轮重试都刷屏。
                if retry_count >= 3 {
                    // `{:#}` 才会带出完整错误链（外层「统一下载器下载失败」→ 内层
                    // HTTP 状态/网络错误），否则日志里只剩一句没有信息量的包装文案。
                    let chained = format!("{:#}", error);
                    warn!(
                        platform = platform_label,
                        source_id = source.id,
                        retry_count,
                        max_retries = MAX_DOWNLOAD_RETRIES,
                        error = %chained,
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
                // 抖音/TikTok 的失败大多是 CDN 限速，窗口是按时间恢复的：5 秒一次的
                // 重试只会继续挨打，改成 30s 起的阶梯退避。
                let retry_delay = if is_douyin_source(&source) || crate::tiktok::is_tiktok_source(&source) {
                    Duration::from_secs((retry_count.max(1) as u64) * 30)
                } else if is_youtube_bot_wall_error(&error) {
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

/// 外源「下载完成」日志用的总大小：普通视频/音频取文件大小，图文作品取目录内
/// 全部文件之和（没有合成 MP4 时目录里只有原图与配乐）。
async fn external_output_total_bytes(path: &Path) -> u64 {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if let Ok(metadata) = entry.metadata().await {
                total += metadata.len();
            }
        }
    }
    total
}

/// 与 B 站下载日志一致的速度单位选择（MB/s → KB/s → B/s）。
fn format_transfer_speed(bytes_per_second: f64) -> (f64, &'static str) {
    if bytes_per_second >= 1_048_576.0 {
        (bytes_per_second / 1_048_576.0, "MB/s")
    } else if bytes_per_second >= 1_024.0 {
        (bytes_per_second / 1_024.0, "KB/s")
    } else {
        (bytes_per_second, "B/s")
    }
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

/// 多段动态作品的 AI 重命名：整篇一起改名，分页编号与 `-images` 原图目录一起跟随。
///
/// 只改第一页会立刻把分页家族打散（其余分页、缩略图、NFO 全变孤儿，前端也再
/// 认不出这是一篇作品），所以这里按「作品名」向 AI 取名，再给每一页补回编号。
/// 返回 `None` 表示这不是多页作品，交给原来的单文件流程处理。
async fn ai_rename_external_pages(
    source: &youtube_source::Model,
    video: &youtube_video::Model,
    downloaded: &DownloadedYouTubeMedia,
    video_prompt: Option<&str>,
    audio_prompt: Option<&str>,
) -> Result<Option<PathBuf>> {
    let pages = crate::douyin::image_post_page_paths(&downloaded.output_path);
    if pages.len() < 2 {
        return Ok(None);
    }
    let platform = source_platform_label(source);
    let config = crate::config::reload_config().ai_rename;
    let mut file = external_ai_file(source, video, downloaded, 1)?;
    let Some((base_stem, _)) = crate::douyin::split_image_post_page_stem(&file.current_stem) else {
        return Ok(None);
    };
    let old_base = base_stem.to_string();
    file.current_stem = old_base.clone();
    let prompt = if audio_prompt.is_some_and(|value| !value.trim().is_empty()) && file.ctx.is_audio {
        audio_prompt.unwrap_or_default()
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
    let Some(new_base) = names
        .first()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if new_base == old_base {
        return Ok(None);
    }
    let Some(parent) = downloaded.output_path.parent() else {
        return Ok(None);
    };
    let ext = file.ext.clone();
    let mut primary: Option<PathBuf> = None;
    for (index, page) in pages.iter().enumerate() {
        let page_stem = format!("{new_base}-P{:02}", index + 1);
        let new_path = parent.join(format!("{page_stem}.{ext}"));
        if *page != new_path {
            if let Err(error) = std::fs::rename(page, &new_path) {
                warn!(platform, path = %page.display(), %error, "{}图集分页 AI 重命名失败", platform);
                if primary.is_none() {
                    primary = Some(page.clone());
                }
                continue;
            }
            if let Err(error) = crate::utils::ai_rename::rename_sidecars(page, &page_stem, &ext) {
                warn!(platform, %error, "{}图集分页侧车文件 AI 重命名失败", platform);
            }
            if let Err(error) = crate::utils::ai_rename::update_nfo_content(&new_path.with_extension("nfo"), &page_stem)
            {
                warn!(platform, %error, "{}图集分页 NFO 更新失败", platform);
            }
        }
        if primary.is_none() {
            primary = Some(new_path);
        }
    }
    let old_image_dir = parent.join(format!("{old_base}-images"));
    let new_image_dir = parent.join(format!("{new_base}-images"));
    if old_image_dir.is_dir() && old_image_dir != new_image_dir {
        if let Err(error) = std::fs::rename(&old_image_dir, &new_image_dir) {
            warn!(platform, %error, "{}图集原图目录 AI 重命名失败", platform);
        }
    }
    Ok(primary)
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
    // 多段动态作品按整篇改名，保持 `<作品>-P01.mp4` 的分页编号。
    if let Some(renamed) = ai_rename_external_pages(source, video, downloaded, video_prompt, audio_prompt).await? {
        return Ok(renamed);
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
    if is_episodic_douyin_source(source) {
        let skipped = youtube_video::Entity::find()
            .filter(youtube_video::Column::SourceId.eq(source.id))
            .filter(youtube_video::Column::DownloadStatus.eq("completed"))
            .count(db)
            .await
            .unwrap_or(0);
        info!(
            "{}剧集源「{}」跳过历史 AI 重命名（保持 S01E01/Season 结构），跳过 {} 个已完成文件",
            platform, source.name, skipped
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

/// 是否应把本轮 TikTok 失败直接落成「无法下载视频」占位（而不是继续普通重试）。
///
/// 分两类：
///  · 官方详情返回 statusCode≠0（地区/内容不可用）：服务端已确认拿不到，立刻落占位；
///  · 官方 embed 只回作品元数据、媒体字段全空：返回随出口地区变化，换节点可能又能
///    取到，所以前几轮仍按普通失败重试，连续失败到阈值才落占位，避免刚换节点就被判死。
fn should_placeholder_unavailable_tiktok(error: &anyhow::Error, retry_count: i32) -> bool {
    crate::tiktok::is_tiktok_unavailable_error(error)
        || (crate::tiktok::is_tiktok_media_missing_error(error)
            && retry_count >= TIKTOK_MEDIA_MISSING_PLACEHOLDER_RETRY)
}

async fn download_youtube_media(
    downloader: &UnifiedDownloader,
    source: &youtube_source::Model,
    video: &youtube_video::Model,
) -> Result<DownloadedYouTubeMedia> {
    let platform = source_platform_label(source);
    let mut metadata = match extract_youtube_metadata(&video.url, Some(source)).await {
        Ok(metadata) => metadata,
        Err(error) if should_placeholder_unavailable_tiktok(&error, video.retry_count) => {
            // TikTok「明确不可下载」分两类（判定见 should_placeholder_unavailable_tiktok）：
            //  · 官方详情返回 statusCode≠0（地区/内容不可用）；
            //  · 官方 embed 只回作品元数据、媒体字段全空（视频/封面/原图/配乐都取不到）。
            //    后者的结果会随出口地区变化（换节点后可能又能取到），所以前几轮仍按普通
            //    失败正常重试，连续失败到阈值才落占位，避免刚换节点就被判死。
            // 两者都与付费/加密占位方案一致：生成占位文件并标记为不可下载，避免反复重试。
            let media_missing = crate::tiktok::is_tiktok_media_missing_error(&error);
            warn!(
                platform,
                source_id = source.id,
                youtube_id = %video.youtube_id,
                %error,
                "{}视频「{}」明确无法下载（{}），生成占位并停止重试",
                platform,
                video.title,
                if media_missing { "媒体内容不可用" } else { "地区/内容不可用" }
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
                fanart: None,
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
                slides: Vec::new(),
            };
            let placeholder_title = video.title.clone();
            let placeholder_uploader = if video.uploader.trim().is_empty() {
                source.name.clone()
            } else {
                video.uploader.clone()
            };
            let output_path = youtube_output_path(
                source,
                video,
                &fallback_metadata,
                &placeholder_title,
                &placeholder_uploader,
            )?;
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
                warning_message: Some(if media_missing {
                    "无法下载视频：作品的媒体内容不可用（视频、封面、原图均取不到）".to_string()
                } else {
                    "无法下载视频：你所在国家或地区无法下载此视频".to_string()
                }),
                paid_content: true,
                skipped: false,
            });
        }
        Err(error) => return Err(error),
    };
    let filter_option = source
        .filter_option
        .as_ref()
        .map(|value| serde_json::from_value::<FilterOption>(value.clone()))
        .transpose()
        .with_context(|| format!("{platform}视频源级流过滤设置无效"))?
        .unwrap_or_else(|| crate::config::reload_config().filter_option);
    // issue #294：YouTube 带 Cookie 时可能只解析到 HLS + 360p 混合流，可下载画质达不到
    // 用户设置的最低画质；此时去掉 Cookie 匿名再解析一次，匿名结果严格更高才采用
    // （匿名失败继续用原结果，私有来源不匿名）。
    metadata = apply_youtube_anonymous_quality_fallback(metadata, &video.url, source, &filter_option, platform).await;
    let title = metadata.title.clone().unwrap_or_else(|| video.title.clone());
    let uploader = metadata
        .uploader
        .clone()
        .or_else(|| metadata.channel.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| source.name.clone());
    let mut output_path = youtube_output_path(source, video, &metadata, &title, &uploader)?;
    info!(
        platform,
        source_id = source.id,
        youtube_id = %video.youtube_id,
        "{}视频源「{}」正在处理视频「{}」",
        platform,
        source.name,
        title
    );
    let mut media_exists = is_reusable_media_file(&output_path, metadata.duration).await;
    // 关闭图文视频后，重置/重新处理旧作品时应清掉上一版生成的幻灯片 MP4；
    // 混排作品含真实视频段，不走这里的清理。
    if media_exists
        && metadata.is_slideshow()
        && !source.generate_image_post_video
        && !metadata
            .slides
            .iter()
            .any(crate::external_media::ExternalImageSlide::is_video)
    {
        remove_file_if_exists(&output_path).await?;
        media_exists = false;
    }
    if media_exists {
        info!(path = %output_path.display(), "{}视频源「{}」视频「{}」目标文件已存在，复用现有文件", platform, source.name, title);
    } else {
        // 上次 ffmpeg 被中断时可能留下非零但不可播放的最终文件，不能仅凭文件
        // 大小就把任务标记为完成。
        remove_file_if_exists(&output_path).await?;
    }
    let selected = if !metadata.is_slideshow() {
        let selected = select_youtube_streams(&metadata.formats, &filter_option, source.audio_only, platform)?;
        log_selected_youtube_streams(&selected, &filter_option, source, &title);
        warn_youtube_below_min_quality(&selected, &filter_option, source, &title);
        Some(selected)
    } else {
        None
    };

    // 未达到设定的最低分辨率时主动跳过下载，不产生失败/错误任务。
    if !media_exists && metadata.is_slideshow() && !source.audio_only {
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
    } else if metadata.is_slideshow() {
        // 图集作品里的视频段会各自成页，主媒体文件（第一个分页）由下载函数返回：
        // 封面、NFO、字幕等附属文件都跟着这个主文件走。
        output_path =
            crate::douyin::download_image_post(downloader, &metadata, &output_path, &filter_option, source).await?;
    } else if source.audio_only {
        let selected = selected.as_ref().context("图文作品不应进入音频流选择")?;
        if let Some(audio) = selected.audio.as_ref() {
            let urls = format_urls(audio, platform)?;
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
                return paid_placeholder_result(output_path, &metadata, title, uploader, platform).await;
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
            let urls = format_urls(mixed, platform)?;
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
                return paid_placeholder_result(output_path, &metadata, title, uploader, platform).await;
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
        let video_urls = format_urls(video_stream, platform)?;
        let audio_urls = format_urls(audio_stream, platform)?;
        let video_temporary =
            output_path.with_extension(format!("video.{}", video_stream.ext.as_deref().unwrap_or("mp4")));
        let audio_temporary =
            output_path.with_extension(format!("audio.{}", audio_stream.ext.as_deref().unwrap_or("m4a")));
        let merge_temporary = output_path.with_extension("merging.mp4");
        // YouTube 的高画质通常是独立 DASH 音视频流。两条直链都交给项目
        // 原生统一下载器并行传输；音频失败时保留已下载的视频临时文件，
        // 下次重试只重新下载音频，避免大视频（数百 MB）整片重下。
        let video_ready = is_reusable_media_file(&video_temporary, metadata.duration).await;
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
            if let Err(error) = fetch_platform_asset(downloader, source, &audio_urls, &audio_temporary).await {
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
        validate_media_duration(&video_temporary, metadata.duration, "视频流").await?;
        validate_media_duration(&audio_temporary, metadata.duration, "音频流").await?;
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
        if let Err(error) = validate_media_duration(&merge_temporary, metadata.duration, "合并文件").await {
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
        let urls = format_urls(mixed, platform)?;
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
            return paid_placeholder_result(output_path, &metadata, title, uploader, platform).await;
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

    if source.source_type == "douyin_collection" {
        if let Err(error) = cleanup_legacy_collection_series_sidecars(source, &output_path).await {
            warn!(platform, %error, "抖音收藏夹旧剧集附属文件清理失败");
        }
    }
    let sidecars = if source.audio_only && source.audio_only_m4a_only && !metadata.is_slideshow() {
        YoutubeSidecarOutcome::default()
    } else {
        ensure_youtube_sidecars(YoutubeSidecarInput {
            downloader,
            metadata: &metadata,
            output_path: &output_path,
            video_url: &video.url,
            title: &title,
            uploader: &uploader,
            source,
            episode_number: video.episode_number,
        })
        .await
    };

    // 多段动态作品的每个分页都要有自己的封面与信息文件，媒体库才会逐段显示。
    ensure_image_post_page_sidecars(
        &metadata,
        &output_path,
        &video.url,
        &title,
        &uploader,
        sidecars.avatar_url.as_deref(),
        source,
    )
    .await;

    // 图集判定要在 `metadata` 被部分移动前取好。
    let is_image_post = metadata.is_slideshow();
    Ok(DownloadedYouTubeMedia {
        output_path,
        title,
        uploader,
        thumbnail: metadata.thumbnail,
        published_at: metadata.upload_date,
        duration_seconds: metadata
            .duration
            .and_then(|value| i32::try_from(value.round() as i64).ok()),
        is_image_post,
        warning_message: sidecars.warning,
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
    let result = crate::utils::process::tokio_command(crate::downloader::resolve_media_tool_path("ffmpeg"))
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

fn media_duration_is_complete(actual: f64, expected: f64) -> bool {
    if !expected.is_finite() || expected <= 0.0 {
        return true;
    }
    if !actual.is_finite() || actual <= 0.0 {
        return false;
    }
    let tolerance = (expected * 0.02).max(2.0);
    actual + tolerance >= expected
}

async fn probe_media_duration(path: &Path) -> Option<f64> {
    let output = crate::utils::process::tokio_command(crate::downloader::resolve_media_tool_path("ffprobe"))
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
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().parse::<f64>().ok())
}

async fn validate_media_duration(path: &Path, expected_duration: Option<f64>, label: &str) -> Result<()> {
    let Some(expected_duration) = expected_duration.filter(|duration| *duration > 0.0) else {
        return Ok(());
    };
    let Some(actual_duration) = probe_media_duration(path).await else {
        warn!(path = %path.display(), label, "无法读取媒体时长，跳过时长校验");
        return Ok(());
    };
    ensure!(
        media_duration_is_complete(actual_duration, expected_duration),
        "{label}时长不完整: 实际 {:.1} 秒，预期 {:.1} 秒（{}）",
        actual_duration,
        expected_duration,
        path.display()
    );
    Ok(())
}

async fn is_reusable_media_file(path: &Path, expected_duration: Option<f64>) -> bool {
    if !tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024)
    {
        return false;
    }
    let Some(actual_duration) = probe_media_duration(path).await else {
        return false;
    };
    if let Some(expected_duration) = expected_duration.filter(|duration| *duration > 0.0) {
        if !media_duration_is_complete(actual_duration, expected_duration) {
            warn!(
                path = %path.display(),
                actual_duration,
                expected_duration,
                "已有媒体文件时长明显短于平台元数据，判定为不完整并重新下载"
            );
            return false;
        }
    }
    true
}

/// 判断解析结果里是否存在真正可下载的视频流。
///
/// TikTok 图片贴（photo post，也就是幻灯片）经 yt-dlp 解析后只剩一条
/// `vcodec=none` 的配乐音轨，拿它去走视频选流必然失败；调用方据此把这类作品
/// 分流到图文链路（下载原图与配乐并合成幻灯片 MP4）。
fn external_metadata_has_video_stream(metadata: &ExternalMediaMetadata) -> bool {
    metadata.formats.iter().any(|format| {
        format.vcodec.as_deref().is_some_and(|codec| {
            let codec = codec.trim();
            !codec.is_empty() && !codec.eq_ignore_ascii_case("none")
        }) && format.url.as_deref().is_some_and(|url| !url.is_empty())
    })
}

async fn extract_youtube_metadata(url: &str, source: Option<&youtube_source::Model>) -> Result<ExternalMediaMetadata> {
    if source.is_some_and(is_douyin_source) || url.contains("douyin.com") {
        let aweme_id = crate::douyin::aweme_id(url).context("抖音作品链接缺少有效作品 ID")?;
        return crate::douyin::extract_metadata(aweme_id, source).await;
    }
    if url.contains("tiktok.com") {
        // 官方详情接口优先：一次请求就能拿到 playAddr / 图片贴原图；而 yt-dlp 的
        // 网页解析在 TikTok 上经常被 WAF 打回 403/验证页（Unable to extract
        // universal data for rehydration），先跑它等于每次先白等一次必败请求。
        let mut api_error = if crate::tiktok::has_tiktok_login() && !crate::tiktok::tiktok_detail_api_in_cooldown() {
            // 只试一次：空响应（BotGuard）时不重签，立刻交给 yt-dlp，避免每个视频白等十几秒。
            match crate::tiktok::extract_tiktok_media_detail_quick(url).await {
                Ok(metadata) => return Ok(metadata),
                Err(error) => {
                    debug!(
                        error = %error,
                        url = %url,
                        "TikTok 官方详情接口未返回直链，回退 yt-dlp 网页解析"
                    );
                    error
                }
            }
        } else if crate::tiktok::has_tiktok_login() {
            anyhow!("官方详情接口近期连续拿不到直链（冷却中），本轮改走 yt-dlp 网页解析")
        } else {
            // 没有登录凭证时详情接口必然失败（Cookie 是它的前提），直接交给 yt-dlp。
            anyhow!("尚未导入 TikTok 登录凭证，跳过官方详情接口")
        };
        // 服务端已明确判定「地区/内容不可用」（statusCode 非 0）：网页解析同样拿不到，直接返回。
        if crate::tiktok::is_tiktok_unavailable_error(&api_error) {
            return Err(api_error);
        }
        // TikWM 备用解析：完全不经过 TikTok 的 API（由对方服务端代拉作品），
        // 所以不受本机出口 IP 信誉、签名、msToken 影响，是当前最稳的媒体来源。
        match crate::tiktok::fetch_tiktok_video_via_tikwm(url).await {
            Ok(Some(metadata)) => {
                info!("TikTok 媒体直链由备用解析服务（TikWM）提供：{url}");
                return Ok(metadata);
            }
            Ok(None) => {}
            Err(error) => {
                debug!(error = %error, url = %url, "TikWM 备用解析失败，继续 yt-dlp 网页解析")
            }
        }
        match extract_ytdlp_metadata(url, "TikTok").await {
            // 正常视频：yt-dlp 给出可下载的视频格式，直接使用。
            Ok(metadata) if external_metadata_has_video_stream(&metadata) => return Ok(metadata),
            ytdlp_result => {
                // 落到这里的两种情况：yt-dlp 只解析出配乐音轨（图片贴 / 幻灯片），
                // 或 yt-dlp 直接失败（rehydration 报错等）。
                //
                // 先用 TikTok 官方 embed 接口（无需登录/签名）探测：
                //  · 图片作品 → 直接拿原图与配乐；
                //  · 媒体字段为空 → 通常是对当前出口地区不可见，换节点后可能可用，
                //    因此只记下线索、继续走后面的兜底，绝不判定为永久不可下载；
                //  · 仍有内容的普通视频 → 记下直链，作为最后兜底。
                let mut embed_video_fallback: Option<Box<ExternalMediaMetadata>> = None;
                let mut embed_media_invisible = false;
                match crate::tiktok::probe_tiktok_embed_media(url).await {
                    Ok(crate::tiktok::TikTokEmbedMedia::Photo(photo)) => return Ok(*photo),
                    Ok(crate::tiktok::TikTokEmbedMedia::Video(metadata)) => {
                        embed_video_fallback = Some(metadata);
                    }
                    Ok(crate::tiktok::TikTokEmbedMedia::MediaUnavailable) => {
                        embed_media_invisible = true;
                    }
                    Err(error) => {
                        debug!(error = %error, url = %url, "TikTok 官方 embed 接口探测失败，继续其它解析路径")
                    }
                }
                // 详情接口再走一次完整重签重试：第一轮的空响应通常已经带回了新的
                // msToken，重新签名后有机会直接拿到 playAddr（比 embed 直链更权威）。
                if crate::tiktok::has_tiktok_login() && !crate::tiktok::tiktok_detail_api_in_cooldown() {
                    match crate::tiktok::extract_tiktok_media_detail(url).await {
                        Ok(metadata) => return Ok(metadata),
                        Err(error) => {
                            debug!(
                                error = %error,
                                url = %url,
                                "TikTok 官方详情接口重签重试仍失败"
                            );
                            api_error = error;
                            if crate::tiktok::is_tiktok_unavailable_error(&api_error) {
                                return Err(api_error);
                            }
                        }
                    }
                }
                let tikwm_note = match crate::tiktok::fetch_tiktok_photo_via_tikwm(url).await {
                    Ok(Some(photo)) => return Ok(photo),
                    Ok(None) => {
                        "TikWM 没有返回原图（该作品可能不是图片作品，或已被删除/设为私密/受地区限制）".to_string()
                    }
                    Err(error) => {
                        debug!(error = %error, url = %url, "TikWM 图片贴解析失败，继续按视频解析结果处理");
                        format!("TikWM 解析服务请求失败：{error:#}")
                    }
                };
                // 兜底：embed 接口给了可下载的视频直链（详情接口与 yt-dlp 都没成功时使用）。
                if let Some(metadata) = embed_video_fallback {
                    warn!(
                        url = %url,
                        "官方详情接口与 yt-dlp 都未解析出 TikTok 媒体直链，改用官方 embed 接口给出的直链下载"
                    );
                    return Ok(*metadata);
                }
                // 官方 embed 已经明确“只有元数据、没有任何媒体”：无论 yt-dlp 这一轮
                // 是直接报错还是只解析出配乐音轨，都统一给出可操作的原因。否则用户看到
                // 的会是 yt-dlp 的底层报错（同一作品的返回会在“报错/只回音轨”之间浮动），
                // 既看不出真正原因，也无法被「连续重试到阈值后落占位」的判定稳定识别。
                if embed_media_invisible {
                    let ytdlp_note = match &ytdlp_result {
                        Ok(_) => "yt-dlp 只解析出配乐音轨，没有任何视频流".to_string(),
                        Err(error) => format!("yt-dlp 报错：{error}"),
                    };
                    bail!(
                        "TikTok 作品的媒体内容取不到：官方 embed 接口只返回了作品信息（标题/作者/播放量等），没有返回任何视频、封面或原图（官方详情接口：{api_error}；{ytdlp_note}）。常见原因是作品已被作者删除/设为私密、被平台下架，或对当前出口地区不可见（这几种情况在服务端无法区分）。建议更换外源代理节点后重试，或在视频管理页对该视频手动重置后再试。"
                    );
                }
                match ytdlp_result {
                    // 只有配乐、又没有取到原图：给出明确原因，不再让它落到
                    // “没有可用的视频流”这种含糊报错上。
                    Ok(_) => bail!(
                        "这个 TikTok 作品没有可下载的媒体：它不是普通视频（yt-dlp 只解析出配乐音轨，没有任何视频流），按图片作品（幻灯片）取原图也没成功——{tikwm_note}。官方详情接口反馈：{api_error}"
                    ),
                    Err(ytdlp_error) => bail!(
                        "TikTok 媒体直链解析失败：官方详情接口 {api_error}；yt-dlp 网页解析 {ytdlp_error}（通常是当前出口 IP 被 TikTok 风控，请更换外源代理节点后重试）"
                    ),
                }
            }
        }
    }
    extract_ytdlp_metadata(url, "YouTube").await
}

pub(crate) async fn extract_ytdlp_metadata(url: &str, platform: &str) -> Result<ExternalMediaMetadata> {
    extract_ytdlp_metadata_with_cookies(url, platform, true).await
}

/// 解析外源媒体元数据；`with_cookies` 决定是否把当前凭证（YouTube Cookie 等）传给 yt-dlp。
///
/// 匿名解析（`with_cookies = false`）只用于 YouTube 画质兜底（issue #294）：带 Cookie 时
/// YouTube 可能只返回 HLS + 低分辨率混合流，匿名解析反而能拿到 http/https 分离流。
async fn extract_ytdlp_metadata_with_cookies(
    url: &str,
    platform: &str,
    with_cookies: bool,
) -> Result<ExternalMediaMetadata> {
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
    let metadata = run_ytdlp_metadata(url, platform, client_args, with_cookies).await?;
    // web_embedded 对部分视频（尤其需要 PO Token/登录会话的高码率源）只返回
    // ≤1080p 格式。此时用备用客户端再解析一次（tv_embedded/tv 为传统兜底，
    // visionos 提供 4K 直链），取分辨率上限更高的一份，避免“设置 4K 却只能
    // 下到 1080p”。仅对最高不足 4K 的情况触发。
    if is_youtube {
        let best = metadata;
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
                    with_cookies,
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
static YOUTUBE_FALLBACK_TRIED: LazyLock<StdMutex<HashSet<String>>> = LazyLock::new(|| StdMutex::new(HashSet::new()));

/// 记录本进程内已经尝试过「匿名兜底解析」的视频 URL。
///
/// 与 `YOUTUBE_FALLBACK_TRIED`（多客户端解析兜底）分开记录，互不影响；每个 URL 只重试
/// 一次，避免同一个视频反复触发额外解析请求（YouTube 会按出口 IP 累计风控分）。
static YOUTUBE_ANONYMOUS_TRIED: LazyLock<StdMutex<HashSet<String>>> = LazyLock::new(|| StdMutex::new(HashSet::new()));

/// 匿名解析结果缓存（issue #294 后续修复）：下载阶段失败后进程内重试时，不应再退回带
/// Cookie 解析出的退化格式（例如只剩 360p），这里缓存每个视频最近一次匿名解析出的格式。
///
/// 只在「带 Cookie 可下载画质低于最低画质」时才读取；缓存最多保留
/// `YOUTUBE_ANONYMOUS_CACHE_CAPACITY` 个条目，超出后按插入顺序淘汰最旧的，长时间运行
/// 也不会无限增长。
///
/// 缓存里存的是 googlevideo 直链（`expire`/`sig` 通常 6 小时后过期），所以条目还带写入
/// 时间，读取时超过 `YOUTUBE_ANONYMOUS_CACHE_TTL` 就视为未命中并移除 —— 不能拿几小时前
/// 的直链去下载（会 403），也不能因为缓存命中就拒绝重新匿名解析。
struct CachedAnonymousFormats {
    formats: Vec<ExternalMediaFormat>,
    /// 写入序号，用于缓存满时找最旧的条目（`HashMap` 本身不保证顺序）。
    seq: u64,
    /// 写入时刻，用于 TTL 淘汰。
    cached_at: Instant,
}

/// 匿名解析结果缓存的容量上限：长跑进程会看很多视频，只保留最近解析过的若干个。
const YOUTUBE_ANONYMOUS_CACHE_CAPACITY: usize = 64;

/// 匿名解析结果缓存的有效期：下载失败重试通常发生在分钟级，30 分钟足够覆盖重试窗口，
/// 又能避开 googlevideo 直链过期（`expire`/`sig` 一般 6 小时），不会复用已失效的直链。
const YOUTUBE_ANONYMOUS_CACHE_TTL: Duration = Duration::from_secs(30 * 60);

static YOUTUBE_ANONYMOUS_CACHE: LazyLock<StdMutex<HashMap<String, CachedAnonymousFormats>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// 缓存写入序号，只增不减，配合 `CachedAnonymousFormats::seq` 找最旧条目。
static YOUTUBE_ANONYMOUS_CACHE_SEQ: AtomicU64 = AtomicU64::new(1);

/// 读取某个视频缓存的匿名解析格式（克隆一份，避免调用方长时间持锁）。
///
/// 超过 `YOUTUBE_ANONYMOUS_CACHE_TTL` 的条目视为未命中并立即移除。
fn youtube_anonymous_cached_formats(url: &str) -> Option<Vec<ExternalMediaFormat>> {
    let mut cache = YOUTUBE_ANONYMOUS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    read_youtube_anonymous_cache(&mut cache, url, Instant::now())
}

/// 在给定「当前时刻」下读取缓存；过期的条目直接移除。
///
/// 单独拆成接收 `&mut HashMap` 与 `now` 的纯函数，方便单测注入时间验证 TTL，
/// 不依赖进程级全局状态。
fn read_youtube_anonymous_cache(
    cache: &mut HashMap<String, CachedAnonymousFormats>,
    url: &str,
    now: Instant,
) -> Option<Vec<ExternalMediaFormat>> {
    let expired = cache
        .get(url)
        .is_some_and(|cached| now.saturating_duration_since(cached.cached_at) > YOUTUBE_ANONYMOUS_CACHE_TTL);
    if expired {
        cache.remove(url);
        return None;
    }
    cache.get(url).map(|cached| cached.formats.clone())
}

/// 把新条目写进缓存；缓存满时按插入顺序淘汰最旧的条目。
///
/// 单独拆成接收 `&mut HashMap` 的函数，方便单测在局部缓存上验证淘汰逻辑，
/// 不依赖进程级全局状态。
fn insert_youtube_anonymous_cache(
    cache: &mut HashMap<String, CachedAnonymousFormats>,
    url: &str,
    formats: Vec<ExternalMediaFormat>,
) {
    if !cache.contains_key(url) && cache.len() >= YOUTUBE_ANONYMOUS_CACHE_CAPACITY {
        let oldest = cache
            .iter()
            .min_by_key(|(_, cached)| cached.seq)
            .map(|(oldest_url, _)| oldest_url.clone());
        if let Some(oldest) = oldest {
            cache.remove(&oldest);
        }
    }
    let seq = YOUTUBE_ANONYMOUS_CACHE_SEQ.fetch_add(1, Ordering::Relaxed);
    cache.insert(
        url.to_string(),
        CachedAnonymousFormats {
            formats,
            seq,
            cached_at: Instant::now(),
        },
    );
}

/// 写入某个视频的匿名解析结果（全局缓存）。
fn cache_youtube_anonymous_formats(url: &str, formats: Vec<ExternalMediaFormat>) {
    let mut cache = YOUTUBE_ANONYMOUS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    insert_youtube_anonymous_cache(&mut cache, url, formats);
}

/// 匿名兜底决策：优先复用缓存，其次决定是否为本 URL 发起一次匿名解析。
enum AnonymousFallbackDecision {
    /// 缓存的匿名结果比带 Cookie 的结果画质严格更高，直接复用，不再发请求。
    ReuseCached(Vec<ExternalMediaFormat>),
    /// 本 URL 还没试过匿名解析，可以发起一次。
    Fetch,
    /// 不需要或不能匿名解析：画质已达标、私有来源、或已经试过且没有更好的缓存结果。
    Skip,
}

/// 决定「带 Cookie 画质不达标」时是复用缓存还是发起匿名解析。
///
/// 与 `should_retry_youtube_anonymous` 共用触发条件，额外多一步缓存优先：同一个视频
/// 下载失败后重试时，直接复用上次匿名解析出的高画质格式，不再发第二次请求。
fn plan_youtube_anonymous_fallback(
    url: &str,
    is_private_source: bool,
    cookies_present: bool,
    filter: &FilterOption,
    platform: &str,
    cookie_height: i32,
    min_height: i32,
) -> AnonymousFallbackDecision {
    // 私有来源（稍后再看/点赞/订阅）一律不匿名：连缓存都不读，避免复用其它来源解析出的
    // 匿名结果 —— 规则是「私有来源完全不匿名、不读不写缓存」。
    if is_private_source {
        return AnonymousFallbackDecision::Skip;
    }
    let already_tried = youtube_anonymous_already_tried(url);
    if let Some(cached) = youtube_anonymous_cached_formats(url) {
        let cached_height = youtube_selectable_height(&cached, filter, platform);
        if youtube_anonymous_result_is_better(cookie_height, cached_height) {
            return AnonymousFallbackDecision::ReuseCached(cached);
        }
        return AnonymousFallbackDecision::Skip;
    }
    if should_retry_youtube_anonymous(
        true,
        is_private_source,
        cookies_present,
        cookie_height,
        min_height,
        already_tried,
    ) {
        AnonymousFallbackDecision::Fetch
    } else {
        AnonymousFallbackDecision::Skip
    }
}

/// 是否触发 YouTube「去掉 Cookie 匿名解析」的画质兜底。
///
/// 触发条件必须全部满足：YouTube 链接、来源公开（稍后再看/点赞/订阅需要登录）、本次
/// 解析确实带了 Cookie、当前可下载画质低于用户设置的最低画质（0 表示没有可下载的
/// 视频格式，同样算不达标）、且本进程内没有对这个 URL 试过匿名解析。
fn should_retry_youtube_anonymous(
    is_youtube: bool,
    is_private_source: bool,
    cookies_present: bool,
    selected_height: i32,
    min_height: i32,
    already_tried: bool,
) -> bool {
    is_youtube
        && !is_private_source
        && cookies_present
        && min_height > 0
        && selected_height < min_height
        && !already_tried
}

/// 匿名解析结果是否更好：只有高度严格更高才采用，避免两份结果来回切换。
fn youtube_anonymous_result_is_better(cookie_height: i32, anonymous_height: i32) -> bool {
    anonymous_height > cookie_height
}

fn youtube_anonymous_already_tried(url: &str) -> bool {
    YOUTUBE_ANONYMOUS_TRIED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(url)
}

/// 原子认领「本 URL 的匿名解析」：同一进程内并发解析同一 URL 时，只有第一个调用者
/// 返回 true 并真正发起请求，其余调用者直接跳过，避免同一 URL 并发打出多次 yt-dlp。
fn claim_youtube_anonymous_attempt(url: &str) -> bool {
    YOUTUBE_ANONYMOUS_TRIED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(url.to_string())
}

/// 匿名（不带 Cookie）解析 YouTube 元数据，用于画质兜底。
async fn extract_youtube_metadata_without_cookies(url: &str) -> Result<ExternalMediaMetadata> {
    extract_ytdlp_metadata_with_cookies(url, "YouTube", false).await
}

/// 某份格式列表在给定过滤条件下实际会下载到的视频高度（0 表示没有可下载的视频流）。
fn youtube_selectable_height(formats: &[ExternalMediaFormat], filter: &FilterOption, platform: &str) -> i32 {
    select_youtube_streams(formats, filter, false, platform)
        .ok()
        .and_then(|selected| selected_effective_video_height(&selected))
        .unwrap_or(0)
}

/// YouTube 画质匿名兜底（issue #294）：带 Cookie 解析出的可下载画质达不到用户设置的
/// 最低画质时，去掉 Cookie 匿名再解析一次；匿名结果严格更高才替换 `formats`。
///
/// 只替换格式列表，标题/上传者/时长等元数据仍用带 Cookie 的结果，避免输出文件名变化；
/// 匿名解析失败（年龄限制/会员/私享/需登录）只记一条 warn，继续用原结果，绝不让下载失败。
/// 解析成功的结果会写进 `YOUTUBE_ANONYMOUS_CACHE`：下载阶段失败后进程内重试时直接复用
/// 上次的高画质格式，不会退回带 Cookie 的退化格式。
async fn apply_youtube_anonymous_quality_fallback(
    metadata: ExternalMediaMetadata,
    url: &str,
    source: &youtube_source::Model,
    filter: &FilterOption,
    platform: &str,
) -> ExternalMediaMetadata {
    apply_youtube_anonymous_quality_fallback_with(metadata, url, source, filter, platform, || {
        extract_youtube_metadata_without_cookies(url)
    })
    .await
}

/// `apply_youtube_anonymous_quality_fallback` 的实现体；匿名解析函数由参数注入，
/// 方便单测验证「命中缓存时不发请求」。
async fn apply_youtube_anonymous_quality_fallback_with<F, Fut>(
    mut metadata: ExternalMediaMetadata,
    url: &str,
    source: &youtube_source::Model,
    filter: &FilterOption,
    platform: &str,
    fetch_anonymous: F,
) -> ExternalMediaMetadata
where
    F: FnOnce() -> Fut + Send,
    Fut: std::future::Future<Output = Result<ExternalMediaMetadata>> + Send,
{
    if !is_youtube_url(url) || source.audio_only || metadata.is_slideshow() {
        return metadata;
    }
    let min_height = youtube_quality_height(filter.video_min_quality);
    let cookie_height = youtube_selectable_height(&metadata.formats, filter, platform);
    if min_height <= 0 || cookie_height >= min_height {
        return metadata;
    }
    let cookies_present = youtube_cookie_file().is_some();
    match plan_youtube_anonymous_fallback(
        url,
        is_youtube_private_source(source),
        cookies_present,
        filter,
        platform,
        cookie_height,
        min_height,
    ) {
        AnonymousFallbackDecision::ReuseCached(formats) => {
            let cached_height = youtube_selectable_height(&formats, filter, platform);
            info!(
                url = %url,
                source_id = source.id,
                cookie_height,
                cached_height,
                "YouTube 带 Cookie 只解析到 {}p，复用此前匿名解析的 {}p 缓存",
                cookie_height,
                cached_height
            );
            metadata.formats = formats;
        }
        AnonymousFallbackDecision::Skip => {}
        AnonymousFallbackDecision::Fetch => {
            // 原子认领：并发解析同一 URL 时只有一个任务会真正发起请求。
            if !claim_youtube_anonymous_attempt(url) {
                return metadata;
            }
            debug!(
                url = %url,
                source_id = source.id,
                cookie_height,
                min_height,
                "YouTube 带 Cookie 只解析到 {}p（最低要求 {}p），尝试去掉 Cookie 匿名解析一次",
                cookie_height,
                min_height
            );
            match fetch_anonymous().await {
                Ok(anonymous) => {
                    let anonymous_height = youtube_selectable_height(&anonymous.formats, filter, platform);
                    cache_youtube_anonymous_formats(url, anonymous.formats.clone());
                    if youtube_anonymous_result_is_better(cookie_height, anonymous_height) {
                        info!(
                            url = %url,
                            source_id = source.id,
                            cookie_height,
                            anonymous_height,
                            "YouTube 带 Cookie 只解析到 {}p，匿名解析可达 {}p，已改用匿名解析结果",
                            cookie_height,
                            anonymous_height
                        );
                        metadata.formats = anonymous.formats;
                    } else {
                        debug!(
                            url = %url,
                            source_id = source.id,
                            cookie_height,
                            anonymous_height,
                            "YouTube 匿名解析未提供更高画质，继续使用带 Cookie 的结果"
                        );
                    }
                }
                Err(error) => {
                    warn!(
                        url = %url,
                        source_id = source.id,
                        %error,
                        "YouTube 匿名兜底解析失败，继续使用带 Cookie 的结果"
                    );
                }
            }
        }
    }
    metadata
}

async fn run_ytdlp_metadata(
    url: &str,
    platform: &str,
    client_args: Option<&str>,
    with_cookies: bool,
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
    append_ytdlp_runtime(&mut command).await;
    if with_cookies {
        append_cookies_for_url(&mut command, url);
    }
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
    formats.iter().filter_map(|format| format.height).max().unwrap_or(0)
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
    matches!(source.source_type.as_str(), "watch_later" | "liked" | "subscriptions")
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
                // 原画候选在同分辨率下优先于编码偏好：抖音原片常见 HEVC，
                // 但它可能比当前 H.264 转码高一个数量级码率。
                i32::from(format.is_original),
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
                // 同上：原画候选优先于同分辨率的编码偏好。
                i32::from(format.is_original),
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
fn selected_effective_video_format(selected: &SelectedStreams) -> Option<&ExternalMediaFormat> {
    if selected.video.is_some() && selected.audio.is_some() {
        selected.video.as_ref()
    } else {
        selected.mixed.as_ref()
    }
}

fn selected_effective_video_height(selected: &SelectedStreams) -> Option<i32> {
    selected_effective_video_format(selected).map(format_quality_height)
}

/// 最终选中的画质低于用户设置的最低画质时补一条 WARN，避免静默降级（issue #294）。
fn warn_youtube_below_min_quality(
    selected: &SelectedStreams,
    filter: &FilterOption,
    source: &youtube_source::Model,
    title: &str,
) {
    // 纯音频源不关心视频画质，不要拿 video_min_quality 报一条误导性的 WARN。
    if source.audio_only {
        return;
    }
    let min_height = youtube_quality_height(filter.video_min_quality);
    let Some(format) = selected_effective_video_format(selected) else {
        return;
    };
    let actual_height = format_quality_height(format);
    if min_height <= 0 || actual_height <= 0 || actual_height >= min_height {
        return;
    }
    warn!(
        source_id = source.id,
        source_type = %source.source_type,
        min_height,
        actual_height,
        format_id = %format.format_id.as_deref().unwrap_or(""),
        protocol = %format.protocol.as_deref().unwrap_or(""),
        "YouTube 视频「{}」可下载画质 {}p 低于设定的最低画质 {}p",
        title,
        actual_height,
        min_height
    );
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
            "id={} {}x{} fps={} vcodec={} acodec={} tbr={} vbr={} abr={} original={}",
            format.format_id.as_deref().unwrap_or("-"),
            format.width.unwrap_or_default(),
            format.height.unwrap_or_default(),
            format.fps.unwrap_or_default(),
            format.vcodec.as_deref().unwrap_or("-"),
            format.acodec.as_deref().unwrap_or("-"),
            format.tbr.unwrap_or_default(),
            format.vbr.unwrap_or_default(),
            format.abr.unwrap_or_default(),
            format.is_original,
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
    // 平台标题里塞满了 #话题、多行署名、回复前缀，原样进目录名会让文件夹又长又乱。
    // 这里只清洗「用于生成路径」的标题，NFO 里仍是完整原标题；长度上限沿用与 B 站
    // 完全相同的机制（模板渲染后每段最多 200 字节，见 utils::filenamify）。
    let title_for_path = crate::utils::title_clean::clean_title_for_path(title);
    let args = serde_json::json!({
        "bvid": metadata.id,
        "title": title_for_path,
        "title_full": title,
        "upper_name": uploader,
        "upper_mid": metadata.channel_id.as_deref().unwrap_or(""),
        "pubtime": formatted_time,
        "fav_time": formatted_time,
        "show_title": title_for_path,
        "ptitle": title_for_path,
        "long_title": title_for_path,
        "pid": 1,
        "pid_pad": "01",
    });
    // 抖音短剧/放映厅/合集与 B 站合集 up_seasonal 模式保持一致：
    // 下载根目录/剧集名/S01E001 - 每集标题.mp4。一个剧集一个目录，
    // 文件名带三位集号保证文件系统排序稳定；无集号时回退到普通单 P 命名。
    let episodic_source = uses_episodic_layout(source, video.episode_number);
    let root = PathBuf::from(&source.path);
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
            // 快捷路径模板可能已经把合集名带入 source.path。此时直接以该目录
            // 作为合集根目录，避免生成 `合集名/合集名/Season 01`。
            let video_folder = if path_leaf_matches(&root, &series_folder) {
                "Season 01".to_string()
            } else {
                format!("{series_folder}/Season 01")
            };
            // 抖音合集按 B 站合集默认模板命名：`S01E01 - 集标题`（标题只做目录名清洗，
            // NFO/媒体库标题仍是原始标题）。短剧/放映厅保持原有的纯集号写法，
            // 避免已有剧集目录成批改名。
            let page_name = if source.source_type == "douyin_mix" {
                let episode_title = crate::utils::title_clean::clean_title_for_path(title);
                if episode_title.is_empty() || episode_title == "无标题" {
                    format!("S01E{:02}", episode)
                } else {
                    // filenamify 会把单个路径段截到 200 字节并处理非法字符，
                    // 加上前缀与扩展名仍远低于文件系统 255 字节上限。
                    format!(
                        "S01E{:02} - {}",
                        episode,
                        crate::utils::filenamify::filenamify(episode_title)
                    )
                }
            } else {
                format!("S01E{:02}", episode)
            };
            (video_folder, page_name)
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
    let extension = if source.audio_only && !metadata.is_slideshow() {
        "m4a"
    } else {
        "mp4"
    };
    if source.flat_folder {
        Ok(root.join(format!("{page_name}.{extension}")))
    } else {
        Ok(root.join(video_folder).join(format!("{page_name}.{extension}")))
    }
}

fn path_leaf_matches(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

/// 附属文件子任务的结果：告警文案 + 本次解析到的头像地址。
#[derive(Default)]
struct YoutubeSidecarOutcome {
    warning: Option<String>,
    /// 本轮解析到的 UP 头像地址，透传给图集分页 NFO，避免重复请求平台接口。
    avatar_url: Option<String>,
}

struct YoutubeSidecarInput<'a> {
    downloader: &'a UnifiedDownloader,
    metadata: &'a ExternalMediaMetadata,
    output_path: &'a Path,
    video_url: &'a str,
    title: &'a str,
    uploader: &'a str,
    source: &'a youtube_source::Model,
    episode_number: Option<i32>,
}

async fn ensure_youtube_sidecars(p: YoutubeSidecarInput<'_>) -> YoutubeSidecarOutcome {
    let YoutubeSidecarInput {
        downloader,
        metadata,
        output_path,
        video_url,
        title,
        uploader,
        source,
        episode_number,
    } = p;
    let mut warnings = Vec::new();
    let platform = source_platform_label(source);
    let profile_url = metadata.channel_url.as_deref().or(metadata.uploader_url.as_deref());
    let mut resolved_avatar_url = None;
    match download_youtube_upper_face(downloader, uploader, profile_url, source).await {
        Ok(url) => resolved_avatar_url = url,
        Err(error) => {
            warn!(platform, youtube_id = %metadata.id, error = %error, "{}视频源「{}」视频「{}」媒体已下载，但 UP 头像子任务失败", platform, source.name, title);
            warnings.push(format!("UP头像下载失败：{error:#}"));
        }
    }
    // NFO 的 <actor><thumb> 用同一份头像地址；头像文件已在缓存里时这里会按需补解析一次。
    let avatar_url = avatar_url_for_nfo(
        source,
        profile_url,
        resolved_avatar_url,
        &output_path.with_extension("nfo"),
    )
    .await;
    if let Err(error) = download_youtube_cover(downloader, metadata, output_path, source).await {
        warn!(platform, youtube_id = %metadata.id, error = %error, "{}视频源「{}」视频「{}」媒体已下载，但封面子任务失败", platform, source.name, title);
        warnings.push(format!("封面下载失败：{error:#}"));
    }
    if let Err(error) = generate_youtube_nfo(
        metadata,
        output_path,
        video_url,
        title,
        uploader,
        avatar_url.as_deref(),
        source,
    )
    .await
    {
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
    // 短剧/放映厅/合集确有集号、按番剧结构补充剧集级附属文件
    //（tvshow.nfo、season.nfo、folder.jpg、poster.jpg）。
    if uses_episodic_layout(source, episode_number) {
        if let Err(error) =
            ensure_youtube_series_sidecars(downloader, metadata, output_path, source, avatar_url.as_deref()).await
        {
            warn!(platform, youtube_id = %metadata.id, error = %error, "{}视频源「{}」媒体已下载，但剧集附属文件子任务失败", platform, source.name);
            warnings.push(format!("剧集附属文件生成失败：{error:#}"));
        }
    }
    YoutubeSidecarOutcome {
        warning: (!warnings.is_empty()).then(|| format!("媒体已完成；{}", warnings.join("；"))),
        avatar_url,
    }
}

fn is_episodic_douyin_source(source: &youtube_source::Model) -> bool {
    matches!(
        source.source_type.as_str(),
        "douyin_theater" | "douyin_series" | "douyin_mix"
    )
}

/// 只有来源本身是剧集类型且当前作品确实有集号时，才使用
/// `剧集名/Season 01/S01E01...` 文件树。否则一律回退普通单视频模板，
/// 避免出现“普通目录里硬塞 season.nfo”的半套剧集结构。
fn uses_episodic_layout(source: &youtube_source::Model, episode_number: Option<i32>) -> bool {
    is_episodic_douyin_source(source) && episode_number.is_some_and(|episode| episode >= 1)
}

/// 剧集根目录（放 `poster.jpg` / `folder.jpg` / `landscape.jpg` / `fanart.jpg` /
/// `tvshow.nfo` 的那一层）。
///
/// 不能一律取视频文件的上两级目录：**平铺目录（flat_folder）**时视频直接放在源目录
/// 里，再往上退一层就写到「源目录的上一级」。多个源共用同一个父目录时，封面会互相
/// 覆盖，还会出现「重设路径后旧目录仍在生成封面」「图片是别的合集的」这类现象。
fn episodic_series_dir(output_path: &Path, source_path: &str) -> Option<PathBuf> {
    let media_dir = output_path.parent()?;
    let in_season_dir = media_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.trim().to_ascii_lowercase().starts_with("season"));
    let candidate = if in_season_dir {
        media_dir.parent()?.to_path_buf()
    } else {
        media_dir.to_path_buf()
    };
    // 兜底：永远不写到源目录之外，避免再次把附属文件甩到父目录。
    let base = PathBuf::from(source_path);
    if candidate.starts_with(&base) {
        Some(candidate)
    } else {
        Some(base)
    }
}

/// 清理旧版本把抖音收藏夹误当剧集时留下的附属文件。
///
/// 仅当根目录 `tvshow.nfo` 确实是本收藏夹源生成的内容时才一并删除同组封面，
/// 避免误删同一目录下用户自己的图片。收藏夹的视频目录内 `season.nfo`
/// 也是旧版本硬塞进去的固定模板文件，可安全删除。
async fn cleanup_legacy_collection_series_sidecars(source: &youtube_source::Model, output_path: &Path) -> Result<()> {
    if source.source_type != "douyin_collection" {
        return Ok(());
    }
    let Some(media_dir) = output_path.parent() else {
        return Ok(());
    };

    let season_path = media_dir.join("season.nfo");
    if let Ok(contents) = tokio::fs::read_to_string(&season_path).await {
        if contents.contains("<season>") && contents.contains("<seasonnumber>1</seasonnumber>") {
            remove_file_if_exists(&season_path).await?;
        }
    }

    let Some(shared_dir) = media_dir.parent() else {
        return Ok(());
    };
    let tvshow_path = shared_dir.join("tvshow.nfo");
    let Ok(tvshow) = tokio::fs::read_to_string(&tvshow_path).await else {
        return Ok(());
    };
    let escaped_source_url = quick_xml::escape::escape(&source.url).into_owned();
    if !tvshow.contains(r#"<uniqueid type="douyin""#) || !tvshow.contains(&escaped_source_url) {
        return Ok(());
    }
    remove_file_if_exists(&tvshow_path).await?;
    for name in ["folder.jpg", "poster.jpg"] {
        remove_file_if_exists(&shared_dir.join(name)).await?;
    }
    Ok(())
}

/// 抖音短剧/放映厅/合集按番剧结构生成剧集级附属文件：
/// 剧集根目录 tvshow.nfo、folder.jpg、poster.jpg，以及 Season 01/season.nfo。
/// 幂等：已存在的文件跳过，不重复下载。
async fn ensure_youtube_series_sidecars(
    downloader: &UnifiedDownloader,
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    source: &youtube_source::Model,
    avatar_url: Option<&str>,
) -> Result<()> {
    let platform = source_platform_label(source);
    let Some(season_dir) = output_path.parent() else {
        return Ok(());
    };
    let Some(series_dir) = episodic_series_dir(output_path, &source.path) else {
        return Ok(());
    };
    tokio::fs::create_dir_all(&series_dir).await.ok();

    // folder.jpg / poster.jpg：优先复用已下载的单集封面，避免额外请求。
    let thumb_path = youtube_sidecar_path(output_path, "-thumb.jpg")?;
    let thumb_exists = tokio::fs::metadata(&thumb_path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024);
    let fanart_path = youtube_sidecar_path(output_path, "-fanart.jpg")?;
    let fanart_exists = tokio::fs::metadata(&fanart_path)
        .await
        .is_ok_and(|metadata| metadata.len() >= 1024);
    for (name, source_path, source_exists) in [
        ("folder.jpg", &thumb_path, thumb_exists),
        ("poster.jpg", &thumb_path, thumb_exists),
        ("landscape.jpg", &fanart_path, fanart_exists),
        ("fanart.jpg", &fanart_path, fanart_exists),
    ] {
        let target = series_dir.join(name);
        if let Ok(target_meta) = tokio::fs::metadata(&target).await {
            if target_meta.len() >= 1024 {
                // 单集封面比合集封面新时刷新一次：否则换了封面、改了「竖图/横图」映射
                // 之后，合集目录里一直是老图（poster.jpg 还是旧版横版图就是这么来的）。
                if source_exists && cover_source_is_newer(source_path, &target_meta).await {
                    match tokio::fs::copy(source_path, &target).await {
                        Ok(_) => info!(
                            platform,
                            path = %target.display(),
                            "{}合集封面已按最新单集封面刷新",
                            platform
                        ),
                        Err(error) => {
                            warn!(platform, path = %target.display(), %error, "{}合集封面刷新失败", platform)
                        }
                    }
                }
                // 已存在的封面也顺手校正：老版本可能把 WebP 存成了 .jpg。
                if let Err(error) = ensure_jpeg_image(&target).await {
                    warn!(platform, path = %target.display(), %error, "{}剧集封面转 JPEG 失败（文件仍是原始格式）", platform);
                }
                continue;
            }
        }
        if source_exists {
            tokio::fs::copy(source_path, &target)
                .await
                .with_context(|| format!("生成{platform}剧集封面失败: {}", target.display()))?;
        } else {
            // 竖图（folder/poster）用竖版封面；横图（landscape/fanart）优先平台给出的
            // 横版封面（抖音 dynamic_cover），没有再退回竖版，避免背景墙被拉伸。
            let url = if matches!(name, "landscape.jpg" | "fanart.jpg") {
                metadata.fanart.as_deref().or(metadata.thumbnail.as_deref())
            } else {
                metadata.thumbnail.as_deref()
            };
            let Some(url) = url else {
                continue;
            };
            // 剧集封面同样按剧集共享，多个分集并发下载时会争用同一临时文件
            let temporary = unique_download_path(&target);
            if let Err(error) = fetch_platform_asset(downloader, source, &[url], &temporary)
                .await
                .with_context(|| format!("使用项目统一下载器下载{platform}剧集封面失败"))
            {
                remove_download_temp(&temporary).await;
                return Err(error);
            }
            replace_file(&temporary, &target).await?;
        }
        // 封面可能是 WebP（抖音常见），落盘名是 .jpg，统一转成真 JPEG。
        if let Err(error) = ensure_jpeg_image(&target).await {
            warn!(platform, path = %target.display(), %error, "{}剧集封面转 JPEG 失败（文件仍是原始格式）", platform);
        }
        info!(platform, path = %target.display(), "{}视频源「{}」剧集封面生成完成", platform, source.name);
    }

    // 剧集级 NFO 仅在启用 NFO 时生成
    if !crate::config::reload_config().nfo_config.enabled {
        return Ok(());
    }
    let tvshow_path = series_dir.join("tvshow.nfo");
    let needs_tvshow_refresh = match tokio::fs::read_to_string(&tvshow_path).await {
        Ok(contents) => !contents.contains("<actor>"),
        Err(_) => true,
    };
    if needs_tvshow_refresh {
        let xml = generate_youtube_tvshow_nfo(source, metadata, avatar_url);
        let temporary = unique_download_path(&series_dir.join("tvshow.nfo"));
        tokio::fs::write(&temporary, xml.as_bytes())
            .await
            .with_context(|| format!("写入{platform}剧集 NFO 失败: {}", temporary.display()))?;
        replace_file(&temporary, &tvshow_path).await?;
        info!(platform, path = %tvshow_path.display(), "{}视频源「{}」剧集 tvshow.nfo 生成完成", platform, source.name);
    }

    let season_path = season_dir.join("season.nfo");
    if !tokio::fs::metadata(&season_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        let xml = generate_youtube_season_nfo();
        let temporary = unique_download_path(&season_dir.join("season.nfo"));
        tokio::fs::write(&temporary, xml.as_bytes())
            .await
            .with_context(|| format!("写入{platform}季度 NFO 失败: {}", temporary.display()))?;
        replace_file(&temporary, &season_path).await?;
        info!(platform, path = %season_path.display(), "{}视频源「{}」季度 season.nfo 生成完成", platform, source.name);
    }
    Ok(())
}

fn generate_youtube_tvshow_nfo(
    source: &youtube_source::Model,
    metadata: &ExternalMediaMetadata,
    avatar_url: Option<&str>,
) -> String {
    let escape = |value: &str| quick_xml::escape::escape(value).into_owned();
    let platform = source_platform(source);
    let studio = source_platform_label(source);
    let description = metadata.description.as_deref().unwrap_or_default();
    let uploader = metadata
        .uploader
        .as_deref()
        .or(metadata.channel.as_deref())
        .unwrap_or_default()
        .trim();
    let actor = if uploader.is_empty() {
        String::new()
    } else {
        let thumb = avatar_url
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("<thumb>{}</thumb>", escape(value)))
            .unwrap_or_default();
        format!(
            "    <actor><name>{}</name><role>UP主</role>{thumb}</actor>\n",
            escape(uploader)
        )
    };
    let thumbnail = metadata.thumbnail.as_deref().unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<tvshow>
    <title>{}</title>
    <originaltitle>{}</originaltitle>
    <sorttitle>{}</sorttitle>
    <plot>{}</plot>
    <studio>{}</studio>
    <uniqueid type="{}" default="true">{}</uniqueid>
{}    <thumb aspect="poster">{}</thumb>
    <fanart><thumb>{}</thumb></fanart>
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
        actor,
        escape(thumbnail),
        escape(thumbnail),
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

/// 解析外源 UP / 频道的头像地址。
///
/// 同一份地址有两个用处：下载 `upper_path` 下的头像文件，以及写进视频 NFO 的
/// `<actor><thumb>`（媒体库据此显示作者头像）。抖音 / TikTok / YouTube 取法各不相同，
/// 统一收在这里，避免两处各写一份。
async fn resolve_platform_avatar_url(source: &youtube_source::Model, profile_url: Option<&str>) -> Result<String> {
    if crate::tiktok::is_tiktok_source(source) {
        let author_handle = profile_url
            .and_then(crate::tiktok::tiktok_handle_from_url)
            .ok_or_else(|| anyhow!("TikTok 元数据没有频道主页地址，无法获取 UP 头像"))?;
        return crate::tiktok::fetch_tiktok_author_avatar_url(&author_handle)
            .await?
            .ok_or_else(|| anyhow!("TikTok 作者作品接口没有返回头像地址"));
    }
    if is_douyin_source(source) {
        let author_profile_url = profile_url
            .filter(|url| url.contains("douyin.com/user/"))
            .unwrap_or(&source.url);
        return crate::douyin::fetch_profile(author_profile_url)
            .await?
            .avatar_url
            .context("抖音作者资料没有返回头像");
    }
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
        .context("YouTube 频道主页没有返回 UP 头像")
}

/// NFO 里 `<actor><thumb>` 用的头像地址。
///
/// 优先复用刚下载头像时解析到的地址（不额外请求平台）；头像文件早就在缓存里、
/// 只有 NFO 还缺时才现解析一次，失败就退化成不写头像，不影响其它字段。
async fn avatar_url_for_nfo(
    source: &youtube_source::Model,
    profile_url: Option<&str>,
    resolved: Option<String>,
    nfo_path: &Path,
) -> Option<String> {
    if let Some(url) = resolved.filter(|url| !url.trim().is_empty()) {
        return Some(url);
    }
    if !crate::config::reload_config().nfo_config.enabled {
        return None;
    }
    if tokio::fs::metadata(nfo_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        // NFO 已存在，本轮不会再写，不必多打一次平台接口
        return None;
    }
    resolve_platform_avatar_url(source, profile_url).await.ok()
}

async fn download_youtube_upper_face(
    downloader: &UnifiedDownloader,
    uploader: &str,
    profile_url: Option<&str>,
    source: &youtube_source::Model,
) -> Result<Option<String>> {
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
            return Ok(None);
        }
        tokio::fs::create_dir_all(&upper_dir)
            .await
            .with_context(|| format!("创建{platform} UP头像目录失败: {}", upper_dir.display()))?;

        let mut resolved_avatar_url = None;
        if !face_exists {
            let avatar_url = resolve_platform_avatar_url(source, profile_url).await?;
            let temporary = unique_download_path(&upper_dir.join("folder.jpg"));
            if let Err(error) = fetch_platform_asset(downloader, source, &[avatar_url.as_str()], &temporary)
                .await
                .with_context(|| format!("使用项目统一下载器下载{platform} UP头像失败"))
            {
                remove_download_temp(&temporary).await;
                return Err(error);
            }
            replace_file(&temporary, &face_path).await?;
            info!(platform = source_platform_label(source), uploader, path = %face_path.display(), "{}视频源「{}」 UP头像「{}」下载完成", source_platform_label(source), source.name, uploader);
            resolved_avatar_url = Some(avatar_url);
        }

        if !person_nfo_exists {
            let channel_id = profile_url
                .and_then(crate::tiktok::tiktok_handle_from_url)
                .unwrap_or_else(|| uploader.clone());
            let xml = generate_youtube_person_nfo(&uploader, &channel_id, "tiktok");
            let temporary = unique_download_path(&upper_dir.join("person.nfo"));
            if let Err(error) = tokio::fs::write(&temporary, xml.as_bytes())
                .await
                .with_context(|| format!("写入{platform} UP主 NFO 临时文件失败: {}", temporary.display()))
            {
                remove_download_temp(&temporary).await;
                return Err(error);
            }
            replace_file(&temporary, &person_nfo_path).await?;
            info!(platform = source_platform_label(source), uploader, path = %person_nfo_path.display(), "{}视频源「{}」 UP主 person.nfo「{}」生成完成", source_platform_label(source), source.name, uploader);
        }
        return Ok(resolved_avatar_url);
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
        return Ok(None);
    }

    tokio::fs::create_dir_all(&upper_dir)
        .await
        .with_context(|| format!("创建{platform} UP头像目录失败: {}", upper_dir.display()))?;

    let mut resolved_avatar_url = None;
    if !face_exists {
        let avatar_url = resolve_platform_avatar_url(source, profile_url).await?;
        let temporary = unique_download_path(&upper_dir.join("folder.jpg"));
        if let Err(error) = fetch_platform_asset(downloader, source, &[avatar_url.as_str()], &temporary)
            .await
            .with_context(|| format!("使用项目统一下载器下载{platform} UP头像失败"))
        {
            remove_download_temp(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &face_path).await?;
        info!(platform = source_platform_label(source), uploader, path = %face_path.display(), "{}视频源「{}」 UP头像「{}」下载完成", source_platform_label(source), source.name, uploader);
        resolved_avatar_url = Some(avatar_url);
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
        let temporary = unique_download_path(&upper_dir.join("person.nfo"));
        if let Err(error) = tokio::fs::write(&temporary, xml.as_bytes())
            .await
            .with_context(|| format!("写入{platform} UP主 NFO 临时文件失败: {}", temporary.display()))
        {
            remove_download_temp(&temporary).await;
            return Err(error);
        }
        replace_file(&temporary, &person_nfo_path).await?;
        info!(platform = source_platform_label(source), uploader, path = %person_nfo_path.display(), "{}视频源「{}」 UP主「{}」 person.nfo 生成完成", source_platform_label(source), source.name, uploader);
    }
    Ok(resolved_avatar_url)
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
    append_ytdlp_runtime(&mut command).await;
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

/// 单集封面是否比合集封面新（用来决定要不要刷新合集目录里的 poster/fanart 等）。
async fn cover_source_is_newer(source_path: &Path, target_meta: &std::fs::Metadata) -> bool {
    let Ok(source_meta) = tokio::fs::metadata(source_path).await else {
        return false;
    };
    match (source_meta.modified(), target_meta.modified()) {
        (Ok(source_time), Ok(target_time)) => source_time > target_time,
        _ => false,
    }
}

fn is_jpeg_header(head: &[u8]) -> bool {
    head.len() >= 3 && head[0] == 0xFF && head[1] == 0xD8 && head[2] == 0xFF
}

/// 是否需要把图片转成 JPEG（WebP / PNG / GIF）。
fn needs_jpeg_conversion(head: &[u8]) -> bool {
    if head.len() < 12 {
        return false;
    }
    (head.starts_with(b"RIFF") && &head[8..12] == b"WEBP")
        || head.starts_with(&[0x89, b'P', b'N', b'G'])
        || head.starts_with(b"GIF8")
}

/// 确保图片文件真的是 JPEG：按文件头判断，不是就调用项目自带的 ffmpeg 转一遍。
///
/// 抖音封面接口会随机返回 `image/jpeg` 或 `image/webp`，而 URL 上完全看不出来，
/// 我们却统一按 `.jpg` / `poster.jpg` 落盘。WebP 顶着 `.jpg` 的名字时，部分媒体库
/// 和播放器会直接显示不出来，所以这里统一转成真正的 JPEG。
async fn ensure_jpeg_image(path: &Path) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let mut head = [0u8; 16];
    let read = match tokio::fs::File::open(path).await {
        Ok(mut file) => file.read(&mut head).await.unwrap_or(0),
        Err(_) => return Ok(()),
    };
    if read == 0 || is_jpeg_header(&head[..read]) || !needs_jpeg_conversion(&head[..read]) {
        return Ok(());
    }
    let file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or("cover.jpg");
    // 同一张合集封面会被多个分集并发校正，临时名必须唯一，否则先完成的那个把
    // 临时文件 rename 走，另一个会读到 0 字节或直接报错。
    let unique = uuid::Uuid::new_v4().simple().to_string();
    let temporary = path.with_file_name(format!("{file_name}.{unique}.convert.jpg"));
    let output = crate::utils::process::tokio_command(crate::downloader::resolve_media_tool_path("ffmpeg"))
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            // mjpeg 需要偶数宽高，顺手抹平，避免 1 像素奇数的封面直接转失败。
            "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2",
            "-pix_fmt",
            "yuvj420p",
            "-c:v",
            "mjpeg",
            "-q:v",
            "2",
        ])
        .arg(&temporary)
        .output()
        .await
        .with_context(|| format!("封面转 JPEG 失败（启动 ffmpeg 出错）: {}", path.display()))?;
    if !output.status.success() {
        let _ = remove_file_if_exists(&temporary).await;
        bail!(
            "封面转 JPEG 失败: {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    replace_file(&temporary, path).await?;
    debug!(path = %path.display(), "封面不是 JPEG，已转换为 JPEG");
    Ok(())
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
        // 优先用平台单独提供的横版封面（抖音 `video.dynamic_cover`）：竖版封面
        // 直接当背景墙会被拉伸/裁切。拿不到横图时才退回竖版封面。
        let mut downloaded = false;
        if let Some(url) = metadata.fanart.as_deref().filter(|value| !value.trim().is_empty()) {
            let temporary = youtube_sidecar_path(output_path, "-fanart.download")?;
            match fetch_platform_asset(downloader, source, &[url], &temporary).await {
                Ok(()) => {
                    replace_file(&temporary, &fanart_path).await?;
                    downloaded = true;
                }
                Err(error) => {
                    let _ = remove_file_if_exists(&temporary).await;
                    warn!(
                        platform,
                        youtube_id = %metadata.id,
                        %error,
                        "{}视频源「{}」横版封面下载失败，回退到竖版封面",
                        platform,
                        source.name
                    );
                }
            }
        }
        if !downloaded {
            tokio::fs::copy(&thumb_path, &fanart_path)
                .await
                .with_context(|| format!("生成{platform} fanart 失败: {}", fanart_path.display()))?;
        }
        info!(platform = source_platform_label(source), youtube_id = %metadata.id, path = %fanart_path.display(), "{}视频源「{}」视频「{}」 fanart 生成完成", source_platform_label(source), source.name, metadata.title.as_deref().unwrap_or(&metadata.id));
    }
    // 抖音封面会随机给 JPEG / WebP，统一转成真 JPEG，避免媒体库认不出 .jpg 里的 WebP。
    for (label, cover) in [("thumb", &thumb_path), ("fanart", &fanart_path)] {
        if let Err(error) = ensure_jpeg_image(cover).await {
            warn!(
                platform = source_platform_label(source),
                youtube_id = %metadata.id,
                path = %cover.display(),
                %error,
                "{}视频「{}」{} 封面转 JPEG 失败（文件仍是原始格式）",
                platform,
                metadata.title.as_deref().unwrap_or(&metadata.id),
                label
            );
        }
    }
    Ok(())
}

/// 图集作品的分页附属文件。
///
/// 视频段各自成页后，第 2 页起如果只有裸媒体文件，媒体库会显示成没有封面、
/// 没有简介的空白条目，所以按分页文件名各补一份封面、fanart 与 NFO。
async fn ensure_image_post_page_sidecars(
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    video_url: &str,
    title: &str,
    uploader: &str,
    avatar_url: Option<&str>,
    source: &youtube_source::Model,
) {
    let pages = crate::douyin::image_post_page_paths(output_path);
    if pages.len() < 2 {
        return;
    }
    let platform = source_platform_label(source);
    let primary_thumb = youtube_sidecar_path(output_path, "-thumb.jpg").ok();
    let primary_fanart = youtube_sidecar_path(output_path, "-fanart.jpg").ok();
    for page in pages.iter().skip(1) {
        for (from, suffix) in [(&primary_thumb, "-thumb.jpg"), (&primary_fanart, "-fanart.jpg")] {
            let Some(from) = from.as_ref() else {
                continue;
            };
            let Ok(target) = youtube_sidecar_path(page, suffix) else {
                continue;
            };
            if tokio::fs::metadata(&target).await.is_ok_and(|meta| meta.len() >= 1024) {
                continue;
            }
            if let Err(error) = tokio::fs::copy(from, &target).await {
                warn!(platform, path = %target.display(), %error, "{}图集分页封面生成失败", platform);
            }
        }
        if let Err(error) = generate_youtube_nfo(metadata, page, video_url, title, uploader, avatar_url, source).await {
            warn!(platform, path = %page.display(), %error, "{}图集分页 NFO 生成失败", platform);
        }
    }
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

struct YoutubeMovieNfoInput<'a> {
    title: &'a str,
    description: &'a str,
    platform: &'a str,
    studio: &'a str,
    id: &'a str,
    aired: chrono::NaiveDateTime,
    uploader: &'a str,
    co_creators: &'a [String],
    thumbnail: &'a str,
    video_url: &'a str,
    avatar_url: Option<&'a str>,
}

/// 构建单视频（movie）NFO XML。
///
/// - `studio`：平台名（如 YouTube/抖音），写入 `<studio>`；
/// - `aired`：上传时间，分别写入 `<year>/<premiered>/<aired>`。
/// - `uploader`：主频道，写入 `<director>` 并作为第一个 `<actor>`；
/// - `co_creators`：联合投稿的合作频道名，逐个补写 `<actor>`；
/// - `avatar_url`：主频道头像地址，写进 `<actor><thumb>`，媒体库里才有作者头像。
///
/// 注意 `<aired>` 与 `<studio>` 两个标签的参数顺序不能写反，
/// 否则会出现"工作室=日期、首播=平台名"的错位。
fn build_youtube_movie_nfo_xml(p: YoutubeMovieNfoInput<'_>) -> String {
    let YoutubeMovieNfoInput {
        title,
        description,
        platform,
        studio,
        id,
        aired,
        uploader,
        co_creators,
        thumbnail,
        video_url,
        avatar_url,
    } = p;
    let escape = |value: &str| quick_xml::escape::escape(value).into_owned();
    let avatar = avatar_url.filter(|url| !url.trim().is_empty());
    let actor_role = if platform == "douyin" { "UP主" } else { "频道" };
    let mut actors = Vec::new();
    for (index, name) in std::iter::once(uploader)
        .chain(co_creators.iter().map(String::as_str))
        .filter(|name| !name.trim().is_empty())
        .enumerate()
    {
        // 头像只有主频道有，合作频道没有单独的头像地址可写
        let thumb = if index == 0 {
            avatar
                .map(|url| format!("<thumb>{}</thumb>", escape(url)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        actors.push(format!(
            "    <actor><name>{}</name><role>{actor_role}</role>{}</actor>",
            escape(name),
            thumb
        ));
    }
    let actor_lines = actors.join("\n");
    let director_block = if platform == "douyin" {
        String::new()
    } else {
        format!("    <director>{}</director>\n", escape(uploader))
    };
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
{}\
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
        director_block,
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
    avatar_url: Option<&str>,
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
    let xml = build_youtube_movie_nfo_xml(YoutubeMovieNfoInput {
        title,
        description,
        platform,
        studio,
        id: &metadata.id,
        aired,
        uploader,
        co_creators: &co_creators,
        thumbnail,
        video_url,
        avatar_url,
    });
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

/// 候选字幕轨之间的重试间隔：连续请求 YouTube 自动翻译端点（`tlang=`）容易 429。
const SUBTITLE_CANDIDATE_GAP_MS: u64 = 400;

/// 一条可下载的字幕轨候选。
#[derive(Clone, Debug)]
struct SubtitleCandidate {
    language: String,
    url: String,
    ext: String,
}

async fn download_youtube_subtitle(
    downloader: &UnifiedDownloader,
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    preferred_language: &str,
    source: &youtube_source::Model,
) -> Result<()> {
    if youtube_subtitle_exists(output_path).await? {
        return Ok(());
    }
    // 语言设置写成 `zh-CN+en` 时走双语分支：中文为目标语言，英文作为上方对照行。
    if let Some((primary_language, secondary_language)) =
        crate::utils::subtitle::parse_bilingual_language_pair(preferred_language)
    {
        return download_youtube_bilingual_subtitle(
            downloader,
            metadata,
            output_path,
            &primary_language,
            &secondary_language,
            source,
        )
        .await;
    }
    download_youtube_single_subtitle(downloader, metadata, output_path, preferred_language, source).await
}

async fn download_youtube_single_subtitle(
    downloader: &UnifiedDownloader,
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    preferred_language: &str,
    source: &youtube_source::Model,
) -> Result<()> {
    let platform = source_platform_label(source);
    let title = metadata.title.as_deref().unwrap_or(&metadata.id);
    let candidates =
        collect_youtube_subtitle_candidates(metadata, &youtube_subtitle_language_order(metadata, preferred_language));
    if candidates.is_empty() {
        info!(youtube_id = %metadata.id, "{}视频源「{}」视频「{}」没有匹配的字幕，跳过字幕子任务", platform, source.name, title);
        return Ok(());
    }
    let mut last_error = None;
    for (index, candidate) in candidates.iter().enumerate() {
        let subtitle_path = youtube_single_subtitle_path(output_path, candidate);
        // 每个候选使用独立临时文件：非播放格式要先转成 VTT 再落盘，也避免上一条
        // 失败留下的残留被下载器当成断点续传内容。
        let temporary = output_path.with_extension(format!("{}.{}.subtitle.tmp", candidate.language, index));
        let _ = remove_file_if_exists(&temporary).await;
        let outcome = match fetch_youtube_subtitle_track(downloader, source, &candidate.url, &temporary).await {
            Ok(()) => persist_youtube_subtitle(&temporary, &subtitle_path, &candidate.ext).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(()) => {
                info!(platform, youtube_id = %metadata.id, language = %candidate.language, path = %subtitle_path.display(), "{}视频源「{}」视频「{}」字幕下载完成", platform, source.name, title);
                let _ = remove_file_if_exists(&temporary).await;
                return Ok(());
            }
            Err(error) => {
                let _ = remove_file_if_exists(&temporary).await;
                let _ = remove_file_if_exists(&subtitle_path).await;
                let _ = remove_file_if_exists(&subtitle_path.with_extension("ass")).await;
                warn!(
                    platform,
                    youtube_id = %metadata.id,
                    language = %candidate.language,
                    "{}视频源「{}」视频「{}」第 {}/{} 个候选字幕轨（{}）下载失败，改试下一个：{:#}",
                    platform,
                    source.name,
                    title,
                    index + 1,
                    candidates.len(),
                    candidate.language,
                    error
                );
                last_error = Some(error);
                if index + 1 < candidates.len() {
                    tokio::time::sleep(Duration::from_millis(SUBTITLE_CANDIDATE_GAP_MS)).await;
                }
            }
        }
    }
    let error = last_error.unwrap_or_else(|| anyhow!("没有可用的{platform}字幕轨"));
    Err(error.context(format!("使用项目统一下载器下载{platform}字幕失败")))
}

/// 单轨字幕的最终文件名：无论源格式（`vtt/srt/json3/srv*/ttml`）都统一解析成
/// cue 后重新渲染，落盘为 `.srt`，并同时生成同名的 `.ass`（见 `persist_youtube_subtitle`）。
/// 旧实现把 `candidate.ext` 直接拼进文件名，`.json3` 落盘后播放器不认、
/// 存在性检查也认不出，于是每轮重复下载。
fn youtube_single_subtitle_path(output_path: &Path, candidate: &SubtitleCandidate) -> PathBuf {
    output_path.with_extension(format!("{}.srt", candidate.language))
}

/// 把下载好的字幕写到最终路径：统一解析成 cue 后同时落盘 `.srt` 与 `.ass`
/// 两种格式（媒体库原生识别、播放器兼容面最广），不再按源格式原样保存。
async fn persist_youtube_subtitle(temporary: &Path, final_path: &Path, ext: &str) -> Result<()> {
    let bytes = tokio::fs::read(temporary).await.context("读取字幕临时文件失败")?;
    let text = String::from_utf8_lossy(&bytes);
    let cues = parse_youtube_subtitle_cues(&text, ext);
    if cues.is_empty() {
        // 仍然解析不出 cue：按**原始后缀**落盘（`srv1/2/3`、`ttml`），
        // 绝不能写不带 `WEBVTT` 头的 `.vtt`——那样媒体库解析不了，而
        // `youtube_subtitle_exists()` 又认这个后缀，这一页从此既不会重下、
        // 也不会生成 `.srt`/`.ass`，坏页就永久化了。
        let fallback_ext = ext.trim_start_matches('.').to_ascii_lowercase();
        if is_xml_subtitle_ext(&fallback_ext) {
            let fallback = final_path.with_extension(&fallback_ext);
            warn!(
                ext,
                path = %fallback.display(),
                "{} 字幕解析不出文本轨，已按原始格式落盘（媒体库可能识别不了这条轨）",
                ext
            );
            tokio::fs::write(temporary, &bytes).await?;
            replace_file(temporary, &fallback).await?;
            return Ok(());
        }
        bail!("无法从 {} 字幕中解析出任何字幕内容", ext);
    }
    write_youtube_subtitle_sidecars(temporary, final_path, &cues).await
}

/// 这些后缀都是 YouTube 的 XML 字幕格式，解析失败时按原名落盘。
fn is_xml_subtitle_ext(ext: &str) -> bool {
    matches!(ext, "srv1" | "srv2" | "srv3" | "ttml" | "xml")
}

/// 按源格式选择解析器：`json3` 走 JSON 结构，`srv*/ttml` 走 XML 计时元素，
/// 其余（`vtt`/`srt`）走通用的文本时间轴解析。
fn parse_youtube_subtitle_cues(text: &str, ext: &str) -> Vec<crate::utils::subtitle::SubtitleCue> {
    let lowered = ext.trim_start_matches('.').to_ascii_lowercase();
    match lowered.as_str() {
        "json3" => parse_json3_cues(text),
        other if is_xml_subtitle_ext(other) => {
            let cues = parse_xml_timed_cues(text);
            if cues.is_empty() {
                crate::utils::subtitle::parse_cues(text)
            } else {
                cues
            }
        }
        _ => crate::utils::subtitle::parse_cues(text),
    }
}

/// `srv1/2/3`、`ttml` 的极小解析器：只认时间属性，正文里的内联标签交给
/// `clean_cue_text` 清理。
///
/// 支持的形状（YouTube 实际会返回这几种）：
/// - `srv3`：`<p t="1234" d="2500">文字</p>`（毫秒）
/// - `srv1/2`：`<text start="1.23" dur="2.5">文字</text>`（秒）
/// - `ttml`：`<p begin="00:00:01.230" end="00:00:03.400">文字</p>`
fn parse_xml_timed_cues(input: &str) -> Vec<crate::utils::subtitle::SubtitleCue> {
    let mut cues: Vec<crate::utils::subtitle::SubtitleCue> = Vec::new();
    let mut current: Option<(i64, i64, String)> = None;
    let mut rest = input;

    while let Some(open) = rest.find('<') {
        // 标签之前的文本属于当前 cue 的正文。
        if let Some((_, _, body)) = current.as_mut() {
            body.push_str(&rest[..open]);
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            break;
        };
        let tag = &after[..close];
        rest = &after[close + 1..];

        let closing = tag.starts_with('/');
        let tag_body = tag.trim_start_matches('/').trim_end_matches('/');
        let name_end = tag_body
            .find(|ch: char| ch.is_whitespace() || ch == '/')
            .unwrap_or(tag_body.len());
        let name = &tag_body[..name_end];
        let attrs = &tag_body[name_end..];
        let is_timed_element = name.eq_ignore_ascii_case("text") || name.eq_ignore_ascii_case("p");

        if closing {
            if is_timed_element {
                if let Some((start, end, body)) = current.take() {
                    push_xml_cue(&mut cues, start, end, &body);
                }
            }
            continue;
        }
        if is_timed_element {
            // 容错：上一个元素没闭合也要先收尾，否则会吞掉下一段的时间。
            if let Some((start, end, body)) = current.take() {
                push_xml_cue(&mut cues, start, end, &body);
            }
            if let Some((start, end)) = parse_xml_timing(attrs) {
                current = Some((start, end, String::new()));
            }
            continue;
        }
        if name.eq_ignore_ascii_case("br") {
            if let Some((_, _, body)) = current.as_mut() {
                body.push('\n');
            }
        }
    }
    if let Some((start, end, body)) = current.take() {
        push_xml_cue(&mut cues, start, end, &body);
    }
    cues.sort_by_key(|cue| cue.start_ms);
    cues
}

fn push_xml_cue(cues: &mut Vec<crate::utils::subtitle::SubtitleCue>, start: i64, end: i64, body: &str) {
    let text = crate::utils::subtitle::clean_cue_text(body);
    if text.trim().is_empty() {
        return;
    }
    cues.push(crate::utils::subtitle::SubtitleCue {
        start_ms: start,
        end_ms: end.max(start),
        text,
    });
}

/// 从标签属性里取时间：`t/d`（毫秒）或 `start/dur/begin/end`（秒或时间码）。
fn parse_xml_timing(attrs: &str) -> Option<(i64, i64)> {
    const DEFAULT_CUE_DURATION_MS: i64 = 2_000;
    let start = if let Some(value) = xml_attr(attrs, "t") {
        parse_cue_time(value, true)?
    } else if let Some(value) = xml_attr(attrs, "begin") {
        parse_cue_time(value, false)?
    } else {
        parse_cue_time(xml_attr(attrs, "start")?, false)?
    };
    let end = if let Some(value) = xml_attr(attrs, "d") {
        start.saturating_add(parse_cue_time(value, true)?)
    } else if let Some(value) = xml_attr(attrs, "dur") {
        start.saturating_add(parse_cue_time(value, false)?)
    } else if let Some(value) = xml_attr(attrs, "end") {
        parse_cue_time(value, false)?
    } else {
        start.saturating_add(DEFAULT_CUE_DURATION_MS)
    };
    Some((start, end.max(start)))
}

/// 取 XML 属性值：只认同名属性（`d` 不会误匹配 `dur`）。
fn xml_attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let mut search = attrs;
    while let Some(index) = search.find(name) {
        let boundary_ok = index == 0 || search[..index].ends_with(|ch: char| ch.is_whitespace());
        let after = &search[index + name.len()..];
        let after_trimmed = after.trim_start();
        if boundary_ok {
            if let Some(rest) = after_trimmed.strip_prefix('=') {
                let value = rest.trim_start();
                if let Some(quote) = value.chars().next().filter(|ch| *ch == '"' || *ch == '\'') {
                    let body = &value[1..];
                    if let Some(end) = body.find(quote) {
                        return Some(&body[..end]);
                    }
                }
            }
        }
        search = after;
    }
    None
}

/// 把字幕时间值解析成毫秒：`12.34`（秒）、`1.5s`、`00:01:02.345`、
/// 以及 `numeric_unit_ms` 为真时的裸毫秒（`1234`）。
fn parse_cue_time(raw: &str, numeric_unit_ms: bool) -> Option<i64> {
    let value = raw.trim().trim_end_matches('s');
    if value.is_empty() {
        return None;
    }
    if value.contains(':') {
        let mut parts = value.split(':').rev();
        let seconds: f64 = parts.next()?.parse().ok()?;
        let minutes: f64 = parts.next().map(|part| part.parse().ok()).unwrap_or(Some(0.0))?;
        let hours: f64 = parts.next().map(|part| part.parse().ok()).unwrap_or(Some(0.0))?;
        return Some(((hours * 3600.0 + minutes * 60.0 + seconds) * 1000.0).round() as i64);
    }
    let number: f64 = value.parse().ok()?;
    let millis = if numeric_unit_ms { number } else { number * 1000.0 };
    Some(millis.round() as i64)
}

/// yt-dlp 的 `json3` 自动字幕：`events[].tStartMs / dDurationMs / segs[].utf8`。
/// 项目统一的 `parse_cues` 只认 WebVTT / SRT 文本，这里补一层 JSON → cue。
fn parse_json3_cues(input: &str) -> Vec<crate::utils::subtitle::SubtitleCue> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return Vec::new();
    };
    let Some(events) = value.get("events").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut cues: Vec<crate::utils::subtitle::SubtitleCue> = events
        .iter()
        .filter_map(|event| {
            let start_ms = event.get("tStartMs").and_then(serde_json::Value::as_i64).unwrap_or(0);
            let duration_ms = event
                .get("dDurationMs")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let text = event
                .get("segs")
                .and_then(serde_json::Value::as_array)
                .map(|segments| {
                    segments
                        .iter()
                        .filter_map(|segment| segment.get("utf8").and_then(serde_json::Value::as_str))
                        .collect::<String>()
                })
                .unwrap_or_default();
            let text = crate::utils::subtitle::clean_cue_text(&text);
            if text.is_empty() {
                return None;
            }
            Some(crate::utils::subtitle::SubtitleCue {
                start_ms,
                end_ms: start_ms.saturating_add(duration_ms.max(0)),
                text,
            })
        })
        .collect();
    cues.sort_by_key(|cue| cue.start_ms);
    cues
}

/// 下载目标语言轨与对照语言轨，按时间轴重叠度合并成一条双语 VTT。
///
/// 对照语言轨缺失时降级为只保存目标语言轨（保持旧行为），不会让整个字幕子任务失败。
async fn download_youtube_bilingual_subtitle(
    downloader: &UnifiedDownloader,
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    primary_language: &str,
    secondary_language: &str,
    source: &youtube_source::Model,
) -> Result<()> {
    let platform = source_platform_label(source);
    let title = metadata.title.as_deref().unwrap_or(&metadata.id);
    let primary_candidates =
        collect_youtube_subtitle_candidates(metadata, &youtube_subtitle_language_candidates(primary_language));
    let secondary_candidates =
        collect_youtube_subtitle_candidates(metadata, &youtube_subtitle_language_candidates(secondary_language));
    if primary_candidates.is_empty() {
        info!(youtube_id = %metadata.id, "{}视频源「{}」视频「{}」没有 {} 字幕轨，跳过双语字幕子任务", platform, source.name, title, primary_language);
        return Ok(());
    }

    let primary_tmp = output_path.with_extension("subtitle.primary.tmp");
    let Some(primary) = fetch_first_youtube_subtitle(downloader, source, &primary_candidates, &primary_tmp).await?
    else {
        let _ = remove_file_if_exists(&primary_tmp).await;
        info!(youtube_id = %metadata.id, "{}视频源「{}」视频「{}」目标语言字幕轨全部下载失败，跳过双语字幕子任务", platform, source.name, title);
        return Ok(());
    };

    let secondary_tmp = output_path.with_extension("subtitle.secondary.tmp");
    let secondary = if secondary_candidates.is_empty() {
        None
    } else {
        fetch_first_youtube_subtitle(downloader, source, &secondary_candidates, &secondary_tmp).await?
    };

    // 双语分支也要按源格式解析：`json3` / `srv*` / `ttml` 都走同一套解析器，
    // 否则双语设置下这些格式会解析出空列表，整条链路退化。
    let primary_cues = read_youtube_subtitle_cues(&primary_tmp, &primary.ext).await?;
    let (merge_language, already_bilingual) = plan_youtube_bilingual_merge(&primary_cues, secondary.as_ref());
    let merged_cues = match merge_language.as_deref() {
        Some(merged_language) => {
            let secondary_ext = secondary.as_ref().map(|item| item.ext.as_str()).unwrap_or("vtt");
            let secondary_cues = read_youtube_subtitle_cues(&secondary_tmp, secondary_ext).await?;
            crate::utils::subtitle::merge_overlay(&primary_cues, &secondary_cues, Some(merged_language))
        }
        // 对照轨缺失、或目标轨本身已内嵌对照语言：都要把目标轨落盘。
        // 旧实现在后一种情况直接 `return Ok(())` 并删掉两个临时文件，于是双语设置下
        // 一个字幕文件都不产生，而 `youtube_subtitle_exists()` 只认 `.vtt/.srt/.ass`，
        // 找不到产物就会每轮重复下载。
        None => primary_cues,
    };
    let _ = remove_file_if_exists(&primary_tmp).await;
    let _ = remove_file_if_exists(&secondary_tmp).await;
    if merged_cues.is_empty() {
        bail!("双语字幕解析后没有任何字幕内容");
    }
    let final_path = youtube_bilingual_subtitle_path(output_path, &primary.language, merge_language.as_deref());
    let temporary = output_path.with_extension("subtitle.bilingual.tmp");
    write_youtube_subtitle_sidecars(&temporary, &final_path, &merged_cues).await?;
    if already_bilingual {
        info!(
            platform,
            youtube_id = %metadata.id,
            path = %final_path.display(),
            cues = merged_cues.len(),
            "{}视频源「{}」视频「{}」的 {} 字幕轨本身已是双语，已直接保存该轨（未生成重复合并文件）",
            platform,
            source.name,
            title,
            primary.language
        );
    } else if let Some(merged_language) = merge_language.as_deref() {
        info!(
            platform,
            youtube_id = %metadata.id,
            path = %final_path.display(),
            cues = merged_cues.len(),
            "{}视频源「{}」视频「{}」双语字幕合并完成（{} + {}），共 {} 条",
            platform,
            source.name,
            title,
            primary.language,
            merged_language,
            merged_cues.len()
        );
    } else {
        warn!(
            platform,
            youtube_id = %metadata.id,
            path = %final_path.display(),
            "{}视频源「{}」视频「{}」找不到 {} 字幕轨，已只保存 {} 单语字幕",
            platform,
            source.name,
            title,
            secondary_language,
            primary.language
        );
    }
    Ok(())
}

/// 规划双语字幕的最终产物，返回 `(需要合并的对照语言, 目标轨本身是否已内嵌对照语言)`。
///
/// - `Some(lang)`：把对照轨叠到目标轨上，产物 `{primary}-{lang}.srt`
/// - `None`：不叠，直接落盘目标轨 `{primary}.srt`
///
/// 两种 `None` 都要落盘，区别只在日志；这样「已是双语」不会再退化成零产物。
/// 每个产物都会同时生成同名 `.ass`（见下方的落盘逻辑）。
fn plan_youtube_bilingual_merge(
    primary_cues: &[crate::utils::subtitle::SubtitleCue],
    secondary: Option<&SubtitleCandidate>,
) -> (Option<String>, bool) {
    let Some(secondary) = secondary else {
        return (None, false);
    };
    // 目标轨自己就带对照语言时，`merge_overlay` 会逐句跳过叠加，
    // 结果与目标轨一致，没必要再写一份重复的双语文件。
    if crate::utils::subtitle::bilingual_ratio(primary_cues, &secondary.language) >= 0.9 {
        return (None, true);
    }
    (Some(secondary.language.clone()), false)
}

/// 双语分支的最终文件名：合并时带两个语言码，否则只带目标语言码。
/// 基准扩展名是 `.srt`；`.ass` 由落盘逻辑按同 stem 生成。
fn youtube_bilingual_subtitle_path(
    output_path: &Path,
    primary_language: &str,
    merge_language: Option<&str>,
) -> PathBuf {
    match merge_language {
        Some(secondary_language) => {
            output_path.with_extension(format!("{}-{}.srt", primary_language, secondary_language))
        }
        None => output_path.with_extension(format!("{}.srt", primary_language)),
    }
}

/// 把解析好的 cue 同时落盘为 `.srt` 与 `.ass` 两个格式（同一份内容）。
async fn write_youtube_subtitle_sidecars(
    temporary: &Path,
    final_path: &Path,
    cues: &[crate::utils::subtitle::SubtitleCue],
) -> Result<()> {
    let rendered = [
        (final_path.to_path_buf(), crate::utils::subtitle::render_srt(cues)),
        (
            final_path.with_extension("ass"),
            crate::utils::subtitle::render_ass(cues),
        ),
    ];
    for (path, body) in rendered {
        tokio::fs::write(temporary, body.as_bytes()).await?;
        replace_file(temporary, &path).await?;
    }
    Ok(())
}

/// 依次尝试候选轨，返回第一个下载成功的候选。
async fn fetch_first_youtube_subtitle(
    downloader: &UnifiedDownloader,
    source: &youtube_source::Model,
    candidates: &[SubtitleCandidate],
    path: &Path,
) -> Result<Option<SubtitleCandidate>> {
    for (index, candidate) in candidates.iter().enumerate() {
        match fetch_youtube_subtitle_track(downloader, source, &candidate.url, path).await {
            Ok(()) => return Ok(Some(candidate.clone())),
            Err(error) => {
                let _ = remove_file_if_exists(path).await;
                debug!(
                    language = %candidate.language,
                    "候选字幕轨（{}）第 {}/{} 个下载失败，改试下一个：{:#}",
                    candidate.language,
                    index + 1,
                    candidates.len(),
                    error
                );
                if index + 1 < candidates.len() {
                    tokio::time::sleep(Duration::from_millis(SUBTITLE_CANDIDATE_GAP_MS)).await;
                }
            }
        }
    }
    Ok(None)
}

async fn read_youtube_subtitle_cues(path: &Path, ext: &str) -> Result<Vec<crate::utils::subtitle::SubtitleCue>> {
    let bytes = tokio::fs::read(path).await.context("读取字幕临时文件失败")?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(parse_youtube_subtitle_cues(&text, ext))
}

/// 按语言优先级展开候选字幕轨，顺序是「语言优先，其次人工优先」。
///
/// 用户把目标语言设成中文时，先试中文人工轨、再试中文自动翻译轨，最后才退到
/// 原语言轨——这样「视频恰好有英文人工轨」不会再抢走中文自动轨。
fn collect_youtube_subtitle_candidates(metadata: &ExternalMediaMetadata, order: &[String]) -> Vec<SubtitleCandidate> {
    let mut candidates: Vec<SubtitleCandidate> = Vec::new();
    let mut seen_urls: HashSet<String> = HashSet::new();
    for language in order {
        for subtitles in [&metadata.subtitles, &metadata.automatic_captions] {
            let Some((item, url)) = select_subtitle_item(subtitles, language) else {
                continue;
            };
            if !seen_urls.insert(url.to_string()) {
                continue;
            }
            candidates.push(SubtitleCandidate {
                language: language.clone(),
                url: url.to_string(),
                ext: item.ext.clone().unwrap_or_else(|| "vtt".to_string()),
            });
        }
    }
    candidates
}

/// 目标语言候选，末尾补上原语言与常见语言兜底。
fn youtube_subtitle_language_order(metadata: &ExternalMediaMetadata, preferred_language: &str) -> Vec<String> {
    let mut order = youtube_subtitle_language_candidates(preferred_language);
    if let Some(language) = metadata.language.as_deref() {
        let language = language.trim();
        if !language.is_empty() && !order.iter().any(|value| value == language) {
            order.push(language.to_string());
        }
    }
    for fallback in ["zh-Hans", "zh-CN", "zh", "en", "ja"] {
        if !order.iter().any(|value| value == fallback) {
            order.push(fallback.to_string());
        }
    }
    order
}

/// 单一语言码对应的候选语言列表：用户点名的语言码永远排第一位。
fn youtube_subtitle_language_candidates(language: &str) -> Vec<String> {
    let requested = language.trim();
    let family: Vec<&str> = match requested {
        value if value.starts_with("zh-Hant") || value == "zh-TW" || value == "zh-HK" => {
            vec!["zh-Hant", "zh-TW", "zh"]
        }
        value if value.starts_with("zh") => vec!["zh-Hans", "zh-CN", "zh", "zh-Hant"],
        "en-US" | "en-GB" | "en" => vec!["en-US", "en"],
        "ja-JP" | "ja" => vec!["ja-JP", "ja"],
        "ko-KR" | "ko" => vec!["ko-KR", "ko"],
        _ => vec![requested],
    };
    if requested.is_empty() {
        return family.into_iter().map(str::to_string).collect();
    }
    let mut order = Vec::with_capacity(family.len() + 1);
    order.push(requested.to_string());
    for value in family {
        if value != requested {
            order.push(value.to_string());
        }
    }
    order
}

async fn write_external_danmaku_ass(
    output_path: &Path,
    title: &str,
    duration_seconds: i32,
    danmaku: Vec<DanmakuElem>,
) -> Result<()> {
    let ass_path = external_ass_path(output_path);
    let temporary = output_path.with_extension("zh-CN.default.ass.download");
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

fn external_ass_path(output_path: &Path) -> PathBuf {
    output_path.with_extension("zh-CN.default.ass")
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
    let live_chat_ass_path = external_ass_path(output_path);
    let _ = remove_file_if_exists(&output_path.with_extension("live_chat.checked")).await;
    if tokio::fs::metadata(&live_chat_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
        && tokio::fs::metadata(&live_chat_ass_path)
            .await
            .is_ok_and(|metadata| metadata.len() > 0)
    {
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
        debug!(youtube_id = %metadata.id, "YouTube 视频没有直播聊天，跳过");
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
    append_ytdlp_runtime(&mut command).await;
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
    let _ = remove_file_if_exists(&output_path.with_extension("subtitle.checked")).await;
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
        if name.starts_with(&format!("{stem}."))
            // `srv*/ttml` 只在「XML 解析不出文本」时才会作为降级产物存在，
            // 也必须被认出来，否则每轮扫描都会重复下载同一条坏轨。
            && matches!(extension, "vtt" | "srt" | "ass" | "srv1" | "srv2" | "srv3" | "ttml" | "xml")
            && tokio::fs::metadata(path).await.is_ok_and(|metadata| metadata.len() > 0)
        {
            return Ok(true);
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

/// 生成目标文件同目录下的唯一临时文件名（`<目标名>.<随机>.download`）。
///
/// UP 头像 / person.nfo / 剧集级 NFO 都是「按 UP 主或剧集」共享的产物：多个视频
/// 同时下载时会各自走同一条生成逻辑。如果都用固定的 `folder.download` /
/// `person.nfo.download`，先完成的任务 rename 走临时文件后，另一个任务还在读写同一
/// 路径，就会报「断点续传后文件大小不一致: 0 != N」之类的错，UP 头像子任务随之失败。
/// 每次生成一个带随机后缀的临时文件即可彻底避开这种争用。
fn unique_download_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let unique = uuid::Uuid::new_v4().simple().to_string();
    target.with_file_name(format!("{name}.{unique}.download"))
}
async fn replace_file(source: &Path, target: &Path) -> Result<()> {
    if tokio::fs::try_exists(target).await? {
        tokio::fs::remove_file(target).await?;
    }
    tokio::fs::rename(source, target)
        .await
        .with_context(|| format!("保存下载文件失败: {}", target.display()))
}

/// 失败时清理临时文件及其断点续传状态文件。
///
/// UP 头像 / 剧集封面这类「按 UP 或剧集共享」的产物用带随机后缀的临时名，
/// 失败后必须连同 `<临时名>.resume` 一起删掉，否则共享目录里会不断堆积残留。
async fn remove_download_temp(temporary: &Path) {
    let _ = remove_file_if_exists(temporary).await;
    let _ = remove_file_if_exists(&crate::downloader::resume_sidecar_path(temporary)).await;
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) async fn source_response(
    db: &DatabaseConnection,
    source: youtube_source::Model,
) -> Result<YouTubeSourceResponse> {
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
        generate_image_post_video: source.generate_image_post_video,
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
        "douyin_mix" => Ok("douyin_mix"),
        _ => bail!("来源类型必须是 subscriptions、channel、playlist、liked、watch_later 或有效的抖音来源类型"),
    }
}
fn resolve_source_url(kind: &str, supplied: Option<&str>) -> Result<String> {
    match kind {
        "subscriptions" => Ok(SUBSCRIPTIONS_URL.to_string()),
        "liked" => Ok(LIKED_URL.to_string()),
        "watch_later" => Ok(WATCH_LATER_URL.to_string()),
        "playlist" => {
            let url = supplied
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .context("播放列表来源必须填写链接")?;
            normalize_youtube_playlist_url(url)
        }
        "douyin" | "douyin_collection" | "douyin_theater" | "douyin_series" | "douyin_mix" | "tiktok"
        | "tiktok_collection" => supplied
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

/// 从 `watch?v=...&list=...`、`playlist?list=...` 等链接中只保留播放列表 ID，
/// 统一转成 `playlist?list=`，避免 yt-dlp 把“视频 + 列表”误当成单个视频。
fn normalize_youtube_playlist_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value.trim()).context("YouTube 播放列表链接格式无效")?;
    let list_id = url
        .query_pairs()
        .find_map(|(key, value)| (key == "list" && !value.trim().is_empty()).then(|| value.into_owned()))
        .context("YouTube 播放列表链接缺少 list= 参数")?;
    Ok(format!("https://www.youtube.com/playlist?list={list_id}"))
}

/// 外源「同一个源」的判定键。
///
/// 外源不像 B 站那样有现成的业务主键（收藏夹 fid / UP mid / 番剧 season_id），
/// 只能从链接里取：抖音作者取 sec_uid、合集 / 放映厅 / 短剧取末段数字 id，
/// TikTok 取 handle（大小写不敏感）、YouTube 取频道 id / 播放列表 id；
/// 「稍后再看」「我的喜欢」这类天然只能有一个的源用固定键。
fn external_source_identity(source_type: &str, url: &str) -> String {
    let lower = url.trim().trim_end_matches('/').to_ascii_lowercase();
    let without_query = lower.split(['?', '#']).next().unwrap_or("");
    let last_segment = without_query
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("");
    let query_param = |key: &str| {
        lower
            .split_once('?')
            .and_then(|(_, query)| query.split('&').find_map(|pair| pair.strip_prefix(&format!("{key}="))))
            .unwrap_or("")
    };
    match source_type {
        "douyin" => format!("douyin:user:{last_segment}"),
        "douyin_collection" => format!("douyin:collection:{last_segment}"),
        "douyin_theater" => format!("douyin:theater:{last_segment}"),
        "douyin_series" => format!("douyin:series:{last_segment}"),
        "douyin_mix" => format!("douyin:mix:{last_segment}"),
        "douyin_liked" => "douyin:liked".to_string(),
        "douyin_watch_later" => "douyin:watch_later".to_string(),
        "tiktok" => format!("tiktok:user:{last_segment}"),
        "tiktok_collection" => format!("tiktok:collection:{last_segment}"),
        "tiktok_favorite" => "tiktok:favorite".to_string(),
        "channel" => format!("youtube:channel:{last_segment}"),
        "playlist" => {
            let list = query_param("list");
            format!("youtube:playlist:{}", if list.is_empty() { last_segment } else { list })
        }
        "subscriptions" => "youtube:subscriptions".to_string(),
        "liked" => "youtube:liked".to_string(),
        "watch_later" => "youtube:watch_later".to_string(),
        _ => lower,
    }
}

/// 外源互斥校验：同一个源（同平台 + 同一个身份）只允许存在一条记录。
///
/// 允许重复添加会导致同一个作者 / 合集被扫描两遍、同一批作品各下一份，
/// 所以这里直接拒绝，并把已存在的那条告诉用户。
async fn ensure_external_source_not_duplicated(
    db: &DatabaseConnection,
    source_type: &str,
    url: &str,
) -> Result<(), ApiError> {
    let identity = external_source_identity(source_type, url);
    let existing = youtube_source::Entity::find().all(db).await?;
    if let Some(duplicated) = existing
        .iter()
        .find(|source| external_source_identity(&source.source_type, &source.url) == identity)
    {
        let platform = source_platform_label(duplicated);
        return Err(ApiError::bad_request(format!(
            "{platform}视频源已存在：\"{}\"（保存路径：{}）。同一个源不能重复添加，如需修改设置请先删除现有源。",
            duplicated.name, duplicated.path
        )));
    }
    Ok(())
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
        crate::utils::process::tokio_command(executable)
            .arg("--version")
            .output(),
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

/// 归一化下载基址：去首尾空白、去尾部斜杠；空白或纯斜杠都视为未配置。
fn normalize_download_base_url(value: Option<&str>) -> Option<String> {
    value
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// 计算 yt-dlp 发布基址：配置了镜像（非空）时用它（去尾部斜杠），否则用官方基址。
fn resolve_ytdlp_release_base_url(mirror_base: Option<&str>) -> String {
    normalize_download_base_url(mirror_base).unwrap_or_else(|| YTDLP_RELEASE_BASE_URL.to_string())
}

/// 读取 yt-dlp 镜像基址；未配置或为空时回退到官方发布基址。
fn ytdlp_release_base_url() -> String {
    let configured = std::env::var(YTDLP_MIRROR_BASE_ENV).ok();
    resolve_ytdlp_release_base_url(configured.as_deref())
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

    let release_base = ytdlp_release_base_url();
    let asset_url = format!("{release_base}/{}", package.asset_name);
    let checksums_url = format!("{release_base}/SHA2-256SUMS");
    let manual_hint = format!(
        "若持续失败，可设置 {} 指向含同名资产与 SHA2-256SUMS 的镜像目录，或把 yt-dlp 手动放到 {}，或设置 BILI_SYNC_YTDLP_PATH 指向任意位置的可执行文件",
        YTDLP_MIRROR_BASE_ENV,
        binary_path.display()
    );
    match old_version {
        Some(version) => info!(version, url = asset_url, "yt-dlp 版本过旧，自动下载最新版替换"),
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
                builder = builder
                    .proxy(reqwest::Proxy::all(proxy).with_context(|| format!("解析 yt-dlp 下载代理失败：{proxy}"))?);
            }
        }
        builder.build().context("创建 yt-dlp 下载客户端失败")
    };
    let attempts: Vec<bool> = if proxy.is_some() {
        vec![true, false]
    } else {
        vec![false]
    };
    let total_attempts = attempts.len();

    let mut last_error = None;
    for (index, use_proxy) in attempts.into_iter().enumerate() {
        let client = build_client(use_proxy)?;
        let fetched = async {
            let (asset_response, checksums_response) =
                tokio::try_join!(client.get(&asset_url).send(), client.get(&checksums_url).send(),)
                    .with_context(|| format!("请求 yt-dlp 发布文件失败（{release_base}）；{manual_hint}"))?;
            let asset_bytes = asset_response
                .error_for_status()
                .with_context(|| format!("下载 yt-dlp 返回错误状态：{asset_url}；{manual_hint}"))?
                .bytes()
                .await
                .context("读取 yt-dlp 下载内容失败")?;
            let checksums = checksums_response
                .error_for_status()
                .with_context(|| format!("下载 yt-dlp 校验文件返回错误状态：{checksums_url}；{manual_hint}"))?
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
            .ok_or_else(|| anyhow!("yt-dlp 校验文件中缺少 {}；{manual_hint}", package.asset_name))?;
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

    match last_error {
        Some(error) => Err(error).with_context(|| format!("yt-dlp 下载失败；{manual_hint}")),
        None => bail!("yt-dlp 下载失败；{manual_hint}"),
    }
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
///
/// 只有两个调用方：导入新凭证前清场、导入失败且无可回滚的旧凭证。守护任务不再
/// 调用它 —— 自动删凭证正是「Cookie 过一会就掉了」的来源。`reason` 会打进日志，
/// 以后再出现「掉了」可以直接看是谁删的。
async fn clear_youtube_login_state(reason: &str) {
    warn!(reason, "清理 YouTube 登录凭证");
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

/// 去掉 Netscape 文件的 `#HttpOnly_` 前缀。
///
/// 浏览器扩展导出的 cookies.txt 会把 HttpOnly Cookie 写成 `#HttpOnly_.youtube.com…`，
/// 而 `SID` / `SAPISID` / `__Secure-1PSID` 这些会话 Cookie **全都是 HttpOnly**。
/// 之前这里直接按「以 # 开头就是注释」跳过，等于把整台机器的会话字段当注释丢掉：
/// 凭证明明在库里、下载也能用，界面却显示「未导入」，守护任务还会判成失效。
fn strip_httponly_prefix(line: &str) -> &str {
    line.strip_prefix("#HttpOnly_").unwrap_or(line)
}

fn has_youtube_session_value(contents: &str) -> bool {
    contents.lines().any(|raw_line| {
        let line = strip_httponly_prefix(raw_line);
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

/// 一次探测判定「已退出」后，等多久再复查一次。
///
/// 只有连续两次都拿到明确失效的证据才清理会话。单次判定的误报来源太多（代理节点
/// 切到需要同意条款的地区、碰到人机验证页、上游抖动），而这些误报一旦直接删凭证
/// 就是不可逆的。
const YOUTUBE_SESSION_RECHECK_DELAY: Duration = Duration::from_secs(30);

/// 最近一次联网登录探测的结论。
///
/// `Unclear` 是独立的一档：代理抖动、风控页、同意条款页都落在这里，既不能当成
/// 「有效」也不能当成「失效」——把它和 `Expired` 合并正是「Cookie 过一会就掉了」
/// 的根源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum YoutubeSessionState {
    Valid,
    Expired,
    Unclear,
}

impl YoutubeSessionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Expired => "expired",
            Self::Unclear => "unclear",
        }
    }
}

/// 最近一次 YouTube 登录探测的结论，只用于设置页展示，不作为是否保留凭证的依据。
#[derive(Debug, Clone)]
struct YoutubeSessionCheck {
    state: YoutubeSessionState,
    message: String,
    checked_at: String,
}

static YOUTUBE_SESSION_CHECK: LazyLock<StdMutex<Option<YoutubeSessionCheck>>> = LazyLock::new(|| StdMutex::new(None));

fn record_youtube_session_check(state: YoutubeSessionState, message: impl Into<String>) {
    let mut guard = YOUTUBE_SESSION_CHECK.lock().unwrap_or_else(|error| error.into_inner());
    *guard = Some(YoutubeSessionCheck {
        state,
        message: message.into(),
        checked_at: now_standard_string(),
    });
}

fn last_youtube_session_check() -> Option<YoutubeSessionCheck> {
    YOUTUBE_SESSION_CHECK
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

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
        match probe_youtube_subscription_channels().await {
            Ok(_) => {
                record_youtube_session_check(YoutubeSessionState::Valid, "登录状态有效，已完成会话续约");
                info!("YouTube 登录状态有效，已完成会话续约");
            }
            Err(YoutubeSessionError::Expired(error)) => {
                warn!(
                    error = %error,
                    delay_secs = YOUTUBE_SESSION_RECHECK_DELAY.as_secs(),
                    "YouTube 登录状态疑似失效，稍后复查确认；本次不清理凭证"
                );
                tokio::time::sleep(YOUTUBE_SESSION_RECHECK_DELAY).await;
                match probe_youtube_subscription_channels().await {
                    Ok(_) => {
                        record_youtube_session_check(
                            YoutubeSessionState::Valid,
                            "复查通过（首次探测疑似误判），已保留会话",
                        );
                        info!("YouTube 登录状态复查通过，保留现有会话");
                    }
                    Err(YoutubeSessionError::Expired(error)) => {
                        // 连续两次都拿不到登录态：只提示，**绝不自动删凭证**。
                        // 代理出口被 YouTube 判定为可疑时同样会返回登录墙，这类误判
                        // 与服务端真过期在协议层面无法区分；删掉用户仍可用的凭证
                        // （表现为「Cookie 过一会就掉了」）代价远大于留着过期凭证。
                        warn!(
                            error = %error,
                            "YouTube 登录状态连续两次探测为未登录，保留凭证不做清理；请到设置页确认后重新导入 cookies.txt"
                        );
                        if let Some(contents) = youtube_cookie_text() {
                            warn!(
                                diagnostic = %youtube_cookie_diagnostic(&contents),
                                "YouTube 已保存的登录凭证现状"
                            );
                        }
                        // 只在「上一次结论不是失效」时推送：守护任务每 6 小时跑一次，
                        // 凭证又不再自动清理，不做去重就会变成每 6 小时一条重复推送。
                        let already_reported = last_youtube_session_check()
                            .is_some_and(|check| check.state == YoutubeSessionState::Expired);
                        record_youtube_session_check(
                            YoutubeSessionState::Expired,
                            "最近探测返回未登录（代理/风控也可能是同样表现），凭证已保留，可重新导入",
                        );
                        if already_reported {
                            info!("YouTube 登录状态仍为未登录，此前已提示过，本次不重复推送");
                        } else {
                            record_external_login_expired(
                                "YouTube",
                                "YouTube 登录状态疑似失效（凭证已保留，未清理）。若订阅、稍后再看等读取失败，请在设置页重新导入 cookies.txt",
                            )
                            .await;
                        }
                    }
                    Err(YoutubeSessionError::Unclear(error)) => {
                        warn!(
                            error = %error,
                            "YouTube 登录状态复查结果不确定，保留现有会话"
                        );
                        record_youtube_session_check(
                            YoutubeSessionState::Unclear,
                            "复查结果不确定（代理/网络/风控），凭证已保留",
                        );
                    }
                }
            }
            Err(YoutubeSessionError::Unclear(error)) => {
                // 代理未开/上游抖动/风控页等：会话无法续约但保留现状。从 debug
                // 提升为 warn，避免用户看到“会话一直失效”却不知道是代理/网络导致
                // 续约根本没发生。
                warn!(
                    error = %error,
                    "YouTube 登录状态检查结果不确定（代理/网络/风控），本次未续约也未清理会话，保留现有凭证。请确认外源代理（proxy/youtube_proxy）可用"
                );
                record_youtube_session_check(
                    YoutubeSessionState::Unclear,
                    "检查结果不确定（代理/网络/风控），凭证已保留",
                );
            }
        }
    }
    Ok(())
}

async fn record_external_login_expired(platform: &str, message: &str) {
    info!(platform, "{}登录状态过期，已发送重新导入提示", platform);
    if let Err(error) =
        crate::utils::notification::send_error_notification(&format!("{platform}登录状态过期"), message, None).await
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
    // 图集作品的视频段各自成页（`<作品>-P01.mp4`…）：分页、分页侧车文件与主文件
    // 属于同一篇作品，移动、删除、重置都要一起处理，否则会留下孤儿分页。
    let base_stem = crate::douyin::split_image_post_page_stem(output_stem)
        .map(|(base, _)| base)
        .unwrap_or(output_stem);
    let is_page_file = |name: &str| -> bool {
        let Some(rest) = name.strip_prefix(&format!("{base_stem}-P")) else {
            return false;
        };
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        let tail = &rest[digits..];
        tail.is_empty() || tail.starts_with('.') || tail.starts_with('-')
    };
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
        if path == output || is_sidecar || is_page_file(name) {
            files.push(path);
        }
    }
    Ok(files)
}

/// 文件名是否符合下载器自己生成「半成品」的命名规则。
///
/// 只认本项目写出来的临时名（`.download.*`、`.merging.*`、`.decrypted.*`、
/// `.audio-source*`、`.resume` 断点状态等），不会误删用户的普通文件。
fn is_partial_download_artifact(name: &str) -> bool {
    name.ends_with(".resume")
        || name.ends_with(".download")
        || name.contains(".download.")
        || name.contains(".merging.")
        || name.contains(".decrypted.")
        || name.contains(".audio-source")
        || name.contains(".tmp_video")
        || name.contains(".tmp_audio")
}

/// 递归清理源目录里残留的半成品文件，返回删除个数（用于重设路径后的旧目录）。
async fn remove_partial_download_artifacts(root: &Path) -> usize {
    /// 媒体目录就是 `源根/UP/季` 这几层，留一点余量防止异常深层目录扫描。
    const MAX_DEPTH: usize = 6;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut removed = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&directory).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_partial_download_artifact(&name) {
                continue;
            }
            if tokio::fs::remove_file(entry.path()).await.is_ok() {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        info!(
            path = %root.display(),
            removed,
            "已清理旧目录中未完成的下载临时文件"
        );
    }
    removed
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
    let mut command = crate::utils::process::tokio_command(ytdlp_executable());
    command.kill_on_drop(true);
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

pub(crate) async fn append_ytdlp_runtime(command: &mut Command) {
    let Some((name, path)) = ytdlp_js_runtime() else {
        return;
    };
    // 老版本 yt-dlp 不认 `--js-runtimes`，无条件下发会让整条 YouTube 链路因为未知
    // 参数直接失败（用户可能把 `BILI_SYNC_YTDLP_PATH` 指向旧版二进制）。
    // 这里按「当前二进制是否支持该参数」判断，只对支持的版本下发。
    if !ytdlp_supports_js_runtimes().await {
        return;
    }
    command.arg("--js-runtimes").arg(format!("{name}:{}", path.display()));
}

/// `--js-runtimes` 的探测结果缓存键：路径 + mtime + 大小。
/// 带上 mtime/大小是为了 yt-dlp 原地升级后能重新探测（只看路径会一直用旧结论）。
type JsRuntimeProbeKey = (PathBuf, Option<std::time::SystemTime>, u64);

/// 探测当前 yt-dlp 是否支持 `--js-runtimes`（`--help` 能看到该选项且命令成功退出）。
///
/// 用 `tokio::process` 而不是 `std::process`：这里在 worker 线程上跑，同步等子进程
/// 会把整个 runtime 线程堵住。探测**失败**（二进制还没就绪、`--help` 非 0 退出）
/// 不写缓存，等 yt-dlp 就绪后再探一次。
async fn ytdlp_supports_js_runtimes() -> bool {
    static CACHE: LazyLock<StdMutex<HashMap<JsRuntimeProbeKey, bool>>> =
        LazyLock::new(|| StdMutex::new(HashMap::new()));
    let binary = ytdlp_executable();
    let key = ytdlp_probe_cache_key(&binary);
    if let Some(supported) = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .copied()
    {
        return supported;
    }
    let output = match tokio::process::Command::new(&binary).arg("--help").output().await {
        Ok(output) => output,
        Err(error) => {
            debug!(path = %binary.display(), "探测 yt-dlp 是否支持 --js-runtimes 失败（不缓存）: {error}");
            return false;
        }
    };
    if !output.status.success() {
        // 非 0 退出通常是二进制还没下载完 / 当前不可执行，不能当成「不支持」缓存住。
        debug!(
            path = %binary.display(),
            status = ?output.status.code(),
            "yt-dlp --help 非 0 退出，本轮跳过 --js-runtimes 探测（不缓存）"
        );
        return false;
    }
    let supported = String::from_utf8_lossy(&output.stdout).contains("--js-runtimes");
    if !supported {
        debug!(
            path = %binary.display(),
            "当前 yt-dlp 不支持 --js-runtimes，跳过 JS runtime 下发（n challenge 可能失败，建议升级 yt-dlp）"
        );
    }
    CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, supported);
    supported
}

fn ytdlp_probe_cache_key(binary: &Path) -> JsRuntimeProbeKey {
    let metadata = std::fs::metadata(binary).ok();
    (
        binary.to_path_buf(),
        metadata.as_ref().and_then(|meta| meta.modified().ok()),
        metadata.as_ref().map(|meta| meta.len()).unwrap_or_default(),
    )
}

fn ytdlp_js_runtime() -> Option<(&'static str, PathBuf)> {
    if let Ok(configured) = std::env::var("BILI_SYNC_YTDLP_JS_RUNTIME") {
        let path = PathBuf::from(configured);
        if is_executable_file(&path) {
            return Some((ytdlp_js_runtime_name(&path), path));
        }
    }
    const RUNTIME_NAMES: [&str; 4] = ["node", "bun", "deno", "qjs"];
    // 1) PATH：跨平台通用。注意 GUI 方式启动时 PATH 可能被裁剪，找不到就再查常见安装位置。
    if let Some(path) = std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).find_map(|dir| runtime_executable_in_dir(&dir, &RUNTIME_NAMES)))
    {
        return Some((ytdlp_js_runtime_name(&path), path));
    }
    // 2) 常见安装位置兜底。
    let mut candidate_dirs: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        candidate_dirs.extend([
            PathBuf::from(r"C:\Program Files\nodejs"),
            PathBuf::from(r"C:\Program Files (x86)\nodejs"),
            CONFIG_DIR.join("bin"),
        ]);
        if let Ok(profile) = std::env::var("USERPROFILE") {
            candidate_dirs.push(PathBuf::from(profile).join(".bun").join("bin"));
        }
    }
    #[cfg(not(windows))]
    {
        candidate_dirs.extend([
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/bin"),
            CONFIG_DIR.join("bin"),
        ]);
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            candidate_dirs.push(home.join(".bun").join("bin"));
            candidate_dirs.push(home.join(".deno").join("bin"));
            // nvm 的 node 按 `~/.nvm/versions/node/<版本>/bin` 布局安装，
            // 版本目录名排序后优先取较新的。
            if let Ok(entries) = std::fs::read_dir(home.join(".nvm").join("versions").join("node")) {
                let mut versions: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .collect();
                versions.sort();
                for version in versions.into_iter().rev() {
                    candidate_dirs.push(version.join("bin"));
                }
            }
        }
    }
    candidate_dirs
        .into_iter()
        .find_map(|dir| runtime_executable_in_dir(&dir, &RUNTIME_NAMES))
        .map(|path| (ytdlp_js_runtime_name(&path), path))
}

/// 在目录中查找第一个可执行的 JS runtime（Windows 下自动补 `.exe`）。
fn runtime_executable_in_dir(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| {
        #[cfg(windows)]
        let candidate = dir.join(format!("{name}.exe"));
        #[cfg(not(windows))]
        let candidate = dir.join(name);
        is_executable_file(&candidate).then_some(candidate)
    })
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
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

fn is_netscape_youtube_cookie_file(contents: &str) -> bool {
    let has_header = contents
        .lines()
        .take(4)
        .any(|line| line.contains("HTTP Cookie File") || line.contains("Netscape"));
    has_header
        && contents.lines().any(|raw_line| {
            // 浏览器扩展导出的会话 Cookie 带 `#HttpOnly_` 前缀，不能当成注释跳过。
            let line = strip_httponly_prefix(raw_line);
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

#[cfg(test)]
mod tests {
    #[test]
    #[allow(invalid_from_utf8, reason = "测试故意用 GBK 字节序列验证非 UTF-8 解码路径")]
    fn decodes_gbk_command_error_text() {
        // GBK 编码的“远程主机强迫关闭了一个现有的连接”（Windows 10054 错误文本）
        let gbk: &[u8] = &[
            0xd4, 0xb6, 0xb3, 0xcc, 0xd6, 0xf7, 0xbb, 0xfa, 0xc7, 0xbf, 0xc6, 0xc8, 0xb9, 0xd8, 0xb1, 0xd5, 0xc1, 0xcb,
            0xd2, 0xbb, 0xb8, 0xf6, 0xcf, 0xd6, 0xd3, 0xd0, 0xb5, 0xc4, 0xc1, 0xac, 0xbd, 0xd3,
        ];
        assert!(std::str::from_utf8(gbk).is_err());
        let decoded = super::decode_command_bytes(gbk);
        assert!(decoded.contains("远程主机"), "decoded: {decoded}");
        assert!(decoded.contains("连接"), "decoded: {decoded}");
    }

    /// 图集落盘扫描：图片段进原图列表、视频段只记标志位，两者不能混淆——
    /// 前端要靠「有视频段但没有原图」判断这是多段视频/动态作品。
    #[test]
    fn image_post_local_files_separates_images_from_video_segments() {
        use std::fs;

        let stamp = std::process::id();
        let root = std::env::temp_dir().join(format!("bili-sync-image-post-scan-{stamp}"));
        let _ = fs::remove_dir_all(&root);
        let output = root.join("20260101000000-7678519182983467944.mp4");
        let image_dir = root.join("20260101000000-7678519182983467944-images");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(image_dir.join("01.mp4"), b"video").unwrap();
        fs::write(image_dir.join("02.jpg"), b"image").unwrap();
        fs::write(image_dir.join("03.JPG"), b"image").unwrap();
        fs::write(image_dir.join("04.segment.mp4"), b"clip").unwrap();
        fs::write(image_dir.join("notes.txt"), b"text").unwrap();

        let mixed = crate::youtube::image_post_local_files(&output);
        assert!(mixed.has_video_segments, "有 mp4 段就要标记视频段");
        let names = mixed
            .images
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["02.jpg".to_string(), "03.JPG".to_string()],
            "只统计真正的原图"
        );

        // 纯视频段图集：没有原图，但必须被识别成图集（否则会显示成图片尚未下载）。
        let video_only_dir = root.join("20260101000000-7689447727160205745-images");
        fs::create_dir_all(&video_only_dir).unwrap();
        fs::write(video_only_dir.join("01.mp4"), b"video").unwrap();
        let video_only = crate::youtube::image_post_local_files(&root.join("20260101000000-7689447727160205745.mp4"));
        assert!(video_only.has_video_segments);
        assert!(video_only.images.is_empty());

        // 还没下载（目录不存在）时两个标志都为空，前端继续显示「尚未下载」。
        let missing = crate::youtube::image_post_local_files(&root.join("20260101000000-1.mp4"));
        assert!(!missing.has_video_segments);
        assert!(missing.images.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unique_download_path_isolates_concurrent_shared_assets() {
        // UP 头像 / 剧集封面按 UP（或剧集）共享同一个目标路径，多个视频并发下载时
        // 必须各自使用互不相同的临时文件：否则先完成的任务 rename 走临时文件后，
        // 另一个任务会读到 0 字节，报「断点续传后文件大小不一致: 0 != N」。
        let target = Path::new("people/白铭/folder.jpg");
        let first = unique_download_path(target);
        let second = unique_download_path(target);
        assert_ne!(first, second);
        assert_eq!(first.parent(), target.parent());
        let name = first.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("folder.jpg."), "临时名应保留目标名: {name}");
        assert!(name.ends_with(".download"), "临时名应以 .download 结尾: {name}");
        assert_ne!(name, "folder.download", "不能退化成所有任务共用的固定临时名");
    }

    #[cfg(windows)]
    #[test]
    fn episodic_series_dir_handles_flat_and_season_layouts() {
        // 平铺目录：视频直接在源目录里，合集根就是源目录，不能再往上退一层
        assert_eq!(
            episodic_series_dir(
                Path::new(r"J:\EMBY\AI短剧\困兽之战\S01E01 - x.mp4"),
                r"J:\EMBY\AI短剧\困兽之战"
            ),
            Some(PathBuf::from(r"J:\EMBY\AI短剧\困兽之战"))
        );
        // Season 结构：合集根是 Season 的上一层
        assert_eq!(
            episodic_series_dir(
                Path::new(r"J:\EMBY\AI短剧\困兽之战\Season 01\S01E01 - x.mp4"),
                r"J:\EMBY\AI短剧\困兽之战"
            ),
            Some(PathBuf::from(r"J:\EMBY\AI短剧\困兽之战"))
        );
        // 源根目录下再套一层合集名的 Season 结构
        assert_eq!(
            episodic_series_dir(
                Path::new(r"J:\EMBY\AI短剧\陆离志怪录\Season 01\S01E01 - x.mp4"),
                r"J:\EMBY\AI短剧"
            ),
            Some(PathBuf::from(r"J:\EMBY\AI短剧\陆离志怪录"))
        );
        // 无论如何都不能写到源目录之外（旧版本就是这样把封面甩到父目录的）
        assert_eq!(
            episodic_series_dir(Path::new(r"D:\other\S01E01 - x.mp4"), r"J:\EMBY\AI短剧\困兽之战"),
            Some(PathBuf::from(r"J:\EMBY\AI短剧\困兽之战"))
        );
    }

    #[test]
    fn detects_non_jpeg_cover_payloads() {
        // 抖音封面会随机返回 JPEG / WebP，只能靠文件头判断，不能看后缀。
        assert!(is_jpeg_header(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0]));
        assert!(!is_jpeg_header(b"RIFF\x00\x00\x00\x00WEBP"));
        assert!(needs_jpeg_conversion(b"RIFF\x24\x00\x00\x00WEBPVP8 "));
        assert!(needs_jpeg_conversion(&[
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0
        ]));
        assert!(needs_jpeg_conversion(b"GIF89a......"));
        // 已经是 JPEG 的不再转（避免每轮扫描都空跑 ffmpeg）
        assert!(!needs_jpeg_conversion(&[
            0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0, 0, 0, 0, 0, 0
        ]));
    }

    #[test]
    fn truncated_reused_media_is_rejected_by_duration() {
        // 平台元数据 3600 秒，旧文件只有 3000 秒（约 1.7GB vs 2GB），必须重下
        assert!(!super::media_duration_is_complete(3000.0, 3600.0));
        // 正常封装/探测误差允许 2 秒或 2%
        assert!(super::media_duration_is_complete(3599.0, 3600.0));
        assert!(super::media_duration_is_complete(3529.0, 3600.0));
        // 元数据缺失或异常时不误判；实际时长无效则不能复用
        assert!(super::media_duration_is_complete(0.0, 0.0));
        assert!(super::media_duration_is_complete(123.0, f64::NAN));
        assert!(!super::media_duration_is_complete(0.0, 3600.0));
    }

    #[test]
    fn partial_artifact_detection_covers_downloader_temp_names() {
        for name in [
            "S01E01 - 标题.download.mp4",
            "S01E01 - 标题.merging.mp4",
            "S01E01 - 标题.decrypted.mp4",
            "S01E01 - 标题.audio-source.mp4",
            "S01E01 - 标题.download.mp4.resume",
            "S01E01 - 标题.nfo.download",
            "folder.jpg.0198a2bc.download",
        ] {
            assert!(is_partial_download_artifact(name), "{name} 应判为半成品");
        }
        for name in [
            "S01E01 - 标题.mp4",
            "S01E01 - 标题.nfo",
            "S01E01 - 标题.zh-CN.default.ass",
            "S01E01 - 标题-thumb.jpg",
        ] {
            assert!(!is_partial_download_artifact(name), "{name} 不应被判为半成品");
        }
    }

    #[tokio::test]
    async fn remove_partial_artifacts_keeps_finished_media() {
        let root = std::env::temp_dir().join(format!(
            "bili-sync-partial-{}-{}",
            std::process::id(),
            chrono::Local::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let season = root.join("合集").join("Season 01");
        tokio::fs::create_dir_all(&season).await.unwrap();
        let keep = season.join("S01E01 - 标题.mp4");
        let keep_nfo = season.join("S01E01 - 标题.nfo");
        let partial = season.join("S01E02 - 标题.download.mp4");
        let resume = season.join("S01E02 - 标题.download.mp4.resume");
        for path in [&keep, &keep_nfo, &partial, &resume] {
            tokio::fs::write(path, b"x").await.unwrap();
        }
        let removed = remove_partial_download_artifacts(&root).await;
        assert_eq!(removed, 2);
        assert!(keep.exists());
        assert!(keep_nfo.exists());
        assert!(!partial.exists());
        assert!(!resume.exists());
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[test]
    fn startup_skips_recently_scanned_sources() {
        let now = chrono::Local::now().naive_local();
        let recent = now.format("%Y-%m-%d %H:%M:%S").to_string();
        assert!(source_scanned_recently(Some(&recent), 1200));
        let old = (now - chrono::Duration::hours(2))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert!(!source_scanned_recently(Some(&old), 1200));
        assert!(!source_scanned_recently(None, 1200));
        assert!(!source_scanned_recently(Some("not-a-date"), 1200));
    }

    #[cfg(windows)]
    use super::episodic_series_dir;
    use super::{
        canonical_channel_url, checksum_for_release_asset, collect_external_scan_result,
        collect_youtube_channel_renderers, current_ytdlp_package, extract_youtube_initial_data,
        generate_youtube_person_nfo, has_youtube_session_value, is_jpeg_header, is_netscape_youtube_cookie_file,
        is_partial_download_artifact, is_youtube_url, needs_jpeg_conversion, normalize_source_type,
        normalize_youtube_playlist_url, parse_youtube_live_chat, remove_partial_download_artifacts, resolve_source_url,
        should_proxy_ytdlp_url, source_scanned_recently, unique_download_path, youtube_cookie_jar,
        youtube_login_verdict, youtube_search_url, ytdlp_js_runtime_name, ytdlp_package_for, ytdlp_runtime_target_env,
        YoutubeLoginVerdict, SUBSCRIPTIONS_URL, WATCH_LATER_URL,
    };
    use std::collections::HashSet;
    use std::path::Path;
    #[cfg(windows)]
    use std::path::PathBuf;

    use bili_sync_migration::MigratorTrait;
    use sea_orm::ActiveModelTrait;

    /// 建一个已完成迁移的临时测试库（单连接 WAL 文件库，避免 :memory: 多连接互不可见）。
    async fn create_notification_test_db(prefix: &str) -> sea_orm::DatabaseConnection {
        use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
        use sea_orm::SqlxSqliteConnector;

        let mut dir = std::env::temp_dir();
        dir.push(format!("bili-sync-{}-{}", prefix, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("应能创建临时数据库目录");
        let db_path = dir.join("data.sqlite");

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("应能连接测试数据库");
        let db = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool);

        bili_sync_migration::Migrator::up(&db, None)
            .await
            .expect("应能完成测试数据库迁移");
        db
    }

    async fn insert_external_video(
        db: &sea_orm::DatabaseConnection,
        source_id: i32,
        youtube_id: &str,
        title: &str,
        created_at: &str,
    ) {
        super::youtube_video::ActiveModel {
            source_id: super::Set(source_id),
            youtube_id: super::Set(youtube_id.to_string()),
            url: super::Set(format!("https://www.youtube.com/watch?v={youtube_id}")),
            title: super::Set(title.to_string()),
            created_at: super::Set(created_at.to_string()),
            updated_at: super::Set(created_at.to_string()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("应能插入测试视频");
    }

    /// 「扫描完成」推送只应包含本轮扫描新入库的外源作品，历史作品不能混进来。
    #[tokio::test]
    async fn collect_external_scan_result_only_reads_videos_added_by_this_scan() {
        let db = create_notification_test_db("external-scan-summary").await;
        let source = sample_external_source("channel");
        let source_id = source.id;
        let source_row: super::youtube_source::ActiveModel = source.clone().into();
        source_row.insert(&db).await.expect("应能插入测试视频源");

        insert_external_video(&db, source_id, "oldOne", "上一轮就有的视频", "2026-09-21 09:00:00").await;
        insert_external_video(&db, source_id, "newOne", "本轮新增视频 A", "2026-09-21 10:00:00").await;
        insert_external_video(&db, source_id, "newTwo", "本轮新增视频 B", "2026-09-21 10:00:05").await;

        let result = collect_external_scan_result(&db, &source, "2026-09-21 10:00:00", 2).await;

        assert_eq!(result.source_type, "YouTube");
        assert_eq!(result.source_name, source.name);
        assert_eq!(result.new_videos.len(), 2, "只应统计本轮新增的作品");
        let titles: Vec<&str> = result.new_videos.iter().map(|video| video.title.as_str()).collect();
        assert!(titles.contains(&"本轮新增视频 A"), "titles: {titles:?}");
        assert!(titles.contains(&"本轮新增视频 B"), "titles: {titles:?}");
        assert!(!titles.contains(&"上一轮就有的视频"), "历史作品不应进推送: {titles:?}");
        assert!(
            result
                .new_videos
                .iter()
                .all(|video| video.url.as_deref().is_some_and(|url| url.starts_with("https://"))),
            "外源视频应带上自身链接: {:?}",
            result.new_videos
        );
    }

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
            generate_image_post_video: true,
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

    /// 真实网络测试（默认 ignore，需要 yt-dlp 与 TikTok 会话可用）：
    /// TikTok 图片贴经统一下载入口解析后应带原图、不带视频流；普通视频相反。
    /// 验证「图片贴 → 图文链路」的分流不会误伤普通视频。
    #[tokio::test]
    #[ignore]
    async fn tiktok_photo_and_video_split_by_media_kind() {
        let photo_url =
            std::env::var("BILI_SYNC_TEST_TIKTOK_PHOTO_URL").expect("需要设置 BILI_SYNC_TEST_TIKTOK_PHOTO_URL");
        let photo = super::extract_youtube_metadata(&photo_url, None)
            .await
            .expect("解析 TikTok 图片贴失败");
        assert!(!photo.images.is_empty(), "图片贴应带原图");
        assert!(photo.formats.is_empty(), "图片贴不应带视频流");

        let video_url =
            std::env::var("BILI_SYNC_TEST_TIKTOK_VIDEO_URL").expect("需要设置 BILI_SYNC_TEST_TIKTOK_VIDEO_URL");
        let video = super::extract_youtube_metadata(&video_url, None)
            .await
            .expect("解析 TikTok 普通视频失败");
        assert!(video.images.is_empty(), "普通视频不应带原图");
        assert!(!video.formats.is_empty(), "普通视频应带视频流");
    }

    /// 真实网络测试（默认 ignore）：只有元数据、媒体取不到的 TikTok 作品必须
    /// 给出“换节点重试”的可操作提示，且**不能**被判为永久不可下载。
    #[tokio::test]
    #[ignore]
    async fn media_unavailable_tiktok_post_stays_retryable() {
        let url = std::env::var("BILI_SYNC_TEST_TIKTOK_REGION_URL").expect("需要设置 BILI_SYNC_TEST_TIKTOK_REGION_URL");
        let error = super::extract_youtube_metadata(&url, None)
            .await
            .expect_err("媒体取不到的作品应返回错误");
        let text = format!("{error:#}");
        assert!(
            !crate::tiktok::is_tiktok_unavailable_error(&error),
            "媒体取不到不能被判定为永久不可下载（换节点或作品恢复后仍应能重试）：{text}"
        );
        assert!(
            text.contains("媒体内容取不到") || text.contains("没有可下载的媒体"),
            "应给出可操作的原因提示：{text}"
        );
        // 该错误必须能被「连续重试到阈值后落占位」的判定识别到，否则这条作品会
        // 一直停在待重试状态空转（这正是本次要修的问题）。
        assert!(
            crate::tiktok::is_tiktok_media_missing_error(&error),
            "媒体取不到的错误必须能被落占位判定识别：{text}"
        );
        assert!(!super::should_placeholder_unavailable_tiktok(&error, 0));
        assert!(super::should_placeholder_unavailable_tiktok(&error, 2));
    }
    /// 媒体取不到的作品：前两轮仍按普通失败重试（给换节点留机会），
    /// 连续到阈值才落「无法下载视频」占位；statusCode 非 0 的明确不可下载则立刻落。
    #[test]
    fn tiktok_media_missing_placeholder_waits_for_retry_threshold() {
        let media_missing = anyhow::anyhow!(
            "TikTok 作品的媒体内容取不到：官方接口只返回了作品信息（标题/作者等），没有返回任何视频、封面或原图"
        );
        assert!(!super::should_placeholder_unavailable_tiktok(&media_missing, 0));
        assert!(!super::should_placeholder_unavailable_tiktok(&media_missing, 1));
        assert!(super::should_placeholder_unavailable_tiktok(&media_missing, 2));
        assert!(super::should_placeholder_unavailable_tiktok(&media_missing, 4));
        // 0 次重试时绝不能落占位：换节点后可能又能取到。
        assert!(!crate::tiktok::is_tiktok_unavailable_error(&media_missing));
        let hard_unavailable =
            anyhow::anyhow!("TikTok视频无法下载：你所在国家或地区无法下载此视频（statusCode 10231）");
        assert!(super::should_placeholder_unavailable_tiktok(&hard_unavailable, 0));
        // 其它类型的失败（风控、解析报错等）继续走原有重试逻辑，不受影响。
        let other = anyhow::anyhow!("yt-dlp 解析 TikTok 媒体直链失败：HTTP 403");
        assert!(!super::should_placeholder_unavailable_tiktok(&other, 9));
        assert!(!super::should_placeholder_unavailable_tiktok(&other, 0));
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
            video_task_status: super::youtube_all_tasks_ok_bits(),
            page_task_status: super::youtube_all_tasks_ok_bits(),
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
        assert!(super::is_episodic_douyin_source(&sample_external_source(
            "douyin_theater"
        )));
        assert!(super::is_episodic_douyin_source(&sample_external_source(
            "douyin_series"
        )));
        assert!(super::is_episodic_douyin_source(&sample_external_source("douyin_mix")));
        assert!(!super::is_episodic_douyin_source(&sample_external_source(
            "douyin_collection"
        )));
        assert!(!super::is_episodic_douyin_source(&sample_external_source("douyin")));
    }

    #[test]
    fn episodic_layout_requires_a_real_episode_number() {
        let series = sample_external_source("douyin_series");
        assert!(super::uses_episodic_layout(&series, Some(1)));
        assert!(!super::uses_episodic_layout(&series, None));
        assert!(!super::uses_episodic_layout(&series, Some(0)));

        let collection = sample_external_source("douyin_collection");
        assert!(
            !super::uses_episodic_layout(&collection, Some(3)),
            "抖音收藏夹即使标题里碰巧带集号，也必须保持普通单视频目录"
        );
    }
    #[test]
    fn validates_types_and_urls() {
        assert_eq!(normalize_source_type("playlist").unwrap(), "playlist");
        assert_eq!(resolve_source_url("subscriptions", None).unwrap(), SUBSCRIPTIONS_URL);
        assert_eq!(resolve_source_url("watch_later", None).unwrap(), WATCH_LATER_URL);
        assert_eq!(
            normalize_youtube_playlist_url("https://www.youtube.com/watch?v=abc&list=PL123&pp=xyz").unwrap(),
            "https://www.youtube.com/playlist?list=PL123"
        );
        assert!(super::youtube_source_video_selection_supported("watch_later"));
        assert!(super::youtube_source_video_selection_supported("playlist"));
        assert!(!super::youtube_source_video_selection_supported("douyin"));
        assert!(is_youtube_url("https://www.youtube.com/watch?v=abc"));
        assert!(!is_youtube_url("https://youtube.example.com"));
    }

    #[test]
    fn playlist_catalog_accepts_bare_ytdlp_playlist_ids() {
        let item = serde_json::json!({"id": "PLabc123", "title": "测试列表"});
        assert_eq!(
            super::youtube_playlist_url_from_item(&item).as_deref(),
            Some("https://www.youtube.com/playlist?list=PLabc123")
        );
    }

    #[test]
    fn filters_unavailable_videos_from_youtube_lists() {
        assert!(super::ytdlp_flat_item_is_unavailable(
            &serde_json::json!({"id": "deleted", "title": "[Deleted video]"})
        ));
        assert!(super::ytdlp_flat_item_is_unavailable(
            &serde_json::json!({"id": "private", "availability": "private"})
        ));
        assert!(!super::ytdlp_flat_item_is_unavailable(
            &serde_json::json!({"id": "public", "title": "正常视频", "availability": "public"})
        ));
    }

    #[test]
    fn douyin_tvshow_nfo_includes_uploader_actor_and_avatar() {
        let source = sample_external_source("douyin_mix");
        let metadata: crate::external_media::ExternalMediaMetadata =
            serde_json::from_str(r#"{"id":"mix-episode","uploader":"测试作者","description":"合集简介"}"#)
                .expect("metadata 应可构造");
        let xml = super::generate_youtube_tvshow_nfo(&source, &metadata, Some("https://example.com/avatar.jpg"));
        assert!(
            xml.contains(
                "<actor><name>测试作者</name><role>UP主</role><thumb>https://example.com/avatar.jpg</thumb></actor>"
            ),
            "{xml}"
        );
    }

    #[test]
    fn youtube_stream_selection_falls_back_to_vp9_for_highest_quality() {
        use super::ExternalMediaFormat;
        use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};

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
            is_original: false,
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
            is_original: false,
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
        use super::ExternalMediaFormat;
        use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};

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
            is_original: false,
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
            is_original: false,
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
    fn douyin_original_stream_wins_same_resolution_over_codec_preference() {
        use super::ExternalMediaFormat;
        use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};

        let filter = FilterOption {
            video_max_quality: VideoQuality::Quality8k,
            video_min_quality: VideoQuality::Quality360p,
            audio_max_quality: AudioQuality::QualityHiRES,
            audio_min_quality: AudioQuality::Quality64k,
            codecs: vec![VideoCodecs::AVC, VideoCodecs::HEV, VideoCodecs::AV1],
            ..Default::default()
        };
        let current = ExternalMediaFormat {
            format_id: Some("normal_1080_0".to_string()),
            url: Some("https://example.com/current.mp4".to_string()),
            protocol: Some("https".to_string()),
            ext: Some("mp4".to_string()),
            vcodec: Some("h264".to_string()),
            acodec: Some("aac".to_string()),
            width: Some(1080),
            height: Some(1920),
            fps: Some(20.0),
            tbr: Some(688.0),
            vbr: Some(688.0),
            abr: Some(128.0),
            dynamic_range: Some("SDR".to_string()),
            is_original: false,
            decryption_key: None,
            fallback_urls: Vec::new(),
        };
        let original = ExternalMediaFormat {
            format_id: Some("douyin-original".to_string()),
            vcodec: Some("h265".to_string()),
            url: Some("https://example.com/original.mp4".to_string()),
            tbr: Some(6830.0),
            vbr: Some(6830.0),
            is_original: true,
            ..current.clone()
        };
        let selected =
            super::select_youtube_streams(&[current, original], &filter, false, "抖音").expect("应选出原画候选");
        let mixed = selected.mixed.expect("应选择混合流");
        assert_eq!(
            mixed.format_id.as_deref(),
            Some("douyin-original"),
            "同分辨率下应优先选择高码率原画，而不是仅按编码偏好选择 H.264"
        );
    }

    #[test]
    fn youtube_stream_selection_vp9_fallback_respects_no_hdr_and_max_height() {
        use super::ExternalMediaFormat;
        use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};

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
            is_original: false,
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
            is_original: false,
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
        let xml = super::build_youtube_movie_nfo_xml(super::YoutubeMovieNfoInput {
            title: "测试视频",
            description: "简介",
            platform: "YouTube",
            studio: "YouTube",
            id: "dQw4w9WgXcQ",
            aired,
            uploader: "测试频道",
            co_creators: &[],
            thumbnail: "https://example.com/thumb.jpg",
            video_url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            avatar_url: None,
        });
        assert!(xml.contains("<studio>YouTube</studio>"), "studio 应为平台名: {xml}");
        assert!(xml.contains("<aired>2026-09-01</aired>"), "aired 应为日期: {xml}");
        assert!(
            xml.contains("<premiered>2026-09-01</premiered>"),
            "premiered 应为日期: {xml}"
        );
        assert!(xml.contains("<year>2026</year>"), "year 应为年份: {xml}");
        assert!(!xml.contains("<studio>2026-09-01</studio>"), "studio 不应是日期: {xml}");
        assert!(!xml.contains("<aired>YouTube</aired>"), "aired 不应是平台名: {xml}");
        assert!(
            xml.contains("<director>测试频道</director>"),
            "director 应为频道: {xml}"
        );
    }

    #[test]
    fn youtube_movie_nfo_includes_co_creators_as_actors() {
        // 回归测试：联合投稿视频的合作频道应逐个补写为 <actor>，主频道仅出现一次。
        let aired = chrono::NaiveDate::from_ymd_opt(2026, 9, 1)
            .expect("有效日期")
            .and_hms_opt(0, 0, 0)
            .expect("有效时间");
        let xml = super::build_youtube_movie_nfo_xml(super::YoutubeMovieNfoInput {
            title: "联合投稿测试",
            description: "简介",
            platform: "YouTube",
            studio: "YouTube",
            id: "vid123",
            aired,
            uploader: "主频道",
            co_creators: &["合作频道A".to_string(), "合作频道B".to_string()],
            thumbnail: "https://example.com/thumb.jpg",
            video_url: "https://www.youtube.com/watch?v=vid123",
            avatar_url: None,
        });
        assert_eq!(
            xml.matches("<actor>").count(),
            3,
            "主频道+2个合作频道共3个 actor: {xml}"
        );
        assert!(
            xml.contains("<actor><name>主频道</name><role>频道</role></actor>"),
            "{xml}"
        );
        assert!(
            xml.contains("<actor><name>合作频道A</name><role>频道</role></actor>"),
            "{xml}"
        );
        assert!(
            xml.contains("<actor><name>合作频道B</name><role>频道</role></actor>"),
            "{xml}"
        );
        assert!(
            xml.contains("<director>主频道</director>"),
            "director 应为主频道: {xml}"
        );
        assert_eq!(
            xml.matches("主频道").count(),
            2,
            "主频道只在 director 与第一个 actor 出现: {xml}"
        );
    }

    #[test]
    fn youtube_movie_nfo_writes_uploader_avatar_thumb() {
        // 抖音等外源的作者头像地址要写进主频道 actor，媒体库才显示 UP 头像；
        // 合作频道没有各自的头像地址，不应误写同一个 <thumb>。
        let aired = chrono::NaiveDate::from_ymd_opt(2026, 10, 4)
            .expect("有效日期")
            .and_hms_opt(0, 0, 0)
            .expect("有效时间");
        let xml = super::build_youtube_movie_nfo_xml(super::YoutubeMovieNfoInput {
            title: "抖音作品",
            description: "简介",
            platform: "douyin",
            studio: "抖音",
            id: "7515762262805777727",
            aired,
            uploader: "古德末泥(高智版)",
            co_creators: &[],
            thumbnail: "https://example.com/cover.jpg",
            video_url: "https://www.douyin.com/video/7515762262805777727",
            avatar_url: Some("https://p3.douyinpic.com/avatar.jpeg"),
        });
        assert!(
            xml.contains("<actor><name>古德末泥(高智版)</name><role>UP主</role><thumb>https://p3.douyinpic.com/avatar.jpeg</thumb></actor>"),
            "主频道 actor 应带 <thumb> 头像: {xml}"
        );
        assert_eq!(
            xml.matches("<role>UP主</role><thumb>").count(),
            1,
            "只有主频道 actor 写头像 <thumb>: {xml}"
        );
        assert!(
            !xml.contains("<director>"),
            "抖音 UGC 不应把同一 UP 重复写成导演: {xml}"
        );
        assert!(
            xml.contains("<fanart><thumb>https://example.com/cover.jpg</thumb></fanart>"),
            "{xml}"
        );

        // 空白头像地址不写 <thumb>，避免生成空标签
        let xml = super::build_youtube_movie_nfo_xml(super::YoutubeMovieNfoInput {
            title: "抖音作品",
            description: "简介",
            platform: "douyin",
            studio: "抖音",
            id: "7515762262805777727",
            aired,
            uploader: "古德末泥(高智版)",
            co_creators: &[],
            thumbnail: "https://example.com/cover.jpg",
            video_url: "https://www.douyin.com/video/7515762262805777727",
            avatar_url: Some("   "),
        });
        assert!(
            xml.contains("<actor><name>古德末泥(高智版)</name><role>UP主</role></actor>"),
            "{xml}"
        );
        assert_eq!(
            xml.matches("<role>UP主</role><thumb>").count(),
            0,
            "空白头像地址不应写 <thumb>: {xml}"
        );
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
        assert_eq!(
            super::youtube_co_creators(&metadata, "主频道VEVO"),
            Vec::<String>::new()
        );
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
        let metadata: ExternalMediaMetadata = serde_json::from_str(r#"{"id":"v1"}"#).expect("metadata 应可解析");
        assert_eq!(super::youtube_co_creators(&metadata, "主频道"), Vec::<String>::new());
    }

    #[test]
    fn youtube_task_status_text_derives_like_bilibili() {
        // 全部子任务 7 + 完成位 → completed。
        let ok = super::youtube_all_tasks_ok_bits();
        assert_eq!(super::youtube_text_from_task_status(ok, ok), "completed");
        // 任一子任务为 0 → pending。
        assert_eq!(super::youtube_text_from_task_status(0, ok), "pending");
        assert_eq!(super::youtube_text_from_task_status(ok, 0), "pending");
        // 子任务带失败计数 → failed。
        let mut video = super::VideoStatus::default();
        video.set(4, 2);
        let video_bits: u32 = video.into();
        assert_eq!(super::youtube_text_from_task_status(video_bits, ok), "failed");
    }

    #[tokio::test]
    async fn youtube_artifact_status_reads_task_bits_not_files() {
        // 已下载完成的外源视频：即使媒体文件被移走，任务位仍是完成，
        // 卡片状态保持“全部完成”（与 B 站一致），不会重新显示“未下载”。
        let mut video = super::youtube_video::Model {
            download_status: "completed".to_string(),
            ..Default::default()
        };
        video.output_path = Some("Z:/__not_exists__/abc123.mp4".to_string());
        let ok = super::youtube_all_tasks_ok_bits();
        video.video_task_status = ok;
        video.page_task_status = ok;
        let source = sample_external_source("douyin");
        let (video_status, page_status) = super::youtube_artifact_status(&video, &source).await;
        assert_eq!(video_status, [7, 7, 7, 7, 7], "已完成视频文件被移走后应保持全部完成");
        assert_eq!(page_status, [7, 7, 7, 7, 7], "分页状态同样应保持全部完成");

        // 只重置“视频信息”(视频1/分页2) → 仅该任务回到未开始，媒体(视频4)仍完成。
        let mut vstatus = super::VideoStatus::from(video.video_task_status);
        let mut pstatus = super::PageStatus::from(video.page_task_status);
        vstatus.set(1, 0);
        pstatus.set(2, 0);
        video.video_task_status = vstatus.into();
        video.page_task_status = pstatus.into();
        let (video_status, _) = super::youtube_artifact_status(&video, &source).await;
        assert_eq!(video_status, [7, 0, 7, 7, 7], "重置视频信息后媒体仍应为已完成");
    }

    #[test]
    fn youtube_tvshow_nfo_studio_uses_platform_label_for_tiktok() {
        // TikTok 剧集 NFO：工作室应为 "TikTok"、uniqueid type 应为 "tiktok"，
        // 而不是被写成 "YouTube"。
        let source = sample_external_source("tiktok_collection");
        let metadata: crate::external_media::ExternalMediaMetadata =
            serde_json::from_str(r#"{"id":"vid1"}"#).expect("metadata 应可构造");
        let xml = super::generate_youtube_tvshow_nfo(&source, &metadata, None);
        assert!(
            xml.contains("<studio>TikTok</studio>"),
            "TikTok tvshow studio 应为 TikTok: {xml}"
        );
        assert!(
            xml.contains(r#"<uniqueid type="tiktok""#),
            "TikTok tvshow uniqueid type 应为 tiktok: {xml}"
        );
        assert!(
            !xml.contains("<studio>YouTube</studio>"),
            "TikTok tvshow studio 不应是 YouTube: {xml}"
        );
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
    fn login_cookie_accepts_httponly_prefixed_session_cookies() {
        // 浏览器扩展（Get cookies.txt LOCALLY 等）导出的会话 Cookie 带 #HttpOnly_ 前缀，
        // 而 SID/SAPISID/__Secure-1PSID 全都是 HttpOnly。
        let exported = concat!(
            "# Netscape HTTP Cookie File\n",
            "#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1800000000\t__Secure-1PSID\tvalue1\n",
            "#HttpOnly_.google.com\tTRUE\t/\tTRUE\t1800000000\tSID\tvalue2\n",
        );
        assert!(is_netscape_youtube_cookie_file(exported));
        assert!(has_youtube_session_value(exported));
        // 只有普通 Cookie（没有会话字段）时仍然判定为未登录。
        let visitor_only = "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\tYSC\tvalue\n";
        assert!(!is_netscape_youtube_cookie_file(visitor_only));
        assert!(!has_youtube_session_value(visitor_only));
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
        assert_eq!(
            youtube_login_verdict(
                "https://www.youtube.com/feed/channels",
                reqwest::StatusCode::OK,
                r#"{"LOGGED_IN":false,"signInUrl":"https://accounts.google.com/ServiceLogin"}"#
            ),
            YoutubeLoginVerdict::LoggedOut
        );
        assert_eq!(
            youtube_login_verdict(
                "https://www.youtube.com/feed/channels",
                reqwest::StatusCode::OK,
                r#"{"LOGGED_IN":true}"#
            ),
            YoutubeLoginVerdict::LoggedIn
        );
    }

    #[test]
    fn youtube_login_verdict_treats_interstitials_as_unclear() {
        // 代理出口需要先同意条款：页面同样带未登录标记，但会话没坏。
        assert!(matches!(
            youtube_login_verdict(
                "https://consent.youtube.com/m?continue=https://www.youtube.com/feed/channels",
                reqwest::StatusCode::OK,
                r#"{"LOGGED_IN":false}"#
            ),
            YoutubeLoginVerdict::Unclear(_)
        ));
        // 风控/异常流量验证页。
        assert!(matches!(
            youtube_login_verdict(
                "https://www.youtube.com/sorry/index?continue=/feed/channels",
                reqwest::StatusCode::OK,
                "<html>Our systems have detected unusual traffic</html>"
            ),
            YoutubeLoginVerdict::Unclear(_)
        ));
        // 上游 5xx：不能据此判断 Cookie 失效。
        assert!(matches!(
            youtube_login_verdict(
                "https://www.youtube.com/feed/channels",
                reqwest::StatusCode::BAD_GATEWAY,
                "<html>502</html>"
            ),
            YoutubeLoginVerdict::Unclear(_)
        ));
        // 页面结构变了（既没有登录标记也没有退出标记）：保持不确定。
        assert!(matches!(
            youtube_login_verdict(
                "https://www.youtube.com/feed/channels",
                reqwest::StatusCode::OK,
                "<html>something unexpected</html>"
            ),
            YoutubeLoginVerdict::Unclear(_)
        ));
    }

    #[test]
    fn youtube_login_verdict_detects_login_redirect() {
        assert_eq!(
            youtube_login_verdict(
                "https://accounts.google.com/ServiceLogin?continue=https://www.youtube.com/feed/channels",
                reqwest::StatusCode::OK,
                "<html></html>"
            ),
            YoutubeLoginVerdict::LoggedOut
        );
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
        use crate::external_media::ExternalMediaMetadata;
        use bili_sync_entity::{youtube_source, youtube_video};
        use std::collections::HashMap;

        let mut source = youtube_source::Model {
            id: 42,
            ..Default::default()
        };
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
            fanart: None,
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
            slides: Vec::new(),
        };
        let path = super::youtube_output_path(&source, &video, &metadata, "第三集标题", "作者").unwrap();
        let expected = Path::new("F:/Downloads/测试")
            .join("测试短剧_带斜杠")
            .join("Season 01")
            .join("S01E03.mp4");
        assert_eq!(path, expected, "短剧应输出 下载根/剧集名/Season 01/S01E03.mp4");
    }

    /// 抖音合集按 B 站合集默认模板命名：`合集名/Season 01/S01E01 - 集标题.mp4`。
    #[test]
    fn builds_douyin_mix_output_path_with_episode_title() {
        use crate::external_media::ExternalMediaMetadata;
        use bili_sync_entity::{youtube_source, youtube_video};
        use std::collections::HashMap;

        let mut source = youtube_source::Model {
            id: 9,
            ..Default::default()
        };
        source.source_type = "douyin_mix".to_string();
        source.name = "传奇剧场（剧情）".to_string();
        source.path = "F:/Downloads/测试".to_string();

        let video = youtube_video::Model {
            episode_number: Some(12),
            ..Default::default()
        };

        let metadata = ExternalMediaMetadata {
            id: "7604472777650785551".to_string(),
            title: None,
            uploader: Some("我是战魂".to_string()),
            uploader_url: None,
            channel: None,
            channel_id: None,
            channel_url: None,
            thumbnail: None,
            fanart: None,
            description: None,
            language: None,
            upload_date: Some("20260208".to_string()),
            duration: None,
            formats: Vec::new(),
            subtitles: HashMap::new(),
            automatic_captions: HashMap::new(),
            images: Vec::new(),
            music_urls: Vec::new(),
            creators: None,
            slides: Vec::new(),
        };

        // 标题里的 #话题 只影响落盘名，不进NFO
        let title = "寂寞的蜈蚣洞的冲突开始了 #热血传奇 #传奇复古 #176传奇";
        let path = super::youtube_output_path(&source, &video, &metadata, title, "我是战魂").unwrap();
        // 目录名与 B 站一致走 filenamify（全角括号会转半角），文件名的标题再按目录名规则清洗
        let mix_folder = crate::utils::filenamify::filenamify(&source.name);
        let expected = Path::new("F:/Downloads/测试")
            .join(&mix_folder)
            .join("Season 01")
            .join("S01E12 - 寂寞的蜈蚣洞的冲突开始了.mp4");
        assert_eq!(path, expected, "合集应输出 下载根/合集名/Season 01/S01E01 - 集标题.mp4");

        // 图文作品没有文案时只保留集号，不出现「无标题」
        let untitled = super::youtube_output_path(&source, &video, &metadata, "无标题", "我是战魂").unwrap();
        assert_eq!(untitled.file_name().and_then(|name| name.to_str()), Some("S01E12.mp4"));
    }

    #[test]
    fn douyin_mix_path_does_not_repeat_collection_folder() {
        use crate::external_media::ExternalMediaMetadata;
        use bili_sync_entity::{youtube_source, youtube_video};

        let mut source = youtube_source::Model {
            id: 9,
            ..Default::default()
        };
        source.source_type = "douyin_mix".to_string();
        source.name = "外卖小哥修仙记".to_string();
        source.path = "J:/EMBY/DY收藏夹合集/外卖小哥修仙记".to_string();
        let video = youtube_video::Model {
            episode_number: Some(1),
            ..Default::default()
        };
        let metadata: ExternalMediaMetadata =
            serde_json::from_str(r#"{"id":"mix1","title":"第一集","uploader":"作者"}"#).unwrap();

        let path = super::youtube_output_path(&source, &video, &metadata, "第一集", "作者").unwrap();
        let expected = Path::new("J:/EMBY/DY收藏夹合集/外卖小哥修仙记")
            .join("Season 01")
            .join("S01E01 - 第一集.mp4");
        assert_eq!(path, expected, "source.path 已包含合集名时不能重复追加");
    }

    #[test]
    fn builds_douyin_collection_output_path_as_regular_video() {
        use crate::external_media::ExternalMediaMetadata;
        use bili_sync_entity::{youtube_source, youtube_video};

        let mut collection_source = youtube_source::Model {
            id: 6,
            ..Default::default()
        };
        collection_source.source_type = "douyin_collection".to_string();
        collection_source.name = "收藏夹".to_string();
        collection_source.path = "F:/Downloads/收藏夹douyin".to_string();

        let mut regular_source = collection_source.clone();
        regular_source.source_type = "douyin".to_string();

        let video = youtube_video::Model {
            episode_number: Some(3),
            ..Default::default()
        };
        let metadata: ExternalMediaMetadata = serde_json::from_str(
            r#"{"id":"7666885554109183667","title":"测试收藏视频","uploader":"测试作者","upload_date":"20260727"}"#,
        )
        .expect("metadata 应可构造");

        let collection_path =
            super::youtube_output_path(&collection_source, &video, &metadata, "测试收藏视频", "测试作者").unwrap();
        let regular_path =
            super::youtube_output_path(&regular_source, &video, &metadata, "测试收藏视频", "测试作者").unwrap();
        assert_eq!(
            collection_path, regular_path,
            "抖音收藏夹应和普通单视频使用同一套目录模板，不能生成 Season 01"
        );
        assert!(
            !collection_path
                .components()
                .any(|component| component.as_os_str() == "Season 01"),
            "收藏夹路径不应包含 Season 01: {}",
            collection_path.display()
        );
    }

    #[test]
    fn external_output_path_cleans_title_for_folder_name() {
        use crate::external_media::ExternalMediaMetadata;
        use bili_sync_entity::{youtube_source, youtube_video};
        use std::collections::HashMap;

        let mut source = youtube_source::Model {
            id: 7,
            ..Default::default()
        };
        source.source_type = "douyin_user".to_string();
        source.name = "白铭(想接正片)".to_string();
        source.path = "F:/Downloads/关注douyin".to_string();

        let video = youtube_video::Model::default();
        let metadata = ExternalMediaMetadata {
            id: "7669751153546071303".to_string(),
            title: None,
            uploader: None,
            uploader_url: None,
            channel: None,
            channel_id: None,
            channel_url: None,
            thumbnail: None,
            fanart: None,
            description: None,
            language: None,
            upload_date: Some("20260822".to_string()),
            duration: None,
            formats: Vec::new(),
            subtitles: HashMap::new(),
            automatic_captions: HashMap::new(),
            images: Vec::new(),
            music_urls: Vec::new(),
            creators: None,
            slides: Vec::new(),
        };
        let title = "怎么不是最权威的大合唱呢 #乐意效劳 #kocchinokento #抖音音乐年终狂想 #抖音音乐年终接力赛";
        let path = super::youtube_output_path(&source, &video, &metadata, title, "白铭").unwrap();
        let folder = path
            .parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str())
            .unwrap()
            .to_string();
        assert_eq!(folder, "怎么不是最权威的大合唱呢", "外源目录名应使用清洗后的标题");
    }

    #[test]
    fn external_output_path_length_budget_matches_bilibili() {
        use crate::external_media::ExternalMediaMetadata;
        use bili_sync_entity::{youtube_source, youtube_video};
        use std::collections::HashMap;

        let mut source = youtube_source::Model {
            id: 8,
            ..Default::default()
        };
        source.source_type = "douyin_user".to_string();
        source.name = "作者".to_string();
        source.path = "F:/Downloads/关注douyin".to_string();

        let video = youtube_video::Model::default();
        let metadata = ExternalMediaMetadata {
            id: "1".to_string(),
            title: None,
            uploader: None,
            uploader_url: None,
            channel: None,
            channel_id: None,
            channel_url: None,
            thumbnail: None,
            fanart: None,
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
            slides: Vec::new(),
        };
        let long_title = "长".repeat(300);
        let path = super::youtube_output_path(&source, &video, &metadata, &long_title, "作者").unwrap();
        let folder = path
            .parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str())
            .unwrap()
            .to_string();
        assert!(
            folder.len() <= 200,
            "外源目录名超出 B 站同款 200 字节上限：{} 字节",
            folder.len()
        );
    }

    /// 「编辑状态」把已完成的「分P下载」重置为「未开始」后必须删掉本地媒体：
    /// 留着文件的话 worker 只会打印「目标文件已存在，复用现有文件」跳过重下，
    /// 用户看到的“重置”就变成了什么都不做。
    #[tokio::test]
    async fn clearing_media_task_removes_local_media_file() {
        let root = std::env::temp_dir().join(format!("bili-sync-reset-media-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        let media = root.join("20260101000000-7573135187270192425.mp4");
        tokio::fs::write(&media, vec![0u8; 4096]).await.unwrap();

        let video = super::youtube_video::Model {
            output_path: Some(media.display().to_string()),
            ..Default::default()
        };
        let source = super::youtube_source::Model::default();

        // 视频子任务 4 = 分P下载
        super::remove_youtube_task_artifact(&video, &source, &[4], &[])
            .await
            .unwrap();
        assert!(!media.exists(), "重置分P下载后应删除本地媒体文件");
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn clearing_subtitle_and_danmaku_tasks_removes_markers_and_empty_dirs() {
        let root = std::env::temp_dir().join(format!(
            "bili-sync-reset-sidecars-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let series_dir = root.join("测试合集");
        let season_dir = series_dir.join("Season 01");
        tokio::fs::create_dir_all(&season_dir).await.unwrap();
        let output = season_dir.join("S01E01 - 测试.mp4");
        tokio::fs::write(output.with_extension("zh-CN.default.ass"), b"danmaku")
            .await
            .unwrap();
        tokio::fs::write(output.with_extension("zh-CN.vtt"), b"subtitle")
            .await
            .unwrap();
        for suffix in ["subtitle.checked", "danmaku.checked"] {
            tokio::fs::write(output.with_extension(suffix), b"checked")
                .await
                .unwrap();
        }
        let mut video = super::youtube_video::Model {
            output_path: Some(output.display().to_string()),
            ..Default::default()
        };
        video.episode_number = Some(1);
        let mut source = sample_external_source("douyin_mix");
        source.path = root.display().to_string();

        super::remove_youtube_task_artifact(&video, &source, &[], &[3, 4])
            .await
            .unwrap();

        assert!(!output.with_extension("zh-CN.default.ass").exists());
        assert!(!output.with_extension("zh-CN.vtt").exists());
        assert!(!output.with_extension("subtitle.checked").exists());
        assert!(!output.with_extension("danmaku.checked").exists());
        assert!(!season_dir.exists(), "空 Season 目录应被清理");
        assert!(!series_dir.exists(), "空合集目录应被清理");
        assert!(root.exists(), "视频源根目录不应被清理");
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn collection_cleanup_removes_legacy_series_sidecars_only_for_matching_source() {
        let root = std::env::temp_dir().join(format!(
            "bili-sync-collection-sidecars-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let uploader_dir = root.join("测试作者");
        let video_dir = uploader_dir.join("测试视频");
        tokio::fs::create_dir_all(&video_dir).await.unwrap();

        let mut source = sample_external_source("douyin_collection");
        source.url = "https://www.douyin.com/collection/7672884606911354673".to_string();
        let output_path = video_dir.join("测试视频.mp4");
        tokio::fs::write(&output_path, b"video").await.unwrap();
        tokio::fs::write(
            uploader_dir.join("tvshow.nfo"),
            format!(
                r#"<tvshow><uniqueid type="douyin" default="true">{}</uniqueid></tvshow>"#,
                source.url
            ),
        )
        .await
        .unwrap();
        tokio::fs::write(
            video_dir.join("season.nfo"),
            "<season><seasonnumber>1</seasonnumber></season>",
        )
        .await
        .unwrap();
        for name in ["folder.jpg", "poster.jpg"] {
            tokio::fs::write(uploader_dir.join(name), vec![1u8; 2048])
                .await
                .unwrap();
        }

        super::cleanup_legacy_collection_series_sidecars(&source, &output_path)
            .await
            .unwrap();

        assert!(!uploader_dir.join("tvshow.nfo").exists());
        assert!(!uploader_dir.join("folder.jpg").exists());
        assert!(!uploader_dir.join("poster.jpg").exists());
        assert!(!video_dir.join("season.nfo").exists());
        assert!(output_path.exists(), "清理剧集附属文件不能删除视频本体");
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    /// 外源互斥的判定键：同一个源不能因为链接写法不同（尾斜杠 / 大小写 /
    /// 多余的查询参数）被当成两个源重复添加。
    #[test]
    fn external_source_identity_normalizes_links() {
        let identity = super::external_source_identity;

        // 抖音作者：尾斜杠不影响
        assert_eq!(
            identity("douyin", "https://www.douyin.com/user/MS4wLjABAAAAtest"),
            identity("douyin", "https://www.douyin.com/user/MS4wLjABAAAAtest/"),
        );
        // 抖音收藏夹 / 放映厅 / 短剧 / 合集：按末段 id
        assert_eq!(
            identity(
                "douyin_collection",
                "https://www.douyin.com/collection/7672884606911354673?from=web"
            ),
            "douyin:collection:7672884606911354673",
        );
        assert_eq!(
            identity("douyin_theater", "https://www.douyin.com/lvdetail/7633372107832820276"),
            "douyin:theater:7633372107832820276",
        );
        assert_eq!(
            identity("douyin_series", "https://www.douyin.com/series/7663823934580033574"),
            "douyin:series:7663823934580033574",
        );
        // 合集与收藏夹共用 /collection/ 路由，但类型不同就是不同的源
        assert_eq!(
            identity("douyin_mix", "https://www.douyin.com/collection/7600294985053702190"),
            "douyin:mix:7600294985053702190",
        );
        // TikTok handle 大小写不敏感
        assert_eq!(
            identity("tiktok", "https://www.tiktok.com/@Nkuu666"),
            identity("tiktok", "https://www.tiktok.com/@nkuu666/"),
        );
        // YouTube 频道 / 播放列表
        assert_eq!(
            identity("channel", "https://www.youtube.com/channel/UCabcdefghijklmnopqrstuv"),
            "youtube:channel:ucabcdefghijklmnopqrstuv",
        );
        assert_eq!(
            identity("playlist", "https://www.youtube.com/playlist?list=PLabc123&si=xyz"),
            identity("playlist", "https://www.youtube.com/playlist?list=PLabc123"),
        );
        // 天然只有一个的源：不同账号也是同一个键
        assert_eq!(
            identity("watch_later", "https://www.youtube.com/playlist?list=WL"),
            "youtube:watch_later"
        );
        assert_eq!(
            identity("liked", "https://www.youtube.com/playlist?list=LL"),
            "youtube:liked"
        );
        assert_eq!(
            identity("douyin_liked", "https://www.douyin.com/user/self?tab=like"),
            "douyin:liked"
        );
        // 不同源必须不同
        assert_ne!(
            identity("douyin", "https://www.douyin.com/user/AAA"),
            identity("douyin", "https://www.douyin.com/user/BBB"),
        );
        assert_ne!(
            identity("douyin_collection", "https://www.douyin.com/collection/111"),
            identity("douyin_collection", "https://www.douyin.com/collection/222"),
        );
    }

    /// 语言候选表的首项必须就是用户点名的语言码，不能被 `zh-Hans` 抢走。
    #[test]
    fn subtitle_language_candidates_keep_requested_language_first() {
        let order = super::youtube_subtitle_language_candidates("zh-CN");
        assert_eq!(order.first().map(String::as_str), Some("zh-CN"));
        assert!(order.iter().any(|value| value == "zh-Hans"));
        assert_eq!(
            super::youtube_subtitle_language_candidates("en")
                .first()
                .map(String::as_str),
            Some("en")
        );
        // 双语设置里的语言部分必须被单独解析，否则 `zh-CN+en` 会当成未知语言码。
        assert_eq!(
            super::youtube_subtitle_language_candidates("zh-CN")
                .first()
                .map(String::as_str),
            crate::utils::subtitle::parse_bilingual_language_pair("zh-CN+en")
                .map(|(primary, _)| primary)
                .as_deref()
        );
    }

    /// 英文视频把目标语言设成中文时，中文自动翻译轨要排在英文人工轨之前：
    /// 旧实现「人工字幕整体优先于自动字幕」，只要有英文人工轨就永远拿不到中文。
    #[test]
    fn subtitle_candidates_prefer_requested_language_over_original() {
        use crate::external_media::ExternalMediaMetadata;

        let metadata: ExternalMediaMetadata = serde_json::from_value(serde_json::json!({
            "id": "vid",
            "language": "en",
            "subtitles": { "en": [ { "url": "https://example.com/en.vtt", "ext": "vtt" } ] },
            "automatic_captions": {
                "en": [ { "url": "https://example.com/asr-en.vtt", "ext": "vtt" } ],
                "zh-Hans": [ { "url": "https://example.com/zh.vtt?tlang=zh-Hans", "ext": "vtt" } ]
            }
        }))
        .expect("元数据应能反序列化");

        let order = super::youtube_subtitle_language_order(&metadata, "zh-CN");
        let candidates = super::collect_youtube_subtitle_candidates(&metadata, &order);
        assert_eq!(candidates.first().map(|value| value.language.as_str()), Some("zh-Hans"));
        assert!(candidates[0].url.contains("tlang=zh-Hans"));
        // 原语言轨仍留在后面兜底，中文轨下载失败（如 429）时可以退回去。
        assert!(candidates.iter().any(|value| value.language == "en"));
    }

    /// 同一条语言下人工轨要排在自动轨前面，且 URL 相同的候选只保留一次。
    #[test]
    fn subtitle_candidates_put_manual_before_automatic_without_duplicates() {
        use crate::external_media::ExternalMediaMetadata;

        let metadata: ExternalMediaMetadata = serde_json::from_value(serde_json::json!({
            "id": "vid",
            "subtitles": { "zh-Hans": [ { "url": "https://example.com/manual-zh.vtt", "ext": "vtt" } ] },
            "automatic_captions": {
                "zh-Hans": [ { "url": "https://example.com/auto-zh.vtt?tlang=zh-Hans", "ext": "vtt" } ]
            }
        }))
        .expect("元数据应能反序列化");

        let candidates = super::collect_youtube_subtitle_candidates(
            &metadata,
            &super::youtube_subtitle_language_candidates("zh-Hans"),
        );
        let urls: Vec<&str> = candidates.iter().map(|value| value.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://example.com/manual-zh.vtt",
                "https://example.com/auto-zh.vtt?tlang=zh-Hans"
            ]
        );
    }

    /// issue #294：匿名兜底的触发条件——只有「公开来源 + 带了 Cookie + 画质不达标 +
    /// 本进程没试过」才重试；私有来源、非 YouTube、画质已达标都不触发。
    #[test]
    fn youtube_anonymous_fallback_only_triggers_for_low_quality_public_video() {
        assert!(super::should_retry_youtube_anonymous(
            true, false, true, 360, 720, false
        ));
        // 没有任何可下载视频流（0）同样算不达标
        assert!(super::should_retry_youtube_anonymous(true, false, true, 0, 1080, false));
        assert!(!super::should_retry_youtube_anonymous(
            true, false, true, 1080, 720, false
        ));
        // 本次没带 Cookie（本来就是匿名解析）不重试
        assert!(!super::should_retry_youtube_anonymous(
            true, false, false, 360, 720, false
        ));
        // 稍后再看/点赞/订阅等需要登录的来源绝不匿名
        assert!(!super::should_retry_youtube_anonymous(
            true, true, true, 360, 720, false
        ));
        // 非 YouTube 平台不触发
        assert!(!super::should_retry_youtube_anonymous(
            false, false, true, 360, 720, false
        ));
        // 同一 URL 本进程内已经试过，不再重复
        assert!(!super::should_retry_youtube_anonymous(
            true, false, true, 360, 720, true
        ));
    }

    /// 匿名结果必须严格更高才采用，相同或更低都保留带 Cookie 的结果。
    #[test]
    fn youtube_anonymous_result_must_be_strictly_better() {
        assert!(super::youtube_anonymous_result_is_better(360, 720));
        assert!(super::youtube_anonymous_result_is_better(0, 360));
        assert!(!super::youtube_anonymous_result_is_better(720, 720));
        assert!(!super::youtube_anonymous_result_is_better(720, 360));
        assert!(!super::youtube_anonymous_result_is_better(0, 0));
    }

    /// 私有来源判定覆盖「稍后再看 / 点赞 / 订阅」，公开来源不误判。
    #[test]
    fn youtube_private_source_detection_covers_login_only_lists() {
        for source_type in ["watch_later", "liked", "subscriptions"] {
            assert!(
                super::is_youtube_private_source(&sample_external_source(source_type)),
                "{source_type} 需要登录，不应匿名解析"
            );
        }
        for source_type in ["channel", "playlist", "video"] {
            assert!(
                !super::is_youtube_private_source(&sample_external_source(source_type)),
                "{source_type} 是公开来源，可以匿名兜底"
            );
        }
    }

    /// 匿名兜底的「每 URL 只试一次」是进程级去重，且按 URL 精确区分；
    /// 认领会原子地标记已尝试，并发调用只有一个能拿到机会。
    #[test]
    fn youtube_anonymous_attempt_is_claimed_only_once() {
        let url = "https://www.youtube.com/watch?v=anonymous-fallback-tracking-test";
        let other = "https://www.youtube.com/watch?v=anonymous-fallback-tracking-other";
        assert!(!super::youtube_anonymous_already_tried(url));
        assert!(super::claim_youtube_anonymous_attempt(url));
        assert!(super::youtube_anonymous_already_tried(url));
        // 第二次认领失败：同一 URL 不会重复发匿名解析请求
        assert!(!super::claim_youtube_anonymous_attempt(url));
        assert!(super::claim_youtube_anonymous_attempt(other));
        assert!(!super::youtube_anonymous_already_tried(
            "https://www.youtube.com/watch?v=anonymous-fallback-never-tried"
        ));
    }

    /// 复现 issue #294 的两份格式集：带 Cookie 只有 HLS（被排除）+ 360p 混合流，
    /// 匿名解析则有 720p 独立视频流 + 独立音频流，匿名结果更高、可被采用。
    #[test]
    fn youtube_cookie_formats_stay_360p_while_anonymous_formats_reach_720p() {
        use super::ExternalMediaFormat;
        use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};

        let filter = FilterOption {
            video_max_quality: VideoQuality::Quality8k,
            video_min_quality: VideoQuality::Quality720p,
            audio_max_quality: AudioQuality::QualityHiRES,
            audio_min_quality: AudioQuality::Quality64k,
            codecs: vec![VideoCodecs::AV1, VideoCodecs::AVC, VideoCodecs::HEV],
            ..Default::default()
        };
        let fmt = |id: &str, h: Option<i32>, w: Option<i32>, vcodec: &str, acodec: &str, protocol: &str| {
            ExternalMediaFormat {
                format_id: Some(id.to_string()),
                url: Some(format!("https://example.com/{id}")),
                protocol: Some(protocol.to_string()),
                ext: Some("mp4".to_string()),
                vcodec: Some(vcodec.to_string()),
                acodec: Some(acodec.to_string()),
                width: w,
                height: h,
                fps: Some(30.0),
                tbr: Some(1000.0),
                vbr: Some(800.0),
                abr: None,
                dynamic_range: Some("SDR".to_string()),
                is_original: false,
                decryption_key: None,
                fallback_urls: Vec::new(),
            }
        };
        // 带 Cookie：高分辨率只有 m3u8_native（HLS）分片流，唯一 http/https 是 360p 混合流
        let cookie_formats = vec![
            fmt("95", Some(720), Some(1280), "avc1", "none", "m3u8_native"),
            fmt("94", Some(480), Some(854), "avc1", "none", "m3u8_native"),
            fmt("93", Some(240), Some(426), "avc1", "none", "m3u8_native"),
            fmt("18", Some(360), Some(640), "avc1.42001E", "mp4a.40.2", "https"),
        ];
        assert_eq!(
            super::youtube_selectable_height(&cookie_formats, &filter, "YouTube"),
            360
        );
        let cookie_selected =
            super::select_youtube_streams(&cookie_formats, &filter, false, "YouTube").expect("360p 混合流应可选");
        assert_eq!(super::selected_effective_video_height(&cookie_selected), Some(360));

        // 匿名解析：720p 独立视频流 + 独立音频流 + 同一个 360p 混合流兜底
        let anonymous_formats = vec![
            fmt("136", Some(720), Some(1280), "avc1.64001f", "none", "https"),
            fmt("140", None, None, "none", "mp4a.40.2", "https"),
            fmt("18", Some(360), Some(640), "avc1.42001E", "mp4a.40.2", "https"),
        ];
        assert_eq!(
            super::youtube_selectable_height(&anonymous_formats, &filter, "YouTube"),
            720
        );
        let anonymous_selected =
            super::select_youtube_streams(&anonymous_formats, &filter, false, "YouTube").expect("独立音视频流应可选");
        assert_eq!(super::selected_effective_video_height(&anonymous_selected), Some(720));
        assert!(super::youtube_anonymous_result_is_better(360, 720));
    }

    /// 实际下载用的高度：有独立视频流 + 独立音频流时取视频流，否则取混合流。
    #[test]
    fn selected_effective_video_height_uses_separate_video_stream_when_available() {
        use super::ExternalMediaFormat;

        let fmt = |id: &str, h: i32, vcodec: &str, acodec: &str| ExternalMediaFormat {
            format_id: Some(id.to_string()),
            url: Some(format!("https://example.com/{id}")),
            protocol: Some("https".to_string()),
            ext: Some("mp4".to_string()),
            vcodec: Some(vcodec.to_string()),
            acodec: Some(acodec.to_string()),
            width: Some(h * 16 / 9),
            height: Some(h),
            fps: Some(30.0),
            tbr: Some(1000.0),
            vbr: None,
            abr: None,
            dynamic_range: Some("SDR".to_string()),
            is_original: false,
            decryption_key: None,
            fallback_urls: Vec::new(),
        };
        let both = super::SelectedStreams {
            video: Some(fmt("136", 720, "avc1.64001f", "none")),
            audio: Some(fmt("140", 0, "none", "mp4a.40.2")),
            mixed: Some(fmt("18", 360, "avc1", "mp4a.40.2")),
        };
        assert_eq!(super::selected_effective_video_height(&both), Some(720));
        let mixed_only = super::SelectedStreams {
            video: None,
            audio: None,
            mixed: Some(fmt("18", 360, "avc1", "mp4a.40.2")),
        };
        assert_eq!(super::selected_effective_video_height(&mixed_only), Some(360));
        assert!(super::selected_effective_video_format(&mixed_only).is_some());
    }

    /// 匿名缓存测试用的最小元数据：只有 `formats` 参与断言。
    fn sample_anonymous_metadata(formats: Vec<super::ExternalMediaFormat>) -> super::ExternalMediaMetadata {
        super::ExternalMediaMetadata {
            id: "anonymous-cache-test".to_string(),
            title: Some("匿名缓存测试".to_string()),
            uploader: None,
            uploader_url: None,
            channel: None,
            channel_id: None,
            channel_url: None,
            thumbnail: None,
            fanart: None,
            description: None,
            language: None,
            upload_date: None,
            duration: Some(60.0),
            formats,
            subtitles: std::collections::HashMap::new(),
            automatic_captions: std::collections::HashMap::new(),
            images: Vec::new(),
            slides: Vec::new(),
            music_urls: Vec::new(),
            creators: None,
        }
    }

    /// 构造一条最小可用的外部媒体格式。
    fn anonymous_test_format(
        id: &str,
        height: Option<i32>,
        vcodec: &str,
        acodec: &str,
        protocol: &str,
    ) -> super::ExternalMediaFormat {
        super::ExternalMediaFormat {
            format_id: Some(id.to_string()),
            url: Some(format!("https://example.com/{id}")),
            protocol: Some(protocol.to_string()),
            ext: Some("mp4".to_string()),
            vcodec: Some(vcodec.to_string()),
            acodec: Some(acodec.to_string()),
            width: height.map(|height| height * 16 / 9),
            height,
            fps: Some(30.0),
            tbr: Some(1000.0),
            vbr: None,
            abr: None,
            dynamic_range: Some("SDR".to_string()),
            is_original: false,
            decryption_key: None,
            fallback_urls: Vec::new(),
        }
    }

    /// 画质过滤：最低 720p，编码不限（缓存测试统一用这一份）。
    fn anonymous_test_filter() -> crate::bilibili::FilterOption {
        use crate::bilibili::{AudioQuality, FilterOption, VideoCodecs, VideoQuality};

        FilterOption {
            video_max_quality: VideoQuality::Quality8k,
            video_min_quality: VideoQuality::Quality720p,
            audio_max_quality: AudioQuality::QualityHiRES,
            audio_min_quality: AudioQuality::Quality64k,
            codecs: vec![VideoCodecs::AV1, VideoCodecs::AVC, VideoCodecs::HEV],
            ..Default::default()
        }
    }

    /// issue #294 后续：下载失败后重试时命中缓存，不再发匿名解析请求、直接复用 1080p。
    #[tokio::test]
    async fn youtube_anonymous_cache_reuses_result_without_refetching() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use std::sync::Arc;

        let url = "https://www.youtube.com/watch?v=anonymous-cache-reuse-test";
        let filter = anonymous_test_filter();
        let cookie_formats = vec![anonymous_test_format(
            "18",
            Some(360),
            "avc1.42001E",
            "mp4a.40.2",
            "https",
        )];
        let cached_formats = vec![
            anonymous_test_format("137", Some(1080), "avc1.640028", "none", "https"),
            anonymous_test_format("140", None, "none", "mp4a.40.2", "https"),
        ];
        super::cache_youtube_anonymous_formats(url, cached_formats);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fetch = Arc::clone(&calls);
        let unexpected =
            sample_anonymous_metadata(vec![anonymous_test_format("399", Some(2160), "av01", "none", "https")]);
        let metadata = sample_anonymous_metadata(cookie_formats);
        let source = sample_external_source("video");
        let result = super::apply_youtube_anonymous_quality_fallback_with(
            metadata,
            url,
            &source,
            &filter,
            "YouTube",
            move || async move {
                calls_for_fetch.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(unexpected)
            },
        )
        .await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 0, "命中缓存不应再调用匿名解析");
        assert_eq!(
            super::youtube_selectable_height(&result.formats, &filter, "YouTube"),
            1080,
            "应复用缓存里的 1080p，而不是重新解析的 2160p"
        );
    }

    /// 缓存里的匿名结果不比带 Cookie 的结果高时，既不采用缓存也不重新发请求。
    #[tokio::test]
    async fn youtube_anonymous_cache_is_ignored_when_not_better() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use std::sync::Arc;

        let url = "https://www.youtube.com/watch?v=anonymous-cache-not-better-test";
        let filter = anonymous_test_filter();
        let cookie_formats = vec![anonymous_test_format(
            "18",
            Some(360),
            "avc1.42001E",
            "mp4a.40.2",
            "https",
        )];
        // 缓存里只有一份 360p 结果，不会比当前带 Cookie 的 360p 更高
        super::cache_youtube_anonymous_formats(
            url,
            vec![anonymous_test_format(
                "18",
                Some(360),
                "avc1.42001E",
                "mp4a.40.2",
                "https",
            )],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fetch = Arc::clone(&calls);
        let unexpected = sample_anonymous_metadata(vec![anonymous_test_format(
            "137",
            Some(1080),
            "avc1.640028",
            "none",
            "https",
        )]);
        let metadata = sample_anonymous_metadata(cookie_formats);
        let source = sample_external_source("video");
        let result = super::apply_youtube_anonymous_quality_fallback_with(
            metadata,
            url,
            &source,
            &filter,
            "YouTube",
            move || async move {
                calls_for_fetch.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(unexpected)
            },
        )
        .await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 0, "缓存不比当前高时不应重新解析");
        assert_eq!(
            super::youtube_selectable_height(&result.formats, &filter, "YouTube"),
            360,
            "保留带 Cookie 的结果"
        );
    }

    /// 缓存超过上限时按插入顺序淘汰最旧的条目，长度稳定在上限。
    #[test]
    fn youtube_anonymous_cache_evicts_oldest_entry_when_full() {
        let mut cache = std::collections::HashMap::new();
        let capacity = super::YOUTUBE_ANONYMOUS_CACHE_CAPACITY;
        for index in 0..capacity + 1 {
            let url = format!("https://www.youtube.com/watch?v=anonymous-cache-evict-{index}");
            super::insert_youtube_anonymous_cache(
                &mut cache,
                &url,
                vec![anonymous_test_format("136", Some(720), "avc1.64001f", "none", "https")],
            );
        }
        assert_eq!(cache.len(), capacity, "缓存长度必须稳定在上限");
        assert!(
            !cache.contains_key("https://www.youtube.com/watch?v=anonymous-cache-evict-0"),
            "最早插入的条目应被淘汰"
        );
        assert!(
            cache.contains_key(&format!(
                "https://www.youtube.com/watch?v=anonymous-cache-evict-{capacity}"
            )),
            "最新插入的条目必须保留"
        );
    }

    /// 缓存条目超过 TTL 后视为未命中并被移除，避免复用已过期的 googlevideo 直链。
    #[test]
    fn youtube_anonymous_cache_entry_expires_after_ttl() {
        let url = "https://www.youtube.com/watch?v=anonymous-cache-ttl-test";
        let now = std::time::Instant::now();
        let formats = vec![anonymous_test_format("137", Some(1080), "avc1.640028", "none", "https")];
        let mut cache = std::collections::HashMap::new();
        cache.insert(
            url.to_string(),
            super::CachedAnonymousFormats {
                formats: formats.clone(),
                seq: 1,
                cached_at: now,
            },
        );
        assert!(
            super::read_youtube_anonymous_cache(&mut cache, url, now).is_some(),
            "TTL 内应命中缓存"
        );
        let expired_at = now
            .checked_sub(super::YOUTUBE_ANONYMOUS_CACHE_TTL + std::time::Duration::from_secs(1))
            .expect("测试时间应能回拨");
        cache.insert(
            url.to_string(),
            super::CachedAnonymousFormats {
                formats,
                seq: 2,
                cached_at: expired_at,
            },
        );
        assert!(
            super::read_youtube_anonymous_cache(&mut cache, url, now).is_none(),
            "超过 TTL 应视为未命中"
        );
        assert!(!cache.contains_key(url), "过期条目必须被移除");
    }

    /// 私有来源（稍后再看/点赞/订阅）绝不匿名解析，也不写缓存。
    #[tokio::test]
    async fn youtube_anonymous_private_source_never_requests_or_caches() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use std::sync::Arc;

        let url = "https://www.youtube.com/watch?v=anonymous-cache-private-test";
        let filter = anonymous_test_filter();
        let source = sample_external_source("liked");
        // 同一个视频先从公开来源入过缓存：私有来源仍然不读缓存，决策必须是 Skip
        let preseeded_url = format!("{url}-preseeded");
        super::cache_youtube_anonymous_formats(
            &preseeded_url,
            vec![
                anonymous_test_format("137", Some(1080), "avc1.640028", "none", "https"),
                anonymous_test_format("140", None, "none", "mp4a.40.2", "https"),
            ],
        );
        assert!(matches!(
            super::plan_youtube_anonymous_fallback(&preseeded_url, true, true, &filter, "YouTube", 360, 720),
            super::AnonymousFallbackDecision::Skip
        ));
        // 纯决策层断言：即使本次带了 Cookie、画质不达标，私有来源也只返回 Skip
        assert!(matches!(
            super::plan_youtube_anonymous_fallback(
                url,
                super::is_youtube_private_source(&source),
                true,
                &filter,
                "YouTube",
                360,
                720
            ),
            super::AnonymousFallbackDecision::Skip
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fetch = Arc::clone(&calls);
        let unexpected = sample_anonymous_metadata(vec![anonymous_test_format(
            "137",
            Some(1080),
            "avc1.640028",
            "none",
            "https",
        )]);
        let metadata = sample_anonymous_metadata(vec![anonymous_test_format(
            "18",
            Some(360),
            "avc1.42001E",
            "mp4a.40.2",
            "https",
        )]);
        let result = super::apply_youtube_anonymous_quality_fallback_with(
            metadata,
            url,
            &source,
            &filter,
            "YouTube",
            move || async move {
                calls_for_fetch.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(unexpected)
            },
        )
        .await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 0, "私有来源不应发匿名解析请求");
        assert!(
            super::youtube_anonymous_cached_formats(url).is_none(),
            "私有来源不应写匿名缓存"
        );
        assert_eq!(
            super::youtube_selectable_height(&result.formats, &filter, "YouTube"),
            360
        );
    }

    /// 增量扫描：已有历史记录的 YouTube 频道只枚举每个标签页最近 500 条。
    #[test]
    fn youtube_incremental_playlist_end_limits_known_channel() {
        assert_eq!(
            super::youtube_incremental_playlist_end("channel", false, false, false, false),
            Some(super::YOUTUBE_INCREMENTAL_PLAYLIST_END)
        );
    }

    /// 没有历史记录的频道必须全量枚举，避免漏掉老视频。
    #[test]
    fn youtube_incremental_playlist_end_skips_empty_history() {
        assert_eq!(
            super::youtube_incremental_playlist_end("channel", true, false, false, false),
            None
        );
    }

    /// 开启「扫描已删除视频」时必须全量枚举，否则发现不了被删条目。
    #[test]
    fn youtube_incremental_playlist_end_skips_deleted_scan() {
        assert_eq!(
            super::youtube_incremental_playlist_end("channel", false, true, false, false),
            None
        );
    }

    /// TikTok 来源不受 YouTube 增量逻辑影响。
    #[test]
    fn youtube_incremental_playlist_end_skips_tiktok() {
        assert_eq!(
            super::youtube_incremental_playlist_end("channel", false, false, true, false),
            None
        );
    }

    /// 抖音来源不受 YouTube 增量逻辑影响。
    #[test]
    fn youtube_incremental_playlist_end_skips_douyin() {
        assert_eq!(
            super::youtube_incremental_playlist_end("channel", false, false, false, true),
            None
        );
    }

    /// 播放列表等非频道来源不限制枚举条数。
    #[test]
    fn youtube_incremental_playlist_end_skips_non_channel() {
        assert_eq!(
            super::youtube_incremental_playlist_end("playlist", false, false, false, false),
            None
        );
    }

    /// 配置镜像基址时替换 yt-dlp 发布基址，并去掉尾部斜杠。
    #[test]
    fn ytdlp_release_base_url_uses_mirror_override() {
        assert_eq!(
            super::resolve_ytdlp_release_base_url(Some("https://mirror.example.com/yt-dlp/")),
            "https://mirror.example.com/yt-dlp"
        );
        assert_eq!(
            super::resolve_ytdlp_release_base_url(Some("  https://mirror.example.com/yt-dlp  ")),
            "https://mirror.example.com/yt-dlp"
        );
    }

    /// 未配置或配置为空时回退到官方发布基址。
    #[test]
    fn ytdlp_release_base_url_falls_back_to_official_release() {
        for mirror in [None, Some(""), Some("   "), Some("/")] {
            assert_eq!(
                super::resolve_ytdlp_release_base_url(mirror),
                super::YTDLP_RELEASE_BASE_URL
            );
        }
    }

    /// 回归：目标轨本身已是双语时，旧实现删掉两个临时文件后直接 `return Ok(())`，
    /// 双语设置下一个字幕文件都不写；而 `youtube_subtitle_exists()` 只认
    /// `.vtt/.srt/.ass`，找不到产物就会每轮重复下载整条字幕轨。
    /// 现在该分支必须仍规划出一条合法的 `.srt` 产物。
    #[test]
    fn already_bilingual_track_is_still_planned_for_a_real_output_file() {
        use crate::utils::subtitle::{parse_cues, render_srt, SubtitleCue};

        let cues = vec![
            SubtitleCue {
                start_ms: 0,
                end_ms: 1000,
                text: "Hello world\n你好世界".to_string(),
            },
            SubtitleCue {
                start_ms: 1000,
                end_ms: 2000,
                text: "How are you\n最近怎么样".to_string(),
            },
        ];
        let secondary = super::SubtitleCandidate {
            language: "en".to_string(),
            url: "https://example.com/en.vtt".to_string(),
            ext: "vtt".to_string(),
        };

        // 每条 cue 都内嵌英文 → ratio = 1.0 → 不合并，但标记为「已是双语」。
        let (merge_language, already_bilingual) = super::plan_youtube_bilingual_merge(&cues, Some(&secondary));
        assert!(already_bilingual);
        assert_eq!(merge_language, None);

        let path = super::youtube_bilingual_subtitle_path(
            std::path::Path::new("/media/movie.mp4"),
            "zh-Hans",
            merge_language.as_deref(),
        );
        assert_eq!(path.file_name().unwrap().to_string_lossy(), "movie.zh-Hans.srt");
        assert_eq!(path.extension().and_then(|value| value.to_str()), Some("srt"));

        // 不合并分支直接用 primary_cues 渲染，产物必须非空且能被重新解析。
        let rendered = render_srt(&cues);
        assert!(parse_cues(&rendered).len() >= 2, "rendered: {rendered}");
    }

    /// 正常合并、对照轨缺失两种分支的产物路径必须保持稳定。
    #[test]
    fn bilingual_subtitle_paths_cover_all_branches() {
        let output = std::path::Path::new("/media/movie.mp4");
        let secondary = super::SubtitleCandidate {
            language: "en".to_string(),
            url: "https://example.com/en.vtt".to_string(),
            ext: "vtt".to_string(),
        };
        let chinese_only = vec![crate::utils::subtitle::SubtitleCue {
            start_ms: 0,
            end_ms: 1000,
            text: "你好世界".to_string(),
        }];

        let (merge_language, already_bilingual) = super::plan_youtube_bilingual_merge(&chinese_only, Some(&secondary));
        assert!(!already_bilingual);
        assert_eq!(merge_language.as_deref(), Some("en"));
        assert_eq!(
            super::youtube_bilingual_subtitle_path(output, "zh-Hans", merge_language.as_deref())
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "movie.zh-Hans-en.srt"
        );

        let (merge_language, already_bilingual) = super::plan_youtube_bilingual_merge(&chinese_only, None);
        assert!(!already_bilingual);
        assert_eq!(merge_language, None);
        assert_eq!(
            super::youtube_bilingual_subtitle_path(output, "zh-Hans", merge_language.as_deref())
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "movie.zh-Hans.srt"
        );
    }

    /// 落盘的 `.srt`/`.ass` 必须能被 `youtube_subtitle_exists()` 识别，否则下一轮扫描
    /// 会把这次「已是双语」的子任务重新做一遍。
    #[tokio::test]
    async fn already_bilingual_output_stops_repeated_downloads() {
        let root = std::env::temp_dir().join(format!("bili-sync-bilingual-output-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("movie.mp4");

        assert!(!super::youtube_subtitle_exists(&output).await.unwrap());
        let path = super::youtube_bilingual_subtitle_path(&output, "zh-Hans", None);
        std::fs::write(&path, "1\n00:00:00,000 --> 00:00:01,000\nHello world\n").unwrap();
        assert!(
            super::youtube_subtitle_exists(&output).await.unwrap(),
            "双语分支落盘的 .srt 必须被识别为已有字幕"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn subtitle_candidate(ext: &str) -> super::SubtitleCandidate {
        super::SubtitleCandidate {
            language: "zh-Hans".to_string(),
            url: "https://example.com/subtitle".to_string(),
            ext: ext.to_string(),
        }
    }

    /// 回归：旧实现把 `candidate.ext` 原样写进文件名，`.json3` / `.srv1` 落盘后
    /// 媒体库不认，`youtube_subtitle_exists()` 也只认 `vtt|srt|ass`，于是每轮重下。
    /// 现在所有源格式统一解析后落盘为 `.srt`。
    #[test]
    fn single_subtitle_paths_normalize_unplayable_formats() {
        let output = std::path::Path::new("/media/movie.mp4");
        let cases = ["vtt", "srt", "ass", "json3", "srv1", "ttml"];
        for ext in cases {
            let path = super::youtube_single_subtitle_path(output, &subtitle_candidate(ext));
            assert_eq!(
                path.file_name().unwrap().to_string_lossy(),
                "movie.zh-Hans.srt",
                "ext={ext}"
            );
            assert_eq!(
                path.extension().and_then(|value| value.to_str()),
                Some("srt"),
                "ext={ext}"
            );
        }
    }

    /// `json3` 自动字幕要能解析出按时间排序、已清洗标签的 cue。
    #[test]
    fn json3_cues_are_parsed_and_sorted() {
        let json = r#"{"events":[
            {"tStartMs":1000,"dDurationMs":1500,"segs":[{"utf8":"Hello "},{"utf8":"world"}]},
            {"tStartMs":0,"dDurationMs":800,"segs":[{"utf8":"<c>第一句</c>"}]},
            {"tStartMs":3000,"dDurationMs":500,"segs":[]}
        ]}"#;
        let cues = super::parse_json3_cues(json);
        assert_eq!(cues.len(), 2, "空 segs 的事件应被丢弃");
        assert_eq!(cues[0].start_ms, 0);
        assert_eq!(cues[0].text, "第一句");
        assert_eq!(cues[1].start_ms, 1000);
        assert_eq!(cues[1].end_ms, 2500);
        assert_eq!(cues[1].text, "Hello world");
        let rendered = crate::utils::subtitle::render_srt(&cues);
        assert!(rendered.starts_with("1\n"));
        assert!(crate::utils::subtitle::parse_cues(&rendered).len() >= 2);
        assert!(super::parse_json3_cues("not json").is_empty());
    }

    /// 端到端（不含网络）：json3 下载结果经转码落盘后，`.srt` 与 `.ass` 两个产物
    /// 都必须生成且能被存在性检查识别，否则下一轮扫描仍会重复下载这条轨。
    #[tokio::test]
    async fn json3_subtitle_is_persisted_as_srt_and_ass() {
        let root = std::env::temp_dir().join(format!("bili-sync-json3-output-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("movie.mp4");
        let temporary = output.with_extension("zh-Hans.0.subtitle.tmp");
        let json = r#"{"events":[{"tStartMs":0,"dDurationMs":1000,"segs":[{"utf8":"第一句"}]}]}"#;
        std::fs::write(&temporary, json).unwrap();

        let candidate = subtitle_candidate("json3");
        let final_path = super::youtube_single_subtitle_path(&output, &candidate);
        super::persist_youtube_subtitle(&temporary, &final_path, &candidate.ext)
            .await
            .unwrap();

        assert!(!temporary.exists(), "临时文件应已被重命名");
        assert_eq!(final_path.extension().and_then(|value| value.to_str()), Some("srt"));
        let srt = std::fs::read_to_string(&final_path).unwrap();
        assert!(srt.starts_with("1\n"), "srt: {srt}");
        assert!(srt.contains("第一句"));
        let ass_path = final_path.with_extension("ass");
        let ass = std::fs::read_to_string(&ass_path).unwrap();
        assert!(ass.contains("[Events]"), "ass: {ass}");
        assert!(ass.contains("Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,第一句"));
        assert!(
            super::youtube_subtitle_exists(&output).await.unwrap(),
            "转码后的 .srt 必须被识别为已有字幕，避免重复下载"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `srv3`（`<p t d>`）、`srv1`（`<text start dur>`）、`ttml`（`<p begin end>`）
    /// 三种 XML 形状都要能解析出带正确时间轴的 cue。
    #[test]
    fn xml_timed_subtitles_are_parsed() {
        let srv3 = r#"<?xml version="1.0" encoding="utf-8"?><timedtext format="3"><body>
            <p t="1200" d="1800"><s>Hello</s><s> world</s></p>
            <p t="3000" d="1000">第二句 &amp; 收尾</p>
        </body></timedtext>"#;
        let cues = super::parse_youtube_subtitle_cues(srv3, "srv3");
        assert_eq!(cues.len(), 2, "cues={cues:?}");
        assert_eq!(cues[0].start_ms, 1200);
        assert_eq!(cues[0].end_ms, 3000);
        assert_eq!(cues[0].text, "Hello world");
        assert_eq!(cues[1].start_ms, 3000);
        assert_eq!(cues[1].end_ms, 4000);
        assert_eq!(cues[1].text, "第二句 & 收尾");

        let srv1 = r#"<transcript><text start="1.20" dur="2.5">第一句</text><text start="4" dur="1">第二句</text></transcript>"#;
        let cues = super::parse_youtube_subtitle_cues(srv1, "srv1");
        assert_eq!(cues.len(), 2, "cues={cues:?}");
        assert_eq!(cues[0].start_ms, 1200);
        assert_eq!(cues[0].end_ms, 3700);
        assert_eq!(cues[1].start_ms, 4000);
        assert_eq!(cues[1].end_ms, 5000);

        let ttml = r#"<tt><body><div>
            <p begin="00:00:01.230" end="00:00:03.400">ttml 第一句</p>
        </div></body></tt>"#;
        let cues = super::parse_youtube_subtitle_cues(ttml, "ttml");
        assert_eq!(cues.len(), 1, "cues={cues:?}");
        assert_eq!(cues[0].start_ms, 1230);
        assert_eq!(cues[0].end_ms, 3400);
        assert_eq!(cues[0].text, "ttml 第一句");

        // `d` 不能被误当成 `dur` 的前缀匹配
        assert_eq!(super::xml_attr(r#" t="12" dur="3""#, "d"), None);
        assert_eq!(super::xml_attr(r#" t="12" d="3""#, "d"), Some("3"));
        assert_eq!(super::parse_cue_time("00:01:02.345", false), Some(62_345));
        assert_eq!(super::parse_cue_time("1.5s", false), Some(1_500));
        assert_eq!(super::parse_cue_time("1234", true), Some(1_234));
    }

    /// XML 字幕解析不出文本时必须按**原后缀**落盘：写不带 `WEBVTT` 头的 `.vtt`
    /// 会让媒体库解析失败，而存在性检查又认这个后缀，于是这一页永久坏掉。
    #[tokio::test]
    async fn unparseable_xml_subtitle_falls_back_to_original_extension() {
        let root = std::env::temp_dir().join(format!(
            "bili-sync-srv3-fallback-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("movie.mp4");
        let temporary = output.with_extension("zh-Hans.0.subtitle.tmp");
        std::fs::write(&temporary, "<timedtext><body>no timing attrs</body></timedtext>").unwrap();

        let candidate = subtitle_candidate("srv3");
        let final_path = super::youtube_single_subtitle_path(&output, &candidate);
        super::persist_youtube_subtitle(&temporary, &final_path, &candidate.ext)
            .await
            .unwrap();

        assert!(!final_path.exists(), "解析失败不应产出空的 .srt");
        assert!(
            !output.with_extension("zh-Hans.vtt").exists(),
            "不能写不带 WEBVTT 头的 .vtt"
        );
        let fallback = output.with_extension("zh-Hans.srv3");
        assert!(fallback.exists(), "解析失败应按原后缀落盘: {fallback:?}");
        assert!(
            super::youtube_subtitle_exists(&output).await.unwrap(),
            "降级产物必须被识别为已有字幕，避免每轮重复下载"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// JS runtime 查找必须以「可执行」为准：普通文件（无执行位）不算，
    /// 目录不算；多个候选按 names 顺序取第一个。
    #[test]
    fn runtime_executable_lookup_requires_executable_files() {
        let root = std::env::temp_dir().join(format!(
            "bili-sync-js-runtime-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let node_name = if cfg!(windows) { "node.exe" } else { "node" };
        let node = root.join(node_name);
        std::fs::write(&node, b"binary").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                super::runtime_executable_in_dir(&root, &["node", "bun"]),
                None,
                "无执行位的普通文件不应被当作 runtime"
            );
            std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            super::runtime_executable_in_dir(&root, &["node", "bun"]),
            Some(node.clone())
        );
        // 子目录不是可执行文件
        std::fs::create_dir_all(root.join("bun")).unwrap();
        assert_eq!(super::runtime_executable_in_dir(&root, &["bun"]), None);
        assert!(!super::is_executable_file(&root.join("not-exist")));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// runtime 名称映射：按文件名主干识别，未知名称归为 node。
    #[test]
    fn runtime_name_is_derived_from_file_stem() {
        use std::path::PathBuf;
        let cases = [
            ("node", "node"),
            ("node.exe", "node"),
            ("bun", "bun"),
            ("deno", "deno"),
            ("qjs", "quickjs"),
            ("quickjs", "quickjs"),
            ("something-else", "node"),
        ];
        for (file, expected) in cases {
            assert_eq!(
                super::ytdlp_js_runtime_name(&PathBuf::from(file)),
                expected,
                "file={file}"
            );
        }
    }
}
