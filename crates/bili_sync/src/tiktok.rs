//! TikTok 作为独立视频源平台的接入。
//!
//! 与 YouTube 共用 yt-dlp 扫描/下载链路和统一外部媒体表，但搜索、登录状态、
//! URL 校验等平台逻辑独立维护，不混用 YouTube/抖音的 Cookie。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    append_ytdlp_runtime, append_youtube_proxy, command_error, create_youtube_source,
    ensure_ytdlp_available, get_platform_sources, normalize_source_type, parse_video_id_set,
    require_source_platform, reset_youtube_source_path, serialize_video_id_set, update_youtube_source,
    update_youtube_source_enabled, ytdlp_command, YouTubeLoginResponse, YouTubeSearchResponse,
    YouTubeSearchResult, YouTubeSourceResponse, YouTubeSourceVideosRequest,
};
use crate::api::response::{SubmissionVideoInfo, SubmissionVideosResponse};

const TIKWM_USER_SEARCH_API: &str = "https://www.tikwm.com/api/user/search";
const TIKTOK_WEB_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const TIKTOK_SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const TIKTOK_SEARCH_CONCURRENCY: usize = 4;

#[derive(Debug, Deserialize)]
pub struct TikTokSearchRequest {
    pub keyword: String,
}

#[derive(Debug, Deserialize)]
pub struct TikTokCookieImportRequest {
    pub cookies: String,
}

