//! TikTok 作为独立视频源平台的接入。
//!
//! 与 YouTube 共用 yt-dlp 扫描/下载链路和统一外部媒体表，但搜索、登录状态、
//! URL 校验等平台逻辑独立维护，不混用 YouTube/抖音的 Cookie。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::{Extension, Json, Path as AxumPath, Query};
use chrono::{Local, TimeZone};
use futures::{stream, StreamExt};
use regex::Regex;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{info, warn};

use bili_sync_entity::{youtube_source, youtube_video};

use crate::api::wrapper::{ApiError, ApiResponse};
use crate::config::CONFIG_DIR;
use crate::utils::live_updates::{notify_queue_status_changed, notify_video_sources_changed, notify_videos_changed};
use crate::utils::time_format::now_standard_string;
use crate::youtube::{
    append_ytdlp_runtime, append_youtube_proxy, command_error,
    create_youtube_source,
    ensure_ytdlp_available, get_platform_sources, normalize_source_type, parse_video_id_set,
    require_source_platform, reset_youtube_source_path, serialize_video_id_set, update_youtube_source,
    update_youtube_source_enabled, ytdlp_command, YouTubeLoginResponse, YouTubeSearchResponse,
    YouTubeSearchResult, YouTubeSourceResponse, YouTubeSourceVideosRequest,
};
use crate::api::response::{SubmissionVideoInfo, SubmissionVideosResponse};

const TIKWM_USER_SEARCH_API: &str = "https://www.tikwm.com/api/user/search";
const TIKTOK_WEB_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";
const TIKTOK_SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const TIKTOK_SEARCH_CONCURRENCY: usize = 4;

/// 用 curl-impersonate（Chrome TLS 指纹）请求 TikTok 官方接口，
/// 返回 (HTTP 状态码, 响应文本)。TikTok 的 Akamai 风控按 TLS/JA3 与 HTTP/2
/// 指纹拒绝 reqwest/OpenSSL 客户端（即使签名与 Cookie 完整也返回空 body）；
/// curl-impersonate 使用与浏览器一致的 Chrome 指纹，可正常拉取关注/我的喜欢等会话接口。
async fn tiktok_impersonated_get(url: &str, cookie: &str) -> Result<(u16, String)> {
    // user-agent 必须与签名器/签名计算使用的 UA 完全一致（webmssdk 签名内嵌
    // md5(UA)，请求 UA 不一致会被 Akamai 判定为伪签名而返回空 body）。
    let mut headers: Vec<(&str, &str)> = vec![
        ("user-agent", TIKTOK_WEB_UA),
        ("accept", "*/*"),
        ("accept-language", "zh-CN,zh;q=0.9,en;q=0.8"),
        ("referer", "https://www.tiktok.com/"),
    ];
    if !cookie.is_empty() {
        headers.push(("cookie", cookie));
    }
    let (status, body) = crate::tiktok_impersonate::tiktok_impersonated_get(
        url,
        &headers,
        crate::tiktok_impersonate::TIKTOK_IMPERSONATE_TIMEOUT,
    )
    .await?;
    Ok((status, String::from_utf8_lossy(&body).to_string()))
}


#[derive(Debug, Deserialize)]
pub struct TikTokSearchRequest {
    pub keyword: String,
}

#[derive(Debug, Deserialize)]
pub struct TikTokCookieImportRequest {
    pub cookies: String,
    /// 浏览器扩展同步的 localStorage 会话状态（webmssdk 的 msToken/security-sdk 等）。
    #[serde(default)]
    pub local_storage: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Serialize)]
pub struct TikTokStatusResponse {
    pub logged_in: bool,
    pub cookie_path: String,
    /// 是否已同步浏览器会话（localStorage），存在时可启用浏览器会话模拟。
    pub browser_session: bool,
    /// 浏览器会话同步时间（ISO 时间字符串）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_session_at: Option<String>,
}

/// 手动设置的 TikTok 账号 secUid 状态。
#[derive(Debug, Serialize)]
pub struct TikTokSecUidStatusResponse {
    /// 手动填写并保存的账号 secUid；未设置时为 None。
    pub manual_sec_uid: Option<String>,
}

/// 保存/清空手动 TikTok 账号 secUid 请求。
#[derive(Debug, Deserialize)]
pub struct UpdateTikTokSecUidRequest {
    pub sec_uid: String,
}

/// 保存/清空手动 TikTok 账号 secUid 响应。
#[derive(Debug, Serialize)]
pub struct UpdateTikTokSecUidResponse {
    pub success: bool,
    pub message: String,
}

/// 搜索 TikTok 作者。
///
/// TikTok 官方搜索接口（/api/search/user/full/）受 Akamai TLS 指纹和
/// webmssdk 签名双重保护，纯服务端请求即使重放浏览器完整签名也会被拒
/// （返回空 body），因此这里改用公开第三方搜索接口拿到候选作者，再通过
/// TikTok 用户主页 SSR（__UNIVERSAL_DATA_FOR_REHYDRATION__）做官方校验
/// 和资料补全，避免把未经验证的第三方数据直接展示给用户。
pub async fn search_tiktok(
    Query(request): Query<TikTokSearchRequest>,
) -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    let keyword = request.keyword.trim();
    if keyword.is_empty() {
        return Err(ApiError::bad_request("请输入 TikTok 搜索关键词"));
    }
    let results = search_tiktok_profiles(keyword).await?;
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