#[derive(Debug, Serialize)]
pub struct TikTokStatusResponse {
    pub logged_in: bool,
    pub cookie_path: String,
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

fn tiktok_user_to_search_result(user: &serde_json::Value) -> Option<YouTubeSearchResult> {
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
    let client = reqwest::Client::builder()
        .user_agent(TIKTOK_WEB_UA)
        .timeout(TIKTOK_SEARCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
    let url = format!("https://www.tiktok.com/@{unique_id}");
    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .send()
        .await
        .context("抓取 TikTok 用户主页失败")?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let html = response.text().await?;
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
    Ok(ApiResponse::ok(TikTokStatusResponse {
        logged_in: has_tiktok_session(&path),
        cookie_path: path.display().to_string(),
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
    Ok(ApiResponse::ok(YouTubeLoginResponse {
        logged_in: true,
        message: tiktok_import_message(),
    }))
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

fn parse_tiktok_item(item: &serde_json::Value) -> Option<TikTokPost> {
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
        .pointer("/video/cover/url_list/0")
        .or_else(|| item.pointer("/video/originCover/url_list/0"))
        .and_then(serde_json::Value::as_str)
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
    let cookie = tiktok_cookie_header()?;
    let client = reqwest::Client::builder()
        .user_agent(TIKTOK_WEB_UA)
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut cursor = 0i64;
    let mut posts = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..500 {
        let device_id: u64 =
            rand::random::<u64>() % 9000000000000000000 + 1000000000000000000;
        let odin_id: u64 =
            rand::random::<u64>() % 9000000000000000000 + 1000000000000000000;
        // 注意：X-Dynosaur 当前仍为占位（VMP 保护，待逆向后替换）。
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
            ("data_collection_enabled", "true".to_string()),
            ("device_id", device_id.to_string()),
            ("device_platform", "web_pc".to_string()),
            ("focus_state", "true".to_string()),
            ("from_page", "user".to_string()),
            ("history_len", "7".to_string()),
            ("is_fullscreen", "false".to_string()),
            ("is_page_visible", "true".to_string()),
            ("language", "zh-Hans".to_string()),
            ("needPinnedItemIds", "true".to_string()),
            ("odinId", odin_id.to_string()),
            ("os", "windows".to_string()),
            ("post_item_list_request_type", "0".to_string()),
            ("priority_region", "US".to_string()),
            ("referer", "https://www.tiktok.com/".to_string()),
            ("region", "US".to_string()),
            ("root_referer", "https://www.tiktok.com/".to_string()),
            ("screen_height", "1440".to_string()),
            ("screen_width", "2560".to_string()),
            ("secUid", String::new()),
            ("tz_name", "Asia/Shanghai".to_string()),
            ("user_is_login", "true".to_string()),
            ("verifyFp", tiktok_login_verify_fp().unwrap_or_default()),
            ("video_encoding", "dash".to_string()),
            ("webcast_language", "zh-Hans".to_string()),
            ("msToken", String::new()),
            ("X-Bogus", "1".to_string()),
            ("X-Dynosaur", format!("X-Dynosaur={}", rand::random::<u128>())),
        ];
        let url = build_tiktok_signed_url(TIKTOK_FAVORITE_API, &params)?;
        let response = client
            .get(url)
            .header(reqwest::header::COOKIE, &cookie)
            .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
            .send()
            .await
            .context("请求 TikTok 我的喜欢失败")?;
        if !response.status().is_success() {
            bail!("TikTok 我的喜欢返回 HTTP {}", response.status());
        }
        let body = response.text().await?;
        if body.trim().is_empty() {
            warn!(cursor, "TikTok 我的喜欢返回空响应，可能登录态过期或触发了风控");
            break;
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
                bail!("TikTok 我的喜欢接口未返回视频列表（可能需要浏览器实时签名或登录态过期），请确认已导入最新 cookies.txt");
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
        tokio::time::sleep(Duration::from_millis(600)).await;
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
    let cookie = tiktok_cookie_header()?;
    let odin_id = tiktok_login_odin_id().ok_or_else(|| {
        anyhow!("无法从 TikTok cookies.txt 解析账号 ID（缺少 multi_sids），请重新导入登录状态")
    })?;
    let client = reqwest::Client::builder()
        .user_agent(TIKTOK_WEB_UA)
        .timeout(Duration::from_secs(30))
        .build()?;
    let device_id: u64 = rand::random::<u64>() % 9000000000000000000 + 1000000000000000000;
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
        ("count", "50".to_string()),
        ("data_collection_enabled", "true".to_string()),
        ("device_id", device_id.to_string()),
        ("device_platform", "web_pc".to_string()),
        ("focus_state", "true".to_string()),
        ("from_page", "user".to_string()),
        ("history_len", "7".to_string()),
        ("isNonPersonalized", "false".to_string()),
        ("is_fullscreen", "false".to_string()),
        ("is_page_visible", "true".to_string()),
        ("maxCursor", "0".to_string()),
        ("minCursor", "0".to_string()),
        ("odinId", odin_id.clone()),
        ("os", "windows".to_string()),
        ("priority_region", "US".to_string()),
        ("referer", "https://www.tiktok.com/".to_string()),
        ("region", "US".to_string()),
        ("root_referer", "https://www.tiktok.com/".to_string()),
        ("scene", "151".to_string()),
        ("screen_height", "1440".to_string()),
        ("screen_width", "2560".to_string()),
        ("targetUserId", odin_id.clone()),
        ("tz_name", "Asia/Shanghai".to_string()),
        ("user_is_login", "true".to_string()),
        ("verifyFp", tiktok_login_verify_fp().unwrap_or_default()),
        ("webcast_language", "zh-Hans".to_string()),
        ("msToken", String::new()),
        ("X-Bogus", "1".to_string()),
        ("X-Dynosaur", format!("X-Dynosaur={}", rand::random::<u128>())),
    ];
    let url = build_tiktok_signed_url(TIKTOK_USER_LIST_API, &params)?;
    let response = client
        .get(url)
        .header(reqwest::header::COOKIE, &cookie)
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
        .context("请求 TikTok 关注列表失败")?;
    if !response.status().is_success() {
        bail!("TikTok 关注列表返回 HTTP {}", response.status());
    }
    let body = response.text().await?;
    if body.trim().is_empty() {
        bail!(
            "TikTok 关注列表接口需要浏览器实时签名（X-Dynosaur/X-Gnarly/msToken），服务端暂无法直接获取；请直接搜索作者，或稍后由浏览器扩展导出关注列表"
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
const TIKTOK_PLAYLIST_API: &str = "https://www.tiktok.com/api/playlist/";

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

/// 抓取 TikTok 用户主页 HTML 提取 secUid（收藏夹/播放列表接口的前置凭证）。
async fn resolve_tiktok_sec_uid(url: &str) -> Result<String> {
    let unique_id = tiktok_unique_id_from_url(url).ok_or_else(|| {
        anyhow!("无法从链接中解析 TikTok 用户名：{url}，请填写形如 https://www.tiktok.com/@用户名 的主页链接")
    })?;
    let cookie = tiktok_cookie_header()?;
    let client = reqwest::Client::builder()
        .user_agent(TIKTOK_WEB_UA)
        .timeout(Duration::from_secs(30))
        .build()?;
    let page_url = format!("https://www.tiktok.com/@{unique_id}");
    let response = client
        .get(&page_url)
        .header(reqwest::header::COOKIE, &cookie)
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .send()
        .await
        .context("请求 TikTok 用户主页失败")?;
    if !response.status().is_success() {
        bail!("TikTok 用户主页返回 HTTP {}", response.status());
    }
    let body = response.text().await?;
    let regex = TIKTOK_SEC_UID_RE.get_or_init(|| {
        Regex::new(r#""secUid":"([^"]+)"#).expect("invalid secUid regex")
    });
    let sec_uid = regex
        .captures(&body)
        .and_then(|captures| captures.get(1))
        .map(|matched| matched.as_str().to_string())
        .filter(|value| !value.is_empty());
    match sec_uid {
        Some(sec_uid) => Ok(sec_uid),
        None => bail!("从主页提取 secUid 失败：请确认链接有效且已导入 TikTok 登录状态"),
    }
}

/// 获取指定 TikTok 用户（默认使用登录用户）的播放列表（收藏夹）。
///
/// 收藏夹需要真实的 secUid 才能通过服务端校验，因此需要用户填写自己的
/// TikTok 主页链接，由后端抓取主页提取 secUid 后再请求官方播放列表接口。
pub async fn get_tiktok_playlists(
    Query(request): Query<TikTokPlaylistsRequest>,
) -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    let raw_url = request.url.as_deref().unwrap_or("").trim();
    if raw_url.is_empty() {
        return Err(ApiError::from(anyhow!(
            "请填写你的 TikTok 主页链接（如 https://www.tiktok.com/@用户名），用于读取收藏夹"
        )));
    }
    if !is_tiktok_url(raw_url) {
        return Err(ApiError::from(anyhow!("请输入有效的 TikTok 主页链接")));
    }
    let sec_uid = resolve_tiktok_sec_uid(raw_url).await.map_err(ApiError::from)?;
    let response = fetch_tiktok_playlists(&sec_uid).await.map_err(ApiError::from)?;
    Ok(response)
}

async fn fetch_tiktok_playlists(sec_uid: &str) -> anyhow::Result<ApiResponse<YouTubeSearchResponse>> {
    let cookie = tiktok_cookie_header()?;
    let client = reqwest::Client::builder()
        .user_agent(TIKTOK_WEB_UA)
        .timeout(Duration::from_secs(30))
        .build()?;
    let device_id: u64 = rand::random::<u64>() % 9000000000000000000 + 1000000000000000000;
    let odin_id: u64 = rand::random::<u64>() % 9000000000000000000 + 1000000000000000000;
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
        ("msToken", String::new()),
        ("X-Bogus", "1".to_string()),
        ("X-Dynosaur", format!("X-Dynosaur={}", rand::random::<u128>())),
    ];
    let url = build_tiktok_signed_url(TIKTOK_USER_PLAYLIST_API, &params)?;
    let response = client
        .get(url)
        .header(reqwest::header::COOKIE, &cookie)
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
        .context("请求 TikTok 播放列表失败")?;
    if !response.status().is_success() {
        bail!("TikTok 播放列表返回 HTTP {}", response.status());
    }
    let body = response.text().await?;
    if body.trim().is_empty() {
        bail!("TikTok 播放列表返回空响应，可能登录态过期或触发了风控");
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
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

/// 拉取一个播放列表（收藏夹）内的全部视频。
async fn fetch_tiktok_playlist_videos(playlist_id: &str, limit: usize) -> Result<Vec<TikTokPost>> {
    let cookie = tiktok_cookie_header()?;
    let client = reqwest::Client::builder()
        .user_agent(TIKTOK_WEB_UA)
        .timeout(Duration::from_secs(30))
        .build()?;
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
            ("msToken", String::new()),
            ("X-Bogus", "1".to_string()),
            ("X-Dynosaur", format!("X-Dynosaur={}", rand::random::<u128>())),
        ];
        let url = build_tiktok_signed_url(&format!("{TIKTOK_PLAYLIST_API}{playlist_id}/"), &params)?;
        let response = client
            .get(url)
            .header(reqwest::header::COOKIE, &cookie)
            .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
            .send()
            .await
            .context("请求 TikTok 播放列表视频失败")?;
        if !response.status().is_success() {
            bail!("TikTok 播放列表视频返回 HTTP {}", response.status());
        }
        let body = response.text().await?;
        if body.trim().is_empty() {
            warn!(playlist_id, cursor, "TikTok 播放列表视频返回空响应，可能登录态过期或触发了风控");
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
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    Ok(posts)
}

/// 扫描“收藏夹”源：拉取播放列表视频并入库。
pub async fn scan_tiktok_collection_source(
    db: &DatabaseConnection,
    source: &youtube_source::Model,
) -> Result<u64> {
    let playlist_id = numeric_playlist_id(&source.url)
        .context("无法从 TikTok 播放列表链接识别播放列表 ID")?;
    let posts = fetch_tiktok_playlist_videos(playlist_id, usize::MAX).await?;
    persist_tiktok_posts(db, source, posts).await
}

fn numeric_playlist_id(value: &str) -> Option<&str> {
    value
        .split(['?', '#', '/'])
        .rev()
        .find(|part| !part.is_empty() && part.chars().all(|character| character.is_ascii_digit()))
}