async fn search_tiktok_profiles(keyword: &str) -> Result<Vec<YouTubeSearchResult>> {
    let client = reqwest::Client::builder()
        .user_agent(TIKTOK_WEB_UA)
        .timeout(TIKTOK_SEARCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
    let url = reqwest::Url::parse_with_params(
        TIKWM_USER_SEARCH_API,
        &[("keywords", keyword), ("count", "12")],
    )?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .send()
        .await
        .context("请求 TikTok 作者搜索失败")?;
    if !response.status().is_success() {
        bail!("TikTok 作者搜索返回 HTTP {}", response.status());
    }
    let payload: serde_json::Value = serde_json::from_str(&response.text().await?)
        .context("解析 TikTok 作者搜索响应失败")?;
    if payload.get("code").and_then(serde_json::Value::as_i64) != Some(0) {
        let message = payload
            .get("msg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("未知错误");
        bail!("TikTok 作者搜索服务返回错误：{message}");
    }
    let Some(user_list) = payload
        .pointer("/data/user_list")
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(Vec::new());
    };

    let mut candidates = Vec::new();
    let mut seen_unique_ids = HashSet::new();
    for entry in user_list {
        let Some(user) = entry.get("user") else {
            continue;
        };
        let Some(unique_id) = user.get("uniqueId").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if unique_id.trim().is_empty() || !seen_unique_ids.insert(unique_id.to_ascii_lowercase()) {
            continue;
        }
        let Some(result) = tiktok_user_to_search_result(user) else {
            continue;
        };
        candidates.push((unique_id.to_string(), result));
    }
    // 用官方主页 SSR 校验并补全资料（昵称、头像、签名、粉丝数），并发限 4。
    let mut results = stream::iter(candidates)
        .map(|(unique_id, result)| async move {
            match fetch_tiktok_ssr_profile(&unique_id).await {
                Ok(Some(verified_user)) => tiktok_user_to_search_result(&verified_user).unwrap_or(result),
                Ok(None) => result,
                Err(error) => {
                    warn!(error = %error, unique_id = %unique_id, "校验 TikTok 作者主页失败，保留第三方搜索结果");
                    result
                }
            }
        })
        .buffer_unordered(TIKTOK_SEARCH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let normalized_keyword = normalized_search_keyword(keyword);
    results.sort_by(|left, right| {
        tiktok_result_match_score(right, &normalized_keyword)
            .cmp(&tiktok_result_match_score(left, &normalized_keyword))
            .then_with(|| {
                right
                    .follower
                    .unwrap_or_default()
                    .cmp(&left.follower.unwrap_or_default())
            })
    });
    Ok(results)
}

pub(crate) fn tiktok_user_to_search_result(user: &serde_json::Value) -> Option<YouTubeSearchResult> {
    let unique_id = user.get("uniqueId").and_then(serde_json::Value::as_str)?;
    let unique_id = unique_id.trim();
    if unique_id.is_empty() {
        return None;
    }
    let title = user
        .get("nickname")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(unique_id)
        .to_string();
    let cover = user
        .get("avatarLarger")
        .or_else(|| user.get("avatarMedium"))
        .or_else(|| user.get("avatarThumb"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let description = user
        .get("signature")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let channel_id = user
        .get("secUid")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let follower = user
        .pointer("/stats/followerCount")
        .and_then(serde_json::Value::as_i64)
        .or_else(|| {
            user.pointer("/statsV2/followerCount")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<i64>().ok())
        });
    Some(YouTubeSearchResult {
        result_type: "tiktok_user".to_string(),
        title,
        author: unique_id.to_string(),
        youtube_url: format!("https://www.tiktok.com/@{unique_id}"),
        channel_id,
        cover,
        description,
        follower,
    })
}

async fn fetch_tiktok_ssr_profile(unique_id: &str) -> Result<Option<serde_json::Value>> {
    let url = format!("https://www.tiktok.com/@{unique_id}");
    let (status, html) = tiktok_impersonated_get(&url, "").await?;
    if status != 200 {
        return Ok(None);
    }
    let regex = Regex::new(r#"<script[^>]*id="__UNIVERSAL_DATA_FOR_REHYDRATION__"[^>]*>(.*?)</script>"#)?;
    let Some(captures) = regex.captures(&html) else {
        return Ok(None);
    };
    let Some(script) = captures.get(1) else {
        return Ok(None);
    };
    let payload: serde_json::Value =
        serde_json::from_str(script.as_str()).context("解析 TikTok 用户主页数据失败")?;
    // stats 位于 userInfo 层（user 对象本身没有），合并进去以便统一映射粉丝数。
    let Some(user_info) = payload
        .pointer("/__DEFAULT_SCOPE__/webapp.user-detail/userInfo")
    else {
        return Ok(None);
    };
    let has_identity = user_info.get("user").is_some()
        && user_info
            .get("user")
            .and_then(|user| user.get("uniqueId"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
    if !has_identity {
        return Ok(None);
    }
    let mut merged = user_info.get("user").cloned().unwrap_or_default();
    if let Some(stats) = user_info.get("stats") {
        merged["stats"] = stats.clone();
    }
    Ok(Some(merged))
}

fn normalized_search_keyword(keyword: &str) -> String {
    keyword
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn tiktok_result_match_score(result: &YouTubeSearchResult, keyword: &str) -> u8 {
    let title = normalized_search_keyword(&result.title);
    let author = normalized_search_keyword(&result.author);
    if title == keyword || author == keyword {
        3
    } else if title.contains(keyword) || author.contains(keyword) {
        2
    } else if keyword.contains(&title) || keyword.contains(&author) {
        1
    } else {
        0
    }
}

/// 当前 TikTok 登录状态：仅检查配置目录中的 cookies.txt 是否可识别。
pub async fn tiktok_status() -> Result<ApiResponse<TikTokStatusResponse>, ApiError> {
    let path = tiktok_cookie_path();
    let browser_session = crate::tiktok_browser::has_tiktok_browser_session();
    let browser_session_at = browser_session
        .then(|| {
            std::fs::metadata(crate::tiktok_browser::tiktok_localstorage_path())
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| {
                    let duration = time
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default();
                    chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0)
                        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string())
                })
        })
        .flatten();
    Ok(ApiResponse::ok(TikTokStatusResponse {
        logged_in: has_tiktok_session(&path),
        cookie_path: path.display().to_string(),
        browser_session,
        browser_session_at,
    }))
}

/// 生成 TikTok Cookie 导入结果提示；登录 Cookie 不完整时附加警告。
fn tiktok_import_message() -> String {
    let mut message =
        "已导入 TikTok cookies.txt；作者扫描和媒体解析将使用此登录状态".to_string();
    if !tiktok_cookie_has_login() {
        message.push_str("；注意：未检测到 sessionid/sid_guard/uid_tt 等登录 Cookie，关注/喜欢列表可能不可用，请确认浏览器处于登录状态后重新导出 cookies.txt");
    }
    message
}

/// 导入电脑浏览器导出的 TikTok Netscape cookies.txt。
/// TikTok 作者主页公开内容无需登录；导入后 yt-dlp 可同步作者主页中需要登录
/// 才可见的私密/受限内容，避免混用 YouTube 或抖音的 Cookie。
pub async fn import_tiktok_cookie_file(
    Json(request): Json<TikTokCookieImportRequest>,
) -> Result<ApiResponse<YouTubeLoginResponse>, ApiError> {
    if !is_netscape_tiktok_cookie_file(&request.cookies) {
        return Err(ApiError::bad_request(
            "文件不是包含 tiktok.com 会话的 Netscape cookies.txt；请在已登录 TikTok 的电脑浏览器中导出 cookies.txt",
        ));
    }
    let path = tiktok_cookie_path();
    let parent = path.parent().context("无效的 TikTok Cookie 文件路径")?;
    tokio::fs::create_dir_all(parent).await?;
    let temporary = path.with_extension("txt.importing");
    tokio::fs::write(&temporary, request.cookies.as_bytes())
        .await
        .context("写入 TikTok cookies.txt 失败")?;
    // 与 YouTube/抖音一致：清理旧会话及其备份/临时快照，避免新旧 Cookie 混用。
    clear_tiktok_login_state_files_except(Some(&temporary)).await;
    replace_cookie_file(&temporary, &path).await?;

    // 导入后立即验证登录态：清除 secUid 缓存并请求官方接口，避免导入失效 Cookie 后
    // “我的喜欢/关注列表”等到真正使用时才报错。浏览器扩展无法导出有效会话时给出明确提示。
    if let Some(cached) = TIKTOK_LOGIN_SEC_UID.get() {
        *cached.lock().await = None;
    }
    if let Some(cached) = TIKTOK_LOGIN_UNIQUE_ID.get() {
        *cached.lock().await = None;
    }
    let mut message = tiktok_import_message();
    match tiktok_login_sec_uid_with_source().await {
        Ok((sec_uid, manual)) => {
            if manual {
                message.push_str(&format!(
                    "；TikTok 服务端验证失败，已采用手动设置的账号 secUid {}（该值需与当前 Cookie 账号一致）",
                    short_sec_uid(&sec_uid)
                ));
            } else {
                message.push_str(&format!("；登录态已验证 ✓（账号 secUid {}）", short_sec_uid(&sec_uid)));
            }
        }
        Err(error) => {
            message.push_str(&format!("；⚠ 登录态未通过 TikTok 服务端验证：{error}"));
        }
    }
    Ok(ApiResponse::ok(YouTubeLoginResponse {
        logged_in: true,
        message,
    }))
}

fn short_sec_uid(sec_uid: &str) -> &str {
    if sec_uid.len() > 24 {
        &sec_uid[..24]
    } else {
        sec_uid
    }
}

pub fn tiktok_cookie_path() -> PathBuf {
    CONFIG_DIR.join("tiktok-cookies.txt")
}

fn has_tiktok_session(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|contents| is_netscape_tiktok_cookie_file(&contents))
}

fn is_netscape_tiktok_cookie_file(contents: &str) -> bool {
    let has_header = contents
        .lines()
        .take(4)
        .any(|line| line.contains("HTTP Cookie File") || line.contains("Netscape"));
    has_header
        && contents.lines().any(|line| {
            let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
            if line.trim_start().starts_with('#') {
                return false;
            }
            let columns = line.split('\t').collect::<Vec<_>>();
            columns.len() >= 7
                && is_tiktok_cookie_domain(columns[0])
                && matches!(
                    columns[5],
                    "sessionid" | "sessionid_ss" | "sid_tt" | "passport_csrf_token" | "ttwid" | "uid_tt"
                )
        })
}

fn is_tiktok_cookie_domain(domain: &str) -> bool {
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    domain == "tiktok.com"
        || domain.ends_with(".tiktok.com")
        || domain.ends_with("tiktokcdn.com")
}

/// 清理旧版或历史导入残留的 TikTok 登录状态文件。
async fn clear_tiktok_login_state_files_except(exclude: Option<&Path>) {
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
        if name == "tiktok-cookies.txt" || name.starts_with("tiktok-cookies.txt.") {
            if let Err(error) = tokio::fs::remove_file(entry.path()).await {
                warn!(path = %entry.path().display(), error = %error, "清理旧 TikTok 登录状态文件失败");
            } else {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        info!(removed, "已清理旧版 TikTok 登录状态文件，重新导入新会话");
    }
}

async fn replace_cookie_file(temporary: &Path, target: &Path) -> Result<()> {
    let backup = target.with_extension("txt.backup");
    let had_target = tokio::fs::try_exists(target).await?;
    if tokio::fs::try_exists(&backup).await? {
        tokio::fs::remove_file(&backup).await?;
    }
    if had_target {
        tokio::fs::rename(target, &backup).await?;
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
            Err(error).context("保存 TikTok cookies.txt 失败")
        }
    }
}

/// 仅在存在已导入的 TikTok 会话时追加 --cookies，避免把 YouTube/抖音 Cookie 误用于 TikTok。
pub fn append_tiktok_cookies(command: &mut Command) {
    let path = tiktok_cookie_path();
    if has_tiktok_session(&path) {
        command.arg("--cookies").arg(path);
    }
}

pub fn is_tiktok_source(source: &youtube_source::Model) -> bool {
    source.source_type == "tiktok"
        || source.source_type == "tiktok_favorite"
        || source.source_type == "tiktok_collection"
}

pub fn is_tiktok_url(value: &str) -> bool {
    let trimmed = value.trim().to_ascii_lowercase();
    let host = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .and_then(|rest| rest.split(['/', '?', '#']).next());
    host.is_some_and(|host| host == "tiktok.com" || host.ends_with(".tiktok.com"))
}

// ---------- TikTok 视频源 CRUD（复用统一外部媒体链路） ----------

pub async fn get_tiktok_sources(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<Vec<YouTubeSourceResponse>>, ApiError> {
    get_platform_sources(db.as_ref(), "tiktok").await
}

pub async fn create_tiktok_source(
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<crate::youtube::CreateYouTubeSourceRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    if !matches!(
        normalize_source_type(&request.source_type)?,
        "tiktok" | "tiktok_favorite" | "tiktok_collection",
    ) {
        return Err(ApiError::bad_request("请使用有效的 TikTok 来源类型"));
    }
    create_youtube_source(Extension(db), Json(request)).await
}

pub async fn update_tiktok_source_enabled(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<crate::youtube::UpdateYouTubeSourceEnabledRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    require_source_platform(db.as_ref(), id, "tiktok").await?;
    update_youtube_source_enabled(AxumPath(id), Extension(db), Json(request)).await
}

pub async fn update_tiktok_source(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<crate::youtube::UpdateYouTubeSourceRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    require_source_platform(db.as_ref(), id, "tiktok").await?;
    update_youtube_source(AxumPath(id), Extension(db), Json(request)).await
}

pub async fn delete_tiktok_source(
    AxumPath(id): AxumPath<i32>,
    Query(request): Query<crate::youtube::DeleteYouTubeSourceRequest>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
) -> Result<ApiResponse<bool>, ApiError> {
    require_source_platform(db.as_ref(), id, "tiktok").await?;
    crate::api::handler::delete_video_source(
        Extension(db),
        AxumPath(("tiktok".to_string(), id)),
        Query(crate::api::request::DeleteVideoSourceRequest {
            delete_local_files: request.delete_local_files,
        }),
    )
    .await?;
    Ok(ApiResponse::ok(true))
}

pub async fn reset_tiktok_source_path(
    AxumPath(id): AxumPath<i32>,
    Extension(db): Extension<std::sync::Arc<DatabaseConnection>>,
    Json(request): Json<crate::youtube::ResetYouTubeSourcePathRequest>,
) -> Result<ApiResponse<YouTubeSourceResponse>, ApiError> {
    require_source_platform(db.as_ref(), id, "tiktok").await?;
    reset_youtube_source_path(AxumPath(id), Extension(db), Json(request)).await
}


// ---------- TikTok 风控/限流退避 ----------

/// 风控事件后的退避保持窗口：窗口内所有 TikTok 会话接口请求间隔都会被放大。
const TIKTOK_RISK_BACKOFF_WINDOW: Duration = Duration::from_secs(120);
/// 风控/限流状态码（403/429/5xx）的最大重试次数（每次按 3 秒 × 次数退避）。
const TIKTOK_RISK_RETRY_ATTEMPTS: usize = 3;

struct TikTokRiskState {
    /// 最近一次风控/限流事件发生时刻。
    last_risk_at: Option<Instant>,
    /// 风控事件连续计数（窗口内累加，用于阶梯放大退避）。
    risk_streak: u64,
}

fn tiktok_risk_state() -> &'static RwLock<TikTokRiskState> {
    static STATE: OnceLock<RwLock<TikTokRiskState>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(TikTokRiskState {
        last_risk_at: None,
        risk_streak: 0,
    }))
}

/// 记录一次 TikTok 风控/限流事件；窗口内的连续事件会阶梯放大后续请求间隔。
async fn record_tiktok_risk_event() {
    let mut state = tiktok_risk_state().write().await;
    let now = Instant::now();
    state.risk_streak = match state.last_risk_at {
        Some(previous) if now.duration_since(previous) < TIKTOK_RISK_BACKOFF_WINDOW => {
            state.risk_streak.saturating_add(1)
        }
        _ => 1,
    };
    state.last_risk_at = Some(now);
}

/// TikTok 会话接口翻页请求之间的人工延迟，避免短时间连续请求触发频率风控
/// （HTTP 429/403）。基础间隔复用全局风控配置（默认 1000ms，最小 800ms）；
/// 自动退避开启且处于风控保持窗口内时，放大到 auto_backoff_base_seconds × 连续次数。
async fn tiktok_page_delay() -> Duration {
    let config = crate::config::reload_config().submission_risk_control;
    let base = Duration::from_millis(config.base_request_delay.max(800));
    if !config.enable_auto_backoff {
        return base;
    }
    let state = tiktok_risk_state().read().await;
    let Some(last_risk_at) = state.last_risk_at else {
        return base;
    };
    if last_risk_at.elapsed() >= TIKTOK_RISK_BACKOFF_WINDOW {
        return base;
    }
    let max_multiplier = config.auto_backoff_max_multiplier.max(1);
    let multiplier = state.risk_streak.clamp(1, max_multiplier);
    let backoff =
        Duration::from_secs(config.auto_backoff_base_seconds.max(1).saturating_mul(multiplier));
    base.max(backoff)
}

/// 是否为可自动退避重试的风控/限流状态码（403/429/5xx）。
fn is_tiktok_risk_status(status: u16) -> bool {
    status == 403 || status == 429 || status >= 500
}

/// 风控/限流状态码的补充提示（用于最终失败时的错误信息）。
fn tiktok_risk_status_hint(status: u16) -> &'static str {
    match status {
        429 => "（触发 TikTok 限流，请稍后重试或更换外源代理）",
        403 | 500..=599 => "（疑似触发 TikTok 风控，请稍后重试或更换外源代理）",
        _ => "",
    }
}

/// 带风控退避重试的 TikTok 会话接口请求（我的喜欢/收藏等极简参数接口）。
///
/// 对 403/429/5xx 记录风控事件并按 3 秒 × 尝试次数退避重试；重试耗尽后把
/// 最后一次状态码交回调用方（由调用方给出带接口名的友好错误）。
async fn tiktok_impersonated_get_with_retry(
    api: &str,
    sec_uid: &str,
    cursor: i64,
    count: usize,
) -> Result<(u16, String)> {
    let cookie = tiktok_cookie_header()?;
    let mut attempt = 0usize;
    loop {
        if attempt > 0 {
            tokio::time::sleep(tiktok_page_delay().await).await;
        }
        let cursor_str = cursor.to_string();
        let count_str = count.to_string();
        let params = vec![
            ("aid", "1988"),
            ("app_name", "tiktok_web"),
            ("device_platform", "web_pc"),
            ("count", count_str.as_str()),
            ("cursor", cursor_str.as_str()),
            ("secUid", sec_uid),
        ];
        let url = reqwest::Url::parse_with_params(api, &params)?;
        let (status, body) = tiktok_impersonated_get(url.as_str(), &cookie).await?;
        if status == 200 {
            return Ok((status, body));
        }
        let risk_limited = is_tiktok_risk_status(status);
        if risk_limited {
            record_tiktok_risk_event().await;
        }
        if !risk_limited || attempt >= TIKTOK_RISK_RETRY_ATTEMPTS {
            return Ok((status, body));
        }
        let wait = Duration::from_secs(3 * (attempt as u64 + 1));
        warn!(
            target: "bili_sync_rs::tiktok",
            status,
            attempt = attempt + 1,
            wait_secs = wait.as_secs(),
            "TikTok Web API 触发风控或限流，延迟后重试"
        );
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

/// 带“空正文重签重试”的 TikTok 签名接口请求（关注/播放列表等）。
///
/// 签名接口偶尔返回 HTTP 200 空正文（临时风控/抖动），重新签名生成新的
/// X-Gnarly/X-Dynosaur 后再请求一次往往能恢复（与社区协议实现一致）。
async fn tiktok_signed_get_with_retry(
    base: &str,
    params: &[(&str, String)],
    cookie: &str,
    label: &str,
) -> Result<(u16, String)> {
    let mut attempt = 0usize;
    loop {
        let unsigned = reqwest::Url::parse_with_params(base, params)?;
        let url = match sign_tiktok_url(unsigned.as_str()).await {
            Ok(signed) => signed,
            Err(error) => {
                warn!(error = %error, label = %label, "TikTok 现场签名不可用，回退旧签名逻辑");
                build_tiktok_signed_url(base, params)?.to_string()
            }
        };
        let (status, body) = tiktok_impersonated_get(&url, cookie).await?;
        if status == 200 && body.trim().is_empty() && attempt == 0 {
            warn!(label = %label, "TikTok 签名接口返回空正文，重新签名后重试一次");
            attempt += 1;
            continue;
        }
        return Ok((status, body));
    }
}

// ---------- TikTok 我的喜欢（favorite/item_list 官方接口） ----------

/// 抖音喜欢列表在 TikTok 上对应 `/api/favorite/item_list/`。该接口在真实
/// 浏览器会话中返回当前登录账号点赞的视频；签名参数（X-Dynosaur/X-Gnarly/
/// X-Bogus/msToken）服务端并不校验内容，唯一门槛是登录 Cookie。这里用设置页
/// 导入的 tiktok-cookies.txt 直接调用，实现“我的喜欢”源类型。

#[derive(Debug, Clone)]
pub struct TikTokPost {
    pub id: String,
    pub url: String,
    pub title: String,
    pub uploader: String,
    pub thumbnail: Option<String>,
    pub published_at: Option<String>,
    pub timestamp: Option<i64>,
    pub duration_seconds: Option<i32>,
    pub is_image_post: bool,
}

const TIKTOK_FAVORITE_API: &str = "https://www.tiktok.com/api/favorite/item_list/";

fn tiktok_cookie_values() -> HashMap<String, String> {
    std::fs::read_to_string(tiktok_cookie_path())
        .ok()
        .map(|contents| {
            contents
                .lines()
                .filter_map(|line| {
                    let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
                    if line.trim_start().starts_with('#') {
                        return None;
                    }
                    let columns = line.split('\t').collect::<Vec<_>>();
                    (columns.len() >= 7 && is_tiktok_cookie_domain(columns[0]))
                        .then(|| (columns[5].to_string(), columns[6].to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn tiktok_cookie_header() -> Result<String> {
    let values = tiktok_cookie_values();
    if values.is_empty() {
        bail!("尚未导入 TikTok cookies.txt，请在设置页导入后使用“我的喜欢”");
    }

    Ok(values
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; "))
}

/// TikTok 登录 verifyFp（来自 s_v_web_id cookie）。
fn tiktok_login_verify_fp() -> Option<String> {
    tiktok_cookie_values()
        .get("s_v_web_id")
        .filter(|value| value.starts_with("verify_"))
        .cloned()
}

/// 手动设置的 TikTok 账号 secUid 文件路径（浏览器控制台执行 common-app-context 取得）。
pub fn tiktok_secuid_path() -> PathBuf {
    CONFIG_DIR.join("tiktok-secuid.txt")
}

/// 读取手动设置的账号 secUid。格式校验：URL 安全字符串且长度足够，
/// 避免把明显无效的输入当作有效凭证。
fn load_manual_tiktok_secuid() -> Option<String> {
    let value = std::fs::read_to_string(tiktok_secuid_path()).ok()?;
    let value = value.trim().to_string();
    if value.len() >= 16
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        Some(value)
    } else {
        None
    }
}

fn valid_manual_tiktok_secuid(value: &str) -> bool {
    value.len() >= 16
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// 保存/清空手动 TikTok 账号 secUid（空值清空文件）。
pub async fn save_tiktok_manual_secuid(value: &str) -> Result<()> {
    let path = tiktok_secuid_path();
    let value = value.trim();
    if value.is_empty() {
        let _ = tokio::fs::remove_file(&path).await;
        return Ok(());
    }
    tokio::fs::write(&path, format!("{value}\n"))
        .await
        .with_context(|| format!("写入 TikTok 手动 secUid 失败: {}", path.display()))?;
    Ok(())
}

/// 当前手动 secUid 设置状态。
pub async fn tiktok_secuid_status() -> Result<ApiResponse<TikTokSecUidStatusResponse>, ApiError> {
    Ok(ApiResponse::ok(TikTokSecUidStatusResponse {
        manual_sec_uid: load_manual_tiktok_secuid(),
    }))
}

/// 保存/清空手动 TikTok 账号 secUid，并重置进程内 secUid 缓存。
pub async fn update_tiktok_secuid(
    Json(request): Json<UpdateTikTokSecUidRequest>,
) -> Result<ApiResponse<UpdateTikTokSecUidResponse>, ApiError> {
    let value = request.sec_uid.trim();
    if !value.is_empty() && !valid_manual_tiktok_secuid(value) {
        return Err(ApiError::from(anyhow!(
            "secUid 格式无效：应为字母/数字/`-`/`_` 组成的至少 16 位字符串（形如 MS4wLjABAAAA...）"
        )));
    }
    save_tiktok_manual_secuid(value).await?;
    if let Some(cached) = TIKTOK_LOGIN_SEC_UID.get() {
        *cached.lock().await = None;
    }
    Ok(ApiResponse::ok(UpdateTikTokSecUidResponse {
        success: true,
        message: if value.is_empty() {
            "已清除手动 TikTok secUid".to_string()
        } else {
            format!("已保存手动 TikTok secUid（{}）", short_sec_uid(value))
        },
    }))
}

/// 获取当前登录 TikTok 账号的 secUid（我的喜欢/收藏夹接口的必需参数）。
///
/// 依次尝试两个官方接口（都不需要签名参数）：
///  1. `node-webapp/api/common-app-context?lang=zh-Hans` -> `user.secUid`
///  2. `passport/web/account/info/?aid=1459&app_language=zh&app_name=tiktok_web` -> `data.sec_user_id`
/// 官方接口失败（常见于服务端出口被 TikTok 风控）时回退到手动设置的文件
/// `tiktok-secuid.txt`，并返回是否使用了手动值。结果按账号 cookie 做进程内缓存。
static TIKTOK_LOGIN_SEC_UID: OnceLock<tokio::sync::Mutex<Option<String>>> = OnceLock::new();

async fn tiktok_login_sec_uid_with_source() -> Result<(String, bool)> {
    let cached = TIKTOK_LOGIN_SEC_UID.get_or_init(|| tokio::sync::Mutex::new(None));
    if let Some(sec_uid) = cached.lock().await.clone() {
        return Ok((sec_uid, false));
    }
    // 手动设置的 secUid 优先使用：common-app-context/passport 网络探测在部分
    // 代理出口下会卡满超时（约 30s×2），而手动值本身是账号级权威值，可直接用。
    if let Some(manual) = load_manual_tiktok_secuid() {
        let sec_uid = manual.clone();
        *cached.lock().await = Some(sec_uid.clone());
        return Ok((sec_uid, true));
    }
    let cookie = tiktok_cookie_header()?;

    let mut sec_uid: Option<String> = None;
    // 1) common-app-context
    match tiktok_impersonated_get(
        "https://www.tiktok.com/node-webapp/api/common-app-context?lang=zh-Hans",
        &cookie,
    )
    .await
    {
        Ok((status, body)) => {
            if status == 200 {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    sec_uid = json
                        .pointer("/user/secUid")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .filter(|value| !value.is_empty());
                }
            } else {
                warn!(status, "获取 TikTok common-app-context 返回非 200，回退 passport 接口");
            }
        }
        Err(error) => warn!(error = %error, "获取 TikTok common-app-context 失败，回退 passport 接口"),
    }
    // 2) passport /account/info
    if sec_uid.is_none() {
        match tiktok_impersonated_get(
            "https://www.tiktok.com/passport/web/account/info/?aid=1459&app_language=zh&app_name=tiktok_web",
            &cookie,
        )
        .await
        {
            Ok((status, body)) => {
                if status == 200 {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                        sec_uid = json
                            .pointer("/data/sec_user_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .filter(|value| !value.is_empty());
                    }
                } else {
                    warn!(status, "获取 TikTok passport 账号信息返回非 200");
                }
            }
            Err(error) => warn!(error = %error, "获取 TikTok passport 账号信息失败"),
        }
    }

    match sec_uid {
        Some(sec_uid) => {
            *cached.lock().await = Some(sec_uid.clone());
            Ok((sec_uid, false))
        }
        None => {
            if let Some(manual) = load_manual_tiktok_secuid() {
                warn!(
                    "TikTok 服务端验证失败，使用手动设置的账号 secUid（{}）",
                    short_sec_uid(&manual)
                );
                *cached.lock().await = Some(manual.clone());
                Ok((manual, true))
            } else {
                bail!(
                    "无法获取当前 TikTok 账号 secUid（登录态可能已失效）：请确认已导入最新 cookies.txt 后重试；或在设置页手动填写账号 secUid"
                )
            }
        }
    }
}

async fn tiktok_login_sec_uid() -> Result<String> {
    Ok(tiktok_login_sec_uid_with_source().await?.0)
}

/// 导入的 cookies.txt 是否包含完整登录会话（而非仅游客 ttwid）。
fn tiktok_cookie_has_login() -> bool {
    let values = tiktok_cookie_values();
    ["sessionid", "sessionid_ss", "sid_tt", "sid_guard", "uid_tt"]
        .iter()
        .any(|name| values.get(*name).is_some_and(|value| !value.is_empty()))
}

/// 构建带 X-Gnarly 签名的 TikTok API URL。
///
/// X-Gnarly 对“去掉 X-* 参数后的完整查询串”做 MD5 签名并加密外壳。这里先用
/// 参数拼出最终 URL，取查询串调用 `tiktok_sign::x_gnarly`，再追加 X-Gnarly，
/// 保证被签名的字符串与最终发出的查询串完全一致。
/// VMP v3（webmssdk 5.3.1 / scm 1.0.0.388）已完整逆向并逐字节复现，见
/// `问题文件夹/VMP逆向分析.md` §13；端到端验证 statusCode=0。
/// X-Dynosaur 非必需（实测仅带 X-Gnarly 即可通过），此处保留占位。
fn build_tiktok_signed_url(base: &str, params: &[(&str, String)]) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse_with_params(base, params)?;
    let query = url.query().unwrap_or("").to_string();
    let gnarly = crate::tiktok_sign::x_gnarly(&query, TIKTOK_WEB_UA, "", None);
    url.query_pairs_mut().append_pair("X-Gnarly", &gnarly);
    Ok(url)
}

/// 从 TikTok 作品项中提取封面 URL。
///
/// 实测官方接口返回的 `video.cover` / `video.originCover` 是**直接字符串 URL**
/// （个别接口为 `{ "url_list": [...] }` 对象），两种结构都兼容。
fn tiktok_cover_url(value: &serde_json::Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value
            .get("url_list")
            .and_then(serde_json::Value::as_array)
            .and_then(|list| list.first())
            .and_then(serde_json::Value::as_str)
    })
}

pub(crate) fn parse_tiktok_item(item: &serde_json::Value) -> Option<TikTokPost> {
    let id = item.get("id").and_then(serde_json::Value::as_str)?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let uploader = item
        .pointer("/author/nickname")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let title = item
        .get("desc")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let unique_id = item
        .pointer("/author/uniqueId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let thumbnail = item
        .pointer("/video/cover")
        .or_else(|| item.pointer("/video/originCover"))
        .and_then(tiktok_cover_url)
        .map(str::to_string);
    let duration_seconds = item
        .pointer("/video/duration")
        .and_then(serde_json::Value::as_i64)
        .map(|millis| (millis / 1000) as i32)
        .filter(|seconds| *seconds > 0);
    let timestamp = item
        .get("createTime")
        .and_then(serde_json::Value::as_i64);
    let published_at = timestamp.map(|value| {
        chrono::DateTime::from_timestamp(value, 0)
            .map(|time| time.with_timezone(&Local).format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    });
    let is_image_post = item
        .get("imagePost")
        .map(|value| value.is_object() && !value.as_object().is_none_or(|object| object.is_empty()))
        .unwrap_or(false);
    Some(TikTokPost {
        id: id.clone(),
        url: format!("https://www.tiktok.com/@{}/video/{id}", unique_id.trim().trim_start_matches('@')),
        title,
        uploader,
        thumbnail,
        published_at,
        timestamp,
        duration_seconds,
        is_image_post,
    })
}

/// 解码 TikTok API 响应：真实浏览器抓到的 body 可能是 base64 编码的 JSON，
/// 服务端直连时通常是明文 JSON，两种都处理。
fn decode_tiktok_body(body: &str) -> Result<serde_json::Value> {
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).context("解析 TikTok 我的喜欢响应失败");
    }
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        trimmed,
    )
    .context("解码 TikTok 我的喜欢响应失败")?;
    serde_json::from_slice(&decoded).context("解析解码后的 TikTok 我的喜欢响应失败")
}

async fn fetch_tiktok_favorites(limit: usize) -> Result<Vec<TikTokPost>> {
    // 极简请求（与实测可用的 tiktok_personal_lists.py 一致）：仅需 cookies.txt 登录态
    // + secUid，不携带签名与浏览器参数。关键前提是出口 IP 未被 TikTok 风控：
    // 本机直连 IP 被标记时该接口返回 HTTP 200 空 body，配置外源代理
    // （proxy/youtube_proxy，如 http://192.168.2.3:7893）后即可正常拉取（实测 18 条）。
    // 429/403/5xx 自动退避重试，短时限流不会直接失败。
    let sec_uid = tiktok_login_sec_uid().await?;
    let mut cursor = 0i64;
    let mut posts = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..500 {
        let (status, body) =
            tiktok_impersonated_get_with_retry(TIKTOK_FAVORITE_API, &sec_uid, cursor, 30).await?;
        if status != 200 {
            bail!(
                "TikTok 我的喜欢返回 HTTP {}{}",
                status,
                tiktok_risk_status_hint(status)
            );
        }
        if body.trim().is_empty() {
            bail!(
                "TikTok 我的喜欢返回空响应：当前出口 IP 可能被 TikTok 风控。请在设置页配置外源代理（proxy/youtube_proxy）后重试"
            );
        }
        let payload = decode_tiktok_body(&body)?;
        let mut page_has_items = false;
        if let Some(items) = payload.get("itemList").and_then(serde_json::Value::as_array) {
            for item in items {
                if let Some(post) = parse_tiktok_item(item) {
                    page_has_items = true;
                    if seen.insert(post.id.clone()) {
                        posts.push(post);
                    }
                }
            }
        }
        if posts.len() >= limit
            || !payload.get("hasMore").and_then(serde_json::Value::as_bool).unwrap_or(false)
        {
            break;
        }
        if !page_has_items {
            if posts.is_empty() {
                bail!(
                    "TikTok 我的喜欢接口未返回视频列表：请确认已导入最新 cookies.txt 且账号 secUid 正确；若出口 IP 被 TikTok 风控，请在设置页配置外源代理（proxy/youtube_proxy）后重试"
                );
            }
            break;
        }
        let next = payload
            .get("cursor")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse::<i64>().ok())
            .or_else(|| payload.get("cursor").and_then(serde_json::Value::as_i64))
            .unwrap_or(cursor);
        if next == cursor {
            break;
        }
        cursor = next;
        tokio::time::sleep(tiktok_page_delay().await).await;
    }
    Ok(posts)
}

/// 扫描“我的喜欢”源：官方接口拉取 + 与抖音源一致的入库/增量逻辑。
pub async fn scan_tiktok_favorite_source(
    db: &DatabaseConnection,
    source: &youtube_source::Model,
) -> Result<u64> {
    let posts = fetch_tiktok_favorites(usize::MAX).await?;
    persist_tiktok_posts(db, source, posts).await
}

/// 公共入库逻辑：与抖音源一致的增量/过滤/关键字/时长/日期处理。
async fn persist_tiktok_posts(
    db: &DatabaseConnection,
    source: &youtube_source::Model,
    posts: Vec<TikTokPost>,
) -> Result<u64> {
    let selected_history = source
        .selected_videos
        .as_deref()
        .and_then(|value| serde_json::from_str::<HashSet<String>>(value).ok())
        .unwrap_or_default();
    let known_video_ids = parse_video_id_set(source.known_video_ids.as_deref());
    let mut deleted_video_ids = parse_video_id_set(source.deleted_video_ids.as_deref());
    let scan_deleted_videos = source.scan_deleted_videos || source.scan_deleted_videos_once;
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
            if existing.is_image_post != post.is_image_post {
                let mut active: youtube_video::ActiveModel = existing.into();
                active.is_image_post = Set(post.is_image_post);
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
            is_image_post: Set(post.is_image_post),
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
        info!(source_id = source.id, added, "TikTok 我的喜欢源发现新作品");
        notify_videos_changed();
        notify_queue_status_changed();
    }
    Ok(added)
}

/// 我的喜欢源需要登录状态；扫描入口统一从 youtube.rs 调用。
pub fn ensure_tiktok_session() -> Result<()> {
    if has_tiktok_session(&tiktok_cookie_path()) {
        Ok(())
    } else {
        bail!("TikTok 我的喜欢需要登录状态，请先在设置页导入 TikTok cookies.txt")
    }
}


// ---------- TikTok 作者历史作品选择（右侧面板，yt-dlp 扫描） ----------

/// 选择面板的数据结构：TikTok 作者历史视频 / 我的喜欢视频列表，供添加来源时
/// 在右侧勾选历史作品（与抖音一致）。
pub async fn get_tiktok_source_videos(
    Query(request): Query<YouTubeSourceVideosRequest>,
) -> Result<ApiResponse<SubmissionVideosResponse>, ApiError> {
    let source_type = request.source_type.trim().to_ascii_lowercase();
    match source_type.as_str() {
        "tiktok" => fetch_tiktok_author_videos(&request).await,
        "tiktok_favorite" => fetch_tiktok_favorite_videos(&request).await,
        _ => Err(ApiError::from(anyhow!(
            "仅 TikTok 作者与我的喜欢支持历史作品选择"
        ))),
    }
}

/// 拉取 TikTok 作者主页历史视频（yt-dlp 平铺），按页返回供右侧勾选。
async fn fetch_tiktok_author_videos(
    request: &YouTubeSourceVideosRequest,
) -> Result<ApiResponse<SubmissionVideosResponse>, ApiError> {
    let raw_url = request.url.as_deref().unwrap_or("").trim();
    if raw_url.is_empty() || !is_tiktok_url(raw_url) {
        return Err(ApiError::from(anyhow!("请输入有效的 TikTok 作者主页链接")));
    }
    let page = request.page.unwrap_or(1).max(1);
    let page_size = request.page_size.unwrap_or(100).clamp(1, 200);
    let keyword = request
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    ensure_ytdlp_available().await?;
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
    append_tiktok_cookies(&mut command);
    append_youtube_proxy(&mut command);
    command.arg(raw_url);
    let output = tokio::time::timeout(Duration::from_secs(10 * 60), command.output())
        .await
        .map_err(|_| anyhow!("加载 TikTok 历史视频超时"))??;
    if !output.status.success() {
        return Err(ApiError::from(anyhow!(
            "加载 TikTok 历史视频失败：{}",
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
            .and_then(|value| value.as_i64())
            .unwrap_or_default();
        let author = item
            .get("uploader")
            .or_else(|| item.get("channel"))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let pubtime = item
            .get("upload_date")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let pubtime = if pubtime.len() == 8 {
            format!("{}-{}-{}", &pubtime[0..4], &pubtime[4..6], &pubtime[6..8])
        } else {
            pubtime
        };
        videos.push(SubmissionVideoInfo {
            bvid: id.to_string(),
            title,
            author: (!author.trim().is_empty()).then_some(author),
            cover,
            pubtime,
            duration,
            view: i32::try_from(view).unwrap_or(i32::MAX),
            danmaku: 0,
            description: String::new(),
        });
    }
    let has_more = videos.len() > page_size as usize;
    videos.truncate(page_size as usize);
    let total = start as i64 + videos.len() as i64 + i64::from(has_more);
    Ok(ApiResponse::ok(SubmissionVideosResponse {
        videos,
        total,
        page,
        page_size,
    }))
}

/// 拉取当前登录账号“我的喜欢”视频列表（官方 /api/favorite/item_list/），
/// 按页返回供右侧勾选。
async fn fetch_tiktok_favorite_videos(
    request: &YouTubeSourceVideosRequest,
) -> Result<ApiResponse<SubmissionVideosResponse>, ApiError> {
    let page = request.page.unwrap_or(1).max(1);
    let page_size = request.page_size.unwrap_or(100).clamp(1, 200);
    let keyword = request
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let mut posts = fetch_tiktok_favorites(usize::MAX).await.map_err(ApiError::from)?;
    posts.sort_by_key(|post| post.timestamp.unwrap_or_default());
    posts.reverse();
    let mut videos = Vec::new();
    for post in posts {
        if keyword
            .as_ref()
            .is_some_and(|keyword| !post.title.to_ascii_lowercase().contains(keyword))
        {
            continue;
        }
        videos.push(SubmissionVideoInfo {
            bvid: post.id.clone(),
            title: post.title.clone(),
            author: (!post.uploader.trim().is_empty()).then_some(post.uploader.clone()),
            cover: post.thumbnail.clone().unwrap_or_default(),
            pubtime: post.published_at.clone().unwrap_or_default(),
            duration: post.duration_seconds.unwrap_or_default(),
            view: 0,
            danmaku: 0,
            description: String::new(),
        });
    }
    let total = videos.len() as i64;
    let start = (page as usize - 1).saturating_mul(page_size as usize);
    let page_videos = videos
        .into_iter()
        .skip(start)
        .take(page_size as usize)
        .collect::<Vec<_>>();
    Ok(ApiResponse::ok(SubmissionVideosResponse {
        videos: page_videos,
        total,
        page,
        page_size,
    }))
}

// ---------- TikTok webmssdk 现场签名（Node 子进程） ----------
//
// /api/user/list（关注列表）等接口要求 X-Dynosaur 签名，而该签名由浏览器内
// webmssdk 按“登录会话 + 本地浏览器指纹状态”实时生成、有时效。纯服务端伪造
// 或重放旧签名均会被 TikTok 风控拒绝（实测返回 HTTP 200 但空 body）。
// 最终解：用 Node 直接执行内嵌的 webmssdk（VMP 字节码 + 运行时 shim），结合
// 已导入的浏览器会话状态（tiktok-cookies.txt + tiktok-localstorage.json）在
// 服务端现场生成有效签名，不依赖任何浏览器。经真实接口验证 statusCode=0。

/// 内嵌的自包含 webmssdk 签名器（含 VM 字节码与运行时 shim，Node 可直接执行）。
const TIKTOK_SIGNER_JS: &str = include_str!("../../../scripts/tiktok-signer.cjs");

/// 签名器落盘位置（首次使用时从二进制内嵌资源释放）。
fn tiktok_signer_path() -> PathBuf {
    CONFIG_DIR.join("tools").join("tiktok-signer.cjs")
}

/// 确保签名器脚本已写入磁盘（缺失或长度不符时重新释放）。
async fn ensure_tiktok_signer() -> Result<PathBuf> {
    let path = tiktok_signer_path();
    let parent = path.parent().expect("签名器路径缺少父目录");
    tokio::fs::create_dir_all(parent)
        .await
        .context("创建 TikTok 签名器目录失败")?;
    let needs_write = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta.len() != TIKTOK_SIGNER_JS.len() as u64,
        Err(_) => true,
    };
    if needs_write {
        tokio::fs::write(&path, TIKTOK_SIGNER_JS)
            .await
            .context("写入 TikTok 签名器失败")?;
        info!(path = %path.display(), "已释放 TikTok webmssdk 签名器");
    }
    Ok(path)
}

/// 把签名器子进程输出截断为可读文本（UTF-8）。
fn signer_output_text(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr_text = String::from_utf8_lossy(stderr).to_string();
    let text = if stderr_text.trim().is_empty() {
        String::from_utf8_lossy(stdout).to_string()
    } else {
        stderr_text
    };
    let text = text.trim();
    if text.chars().count() > 300 {
        let tail: String = text.chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    } else {
        text.to_string()
    }
}

/// 查找 Node.js 运行时：优先环境变量 BILI_SYNC_TIKTOK_NODE（可指定可执行文件
/// 或所在目录），其次常见安装路径与 CONFIG_DIR/tools，最后系统 PATH。
fn find_tiktok_node() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("BILI_SYNC_TIKTOK_NODE") {
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

/// 用 webmssdk 现场签名 TikTok API URL，返回签名后的完整 URL。
///
/// 需要 Node.js 与已同步的浏览器会话（CONFIG_DIR/tiktok-cookies.txt +
/// tiktok-localstorage.json，后者由浏览器扩展“同步 TikTok 会话”写入）。
async fn sign_tiktok_url(url: &str) -> Result<String> {
    let signer = ensure_tiktok_signer().await?;
    let node = find_tiktok_node().ok_or_else(|| {
        anyhow!(
            "未找到 Node.js 运行时：TikTok 关注/收藏夹接口需要 Node.js 执行 webmssdk 现场签名。请安装 Node.js，或通过环境变量 BILI_SYNC_TIKTOK_NODE 指定 node 路径"
        )
    })?;
    let mut command = Command::new(&node);
    command
        .arg(&signer)
        .arg(url)
        .env("BILI_SYNC_CONFIG_DIR", CONFIG_DIR.as_os_str())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(90), command.output())
        .await
        .map_err(|_| anyhow!("TikTok 签名器执行超时（90s）"))?
        .map_err(|error| anyhow!("启动 TikTok 签名器失败（Node 运行时不完整？）：{error}"))?;
    if !output.status.success() {
        bail!(
            "TikTok 签名器执行失败（退出码 {}）：{}",
            output.status.code().unwrap_or(-1),
            signer_output_text(&output.stderr, &output.stdout)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let signed = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .last()
        .unwrap_or("")
        .to_string();
    if !signed.starts_with("https://") {
        bail!(
            "TikTok 签名器未返回有效签名 URL：{}",
            signer_output_text(&output.stderr, &output.stdout)
        );
    }
    Ok(signed)
}

// ---------- TikTok 已关注作者（/api/user/list/ 官方接口） ----------

const TIKTOK_USER_LIST_API: &str = "https://www.tiktok.com/api/user/list/";

/// 当前登录账号的 odinId（来自 multi_sids cookie 的第一段）。
fn tiktok_login_odin_id() -> Option<String> {
    tiktok_cookie_values()
        .get("multi_sids")
        .and_then(|value| value.split('%').next())
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

/// 获取当前登录账号已关注的 TikTok 作者列表。
///
/// 官方 /api/user/list/ 需要浏览器实时签名（X-Dynosaur/X-Gnarly/msToken），
/// 服务端直连通常会返回空响应；这里按完整参数尽力请求，拿不到数据时给出明确提示。
pub async fn get_tiktok_followings() -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    fetch_tiktok_followings().await.map_err(ApiError::from)
}

async fn fetch_tiktok_followings() -> anyhow::Result<ApiResponse<YouTubeSearchResponse>> {
    // 服务端直连：curl-impersonate（Chrome TLS 指纹）+ Node webmssdk 现场签名。
    let cookie = tiktok_cookie_header()?;
    let ms_token = tiktok_cookie_values().get("msToken").cloned().unwrap_or_default();
    let odin_id = tiktok_login_odin_id().ok_or_else(|| {
        anyhow!("无法从 TikTok cookies.txt 解析账号 ID（缺少 multi_sids），请重新导入登录状态")
    })?;
    let device_id = tiktok_web_device_id().to_string();
    let params: Vec<(&str, String)> = vec![
        ("aid", "1988".to_string()),
        ("app_language", "zh-Hans".to_string()),
        ("app_name", "tiktok_web".to_string()),
        ("browser_language", "zh-CN".to_string()),
        ("browser_name", "Mozilla".to_string()),
        ("browser_online", "true".to_string()),
        ("browser_platform", "Win32".to_string()),
        ("browser_version", TIKTOK_WEB_UA.to_string()),
        ("channel", "tiktok_web".to_string()),
        ("cookie_enabled", "true".to_string()),
        ("count", "30".to_string()),
        ("data_collection_enabled", "false".to_string()),
        ("device_id", device_id.to_string()),
        ("device_platform", "web_pc".to_string()),
        ("focus_state", "true".to_string()),
        ("from_page", "user".to_string()),
        ("history_len", "4".to_string()),
        ("isNonPersonalized", "false".to_string()),
        ("is_fullscreen", "false".to_string()),
        ("is_page_visible", "true".to_string()),
        ("maxCursor", "0".to_string()),
        ("minCursor", "0".to_string()),
        ("odinId", odin_id.clone()),
        ("os", "windows".to_string()),
        ("priority_region", String::new()),
        ("referer", "https://www.tiktok.com/".to_string()),
        ("region", "SG".to_string()),
        ("root_referer", "https://www.tiktok.com/".to_string()),
        ("scene", "21".to_string()),
        ("screen_height", "1440".to_string()),
        ("screen_width", "2560".to_string()),
        ("targetUserId", odin_id.clone()),
        ("tz_name", "Asia/Shanghai".to_string()),
        ("user_is_login", "true".to_string()),
        ("WebIdLastTime", (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(1))
        .to_string()),
        ("verifyFp", tiktok_login_verify_fp().unwrap_or_default()),
        ("webcast_language", "zh-Hans".to_string()),
        ("msToken", ms_token.clone()),
        ("X-Bogus", "1".to_string()),
    ];
    let (status, body) =
        tiktok_signed_get_with_retry(TIKTOK_USER_LIST_API, &params, &cookie, "关注列表").await?;
    if status != 200 {
        bail!(
            "TikTok 关注列表返回 HTTP {}{}",
            status,
            tiktok_risk_status_hint(status)
        );
    }
    if body.trim().is_empty() {
        bail!(
            "TikTok 关注列表接口返回空响应（已自动重新签名一次仍失败）：请确认当前出口网络未被 TikTok 风控，稍后重试或更换外源代理（proxy/youtube_proxy），或重新导入最新 cookies.txt"
        );
    }
    let payload = decode_tiktok_body(&body)?;
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    if let Some(user_list) = payload.get("userList").and_then(serde_json::Value::as_array) {
        for item in user_list {
            let Some(user) = item.get("user") else {
                continue;
            };
            let Some(result) = tiktok_user_to_search_result(user) else {
                continue;
            };
            if seen.insert(result.youtube_url.to_ascii_lowercase()) {
                results.push(result);
            }
        }
    }
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

// ---------- TikTok 收藏夹（用户播放列表 Playlist） ----------

const TIKTOK_USER_PLAYLIST_API: &str = "https://www.tiktok.com/api/user/playlist/";
const TIKTOK_USER_DETAIL_API: &str = "https://www.tiktok.com/api/user/detail/";
const TIKTOK_PLAYLIST_API: &str = "https://www.tiktok.com/api/playlist/";
/// 当前登录账号“收藏”（收藏夹中的全部视频）官方接口。
///
/// 与“我的喜欢”（/api/favorite/item_list/）一致：极简参数 + cookies.txt 登录态
/// 即可服务端直连（实测无需浏览器签名），前提是出口 IP 未被 TikTok 风控。
const TIKTOK_USER_COLLECT_API: &str = "https://www.tiktok.com/api/user/collect/item_list/";

#[derive(Debug, Deserialize)]
pub struct TikTokPlaylistsRequest {
    #[serde(default)]
    pub url: Option<String>,
}

/// 从 TikTok 作者主页链接中解析用户名（uniqueId）。
fn tiktok_unique_id_from_url(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    let without_query = trimmed.split('?').next().unwrap_or(trimmed);
    let last = without_query.rsplit('/').next()?;
    last.strip_prefix('@')
        .map(str::to_string)
        .filter(|name| !name.is_empty())
}

static TIKTOK_SEC_UID_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

/// 当前登录 TikTok 账号的 uniqueId（common-app-context，进程内缓存）。
///
/// 用于判断填写的作者主页链接是否就是登录账号本人：是则直接复用登录
/// secUid，避免抓取主页 HTML 被 TikTok 403 风控拦截。接口失败返回 None，
/// 不影响后续走 user/detail 或主页 HTML 兜底流程。
static TIKTOK_LOGIN_UNIQUE_ID: std::sync::OnceLock<tokio::sync::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

async fn tiktok_login_account_unique_id() -> Option<String> {
    let cached = TIKTOK_LOGIN_UNIQUE_ID.get_or_init(|| tokio::sync::Mutex::new(None));
    if let Some(value) = cached.lock().await.clone() {
        return Some(value);
    }
    let cookie = match tiktok_cookie_header() {
        Ok(cookie) => cookie,
        Err(_) => return None,
    };
    match tiktok_impersonated_get(
        "https://www.tiktok.com/node-webapp/api/common-app-context?lang=zh-Hans",
        &cookie,
    )
    .await
    {
        Ok((status, body)) if status == 200 => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                let unique_id = json
                    .pointer("/user/uniqueId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .map(|value| value.trim().trim_start_matches('@').to_string())
                    .filter(|value| !value.is_empty());
                if let Some(value) = unique_id {
                    *cached.lock().await = Some(value.clone());
                    return Some(value);
                }
            }
        }
        Ok((status, _)) => warn!(
            status,
            "获取 TikTok common-app-context 返回非 200（用于判断是否为本人主页）"
        ),
        Err(error) => warn!(
            error = %error,
            "获取 TikTok common-app-context 失败（用于判断是否为本人主页）"
        ),
    }
    None
}

/// 通过官方 `user/detail` 接口解析任意作者的 secUid（完整浏览器参数 + 现场签名）。
///
/// Akamai 对该接口风控较严（部分出口返回 HTTP 200 空 body / 429），失败返回
/// None，由上层继续尝试主页 HTML 兜底。
async fn fetch_tiktok_user_detail_sec_uid(unique_id: &str, cookie: &str) -> Result<Option<String>> {
    let ms_token = tiktok_cookie_values().get("msToken").cloned().unwrap_or_default();
    let verify_fp = tiktok_login_verify_fp().unwrap_or_default();
    let device_id = tiktok_web_device_id().to_string();
    let odin_id = tiktok_web_device_id().to_string();
    let referer = format!("https://www.tiktok.com/@{unique_id}");
    let params: Vec<(&str, String)> = vec![
        ("WebIdLastTime", (chrono::Utc::now().timestamp() - 900).to_string()),
        ("abTestVersion", "[object Object]".to_string()),
        ("aid", "1988".to_string()),
        ("appType", "m".to_string()),
        ("app_language", "zh-Hans".to_string()),
        ("app_name", "tiktok_web".to_string()),
        ("browser_language", "zh-CN".to_string()),
        ("browser_name", "Mozilla".to_string()),
        ("browser_online", "true".to_string()),
        ("browser_platform", "Win32".to_string()),
        ("browser_version", TIKTOK_WEB_UA.to_string()),
        ("channel", "tiktok_web".to_string()),
        ("cookie_enabled", "true".to_string()),
        ("data_collection_enabled", "true".to_string()),
        ("device_id", device_id.clone()),
        ("device_platform", "web_pc".to_string()),
        ("focus_state", "true".to_string()),
        ("from_page", "user".to_string()),
        ("history_len", "7".to_string()),
        ("is_fullscreen", "false".to_string()),
        ("is_page_visible", "true".to_string()),
        ("language", "zh-Hans".to_string()),
        ("needAudienceControl", "false".to_string()),
        ("odinId", odin_id.clone()),
        ("os", "windows".to_string()),
        ("priority_region", "US".to_string()),
        ("referer", referer.clone()),
        ("region", "US".to_string()),
        ("root_referer", referer.clone()),
        ("screen_height", "1440".to_string()),
        ("screen_width", "2560".to_string()),
        ("secUid", String::new()),
        ("tz_name", "Asia/Shanghai".to_string()),
        ("uniqueId", unique_id.to_string()),
        ("user", "[object Object]".to_string()),
        ("user_is_login", "true".to_string()),
        ("verifyFp", verify_fp),
        ("webcast_language", "zh-Hans".to_string()),
        ("msToken", ms_token),
    ];
    let unsigned = reqwest::Url::parse_with_params(TIKTOK_USER_DETAIL_API, &params)?;
    let url = match sign_tiktok_url(unsigned.as_str()).await {
        Ok(signed) => signed,
        Err(error) => {
            warn!(error = %error, "TikTok 现场签名不可用，回退旧签名逻辑（user/detail 接口）");
            build_tiktok_signed_url(TIKTOK_USER_DETAIL_API, &params)?.to_string()
        }
    };
    let (status, body) = tiktok_impersonated_get(&url, cookie).await?;
    if status != 200 {
        return Ok(None);
    }
    if body.trim().is_empty() {
        return Ok(None);
    }
    let payload = decode_tiktok_body(&body)?;
    let sec_uid = payload
        .pointer("/userInfo/user/secUid")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    Ok(sec_uid)
}

/// 解析 TikTok 作者 secUid（收藏夹/播放列表接口的前置凭证）。
///
/// 优先级：
///  1. 链接是当前登录账号本人（common-app-context 的 uniqueId 一致）→ 直接用登录
///     secUid，避免抓主页 HTML 被 403 风控拦截；
///  2. `user/detail` 官方接口（完整浏览器参数 + 签名）解析其他作者；
///  3. 抓取作者主页 HTML 提取（旧方案兜底，部分网络环境可用）；
///  4. 全部失败时给出可操作的错误提示。
async fn resolve_tiktok_sec_uid(url: &str) -> Result<String> {
    let unique_id = tiktok_unique_id_from_url(url).ok_or_else(|| {
        anyhow!("无法从链接中解析 TikTok 用户名：{url}，请填写形如 https://www.tiktok.com/@用户名 的主页链接")
    })?;

    // 1) 本人主页：直接使用登录账号 secUid，无需抓取
    if let Some(login_unique_id) = tiktok_login_account_unique_id().await {
        if login_unique_id.eq_ignore_ascii_case(&unique_id) {
            return tiktok_login_sec_uid().await;
        }
    }

    let cookie = tiktok_cookie_header()?;

    // 2) user/detail 官方接口
    if let Some(sec_uid) = fetch_tiktok_user_detail_sec_uid(&unique_id, &cookie).await? {
        return Ok(sec_uid);
    }

    // 3) 主页 HTML 兜底
    let page_url = format!("https://www.tiktok.com/@{unique_id}");
    let (status, body) = tiktok_impersonated_get(&page_url, &cookie).await?;
    if status == 200 {
        let regex = TIKTOK_SEC_UID_RE.get_or_init(|| {
            Regex::new(r#""secUid":"([^"]+)"#).expect("invalid secUid regex")
        });
        if let Some(sec_uid) = regex
            .captures(&body)
            .and_then(|captures| captures.get(1))
            .map(|matched| matched.as_str().to_string())
            .filter(|value| !value.is_empty())
        {
            return Ok(sec_uid);
        }
    }

    bail!(
        "无法解析 TikTok 作者 @{unique_id} 的 secUid：主页与详情接口均被 TikTok 风控拦截。若这是你的账号，请直接不填链接点击“获取自己的列表”；其他作者请稍后重试或更换外源代理"
    )
}

/// 获取 TikTok 播放列表（收藏夹）。
///
/// 链接为空时读取当前登录账号自己的播放列表（使用账号 secUid，无需填链接）；
/// 填写主页链接时抓取该作者主页提取 secUid 后读取其公开播放列表。
pub async fn get_tiktok_playlists(
    Query(request): Query<TikTokPlaylistsRequest>,
) -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    let raw_url = request.url.as_deref().unwrap_or("").trim();
    let own = raw_url.is_empty();
    let sec_uid = if own {
        // 获取自己的列表：使用登录账号的 secUid（自动/手动均可），无需主页链接。
        tiktok_login_sec_uid().await.map_err(ApiError::from)?
    } else {
        if !is_tiktok_url(raw_url) {
            return Err(ApiError::from(anyhow!("请输入有效的 TikTok 主页链接")));
        }
        resolve_tiktok_sec_uid(raw_url).await.map_err(ApiError::from)?
    };
    let response = fetch_tiktok_playlists(&sec_uid, own).await.map_err(ApiError::from)?;
    Ok(response)
}

/// 拉取指定作者（或当前登录账号）的公开播放列表。
///
/// 仅返回 /api/user/playlist/ 的公开播放列表；自有账号的私有“收藏”由上层
/// “全部收藏”兜底入口补齐。
async fn fetch_tiktok_public_playlists(sec_uid: &str) -> anyhow::Result<Vec<YouTubeSearchResult>> {
    let ms_token = tiktok_cookie_values().get("msToken").cloned().unwrap_or_default();
    let cookie = tiktok_cookie_header()?;
    let device_id = tiktok_web_device_id().to_string();
    let odin_id = tiktok_web_device_id().to_string();
    let params: Vec<(&str, String)> = vec![
        ("aid", "1988".to_string()),
        ("app_language", "zh-Hans".to_string()),
        ("app_name", "tiktok_web".to_string()),
        ("browser_language", "zh-CN".to_string()),
        ("browser_name", "Mozilla".to_string()),
        ("browser_online", "true".to_string()),
        ("browser_platform", "Win32".to_string()),
        ("browser_version", TIKTOK_WEB_UA.to_string()),
        ("channel", "tiktok_web".to_string()),
        ("cookie_enabled", "true".to_string()),
        ("count", "30".to_string()),
        ("cursor", "0".to_string()),
        ("data_collection_enabled", "true".to_string()),
        ("device_id", device_id.to_string()),
        ("device_platform", "web_pc".to_string()),
        ("focus_state", "true".to_string()),
        ("from_page", "user".to_string()),
        ("history_len", "7".to_string()),
        ("is_fullscreen", "false".to_string()),
        ("is_page_visible", "true".to_string()),
        ("language", "zh-Hans".to_string()),
        ("odinId", odin_id.to_string()),
        ("os", "windows".to_string()),
        ("priority_region", "US".to_string()),
        ("referer", "https://www.tiktok.com/".to_string()),
        ("region", "US".to_string()),
        ("root_referer", "https://www.tiktok.com/".to_string()),
        ("screen_height", "1440".to_string()),
        ("screen_width", "2560".to_string()),
        ("secUid", sec_uid.to_string()),
        ("tz_name", "Asia/Shanghai".to_string()),
        ("user_is_login", "true".to_string()),
        ("verifyFp", tiktok_login_verify_fp().unwrap_or_default()),
        ("webcast_language", "zh-Hans".to_string()),
        ("msToken", ms_token.clone()),
        ("X-Bogus", "1".to_string()),
    ];
    let (status, body) =
        tiktok_signed_get_with_retry(TIKTOK_USER_PLAYLIST_API, &params, &cookie, "播放列表").await?;
    if status != 200 {
        bail!(
            "TikTok 播放列表返回 HTTP {}{}",
            status,
            tiktok_risk_status_hint(status)
        );
    }
    if body.trim().is_empty() {
        bail!(
            "TikTok 播放列表返回空响应（已自动重新签名一次仍失败）：可能登录态过期或触发了风控，请稍后重试或更换外源代理"
        );
    }
    let payload = decode_tiktok_body(&body)?;
    let mut results = Vec::new();
    if let Some(playlists) = payload.get("playlistList").and_then(serde_json::Value::as_array) {
        for playlist in playlists {
            let Some(id) = playlist.get("id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let title = playlist
                .get("name")
                .or_else(|| playlist.get("title"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(id)
                .to_string();
            let cover = playlist
                .pointer("/videoCover/url_list/0")
                .or_else(|| playlist.pointer("/cover/url_list/0"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let count = playlist
                .get("videoCount")
                .or_else(|| playlist.get("itemCount"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default();
            results.push(YouTubeSearchResult {
                result_type: "tiktok_playlist".to_string(),
                title,
                author: format!("{count} 个视频"),
                youtube_url: format!("https://www.tiktok.com/playlist/{id}"),
                channel_id: Some(id.to_string()),
                cover,
                description: String::new(),
                follower: None,
            });
        }
    }
    Ok(results)
}

async fn fetch_tiktok_playlists(sec_uid: &str, own: bool) -> anyhow::Result<ApiResponse<YouTubeSearchResponse>> {
    // 公开播放列表：仅对自有账号失败时降级（很多账号根本没有公开播放列表，
    // 接口被风控打回不影响“全部收藏”兜底）。
    let mut results = match fetch_tiktok_public_playlists(sec_uid).await {
        Ok(results) => results,
        Err(error) if own => {
            warn!(error = %error, "TikTok 公开播放列表获取失败，回退“全部收藏”");
            Vec::new()
        }
        Err(error) => return Err(error),
    };
    // 自己的账号：/api/user/playlist/ 只返回公开播放列表，多数账号的“收藏夹”
    // 其实是私有收藏（/api/user/collection_list/ 在服务端被风控隐藏列表，只回
    // total 不回明细）。这里用“全部收藏”（/api/user/collect/item_list/）兜底，
    // 保证“获取自己的列表”始终能拿到一个可添加的入口。
    if own {
        if let Some((total, cover)) = fetch_tiktok_collect_preview().await? {
            if total > 0 {
                results.push(YouTubeSearchResult {
                    result_type: "tiktok_playlist".to_string(),
                    title: "全部收藏".to_string(),
                    author: format!("{total} 个视频"),
                    youtube_url: "https://www.tiktok.com/collect/favorites".to_string(),
                    channel_id: Some("favorites".to_string()),
                    cover,
                    description: "当前账号收藏夹中的全部视频".to_string(),
                    follower: None,
                });
            }
        }
    }
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

/// 单页拉取“收藏”预览：返回 (视频总数, 第一张封面)，供收藏夹列表兜底入口展示。
/// 任何失败都返回 Ok(None)，由上层决定是否继续展示空的播放列表结果。
async fn fetch_tiktok_collect_preview() -> Result<Option<(i64, String)>> {
    let sec_uid = tiktok_login_sec_uid().await?;
    let (status, body) =
        tiktok_impersonated_get_with_retry(TIKTOK_USER_COLLECT_API, &sec_uid, 0, 30).await?;
    if status != 200 || body.trim().is_empty() {
        return Ok(None);
    }
    let payload = decode_tiktok_body(&body)?;
    let total = payload
        .get("total")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_else(|| {
            payload
                .get("itemList")
                .and_then(serde_json::Value::as_array)
                .map(|items| items.len() as i64)
                .unwrap_or(0)
        });
    let cover = payload
        .get("itemList")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| {
            item.pointer("/video/cover")
                .or_else(|| item.pointer("/video/originCover"))
                .or_else(|| item.pointer("/video/dynamicCover"))
        })
        .and_then(tiktok_cover_url)
        .unwrap_or_default()
        .to_string();
    Ok(Some((total, cover)))
}

/// 拉取当前登录账号“收藏”中的全部视频（官方 /api/user/collect/item_list/）。
///
/// 与“我的喜欢”（/api/favorite/item_list/）一致：极简参数 + cookies.txt 登录态
/// 即可服务端直连（实测无需浏览器签名），前提是出口 IP 未被 TikTok 风控。
async fn fetch_tiktok_collect_videos(limit: usize) -> Result<Vec<TikTokPost>> {
    let sec_uid = tiktok_login_sec_uid().await?;
    let mut cursor = 0i64;
    let mut posts = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..500 {
        let (status, body) =
            tiktok_impersonated_get_with_retry(TIKTOK_USER_COLLECT_API, &sec_uid, cursor, 30).await?;
        if status != 200 {
            bail!(
                "TikTok 收藏返回 HTTP {}{}",
                status,
                tiktok_risk_status_hint(status)
            );
        }
        if body.trim().is_empty() {
            bail!(
                "TikTok 收藏返回空响应：当前出口 IP 可能被 TikTok 风控。请在设置页配置外源代理（proxy/youtube_proxy）后重试"
            );
        }
        let payload = decode_tiktok_body(&body)?;
        let mut page_has_items = false;
        if let Some(items) = payload.get("itemList").and_then(serde_json::Value::as_array) {
            for item in items {
                if let Some(post) = parse_tiktok_item(item) {
                    page_has_items = true;
                    if seen.insert(post.id.clone()) {
                        posts.push(post);
                    }
                }
            }
        }
        if posts.len() >= limit
            || !payload.get("hasMore").and_then(serde_json::Value::as_bool).unwrap_or(false)
        {
            break;
        }
        if !page_has_items {
            if posts.is_empty() {
                bail!(
                    "TikTok 收藏接口未返回视频列表：请确认已导入最新 cookies.txt 且账号 secUid 正确；若出口 IP 被 TikTok 风控，请在设置页配置外源代理（proxy/youtube_proxy）后重试"
                );
            }
            break;
        }
        let next = payload
            .get("cursor")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse::<i64>().ok())
            .or_else(|| payload.get("cursor").and_then(serde_json::Value::as_i64))
            .unwrap_or(cursor);
        if next == cursor {
            break;
        }
        cursor = next;
        tokio::time::sleep(tiktok_page_delay().await).await;
    }
    Ok(posts)
}

/// 拉取一个播放列表（收藏夹）内的全部视频。
async fn fetch_tiktok_playlist_videos(playlist_id: &str, limit: usize) -> Result<Vec<TikTokPost>> {
    let cookie = tiktok_cookie_header()?;
    let ms_token = tiktok_cookie_values().get("msToken").cloned().unwrap_or_default();
    let mut cursor = 0i64;
    let mut posts = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..500 {
        let params: Vec<(&str, String)> = vec![
            ("aid", "1988".to_string()),
            ("app_language", "zh-Hans".to_string()),
            ("app_name", "tiktok_web".to_string()),
            ("browser_language", "zh-CN".to_string()),
            ("browser_name", "Mozilla".to_string()),
            ("browser_online", "true".to_string()),
            ("browser_platform", "Win32".to_string()),
            ("browser_version", TIKTOK_WEB_UA.to_string()),
            ("channel", "tiktok_web".to_string()),
            ("cookie_enabled", "true".to_string()),
            ("count", "30".to_string()),
            ("cursor", cursor.to_string()),
            ("device_platform", "web_pc".to_string()),
            ("language", "zh-Hans".to_string()),
            ("os", "windows".to_string()),
            ("playlistId", playlist_id.to_string()),
            ("region", "US".to_string()),
            ("screen_height", "1440".to_string()),
            ("screen_width", "2560".to_string()),
            ("user_is_login", "true".to_string()),
            ("msToken", ms_token.clone()),
            ("X-Bogus", "1".to_string()),
        ];
        let base = format!("{TIKTOK_PLAYLIST_API}{playlist_id}/");
        let (status, body) =
            tiktok_signed_get_with_retry(&base, &params, &cookie, "播放列表视频").await?;
        if status != 200 {
            bail!(
                "TikTok 播放列表视频返回 HTTP {}{}",
                status,
                tiktok_risk_status_hint(status)
            );
        }
        if body.trim().is_empty() {
            warn!(playlist_id, cursor, "TikTok 播放列表视频返回空响应（已自动重新签名一次仍失败），可能登录态过期或触发了风控");
            break;
        }
        let payload = decode_tiktok_body(&body)?;
        if let Some(items) = payload.get("itemList").and_then(serde_json::Value::as_array) {
            for item in items {
                if let Some(post) = parse_tiktok_item(item) {
                    if seen.insert(post.id.clone()) {
                        posts.push(post);
                    }
                }
            }
        }
        if posts.len() >= limit
            || !payload.get("hasMore").and_then(serde_json::Value::as_bool).unwrap_or(false)
        {
            break;
        }
        let next = payload
            .get("cursor")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse::<i64>().ok())
            .or_else(|| payload.get("cursor").and_then(serde_json::Value::as_i64))
            .unwrap_or(cursor);
        if next == cursor {
            break;
        }
        cursor = next;
        tokio::time::sleep(tiktok_page_delay().await).await;
    }
    Ok(posts)
}

/// 扫描“收藏夹”源：拉取播放列表视频并入库。
pub async fn scan_tiktok_collection_source(
    db: &DatabaseConnection,
    source: &youtube_source::Model,
) -> Result<u64> {
    // “全部收藏”兜底入口（get_tiktok_playlists 对自有账号返回的标记链接）：
    // 直接走 /api/user/collect/item_list/ 拉取当前账号收藏的全部视频。
    let posts = if source.url.to_ascii_lowercase().contains("/collect/favorites") {
        fetch_tiktok_collect_videos(usize::MAX).await?
    } else {
        let playlist_id = numeric_playlist_id(&source.url)
            .context("无法从 TikTok 播放列表链接识别播放列表 ID")?;
        fetch_tiktok_playlist_videos(playlist_id, usize::MAX).await?
    };
    persist_tiktok_posts(db, source, posts).await
}

fn numeric_playlist_id(value: &str) -> Option<&str> {
    value
        .split(['?', '#', '/'])
        .rev()
        .find(|part| !part.is_empty() && part.chars().all(|character| character.is_ascii_digit()))
}

// ---------- TikTok 作者头像获取（creator/item_list Web API） ----------

/// 进程内缓存的 TikTok Web API 设备 ID。
///
/// 实测 `api/creator/item_list/` 接受进程级随机 device_id（与 yt-dlp 行为一致），
/// 但每次调用都换新值会被软风控，因此进程生命周期内保持稳定。
static TIKTOK_WEB_DEVICE_ID: OnceLock<u64> = OnceLock::new();

fn tiktok_web_device_id() -> u64 {
    *TIKTOK_WEB_DEVICE_ID.get_or_init(|| rand::random::<u64>() % 9_000_000_000_000_000_000 + 1_000_000_000_000_000_000)
}

/// 通过 `api/creator/item_list/` 获取指定作者的公开头像地址。
///
/// 该接口是 yt-dlp 扫描 TikTok 用户作品所用的 Web API：带登录 Cookie + 随机
/// verifyFp + 进程稳定 device_id 即可在服务端直接访问（不要求浏览器签名）。
/// 头像为可选资源，任何失败都返回 `Ok(None)`，不影响主下载流程。
pub(crate) async fn fetch_tiktok_author_avatar_url(sec_uid: &str) -> anyhow::Result<Option<String>> {
    let cookie = tiktok_cookie_header()?;
    let device_id = tiktok_web_device_id();
    let verify_fp = format!(
        "verify_{}",
        (0..7).map(|_| format!("{:x}", rand::random::<u8>() % 16)).collect::<String>()
    );
    let cursor = chrono::Utc::now().timestamp_millis().to_string();
    let params: Vec<(&str, String)> = vec![
        ("aid", "1988".to_string()),
        ("app_language", "en".to_string()),
        ("app_name", "tiktok_web".to_string()),
        ("browser_language", "en-US".to_string()),
        ("browser_name", "Mozilla".to_string()),
        ("browser_online", "true".to_string()),
        ("browser_platform", "Win32".to_string()),
        ("browser_version", "5.0 (Windows)".to_string()),
        ("channel", "tiktok_web".to_string()),
        ("cookie_enabled", "true".to_string()),
        ("count", "5".to_string()),
        ("cursor", cursor),
        ("device_id", device_id.to_string()),
        ("device_platform", "web_pc".to_string()),
        ("focus_state", "true".to_string()),
        ("from_page", "user".to_string()),
        ("history_len", "2".to_string()),
        ("is_fullscreen", "false".to_string()),
        ("is_page_visible", "true".to_string()),
        ("language", "en".to_string()),
        ("os", "windows".to_string()),
        ("priority_region", "".to_string()),
        ("referer", "".to_string()),
        ("region", "US".to_string()),
        ("screen_height", "1080".to_string()),
        ("screen_width", "1920".to_string()),
        ("secUid", sec_uid.to_string()),
        ("type", "1".to_string()),
        ("tz_name", "UTC".to_string()),
        ("verifyFp", verify_fp),
        ("webcast_language", "en".to_string()),
    ];
    let url = reqwest::Url::parse_with_params("https://www.tiktok.com/api/creator/item_list/", &params)?;
    let (status, body) = tiktok_impersonated_get(url.as_str(), &cookie).await?;
    if status != 200 {
        return Ok(None);
    }
    let payload: serde_json::Value =
        serde_json::from_str(&body).with_context(|| "解析 TikTok 作者作品响应失败")?;
    let avatar = payload
        .get("itemList")
        .and_then(|items| items.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("author"))
        .and_then(|author| {
            author
                .get("avatarLarger")
                .or_else(|| author.get("avatarMedium"))
                .or_else(|| author.get("avatarThumb"))
        })
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty());
    Ok(avatar)
}

/// 从 TikTok 主页链接提取 `@` 句柄（用户名或 secUid）。
pub(crate) fn tiktok_handle_from_url(url: &str) -> Option<String> {
    url.trim()
        .split(['?', '#'])
        .next()
        .and_then(|path| path.rsplit('/').next())
        .map(|segment| segment.trim().trim_start_matches('@').to_string())
        .filter(|segment| !segment.is_empty())
}

