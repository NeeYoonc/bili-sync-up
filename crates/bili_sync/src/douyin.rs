//! 抖音作者作品源。
//!
//! 作者枚举、作品详情、视频/图文媒体解析、弹幕和抖音侧 Cookie 均由本模块处理；
//! 实际文件传输仍复用项目的 `UnifiedDownloader`，不另建一套下载器。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::{Json, Query};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

use bili_sync_entity::youtube_source;

use crate::api::response::{SubmissionVideoInfo, SubmissionVideosResponse};
use crate::api::wrapper::{ApiError, ApiResponse};
use crate::bilibili::{DanmakuElem, DanmakuWriter, FilterOption, PageInfo as BiliPageInfo, VideoQuality};
use crate::config::CONFIG_DIR;
use crate::douyin_sign;
use crate::external_media::{ExternalMediaFormat, ExternalMediaMetadata};
use crate::unified_downloader::UnifiedDownloader;
use crate::youtube::{YouTubeLoginResponse, YouTubeSearchResponse, YouTubeSearchResult};

const DOUYIN_POST_API: &str = "https://www.douyin.com/aweme/v1/web/aweme/post/";
const DOUYIN_DETAIL_API: &str = "https://www.douyin.com/aweme/v1/web/aweme/detail/";
const DOUYIN_DANMAKU_API: &str = "https://www.douyin.com/aweme/v1/web/danmaku/get_v2/";
// `/search/<keyword>?type=user` 当前实际请求这个用户搜索接口。
// `general/search/single` 是“综合”标签接口，用 `aweme_user_web` 强行请求会被
// 抖音返回 `verify_check`，这也是此前所有作者关键词都搜索失败的根因。
const DOUYIN_USER_SEARCH_API: &str = "https://www.douyin.com/aweme/v1/web/discover/search/";
const DOUYIN_PROFILE_SELF_API: &str = "https://www.douyin.com/aweme/v1/web/user/profile/self/";
const DOUYIN_PROFILE_OTHER_API: &str = "https://www.douyin.com/aweme/v1/web/user/profile/other/";
const DOUYIN_FOLLOWING_API: &str = "https://www.douyin.com/aweme/v1/web/user/following/list/";
const DOUYIN_FAVORITE_API: &str = "https://www.douyin.com/aweme/v1/web/aweme/favorite/";
const DOUYIN_COLLECTIONS_API: &str = "https://www.douyin.com/aweme/v1/web/collects/list/";
const DOUYIN_COLLECTION_VIDEOS_API: &str = "https://www.douyin.com/aweme/v1/web/collects/video/list/";
const DOUYIN_WATCH_LATER_API: &str = "https://www.douyin.com/aweme/v1/web/watchlater/list/";
const DOUYIN_THEATER_FEED_API: &str = "https://www.douyin.com/aweme/v1/web/lvideo/theater/feed/";
const DOUYIN_THEATER_ITEMS_API: &str = "https://www.douyin.com/aweme/v1/web/lvideo/ent/aweme_list/";
const DOUYIN_SERIES_FEED_API: &str = "https://www.douyin.com/aweme/v1/web/series/card/feed/";
const DOUYIN_SERIES_ITEMS_API: &str = "https://www.douyin.com/aweme/v1/web/series/aweme/";
const DOUYIN_PUBLIC_SEARCH_API: &str = "https://www.sogou.com/web";
const DOUYIN_MSTOKEN_API: &str = "https://mssdk.bytedance.com/web/r/token?ms_appid=6383&msToken=T4bNG9W2rKF7hBNwaYssDErnJEobDAk641DFaOn4hcsfAM8slpbZeKPM4Ml4rhDQq18iY8nQ0JR3J87SLZtDiDqtZdZawfBjCWAgtolQsoEtG6MLETvo4fwr7F28zGJUFDdJgKEZHibNR0QshVBv28ygsQsJDzerKAtsgj9Pn5WsxyS1vfkiX3I%3D";
const DOUYIN_MSTOKEN_STR_DATA: &str = include_str!("douyin_mstoken_strdata.txt");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Deserialize)]
pub struct DouyinCookieImportRequest {
    pub cookies: String,
    #[serde(default)]
    pub webid: Option<String>,
    #[serde(default)]
    pub verify_fp: Option<String>,
    #[serde(default)]
    pub ms_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DouyinStatusResponse {
    pub logged_in: bool,
    pub cookie_path: String,
}

#[derive(Debug, Deserialize)]
pub struct DouyinSearchRequest {
    pub keyword: String,
}

#[derive(Debug, Deserialize)]
pub struct DouyinSourceVideosRequest {
    pub url: String,
    #[serde(default = "default_douyin_source_type")]
    pub source_type: String,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub keyword: Option<String>,
}

fn default_douyin_source_type() -> String {
    "douyin".to_string()
}

#[derive(Debug, Deserialize)]
pub struct DouyinCatalogRequest {
    pub source_type: String,
    pub keyword: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DouyinPost {
    pub id: String,
    pub url: String,
    pub title: String,
    pub uploader: String,
    pub thumbnail: Option<String>,
    pub published_at: Option<String>,
    pub timestamp: Option<i64>,
    pub duration_seconds: Option<i32>,
    pub digg_count: i64,
    pub is_image_post: bool,
}

#[derive(Debug, Clone)]
pub struct DouyinProfile {
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct DouyinDanmaku {
    pub danmaku_id: String,
    pub user_id: String,
    pub offset_time: i32,
    pub text: String,
    #[serde(default)]
    pub digg_count: i64,
}

struct PostPage {
    posts: Vec<DouyinPost>,
    profile: Option<DouyinProfile>,
    has_more: bool,
    cursor: i64,
}

pub async fn douyin_status() -> Result<ApiResponse<DouyinStatusResponse>, ApiError> {
    let path = cookie_path();
    Ok(ApiResponse::ok(DouyinStatusResponse {
        logged_in: has_douyin_session(&path),
        cookie_path: path.display().to_string(),
    }))
}

pub async fn import_douyin_cookie_file(
    Json(request): Json<DouyinCookieImportRequest>,
) -> Result<ApiResponse<YouTubeLoginResponse>, ApiError> {
    if !is_netscape_douyin_cookie_file(&request.cookies) {
        return Err(ApiError::bad_request(
            "文件不是包含 douyin.com 会话的 Netscape cookies.txt；请在电脑浏览器打开抖音后导出 cookies.txt",
        ));
    }
    let path = cookie_path();
    let parent = path.parent().context("无效的抖音 Cookie 文件路径")?;
    tokio::fs::create_dir_all(parent).await?;
    let temporary = path.with_extension("txt.importing");
    tokio::fs::write(&temporary, request.cookies.as_bytes())
        .await
        .context("写入抖音 cookies.txt 失败")?;
    validate_cookie_file(&temporary).await?;
    replace_cookie_file(&temporary, &path).await?;
    let mut imported_device_fields = 0usize;
    if let Some(webid) = request
        .webid
        .as_deref()
        .map(str::trim)
        .filter(|value| valid_webid(value))
    {
        persist_session_value(&CONFIG_DIR.join("douyin-webid.txt"), webid).await?;
        imported_device_fields += 1;
    }
    if let Some(verify_fp) = request
        .verify_fp
        .as_deref()
        .map(str::trim)
        .filter(|value| valid_verify_fp(value))
    {
        persist_session_value(&CONFIG_DIR.join("douyin-verify-fp.txt"), verify_fp).await?;
        imported_device_fields += 1;
    }
    if let Some(ms_token) = request
        .ms_token
        .as_deref()
        .map(str::trim)
        .filter(|value| valid_ms_token(value))
    {
        persist_session_value(&ms_token_path(), ms_token).await?;
        imported_device_fields += 1;
    }
    Ok(ApiResponse::ok(YouTubeLoginResponse {
        logged_in: true,
        message: format!(
            "已导入抖音 cookies.txt{}；作者作品扫描和媒体解析将使用此状态",
            if imported_device_fields > 0 {
                format!("及 {imported_device_fields} 项浏览器设备参数")
            } else {
                String::new()
            }
        ),
    }))
}

pub async fn search_douyin(
    Query(request): Query<DouyinSearchRequest>,
) -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    ensure_session()?;
    let keyword = request.keyword.trim();
    if keyword.is_empty() {
        return Err(ApiError::bad_request("请输入抖音作者关键词"));
    }
    let mut response = fetch_douyin_user_search(keyword, false).await?;
    if douyin_search_was_blocked(&response) && !imported_cookie_has_ms_token() {
        // mssdk token 有时会提前失效。仅对程序补出的 token 刷新并重试一次；
        // 浏览器导出的 msToken 则始终以用户真实会话为准。
        response = fetch_douyin_user_search(keyword, true).await?;
    }
    let search_was_blocked = douyin_search_was_blocked(&response);
    let mut results = Vec::new();
    let mut users = Vec::new();
    collect_user_infos(&response, &mut users);
    let mut seen = HashSet::new();
    for user in users {
        if let Some(result) = user_to_search_result(user) {
            let Some(sec_uid) = result.channel_id.as_ref() else {
                continue;
            };
            if seen.insert(sec_uid.clone()) {
                results.push(result);
            }
        }
    }

    // 抖音的用户搜索接口经常对服务端 Cookie 重放返回 verify_check / hit_shark，
    // 即使同一 Cookie 的作者作品、本人资料和关注列表都正常。这里仅把公共搜索
    // 当作 sec_uid 发现入口，再逐个调用抖音官方 profile/other 接口校验和补全资料，
    // 避免把搜索引擎摘要当成最终作者数据。
    if results.is_empty() {
        match search_public_douyin_profiles(keyword).await {
            Ok(fallback) => results = fallback,
            Err(error) if search_was_blocked => {
                return Err(anyhow!("抖音搜索触发安全验证，备用作者检索也失败：{error}").into());
            }
            Err(error) => warn!(error = %error, keyword, "抖音备用作者搜索失败"),
        }
    }
    if results.is_empty() && search_was_blocked {
        return Err(anyhow!("抖音搜索触发安全验证，且没有找到可由抖音官方资料接口确认的作者").into());
    }
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

async fn fetch_douyin_user_search(keyword: &str, force_ms_token_refresh: bool) -> Result<serde_json::Value> {
    ensure_search_ms_token(force_ms_token_refresh).await?;
    let cookies = cookie_values();
    let uifid = cookies
        .get("UIFID")
        .or_else(|| cookies.get("UIFID_TEMP"))
        .cloned()
        .context("抖音 Cookie 缺少 UIFID，请重新导出并导入完整 cookies.txt")?;
    let verify_fp = stable_verify_fp().await?;
    // 这里严格使用浏览器 HAR 中 type=user 首屏请求的参数、顺序和网页版本。
    // 搜索接口比作品/资料接口校验更严格，不能复用后者的 29.1.0 参数集合。
    let mut pairs = vec![
        ("device_platform", "webapp".to_string()),
        ("aid", "6383".to_string()),
        ("channel", "channel_pc_web".to_string()),
        ("search_channel", "aweme_user_web".to_string()),
        ("keyword", keyword.to_string()),
        ("search_source", "normal_search".to_string()),
        ("query_correct_type", "1".to_string()),
        ("is_filter_search", "0".to_string()),
        ("from_group_id", String::new()),
        ("disable_rs", "0".to_string()),
        ("offset", "0".to_string()),
        ("count", "10".to_string()),
        ("need_filter_settings", "1".to_string()),
        ("list_type", "single".to_string()),
        ("pc_search_top_1_params", r#"{"enable_ai_search_top_1":1}"#.to_string()),
        ("update_version_code", "170400".to_string()),
        ("pc_client_type", "1".to_string()),
        ("pc_libra_divert", "Windows".to_string()),
        ("support_h265", "1".to_string()),
        ("support_dash", "1".to_string()),
        ("cpu_core_num", "16".to_string()),
        ("version_code", "170400".to_string()),
        ("version_name", "17.4.0".to_string()),
        ("cookie_enabled", "true".to_string()),
        ("screen_width", "2560".to_string()),
        ("screen_height", "1440".to_string()),
        ("browser_language", "zh-CN".to_string()),
        ("browser_platform", "Win32".to_string()),
        ("browser_name", "Chrome".to_string()),
        ("browser_version", "149.0.0.0".to_string()),
        ("browser_online", "true".to_string()),
        ("engine_name", "Blink".to_string()),
        ("engine_version", "149.0.0.0".to_string()),
        ("os_name", "Windows".to_string()),
        ("os_version", "10".to_string()),
        ("device_memory", "32".to_string()),
        ("platform", "PC".to_string()),
        ("downlink", "10".to_string()),
        ("effective_type", "4g".to_string()),
        ("round_trip_time", "0".to_string()),
        ("webid", stable_webid().await?),
        ("uifid", uifid.clone()),
        ("verifyFp", verify_fp.clone()),
        ("fp", verify_fp),
    ];
    if let Some(ms_token) = cookies.get("msToken").cloned() {
        pairs.push(("msToken", ms_token));
    }

    let mut referer = reqwest::Url::parse("https://www.douyin.com/jingxuan/search/")?;
    referer
        .path_segments_mut()
        .map_err(|_| anyhow!("无法构造抖音搜索来源页面"))?
        .pop_if_empty()
        .push(keyword);
    referer
        .query_pairs_mut()
        .append_pair("aid", &Uuid::new_v4().to_string())
        .append_pair("type", "user");
    // 当前搜索接口在具有真实 msToken、webid、verifyFp 和浏览器 TLS 指纹时，
    // 首屏请求不要求 a_bogus。反而附加旧版签名会直接触发 verify_check。
    // 作品、资料等既有接口仍继续走项目原来的 signed_get，互不影响。
    browser_get_with_referer(DOUYIN_USER_SEARCH_API, pairs, referer.as_str()).await
}

pub async fn get_douyin_followings() -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    ensure_session()?;
    let self_response = signed_get(DOUYIN_PROFILE_SELF_API, common_query_pairs()).await?;
    ensure_douyin_status_ok(&self_response, "获取当前抖音账号")?;
    let user = self_response
        .get("user")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow!("抖音返回成功但缺少当前账号资料，请重新导入 Cookie"))?;
    let user = serde_json::Value::Object(user.clone());
    let uid = text(&user, &["uid"]).ok_or_else(|| anyhow!("当前抖音账号资料缺少 uid"))?;
    let sec_uid = text(&user, &["sec_uid", "sec_user_id"]).ok_or_else(|| anyhow!("当前抖音账号资料缺少 sec_uid"))?;

    let mut results = Vec::new();
    let mut seen = HashSet::new();
    let mut offset = 0i64;
    let mut min_time = 0i64;
    let mut max_time = 0i64;
    for page in 0..250 {
        let mut pairs = common_query_pairs();
        pairs.extend([
            ("user_id", uid.clone()),
            ("sec_user_id", sec_uid.clone()),
            ("offset", offset.to_string()),
            ("min_time", min_time.to_string()),
            ("max_time", max_time.to_string()),
            ("count", "20".to_string()),
            ("source_type", "4".to_string()),
            ("gps_access", "0".to_string()),
            ("address_book_access", "0".to_string()),
            ("is_top", "1".to_string()),
        ]);
        let response = signed_get(DOUYIN_FOLLOWING_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取已关注抖音作者")?;
        let followings = ["followings", "follow_list", "user_list"]
            .iter()
            .find_map(|key| response.get(*key).and_then(serde_json::Value::as_array));
        let Some(followings) = followings else {
            if page == 0 {
                return Err(anyhow!("抖音关注列表响应缺少作者数据，请重新导入 Cookie").into());
            }
            break;
        };
        for following in followings {
            if let Some(result) = user_to_search_result(following) {
                let Some(following_sec_uid) = result.channel_id.as_ref() else {
                    continue;
                };
                if seen.insert(following_sec_uid.clone()) {
                    results.push(result);
                }
            }
        }

        let has_more = response.get("has_more").and_then(value_as_bool).unwrap_or(false);
        if !has_more {
            break;
        }
        let next_offset = response.get("offset").and_then(value_as_i64).unwrap_or(offset);
        let next_min_time = response.get("min_time").and_then(value_as_i64).unwrap_or(min_time);
        let next_max_time = response.get("max_time").and_then(value_as_i64).unwrap_or(max_time);
        if next_offset == offset && next_min_time == min_time && next_max_time == max_time {
            warn!(page, "抖音关注列表游标没有前进，停止继续分页");
            break;
        }
        offset = next_offset;
        min_time = next_min_time;
        max_time = next_max_time;
    }
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

/// 返回需要在添加源页面右侧选择的抖音列表，交互方式与 B 站收藏夹/合集一致。
pub async fn get_douyin_catalog(
    Query(request): Query<DouyinCatalogRequest>,
) -> Result<ApiResponse<YouTubeSearchResponse>, ApiError> {
    ensure_session()?;
    let source_type = request.source_type.trim().to_ascii_lowercase();
    let keyword = request
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let direct_id = keyword.filter(|value| value.chars().all(|character| character.is_ascii_digit()));
    let mut results: Vec<YouTubeSearchResult> = match (source_type.as_str(), direct_id) {
        ("douyin_theater", Some(id)) => fetch_theater_catalog_item(id).await?.into_iter().collect(),
        ("douyin_series", Some(id)) => fetch_series_catalog_item(id).await?.into_iter().collect(),
        ("douyin_collection", _) => fetch_collection_catalog().await?,
        ("douyin_theater", _) => fetch_theater_catalog().await?,
        ("douyin_series", _) => fetch_series_catalog().await?,
        _ => return Err(ApiError::bad_request("右侧列表仅支持收藏夹、放映厅或短剧")),
    };
    if let Some(keyword) = keyword {
        let keyword = keyword.to_lowercase();
        results.retain(|item| {
            item.title.to_lowercase().contains(&keyword)
                || item.author.to_lowercase().contains(&keyword)
                || item.description.to_lowercase().contains(&keyword)
                || item
                    .channel_id
                    .as_deref()
                    .is_some_and(|id| id.to_lowercase().contains(&keyword))
                || item.youtube_url.to_lowercase().contains(&keyword)
        });
    }
    let total = results.len();
    Ok(ApiResponse::ok(YouTubeSearchResponse {
        success: true,
        results,
        total,
    }))
}

fn theater_to_search_result(item: &serde_json::Value) -> Option<YouTubeSearchResult> {
    let album = item.pointer("/lvideo_brief/album_info")?;
    let id = text(album, &["album_id"])?;
    Some(YouTubeSearchResult {
        result_type: "douyin_theater".to_string(),
        title: text(album, &["title"]).unwrap_or_else(|| format!("放映厅 {id}")),
        author: text(item.get("author").unwrap_or(&serde_json::Value::Null), &["nickname"]).unwrap_or_default(),
        youtube_url: format!("https://www.douyin.com/lvdetail/{id}"),
        channel_id: Some(id),
        cover: image_url(album.get("cover")).unwrap_or_default(),
        description: text(album, &["category_str_topic", "region"]).unwrap_or_default(),
        follower: None,
    })
}

fn series_to_search_result(series: &serde_json::Value) -> Option<YouTubeSearchResult> {
    let id = text(series, &["series_id"])?;
    let stats = series.get("stats").unwrap_or(&serde_json::Value::Null);
    let episodes = integer(stats, &["total_episode", "updated_to_episode"]).unwrap_or_default();
    Some(YouTubeSearchResult {
        result_type: "douyin_series".to_string(),
        title: text(series, &["series_name"]).unwrap_or_else(|| format!("短剧 {id}")),
        author: series
            .get("author")
            .and_then(|author| text(author, &["nickname"]))
            .unwrap_or_default(),
        youtube_url: format!("https://www.douyin.com/series/{id}"),
        channel_id: Some(id),
        cover: image_url(series.get("cover_url")).unwrap_or_default(),
        description: text(series, &["desc"]).unwrap_or_else(|| format!("共 {episodes} 集")),
        follower: Some(episodes),
    })
}

async fn fetch_theater_catalog_item(id: &str) -> Result<Option<YouTubeSearchResult>> {
    let mut pairs = common_query_pairs();
    pairs.push(("album_ids", id.to_string()));
    let response = signed_get(DOUYIN_THEATER_ITEMS_API, pairs).await?;
    Ok(response
        .get("aweme_list")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.iter().find_map(theater_to_search_result)))
}

async fn fetch_series_catalog_item(id: &str) -> Result<Option<YouTubeSearchResult>> {
    let mut pairs = common_query_pairs();
    pairs.extend([
        ("offset", "0".to_string()),
        ("count", "10".to_string()),
        ("content_type", "0".to_string()),
        ("insert_series_id_list", id.to_string()),
    ]);
    let response = signed_get(DOUYIN_SERIES_FEED_API, pairs).await?;
    ensure_douyin_status_ok(&response, "获取抖音短剧详情")?;
    Ok(response
        .get("card_list")
        .and_then(serde_json::Value::as_array)
        .and_then(|cards| {
            cards.iter().find_map(|card| {
                let series = card.get("series")?;
                (text(series, &["series_id"]).as_deref() == Some(id))
                    .then(|| series_to_search_result(series))
                    .flatten()
            })
        }))
}

async fn fetch_collection_catalog() -> Result<Vec<YouTubeSearchResult>> {
    let mut cursor = 0i64;
    let mut results = Vec::new();
    for _ in 0..100 {
        let mut pairs = common_query_pairs();
        pairs.extend([("cursor", cursor.to_string()), ("count", "12".to_string())]);
        let response = signed_get(DOUYIN_COLLECTIONS_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取抖音收藏夹")?;
        for item in response
            .get("collects_list")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = text(item, &["collects_id_str", "collects_id"]) else {
                continue;
            };
            let title = text(item, &["collects_name"]).unwrap_or_else(|| format!("收藏夹 {id}"));
            let count = integer(item, &["total_number"]).unwrap_or_default();
            results.push(YouTubeSearchResult {
                result_type: "douyin_collection".to_string(),
                title,
                author: item
                    .get("user_info")
                    .and_then(|user| text(user, &["nickname"]))
                    .unwrap_or_default(),
                youtube_url: format!("https://www.douyin.com/collection/{id}"),
                channel_id: Some(id),
                cover: image_url(item.get("collects_cover")).unwrap_or_default(),
                description: format!("共 {count} 个作品"),
                follower: Some(count),
            });
        }
        if !response.get("has_more").and_then(value_as_bool).unwrap_or(false) {
            break;
        }
        let next = response.get("cursor").and_then(value_as_i64).unwrap_or(cursor);
        if next == cursor {
            break;
        }
        cursor = next;
    }
    Ok(results)
}

async fn fetch_theater_catalog() -> Result<Vec<YouTubeSearchResult>> {
    let mut cursor = 0i64;
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..30 {
        let mut pairs = common_query_pairs();
        pairs.extend([
            ("count", "12".to_string()),
            ("custom_album_type", "0".to_string()),
            ("cursor", cursor.to_string()),
        ]);
        let response = signed_get(DOUYIN_THEATER_FEED_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取抖音放映厅")?;
        for item in response
            .get("aweme_list")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(result) = theater_to_search_result(item) else {
                continue;
            };
            let id = result.channel_id.clone().unwrap_or_default();
            if !seen.insert(id) {
                continue;
            }
            results.push(result);
        }
        if !response.get("has_more").and_then(value_as_bool).unwrap_or(false) {
            break;
        }
        let next = response.get("next_cursor").and_then(value_as_i64).unwrap_or(cursor);
        if next == cursor {
            break;
        }
        cursor = next;
    }
    Ok(results)
}

async fn fetch_series_catalog() -> Result<Vec<YouTubeSearchResult>> {
    let mut offset = 0i64;
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..30 {
        let mut pairs = common_query_pairs();
        pairs.extend([
            ("offset", offset.to_string()),
            ("count", "10".to_string()),
            ("content_type", "0".to_string()),
            ("insert_series_id_list", String::new()),
        ]);
        let response = signed_get(DOUYIN_SERIES_FEED_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取抖音短剧")?;
        for card in response
            .get("card_list")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(result) = card.get("series").and_then(series_to_search_result) else {
                continue;
            };
            let id = result.channel_id.clone().unwrap_or_default();
            if !seen.insert(id) {
                continue;
            }
            results.push(result);
        }
        if !response.get("has_more").and_then(value_as_bool).unwrap_or(false) {
            break;
        }
        let next = response.get("offset").and_then(value_as_i64).unwrap_or(offset);
        if next == offset {
            break;
        }
        offset = next;
    }
    Ok(results)
}

fn user_to_search_result(user: &serde_json::Value) -> Option<YouTubeSearchResult> {
    let sec_uid = text(user, &["sec_uid", "sec_user_id"])?;
    let nickname = text(user, &["nickname", "unique_id"]).unwrap_or_else(|| sec_uid.clone());
    Some(YouTubeSearchResult {
        result_type: "douyin_user".to_string(),
        title: nickname,
        author: text(user, &["unique_id", "short_id"]).unwrap_or_default(),
        youtube_url: format!("https://www.douyin.com/user/{sec_uid}"),
        channel_id: Some(sec_uid),
        cover: image_url(user.get("avatar_larger").or_else(|| user.get("avatar_thumb"))).unwrap_or_default(),
        description: text(user, &["signature"]).unwrap_or_default(),
        follower: integer(user, &["follower_count", "mplatform_followers_count"]),
    })
}

fn douyin_search_was_blocked(response: &serde_json::Value) -> bool {
    response
        .get("search_nil_info")
        .and_then(|value| value.get("search_nil_type").or_else(|| value.get("search_nil_item")))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| matches!(value, "verify_check" | "antispam_check" | "hit_shark"))
}

async fn search_public_douyin_profiles(keyword: &str) -> Result<Vec<YouTubeSearchResult>> {
    let query = format!("{keyword} 抖音");
    let url = reqwest::Url::parse_with_params(DOUYIN_PUBLIC_SEARCH_API, &[("query", query)])?;
    let response = reqwest::Client::builder()
        .user_agent(douyin_sign::user_agent())
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?
        .get(url)
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9")
        .send()
        .await
        .context("请求抖音作者备用搜索失败")?;
    if !response.status().is_success() {
        bail!("抖音作者备用搜索返回 HTTP {}", response.status());
    }
    let body = response.text().await?;
    // 搜索引擎不一定直接收录作者主页，通常更容易命中该作者发布的视频或图文。
    // 两类链接都收集：主页通过 profile/other 校验，作品通过 aweme/detail 反查
    // author。这样官方搜索接口临时触发 verify_check 时仍能得到经过抖音官方
    // 接口确认的作者，而不是把搜索引擎摘要直接返回给前端。
    let user_regex = Regex::new(r#"https?://www\.douyin\.com/user/(MS4w[A-Za-z0-9_-]+)"#)?;
    let aweme_regex = Regex::new(r#"https?://www\.douyin\.com/(?:video|note)/(\d+)"#)?;
    let mut sec_uids = Vec::new();
    let mut aweme_ids = Vec::new();
    let mut seen_candidates = HashSet::new();
    let mut seen_aweme_ids = HashSet::new();
    for captures in user_regex.captures_iter(&body) {
        let Some(sec_uid) = captures.get(1).map(|value| value.as_str().to_string()) else {
            continue;
        };
        if seen_candidates.insert(sec_uid.clone()) {
            sec_uids.push(sec_uid);
        }
        if sec_uids.len() >= 20 {
            break;
        }
    }
    for captures in aweme_regex.captures_iter(&body) {
        let Some(aweme_id) = captures.get(1).map(|value| value.as_str().to_string()) else {
            continue;
        };
        if seen_aweme_ids.insert(aweme_id.clone()) {
            aweme_ids.push(aweme_id);
        }
        if aweme_ids.len() >= 20 {
            break;
        }
    }

    let mut results = Vec::new();
    let mut seen_results = HashSet::new();
    for sec_uid in sec_uids {
        let mut pairs = common_query_pairs();
        pairs.push(("sec_user_id", sec_uid));
        let profile = match signed_get(DOUYIN_PROFILE_OTHER_API, pairs).await {
            Ok(profile) => profile,
            Err(error) => {
                warn!(error = %error, "校验备用抖音作者资料失败");
                continue;
            }
        };
        if ensure_douyin_status_ok(&profile, "校验抖音作者资料").is_err() {
            continue;
        }
        let Some(user) = profile.get("user") else {
            continue;
        };
        let Some(result) = user_to_search_result(user) else {
            continue;
        };
        if search_result_matches(&result, keyword) && seen_results.insert(result.channel_id.clone().unwrap_or_default())
        {
            results.push(result);
        }
    }
    for aweme_id in aweme_ids {
        let mut pairs = common_query_pairs();
        pairs.push(("aweme_id", aweme_id));
        let detail = match signed_get(DOUYIN_DETAIL_API, pairs).await {
            Ok(detail) => detail,
            Err(error) => {
                warn!(error = %error, "通过搜索作品反查抖音作者失败");
                continue;
            }
        };
        if ensure_douyin_status_ok(&detail, "校验抖音搜索作品").is_err() {
            continue;
        }
        let Some(author) = detail.pointer("/aweme_detail/author") else {
            continue;
        };
        let Some(result) = user_to_search_result(author) else {
            continue;
        };
        let Some(sec_uid) = result.channel_id.as_ref() else {
            continue;
        };
        if !search_result_matches(&result, keyword) || !seen_results.insert(sec_uid.clone()) {
            continue;
        }

        // 作品详情里的作者字段足以识别账号，但粉丝数等资料有时被裁剪；再用
        // profile/other 补全一次，失败时仍保留已由详情接口确认的结果。
        let mut profile_pairs = common_query_pairs();
        profile_pairs.push(("sec_user_id", sec_uid.clone()));
        let result = match signed_get(DOUYIN_PROFILE_OTHER_API, profile_pairs).await {
            Ok(profile) if ensure_douyin_status_ok(&profile, "补全抖音作者资料").is_ok() => {
                profile.get("user").and_then(user_to_search_result).unwrap_or(result)
            }
            Ok(_) => result,
            Err(error) => {
                warn!(error = %error, "补全搜索到的抖音作者资料失败");
                result
            }
        };
        results.push(result);
    }
    results.sort_by(|left, right| {
        search_result_score(right, keyword)
            .cmp(&search_result_score(left, keyword))
            .then_with(|| {
                right
                    .follower
                    .unwrap_or_default()
                    .cmp(&left.follower.unwrap_or_default())
            })
    });
    Ok(results)
}

fn normalized_search_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn search_result_score(result: &YouTubeSearchResult, keyword: &str) -> u8 {
    let keyword = normalized_search_text(keyword);
    let title = normalized_search_text(&result.title);
    let author = normalized_search_text(&result.author);
    if author == keyword && !author.is_empty() {
        5
    } else if title == keyword {
        4
    } else if title.starts_with(&keyword) {
        3
    } else if title.contains(&keyword) || author.contains(&keyword) {
        2
    } else if normalized_search_text(&result.description).contains(&keyword) {
        1
    } else {
        0
    }
}

fn search_result_matches(result: &YouTubeSearchResult, keyword: &str) -> bool {
    !normalized_search_text(keyword).is_empty() && search_result_score(result, keyword) > 0
}

fn ensure_douyin_status_ok(response: &serde_json::Value, action: &str) -> Result<()> {
    match response.get("status_code").and_then(value_as_i64) {
        Some(0) => Ok(()),
        Some(8) => bail!("{action}失败：抖音 Cookie 已失效，请在设置页重新导入"),
        Some(code) => bail!("{action}失败：抖音返回状态码 {code}"),
        None => bail!("{action}失败：抖音响应缺少状态码"),
    }
}

fn value_as_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn value_as_bool(value: &serde_json::Value) -> Option<bool> {
    value.as_bool().or_else(|| value_as_i64(value).map(|value| value != 0))
}

fn collect_user_infos<'a>(value: &'a serde_json::Value, users: &mut Vec<&'a serde_json::Value>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_user_infos(item, users);
            }
        }
        serde_json::Value::Object(object) => {
            if object.contains_key("sec_uid") || object.contains_key("sec_user_id") {
                users.push(value);
            }
            for (key, child) in object {
                // 用户搜索响应在不同 Web 版本中会包装成
                // data[].user_list[].user_info，也可能直接返回 user_info。
                if key == "user_info" && child.is_object() {
                    users.push(child);
                }
                collect_user_infos(child, users);
            }
        }
        _ => {}
    }
}

pub async fn get_douyin_source_videos(
    Query(request): Query<DouyinSourceVideosRequest>,
) -> Result<ApiResponse<SubmissionVideosResponse>, ApiError> {
    let page = request.page.unwrap_or(1).max(1);
    let page_size = request.page_size.unwrap_or(100).clamp(1, 200);
    let target_end = page.saturating_mul(page_size) as usize;
    let posts = fetch_source_posts(&request.source_type, &request.url, target_end.saturating_add(1)).await?;
    let keyword = request
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let start = ((page - 1) * page_size) as usize;
    let mut selected = posts
        .into_iter()
        .filter(|post| {
            keyword
                .as_ref()
                .is_none_or(|keyword| post.title.to_lowercase().contains(keyword))
        })
        .skip(start)
        .take(page_size as usize + 1)
        .collect::<Vec<_>>();
    let has_more = selected.len() > page_size as usize;
    selected.truncate(page_size as usize);
    let videos = selected
        .into_iter()
        .map(|post| SubmissionVideoInfo {
            bvid: post.id,
            title: post.title,
            author: (!post.uploader.trim().is_empty()).then_some(post.uploader),
            cover: post.thumbnail.unwrap_or_default(),
            pubtime: post.published_at.unwrap_or_default(),
            duration: post.duration_seconds.unwrap_or_default(),
            // 抖音 Web 接口对历史作品的 play_count 经常统一返回 0，
            // digg_count 才是稳定、真实的公开统计，因此这里按抖音语义展示点赞数。
            view: i32::try_from(post.digg_count).unwrap_or(i32::MAX),
            danmaku: 0,
            description: String::new(),
        })
        .collect::<Vec<_>>();
    let total = start as i64 + videos.len() as i64 + i64::from(has_more);
    Ok(ApiResponse::ok(SubmissionVideosResponse {
        videos,
        total,
        page,
        page_size,
    }))
}

pub async fn fetch_source_posts(source_type: &str, source_url: &str, limit: usize) -> Result<Vec<DouyinPost>> {
    match source_type.trim().to_ascii_lowercase().as_str() {
        "douyin" => {
            let sec_uid = resolve_sec_user_id(source_url).await?;
            fetch_posts_until(&sec_uid, limit).await
        }
        "douyin_liked" => fetch_liked_posts(limit).await,
        "douyin_collection" => {
            let id = numeric_id(source_url).context("无法从抖音收藏夹链接识别收藏夹 ID")?;
            fetch_collection_posts(id, limit).await
        }
        "douyin_watch_later" => fetch_watch_later_posts(limit).await,
        "douyin_theater" => {
            let id = numeric_id(source_url).context("无法从放映厅详情链接识别专辑 ID")?;
            fetch_theater_posts(id, limit).await
        }
        "douyin_series" => {
            let id = numeric_id(source_url).context("无法从短剧链接识别短剧 ID")?;
            fetch_series_posts(id, limit).await
        }
        value => bail!("不支持的抖音来源类型: {value}"),
    }
}

fn numeric_id(value: &str) -> Option<&str> {
    value
        .split(['?', '#', '/'])
        .rev()
        .find(|part| !part.is_empty() && part.chars().all(|character| character.is_ascii_digit()))
}

async fn current_user_ids() -> Result<(String, String)> {
    let response = signed_get(DOUYIN_PROFILE_SELF_API, common_query_pairs()).await?;
    ensure_douyin_status_ok(&response, "获取当前抖音账号")?;
    let user = response.get("user").context("抖音当前账号资料为空")?;
    Ok((
        text(user, &["uid"]).context("当前抖音账号缺少 uid")?,
        text(user, &["sec_uid", "sec_user_id"]).context("当前抖音账号缺少 sec_uid")?,
    ))
}

async fn fetch_liked_posts(limit: usize) -> Result<Vec<DouyinPost>> {
    let (_, sec_uid) = current_user_ids().await?;
    let mut cursor = 0i64;
    let mut posts = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..1000 {
        let page_size = page_size_for(limit, posts.len());
        let mut pairs = common_query_pairs();
        pairs.extend([
            ("sec_user_id", sec_uid.clone()),
            ("max_cursor", cursor.to_string()),
            ("min_cursor", "0".to_string()),
            ("whale_cut_token", String::new()),
            ("cut_version", "1".to_string()),
            ("count", page_size.to_string()),
        ]);
        let response = signed_get(DOUYIN_FAVORITE_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取我的喜欢")?;
        append_awemes(&response, "aweme_list", &mut posts, &mut seen);
        if posts.len() >= limit || !response.get("has_more").and_then(value_as_bool).unwrap_or(false) {
            break;
        }
        let next = response.get("max_cursor").and_then(value_as_i64).unwrap_or(cursor);
        if next == cursor {
            break;
        }
        cursor = next;
    }
    Ok(posts)
}

async fn fetch_collection_posts(id: &str, limit: usize) -> Result<Vec<DouyinPost>> {
    let mut cursor = 0i64;
    let mut posts = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..1000 {
        let page_size = page_size_for(limit, posts.len());
        let mut pairs = common_query_pairs();
        pairs.extend([
            ("collects_id", id.to_string()),
            ("cursor", cursor.to_string()),
            ("count", page_size.to_string()),
        ]);
        let response = signed_get(DOUYIN_COLLECTION_VIDEOS_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取抖音收藏夹作品")?;
        append_awemes(&response, "aweme_list", &mut posts, &mut seen);
        if posts.len() >= limit || !response.get("has_more").and_then(value_as_bool).unwrap_or(false) {
            break;
        }
        let next = response.get("cursor").and_then(value_as_i64).unwrap_or(cursor);
        if next == cursor {
            break;
        }
        cursor = next;
    }
    Ok(posts)
}

async fn fetch_watch_later_posts(limit: usize) -> Result<Vec<DouyinPost>> {
    let mut offset = 0i64;
    let mut posts = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..1000 {
        let page_size = page_size_for(limit, posts.len());
        let mut pairs = common_query_pairs();
        pairs.extend([
            ("offset", offset.to_string()),
            ("count", page_size.to_string()),
            ("list_type", "0".to_string()),
            ("operate_type", "0".to_string()),
        ]);
        let response = signed_get(DOUYIN_WATCH_LATER_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取抖音稍后再看")?;
        append_awemes(&response, "items", &mut posts, &mut seen);
        if posts.len() >= limit || !response.get("has_more").and_then(value_as_bool).unwrap_or(false) {
            break;
        }
        let next = response.get("offset").and_then(value_as_i64).unwrap_or(offset);
        offset = if next == offset {
            offset + i64::from(page_size)
        } else {
            next
        };
    }
    Ok(posts)
}

async fn fetch_theater_posts(id: &str, limit: usize) -> Result<Vec<DouyinPost>> {
    let mut pairs = common_query_pairs();
    pairs.push(("album_ids", id.to_string()));
    let response = signed_get(DOUYIN_THEATER_ITEMS_API, pairs).await?;
    Ok(response
        .get("aweme_list")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_post)
        .take(limit)
        .collect())
}

async fn fetch_series_posts(id: &str, limit: usize) -> Result<Vec<DouyinPost>> {
    let mut cursor = 0i64;
    let mut posts = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..1000 {
        let page_size = page_size_for(limit, posts.len());
        let mut pairs = common_query_pairs();
        pairs.extend([
            ("series_id", id.to_string()),
            ("pull_type", "2".to_string()),
            ("cursor", cursor.to_string()),
            ("count", page_size.to_string()),
        ]);
        let response = signed_get(DOUYIN_SERIES_ITEMS_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取抖音短剧剧集")?;
        append_awemes(&response, "aweme_list", &mut posts, &mut seen);
        if posts.len() >= limit || !response.get("has_more").and_then(value_as_bool).unwrap_or(false) {
            break;
        }
        let next = response.get("max_cursor").and_then(value_as_i64).unwrap_or(cursor);
        if next == cursor {
            break;
        }
        cursor = next;
    }
    Ok(posts)
}

fn page_size_for(limit: usize, loaded: usize) -> i32 {
    i32::try_from(limit.saturating_sub(loaded).min(50)).unwrap_or(50).max(1)
}

fn append_awemes(response: &serde_json::Value, key: &str, posts: &mut Vec<DouyinPost>, seen: &mut HashSet<String>) {
    for item in response
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(post) = parse_post(item) {
            if seen.insert(post.id.clone()) {
                posts.push(post);
            }
        }
    }
}

pub async fn resolve_sec_user_id(value: &str) -> Result<String> {
    ensure_session()?;
    let client = client()?;
    let response = client.get(value.trim()).send().await.context("打开抖音作者链接失败")?;
    let final_url = response.url().clone();
    let body = response.text().await.unwrap_or_default();
    let candidates = [final_url.as_str(), value, body.as_str()];
    let regex = Regex::new(r"(?:sec_user_id=|/user/)([A-Za-z0-9._~-]+)")?;
    for candidate in candidates {
        if let Some(value) = regex
            .captures(candidate)
            .and_then(|capture| capture.get(1))
            .map(|matched| matched.as_str().trim_end_matches('/'))
            .filter(|value| !value.is_empty())
        {
            return Ok(value.to_string());
        }
    }
    bail!("无法从抖音作者链接中识别 sec_user_id，请使用作者主页链接")
}

pub async fn fetch_all_posts(source_url: &str) -> Result<Vec<DouyinPost>> {
    let sec_uid = resolve_sec_user_id(source_url).await?;
    fetch_posts_until(&sec_uid, usize::MAX).await
}

pub async fn fetch_profile(source_url: &str) -> Result<DouyinProfile> {
    let sec_uid = resolve_sec_user_id(source_url).await?;
    let mut pairs = common_query_pairs();
    pairs.push(("sec_user_id", sec_uid));
    let response = signed_get(DOUYIN_PROFILE_OTHER_API, pairs).await?;
    ensure_douyin_status_ok(&response, "获取抖音作者头像")?;
    let user = response.get("user").context("抖音作者资料为空")?;
    Ok(DouyinProfile {
        avatar_url: image_url(user.get("avatar_larger").or_else(|| user.get("avatar_thumb"))),
    })
}

/// 获取单个视频作品的原生详情。抖音 Web 详情接口会直接返回各档
/// MP4 播放地址，后续仍交给项目的统一下载器传输，不让 yt-dlp 接管下载。
pub(crate) async fn fetch_aweme_detail(aweme_id: &str) -> Result<serde_json::Value> {
    ensure_session()?;
    let mut pairs = common_query_pairs();
    pairs.push(("aweme_id", aweme_id.to_string()));
    let response = signed_get(DOUYIN_DETAIL_API, pairs).await?;
    let status = response
        .get("status_code")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(-1);
    if status != 0 {
        bail!(
            "抖音作品详情接口返回错误 {status}：{}",
            text(&response, &["status_msg", "message"]).unwrap_or_default()
        );
    }
    response
        .get("aweme_detail")
        .cloned()
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| anyhow!("抖音作品详情为空，请重新导入电脑浏览器刚导出的抖音 Cookie"))
}

/// 放映厅和短剧的列表接口本身已经返回完整 aweme（包括视频码率和播放地址）。
/// 这两类作品不应再强制经过普通 `aweme/detail`：后者会额外校验浏览器临时
/// challenge Cookie，即使同一登录状态能够正常读取关注列表和剧集列表，也可能
/// 对放映厅/短剧返回空详情。
pub(crate) async fn fetch_aweme_detail_for_source(
    source_type: &str,
    source_url: &str,
    aweme_id: &str,
) -> Result<serde_json::Value> {
    ensure_session()?;
    let source_result = match source_type.trim().to_ascii_lowercase().as_str() {
        "douyin_theater" => {
            let album_id = numeric_id(source_url).context("无法从放映厅详情链接识别专辑 ID")?;
            fetch_theater_aweme_detail(album_id, aweme_id).await
        }
        "douyin_series" => {
            let series_id = numeric_id(source_url).context("无法从短剧链接识别短剧 ID")?;
            fetch_series_aweme_detail(series_id, aweme_id).await
        }
        _ => return fetch_aweme_detail(aweme_id).await,
    };

    match source_result {
        Ok(Some(detail)) => Ok(detail),
        Ok(None) => fetch_aweme_detail(aweme_id)
            .await
            .with_context(|| format!("来源列表中未找到抖音作品 {aweme_id}，普通详情接口也不可用")),
        Err(source_error) => fetch_aweme_detail(aweme_id)
            .await
            .with_context(|| format!("读取抖音来源列表中的作品详情失败：{source_error:#}")),
    }
}

fn find_aweme_detail(response: &serde_json::Value, key: &str, aweme_id: &str) -> Option<serde_json::Value> {
    response
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| text(item, &["aweme_id", "group_id"]).as_deref() == Some(aweme_id))
        .cloned()
}

async fn fetch_theater_aweme_detail(album_id: &str, aweme_id: &str) -> Result<Option<serde_json::Value>> {
    let mut pairs = common_query_pairs();
    pairs.push(("album_ids", album_id.to_string()));
    let response = signed_get(DOUYIN_THEATER_ITEMS_API, pairs).await?;
    // 该接口成功响应没有 status_code，与普通 aweme 接口不同；是否成功以
    // aweme_list 为准，保持和已经验证可用的放映厅扫描路径一致。
    Ok(find_aweme_detail(&response, "aweme_list", aweme_id))
}

async fn fetch_series_aweme_detail(series_id: &str, aweme_id: &str) -> Result<Option<serde_json::Value>> {
    let mut cursor = 0i64;
    for _ in 0..1000 {
        let mut pairs = common_query_pairs();
        pairs.extend([
            ("series_id", series_id.to_string()),
            ("pull_type", "2".to_string()),
            ("cursor", cursor.to_string()),
            ("count", "50".to_string()),
        ]);
        let response = signed_get(DOUYIN_SERIES_ITEMS_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取抖音短剧剧集详情")?;
        if let Some(detail) = find_aweme_detail(&response, "aweme_list", aweme_id) {
            return Ok(Some(detail));
        }
        if !response.get("has_more").and_then(value_as_bool).unwrap_or(false) {
            return Ok(None);
        }
        let next = response.get("max_cursor").and_then(value_as_i64).unwrap_or(cursor);
        if next == cursor {
            return Ok(None);
        }
        cursor = next;
    }
    Ok(None)
}

/// 获取抖音点播作品的时间轴弹幕。Web 端按 32 秒窗口返回弹幕，因此这里
/// 完整遍历视频时长并按 danmaku_id 去重，避免只保存首屏弹幕。
pub(crate) async fn fetch_aweme_danmaku(aweme_id: &str, duration_seconds: i32) -> Result<Vec<DouyinDanmaku>> {
    ensure_session()?;
    let duration_ms = i64::from(duration_seconds.max(1)).saturating_mul(1000);
    let mut start_time = 0i64;
    let mut seen = HashSet::new();
    let mut danmaku = Vec::new();

    while start_time < duration_ms.max(32_000) {
        let end_time = start_time.saturating_add(32_000);
        let mut pairs = common_query_pairs();
        pairs.extend([
            ("item_id", aweme_id.to_string()),
            ("duration", duration_ms.to_string()),
            ("start_time", start_time.to_string()),
            ("end_time", end_time.to_string()),
        ]);
        let response = signed_get(DOUYIN_DANMAKU_API, pairs).await?;
        ensure_douyin_status_ok(&response, "获取抖音视频弹幕")?;
        for item in response
            .get("danmaku_list")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = text(item, &["danmaku_id"]) else {
                continue;
            };
            let Some(content) = text(item, &["text"]).filter(|value| !value.trim().is_empty()) else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            danmaku.push(DouyinDanmaku {
                danmaku_id: id,
                user_id: text(item, &["user_id"]).unwrap_or_default(),
                offset_time: item
                    .get("offset_time")
                    .and_then(serde_json::Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                    .unwrap_or_default()
                    .max(0),
                text: content,
                digg_count: integer(item, &["digg_count"]).unwrap_or_default(),
            });
        }
        start_time = response
            .get("end_time")
            .and_then(serde_json::Value::as_i64)
            .filter(|next| *next > start_time)
            .unwrap_or(end_time);
    }
    danmaku.sort_by_key(|item| item.offset_time);
    Ok(danmaku)
}

async fn fetch_posts_until(sec_uid: &str, limit: usize) -> Result<Vec<DouyinPost>> {
    let mut cursor = 0i64;
    let mut posts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        let page = fetch_post_page(sec_uid, cursor, 18).await?;
        for post in page.posts {
            if seen.insert(post.id.clone()) {
                posts.push(post);
            }
        }
        if posts.len() >= limit || !page.has_more || page.cursor == cursor {
            break;
        }
        cursor = page.cursor;
        if posts.len() > 20_000 {
            warn!(sec_user_id = sec_uid, "抖音作者作品超过 20000 条，停止继续枚举");
            break;
        }
    }
    Ok(posts)
}

async fn fetch_post_page(sec_uid: &str, cursor: i64, count: i32) -> Result<PostPage> {
    ensure_session()?;
    let mut pairs = common_query_pairs();
    pairs.extend([
        ("sec_user_id", sec_uid.to_string()),
        ("max_cursor", cursor.to_string()),
        ("locate_query", "false".to_string()),
        ("show_live_replay_strategy", "1".to_string()),
        ("need_time_list", "1".to_string()),
        ("time_list_query", "0".to_string()),
        ("whale_cut_token", String::new()),
        ("cut_version", "1".to_string()),
        ("count", count.to_string()),
        ("publish_video_strategy_type", "2".to_string()),
        ("from_user_page", "1".to_string()),
    ]);
    let response = signed_get(DOUYIN_POST_API, pairs).await?;
    if response
        .get("status_code")
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|status| status != 0)
    {
        bail!(
            "抖音作品接口返回错误 {}：{}",
            response
                .get("status_code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(-1),
            text(&response, &["status_msg", "message"]).unwrap_or_default()
        );
    }
    let awemes = response
        .get("aweme_list")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let profile = awemes
        .first()
        .and_then(|item| item.get("author"))
        .map(|author| DouyinProfile {
            avatar_url: image_url(author.get("avatar_larger").or_else(|| author.get("avatar_thumb"))),
        });
    let posts = awemes.iter().filter_map(parse_post).collect::<Vec<_>>();
    Ok(PostPage {
        posts,
        profile,
        has_more: response
            .get("has_more")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default()
            != 0,
        cursor: response
            .get("max_cursor")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(cursor),
    })
}

fn parse_post(item: &serde_json::Value) -> Option<DouyinPost> {
    let is_image_post = item
        .get("images")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|images| !images.is_empty());
    let id = text(item, &["aweme_id", "group_id"])?;
    let author = item.get("author");
    let timestamp = item.get("create_time").and_then(serde_json::Value::as_i64);
    Some(DouyinPost {
        url: format!("https://www.douyin.com/video/{id}"),
        id,
        title: text(item, &["desc"])
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "无标题".to_string()),
        uploader: author
            .and_then(|value| text(value, &["nickname", "unique_id"]))
            .unwrap_or_default(),
        thumbnail: if is_image_post {
            item.get("images")
                .and_then(serde_json::Value::as_array)
                .and_then(|images| images.first())
                .and_then(|image| image_url(Some(image)))
        } else {
            item.get("video")
                .and_then(|video| image_url(video.get("cover").or_else(|| video.get("origin_cover"))))
        },
        published_at: timestamp
            .and_then(|value| chrono::DateTime::from_timestamp(value, 0).map(|date| date.format("%Y%m%d").to_string())),
        timestamp,
        duration_seconds: if is_image_post {
            item.get("music")
                .and_then(|music| integer(music, &["duration"]))
                .and_then(|duration| i32::try_from(duration).ok())
        } else {
            item.get("duration")
                .and_then(serde_json::Value::as_i64)
                .and_then(|duration| i32::try_from((duration + 500) / 1000).ok())
        },
        digg_count: item
            .get("statistics")
            .and_then(|statistics| integer(statistics, &["digg_count"]))
            .unwrap_or_default(),
        is_image_post,
    })
}

fn common_query_pairs() -> Vec<(&'static str, String)> {
    let pairs = vec![
        ("device_platform", "webapp".to_string()),
        ("aid", "6383".to_string()),
        ("channel", "channel_pc_web".to_string()),
        ("update_version_code", "170400".to_string()),
        ("pc_client_type", "1".to_string()),
        ("pc_libra_divert", "Windows".to_string()),
        ("support_h265", "1".to_string()),
        ("support_dash", "0".to_string()),
        ("version_code", "290100".to_string()),
        ("version_name", "29.1.0".to_string()),
        ("cookie_enabled", "true".to_string()),
        ("screen_width", "1920".to_string()),
        ("screen_height", "1080".to_string()),
        ("browser_language", "zh-CN".to_string()),
        ("browser_platform", "Win32".to_string()),
        ("browser_name", "Edge".to_string()),
        ("browser_version", "131.0.0.0".to_string()),
        ("browser_online", "true".to_string()),
        ("engine_name", "Blink".to_string()),
        ("engine_version", "131.0.0.0".to_string()),
        ("os_name", "Windows".to_string()),
        ("os_version", "10".to_string()),
        ("cpu_core_num", "12".to_string()),
        ("device_memory", "8".to_string()),
        ("platform", "PC".to_string()),
        ("downlink", "10".to_string()),
        ("effective_type", "4g".to_string()),
        ("round_trip_time", "50".to_string()),
    ];
    pairs
}

async fn signed_get(base_url: &str, mut pairs: Vec<(&str, String)>) -> Result<serde_json::Value> {
    // 抓包中的所有受保护 Web API 都会把同一浏览器会话的 webid、verifyFp/fp
    // 和 msToken 一并纳入 a_bogus。关注列表对这些字段相对宽松，但作品详情、
    // 放映厅和短剧接口会严格校验，不能因为登录 Cookie 可用就省略设备参数。
    ensure_search_ms_token(false).await?;
    let cookies = cookie_values();
    pairs.push(("webid", stable_webid().await?));
    if let Some(uifid) = cookies.get("UIFID").or_else(|| cookies.get("UIFID_TEMP")) {
        pairs.push(("uifid", uifid.clone()));
    }
    let verify_fp = stable_verify_fp().await?;
    pairs.push(("verifyFp", verify_fp.clone()));
    pairs.push(("fp", verify_fp));
    if let Some(ms_token) = cookies.get("msToken").cloned() {
        pairs.push(("msToken", ms_token));
    }
    let params = serde_urlencoded::to_string(&pairs)?;
    let signature = douyin_sign::generate(&params);
    let mut url = reqwest::Url::parse_with_params(base_url, &pairs)?;
    url.query_pairs_mut().append_pair("a_bogus", &signature);
    let mut request = client()?
        .get(url)
        .header(reqwest::header::REFERER, "https://www.douyin.com/");
    let cookies = cookie_values();
    if let Some(uifid) = cookies.get("UIFID").or_else(|| cookies.get("UIFID_TEMP")) {
        request = request.header("uifid", uifid);
    }
    let response = request.send().await.context("请求抖音 Web API 失败")?;
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        bail!("抖音 Web API 返回 HTTP {status}");
    }
    if bytes.is_empty() {
        bail!("抖音 Web API 返回空响应；请重新导入电脑浏览器刚导出的抖音 Cookie");
    }
    serde_json::from_slice(&bytes).context("解析抖音 Web API 响应失败")
}

async fn browser_get_with_referer(
    base_url: &str,
    pairs: Vec<(&str, String)>,
    referer: &str,
) -> Result<serde_json::Value> {
    let url = reqwest::Url::parse_with_params(base_url, &pairs)?;
    // 搜索请求使用与查询参数一致的 Chrome UA，但继续复用项目原有 HTTP
    // 客户端栈；不内置 Chromium，也不额外增加 Docker 运行进程或镜像依赖。
    let mut request = reqwest::Client::builder()
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36",
        )
        .timeout(REQUEST_TIMEOUT)
        .build()?
        .get(url.as_str())
        .header(reqwest::header::COOKIE.as_str(), cookie_header()?)
        .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::PRAGMA, "no-cache")
        .header(
            "sec-ch-ua",
            r#""Not;A=Brand";v="8", "Chromium";v="149", "Google Chrome";v="149""#,
        )
        .header("sec-ch-ua-mobile", "?0")
        .header("sec-ch-ua-platform", r#""Windows""#)
        .header("sec-fetch-dest", "empty")
        .header("sec-fetch-mode", "cors")
        .header("sec-fetch-site", "same-origin")
        .header(reqwest::header::REFERER, referer);
    let cookies = cookie_values();
    if let Some(uifid) = cookies.get("UIFID").or_else(|| cookies.get("UIFID_TEMP")) {
        request = request.header("uifid", uifid);
    }
    let response = request.send().await.context("请求抖音 Web API 失败")?;
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        bail!("抖音 Web API 返回 HTTP {status}");
    }
    if bytes.is_empty() {
        bail!("抖音 Web API 返回空响应；请重新导入电脑浏览器刚导出的抖音 Cookie");
    }
    serde_json::from_slice(&bytes).context("解析抖音 Web API 响应失败")
}

async fn generate_webid() -> Result<String> {
    let response = reqwest::Client::builder()
        .user_agent(douyin_sign::user_agent())
        .timeout(REQUEST_TIMEOUT)
        .build()?
        .post("https://mcs.zijieapi.com/webid?aid=6383&sdk_version=5.1.18_zip&device_platform=web")
        .header(reqwest::header::REFERER, "https://www.douyin.com/")
        .json(&serde_json::json!({
            "app_id": 6383,
            "referer": "https://www.douyin.com/",
            "url": "https://www.douyin.com/",
            "user_agent": douyin_sign::user_agent(),
            "user_unique_id": ""
        }))
        .send()
        .await
        .context("获取抖音搜索 webid 失败")?;
    if !response.status().is_success() {
        bail!("获取抖音搜索 webid 返回 HTTP {}", response.status());
    }
    response
        .json::<serde_json::Value>()
        .await?
        .get("web_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .context("抖音 webid 响应缺少 web_id")
}

/// webid 是抖音网页的设备标识，不是一次性请求参数。浏览器会长期复用同一个
/// user_unique_id；每次搜索都重新生成会被识别为同一会话在不断更换设备，几次
/// 请求后即返回 verify_check。因此首次生成后与 Cookie 一样落到配置目录复用。
async fn stable_webid() -> Result<String> {
    let path = CONFIG_DIR.join("douyin-webid.txt");
    if let Ok(value) = tokio::fs::read_to_string(&path).await {
        let value = value.trim();
        if valid_webid(value) {
            return Ok(value.to_string());
        }
    }
    let webid = generate_webid().await?;
    tokio::fs::create_dir_all(&*CONFIG_DIR).await?;
    let temporary = path.with_extension("txt.importing");
    tokio::fs::write(&temporary, webid.as_bytes()).await?;
    replace_cookie_file(&temporary, &path).await?;
    Ok(webid)
}

fn valid_webid(value: &str) -> bool {
    (15..=24).contains(&value.len()) && value.chars().all(|character| character.is_ascii_digit())
}

fn valid_verify_fp(value: &str) -> bool {
    value.len() == 52
        && value.starts_with("verify_")
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_string();
    }
    let mut output = Vec::new();
    while value > 0 {
        output.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    output.reverse();
    String::from_utf8(output).unwrap_or_default()
}

fn generate_verify_fp() -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut random = rand::thread_rng();
    let mut suffix = [0u8; 36];
    for (index, byte) in suffix.iter_mut().enumerate() {
        *byte = match index {
            8 | 13 | 18 | 23 => b'_',
            14 => b'4',
            19 => ALPHABET[(random.gen_range(0..ALPHABET.len()) & 3) | 8],
            _ => ALPHABET[random.gen_range(0..ALPHABET.len())],
        };
    }
    format!("verify_{}_{}", base36(timestamp), String::from_utf8_lossy(&suffix))
}

/// verifyFp/fp 与 webid 一样属于浏览器会话指纹。每次搜索重新生成会导致同一
/// Cookie 在短时间内不断更换设备指纹，因此首次生成后持久化复用。
async fn stable_verify_fp() -> Result<String> {
    let path = CONFIG_DIR.join("douyin-verify-fp.txt");
    if let Ok(value) = tokio::fs::read_to_string(&path).await {
        let value = value.trim();
        if valid_verify_fp(value) {
            return Ok(value.to_string());
        }
    }
    let value = generate_verify_fp();
    tokio::fs::create_dir_all(&*CONFIG_DIR).await?;
    let temporary = path.with_extension("txt.importing");
    tokio::fs::write(&temporary, value.as_bytes()).await?;
    replace_cookie_file(&temporary, &path).await?;
    Ok(value)
}

fn ms_token_path() -> PathBuf {
    CONFIG_DIR.join("douyin-mstoken.txt")
}

fn valid_ms_token(value: &str) -> bool {
    matches!(value.trim().len(), 164 | 184)
}

fn imported_cookie_has_ms_token() -> bool {
    read_cookie_file_values()
        .get("msToken")
        .is_some_and(|value| valid_ms_token(value))
}

async fn ensure_search_ms_token(force_refresh: bool) -> Result<()> {
    if imported_cookie_has_ms_token() {
        return Ok(());
    }
    let path = ms_token_path();
    if !force_refresh {
        let fresh = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed < Duration::from_secs(12 * 60 * 60));
        if fresh {
            if let Ok(value) = tokio::fs::read_to_string(&path).await {
                if valid_ms_token(&value) {
                    return Ok(());
                }
            }
        }
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let response = reqwest::Client::builder()
        .user_agent(douyin_sign::user_agent())
        .timeout(REQUEST_TIMEOUT)
        .build()?
        .post(DOUYIN_MSTOKEN_API)
        .header(reqwest::header::CONTENT_TYPE, "application/json; charset=utf-8")
        .json(&serde_json::json!({
            "magic": 538969122u64,
            "version": 1,
            "dataType": 8,
            "strData": DOUYIN_MSTOKEN_STR_DATA.trim(),
            "ulr": 0,
            "tspFromClient": timestamp
        }))
        .send()
        .await
        .context("生成抖音搜索 msToken 失败")?;
    if !response.status().is_success() {
        bail!("生成抖音搜索 msToken 返回 HTTP {}", response.status());
    }
    let token = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|part| part.trim().strip_prefix("msToken=").map(str::to_string))
        .filter(|value| valid_ms_token(value))
        .context("抖音 mssdk 响应没有返回有效 msToken")?;
    tokio::fs::create_dir_all(&*CONFIG_DIR).await?;
    let temporary = path.with_extension("txt.importing");
    tokio::fs::write(&temporary, token.as_bytes()).await?;
    replace_cookie_file(&temporary, &path).await?;
    Ok(())
}

fn client() -> Result<reqwest::Client> {
    let cookie = cookie_header()?;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::COOKIE,
        reqwest::header::HeaderValue::from_str(&cookie).context("抖音 Cookie 包含无效字符")?,
    );
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .user_agent(douyin_sign::user_agent())
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?)
}

pub(crate) fn cookie_header() -> Result<String> {
    let values = cookie_values();
    if values.is_empty() {
        bail!("尚未导入抖音 cookies.txt");
    }
    Ok(values
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; "))
}

fn read_cookie_file_values() -> HashMap<String, String> {
    std::fs::read_to_string(cookie_path())
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
                    (columns.len() >= 7 && is_douyin_cookie_domain(columns[0]))
                        .then(|| (columns[5].to_string(), columns[6].to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn cookie_values() -> HashMap<String, String> {
    let mut values = read_cookie_file_values();
    if !values.contains_key("msToken") {
        if let Ok(token) = std::fs::read_to_string(ms_token_path()) {
            if valid_ms_token(&token) {
                values.insert("msToken".to_string(), token.trim().to_string());
            }
        }
    }
    values
}

pub(crate) fn append_cookies(command: &mut tokio::process::Command) {
    let path = cookie_path();
    if has_douyin_session(&path) {
        command.arg("--cookies").arg(path);
    }
}

pub(crate) fn cookie_path() -> PathBuf {
    CONFIG_DIR.join("douyin-cookies.txt")
}

fn ensure_session() -> Result<()> {
    if has_douyin_session(&cookie_path()) {
        Ok(())
    } else {
        bail!("抖音扫描需要新鲜 Cookie，请先在设置页导入电脑浏览器导出的 douyin.com cookies.txt")
    }
}

fn has_douyin_session(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|contents| is_netscape_douyin_cookie_file(&contents))
}

fn is_netscape_douyin_cookie_file(contents: &str) -> bool {
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
                && is_douyin_cookie_domain(columns[0])
                && matches!(
                    columns[5],
                    "ttwid" | "msToken" | "passport_csrf_token" | "sessionid" | "sid_guard"
                )
        })
}

async fn validate_cookie_file(path: &Path) -> Result<()> {
    let contents = tokio::fs::read_to_string(path).await?;
    if !is_netscape_douyin_cookie_file(&contents) {
        bail!("Cookie 文件没有可用的 douyin.com 会话字段");
    }
    let response = reqwest::Client::builder()
        .user_agent(douyin_sign::user_agent())
        .timeout(REQUEST_TIMEOUT)
        .build()?
        .get("https://www.douyin.com/")
        .header(
            reqwest::header::COOKIE,
            contents
                .lines()
                .filter_map(|line| {
                    let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
                    if line.trim_start().starts_with('#') {
                        return None;
                    }
                    let columns = line.split('\t').collect::<Vec<_>>();
                    (columns.len() >= 7 && is_douyin_cookie_domain(columns[0]))
                        .then(|| format!("{}={}", columns[5], columns[6]))
                })
                .collect::<Vec<_>>()
                .join("; "),
        )
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("抖音首页验证返回 HTTP {}", response.status());
    }
    Ok(())
}

fn is_douyin_cookie_domain(domain: &str) -> bool {
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    domain == "douyin.com"
        || domain.ends_with(".douyin.com")
        || domain == "bytedance.com"
        || domain.ends_with(".bytedance.com")
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
            Err(error.into())
        }
    }
}

async fn persist_session_value(path: &Path, value: &str) -> Result<()> {
    tokio::fs::create_dir_all(&*CONFIG_DIR).await?;
    let temporary = path.with_extension("txt.importing");
    tokio::fs::write(&temporary, value.as_bytes()).await?;
    replace_cookie_file(&temporary, path).await
}

fn image_url(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(|value| value.get("url_list").and_then(serde_json::Value::as_array))
        .and_then(|items| items.first())
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| value.and_then(serde_json::Value::as_str).map(str::to_string))
}

/// 从抖音作品、短剧或放映厅链接中提取作品 ID。
pub(crate) fn aweme_id(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next()?;
    let id = path.trim_end_matches('/').rsplit('/').next()?;
    (!id.is_empty() && id.chars().all(|ch| ch.is_ascii_digit())).then_some(id)
}

/// 解析抖音媒体元数据。原生详情接口优先，yt-dlp 仅作为普通作品的兜底解析器。
pub(crate) async fn extract_metadata(
    aweme_id: &str,
    source: Option<&youtube_source::Model>,
) -> Result<ExternalMediaMetadata> {
    match extract_metadata_native(aweme_id, source).await {
        Ok(metadata) => Ok(metadata),
        Err(native_error) => {
            if native_error.to_string().contains("CENC 加密 DASH") {
                return Err(native_error);
            }
            warn!(
                target: "bili_sync_rs::douyin",
                aweme_id,
                error = %native_error,
                "抖音原生详情接口失败，改用 yt-dlp 解析媒体直链"
            );
            let url = format!("https://www.douyin.com/video/{aweme_id}");
            crate::youtube::extract_ytdlp_metadata(&url, "抖音")
                .await
                .with_context(|| format!("抖音原生详情接口失败：{native_error:#}；yt-dlp 回退也失败"))
        }
    }
}

async fn extract_metadata_native(
    aweme_id: &str,
    source: Option<&youtube_source::Model>,
) -> Result<ExternalMediaMetadata> {
    let detail = if let Some(source) = source.filter(|source| source.source_type.starts_with("douyin")) {
        fetch_aweme_detail_for_source(&source.source_type, &source.url, aweme_id).await?
    } else {
        fetch_aweme_detail(aweme_id).await?
    };
    let images = detail
        .get("images")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|image| urls_from_value(Some(image)))
        .filter(|urls| !urls.is_empty())
        .collect::<Vec<_>>();
    let video = detail.get("video");
    let mut formats = video
        .and_then(|video| video.get("bit_rate"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(native_format)
        .collect::<Vec<_>>();
    if formats.is_empty() && images.is_empty() {
        let video = video.context("抖音作品详情既没有视频流也没有原图")?;
        let width = json_i32(video.get("width"));
        let height = json_i32(video.get("height"));
        if let Some((url, fallback_urls)) = media_urls(video.get("play_addr")) {
            formats.push(ExternalMediaFormat {
                format_id: Some("douyin-default".to_string()),
                url: Some(url),
                protocol: Some("https".to_string()),
                ext: Some("mp4".to_string()),
                vcodec: Some(
                    if video.get("is_h265").and_then(serde_json::Value::as_i64) == Some(1) {
                        "h265"
                    } else {
                        "h264"
                    }
                    .to_string(),
                ),
                acodec: Some("aac".to_string()),
                width,
                height,
                fps: None,
                tbr: None,
                vbr: None,
                abr: Some(128.0),
                dynamic_range: Some("SDR".to_string()),
                fallback_urls,
            });
        }
    }
    if formats.is_empty() && images.is_empty() {
        let protected_long_video = video.is_some_and(|video| {
            let is_long_video = video
                .get("is_long_video")
                .and_then(|value| {
                    value
                        .as_i64()
                        .or_else(|| value.as_bool().map(|value| if value { 1 } else { 0 }))
                })
                .unwrap_or_default()
                != 0;
            is_long_video && video.get("video_model").is_some()
        });
        if protected_long_video {
            bail!(
                "该抖音放映厅长视频只返回 CENC 加密 DASH（不是 Cookie 缺少）；当前项目统一下载链路不能直接处理此类加密长视频"
            );
        }
        let detail_keys = detail
            .as_object()
            .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        let video_keys = video
            .and_then(serde_json::Value::as_object)
            .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        bail!("抖音作品详情没有可用的原生 MP4 地址（作品字段：{detail_keys}；视频字段：{video_keys}）");
    }
    let author = detail.get("author");
    let uploader = author.and_then(|value| text(value, &["nickname", "unique_id"]));
    let channel_id = author.and_then(|value| text(value, &["sec_uid", "uid"]));
    let thumbnail = images.first().and_then(|urls| urls.first()).cloned().or_else(|| {
        video.and_then(|video| {
            image_url(
                video
                    .get("cover")
                    .or_else(|| video.get("origin_cover"))
                    .or_else(|| video.get("dynamic_cover")),
            )
        })
    });
    let music_urls = urls_from_value(detail.pointer("/music/play_url"));
    let timestamp = detail.get("create_time").and_then(serde_json::Value::as_i64);
    Ok(ExternalMediaMetadata {
        id: detail
            .get("aweme_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(aweme_id)
            .to_string(),
        title: text(&detail, &["desc"]),
        uploader: uploader.clone(),
        uploader_url: channel_id
            .as_deref()
            .map(|id| format!("https://www.douyin.com/user/{id}")),
        channel: uploader,
        channel_id: channel_id.clone(),
        channel_url: channel_id.map(|id| format!("https://www.douyin.com/user/{id}")),
        thumbnail,
        description: text(&detail, &["desc"]),
        language: Some("zh-CN".to_string()),
        upload_date: timestamp
            .and_then(|value| chrono::DateTime::from_timestamp(value, 0).map(|date| date.format("%Y%m%d").to_string())),
        duration: if images.is_empty() {
            video
                .and_then(|video| video.get("duration"))
                .and_then(serde_json::Value::as_f64)
                .map(|value| value / 1000.0)
        } else {
            detail.pointer("/music/duration").and_then(serde_json::Value::as_f64)
        },
        formats,
        subtitles: HashMap::new(),
        automatic_captions: HashMap::new(),
        images,
        music_urls,
    })
}

fn native_format(value: &serde_json::Value) -> Option<ExternalMediaFormat> {
    let play_addr = value.get("play_addr")?;
    let (url, fallback_urls) = media_urls(Some(play_addr))?;
    let bitrate = value
        .get("bit_rate")
        .and_then(serde_json::Value::as_f64)
        .map(|value| value / 1000.0);
    let is_h265 = value.get("is_h265").and_then(serde_json::Value::as_i64) == Some(1)
        || value.get("is_bytevc1").and_then(serde_json::Value::as_i64) == Some(1);
    let dynamic_range = text(value, &["HDR_type", "HDR_bit"])
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "SDR".to_string());
    Some(ExternalMediaFormat {
        format_id: text(value, &["gear_name"]).or_else(|| text(play_addr, &["url_key", "uri"])),
        url: Some(url),
        protocol: Some("https".to_string()),
        ext: text(value, &["format"]).or_else(|| Some("mp4".to_string())),
        vcodec: Some(if is_h265 { "h265" } else { "h264" }.to_string()),
        acodec: Some("aac".to_string()),
        width: json_i32(play_addr.get("width")),
        height: json_i32(play_addr.get("height")),
        fps: value
            .get("FPS")
            .or_else(|| value.get("fps"))
            .and_then(serde_json::Value::as_f64),
        tbr: bitrate,
        vbr: bitrate,
        abr: Some(128.0),
        dynamic_range: Some(dynamic_range),
        fallback_urls,
    })
}

fn media_urls(value: Option<&serde_json::Value>) -> Option<(String, Vec<String>)> {
    let mut urls = value?
        .get("url_list")?
        .as_array()?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter(|url| url.starts_with("http"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let first = urls.first()?.clone();
    urls.remove(0);
    Some((first, urls))
}

fn urls_from_value(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    ["url_list", "download_url_list"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(serde_json::Value::as_array))
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter(|url| url.starts_with("http"))
        .map(str::to_string)
        .collect()
}

/// 下载抖音图文原图和配乐，并生成可被现有视频管理页播放的 MP4。
pub(crate) async fn download_image_post(
    downloader: &UnifiedDownloader,
    metadata: &ExternalMediaMetadata,
    output_path: &Path,
    filter: &FilterOption,
) -> Result<()> {
    let parent = output_path.parent().context("抖音图文输出路径没有父目录")?;
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .context("抖音图文输出文件名无效")?;
    let image_dir = parent.join(format!("{stem}-images"));
    tokio::fs::create_dir_all(&image_dir).await?;
    let mut image_paths = Vec::with_capacity(metadata.images.len());
    for (index, urls) in metadata.images.iter().enumerate() {
        let path = image_dir.join(format!("{:02}.jpg", index + 1));
        if !tokio::fs::metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.len() >= 1024)
        {
            let temporary = image_dir.join(format!("{:02}.download", index + 1));
            let url_refs = urls.iter().map(String::as_str).collect::<Vec<_>>();
            if let Err(error) = fetch_media(downloader, &url_refs, &temporary)
                .await
                .with_context(|| format!("使用项目统一下载器下载抖音第 {} 张原图失败", index + 1))
            {
                let _ = remove_file_if_exists(&temporary).await;
                return Err(error);
            }
            replace_file(&temporary, &path).await?;
        }
        image_paths.push(path);
    }
    if image_paths.is_empty() {
        bail!("抖音图文作品没有可下载的原图");
    }

    let music_path = parent.join(format!("{stem}-music.mp3"));
    let has_music = if metadata.music_urls.is_empty() {
        false
    } else {
        if !tokio::fs::metadata(&music_path)
            .await
            .is_ok_and(|metadata| metadata.len() >= 1024)
        {
            let temporary = parent.join(format!("{stem}-music.download"));
            let urls = metadata.music_urls.iter().map(String::as_str).collect::<Vec<_>>();
            if let Err(error) = fetch_media(downloader, &urls, &temporary)
                .await
                .context("使用项目统一下载器下载抖音图文配乐失败")
            {
                let _ = remove_file_if_exists(&temporary).await;
                return Err(error);
            }
            replace_file(&temporary, &music_path).await?;
        }
        true
    };

    let concat_path = parent.join(format!("{stem}-images.concat"));
    let quote_path = |path: &Path| path.to_string_lossy().replace('\\', "/").replace('\'', "'\\''");
    let seconds_per_image = 3u64;
    let mut concat = String::new();
    for path in &image_paths {
        concat.push_str(&format!("file '{}'\nduration {seconds_per_image}\n", quote_path(path)));
    }
    concat.push_str(&format!("file '{}'\n", quote_path(image_paths.last().unwrap())));
    tokio::fs::write(&concat_path, concat.as_bytes()).await?;

    let max_short_edge = quality_height(filter.video_max_quality).clamp(360, 2160);
    let width = max_short_edge - (max_short_edge % 2);
    let height = ((i64::from(width) * 16 / 9) as i32).max(width) & !1;
    let total_seconds = u64::try_from(image_paths.len())
        .unwrap_or(1)
        .saturating_mul(seconds_per_image)
        .max(3);
    let temporary = output_path.with_extension("slideshow.mp4");
    let mut command = tokio::process::Command::new(crate::downloader::resolve_media_tool_path("ffmpeg"));
    command
        .args(["-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&concat_path);
    if has_music {
        command.args(["-stream_loop", "-1", "-i"]).arg(&music_path);
    }
    command
        .args(["-vf", &format!("scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:black,fps=30")])
        .args(["-t", &total_seconds.to_string(), "-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart"]);
    if has_music {
        command.args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0?",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-shortest",
        ]);
    } else {
        command.arg("-an");
    }
    let result = command
        .arg(&temporary)
        .output()
        .await
        .context("启动 ffmpeg 生成抖音图文幻灯片失败")?;
    let _ = remove_file_if_exists(&concat_path).await;
    if !result.status.success() {
        let _ = remove_file_if_exists(&temporary).await;
        bail!("ffmpeg 生成抖音图文 MP4 失败：{}", process_error(&result));
    }
    replace_file(&temporary, output_path).await?;
    info!(aweme_id = %metadata.id, images = image_paths.len(), path = %output_path.display(), "抖音图文原图、配乐和 MP4 幻灯片生成完成");
    Ok(())
}

/// 下载抖音弹幕并写入 JSON/ASS；图文或无弹幕作品只写检查标记。
pub(crate) async fn download_danmaku(metadata: &ExternalMediaMetadata, output_path: &Path, title: &str) -> Result<()> {
    let ass_path = output_path.with_extension("ass");
    let json_path = output_path.with_extension("danmaku.json");
    let checked_path = output_path.with_extension("danmaku.checked");
    if tokio::fs::metadata(&ass_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
        || tokio::fs::metadata(&checked_path).await.is_ok()
    {
        return Ok(());
    }
    if !metadata.images.is_empty() {
        tokio::fs::write(&checked_path, b"image post has no video danmaku\n").await?;
        debug!(aweme_id = %metadata.id, "抖音图文作品没有视频弹幕，已记录检查结果");
        return Ok(());
    }
    let duration = metadata.duration.map(|value| value.ceil() as i32).unwrap_or(1).max(1);
    let danmaku = fetch_aweme_danmaku(&metadata.id, duration).await?;
    if danmaku.is_empty() {
        tokio::fs::write(&checked_path, b"no douyin danmaku\n")
            .await
            .with_context(|| format!("写入抖音弹幕检查标记失败: {}", checked_path.display()))?;
        info!(aweme_id = %metadata.id, "抖音视频没有可下载的弹幕，已记录检查结果");
        return Ok(());
    }
    let json_temporary = output_path.with_extension("danmaku.json.download");
    tokio::fs::write(&json_temporary, serde_json::to_vec_pretty(&danmaku)?)
        .await
        .with_context(|| format!("写入抖音弹幕 JSON 失败: {}", json_temporary.display()))?;
    replace_file(&json_temporary, &json_path).await?;
    let count = danmaku.len();
    let elems = danmaku
        .into_iter()
        .map(|item| DanmakuElem {
            id: item.danmaku_id.parse().unwrap_or_default(),
            progress: item.offset_time,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            mid_hash: item.user_id,
            content: item.text,
            ctime: 0,
            weight: i32::try_from(item.digg_count).unwrap_or(i32::MAX),
            action: String::new(),
            pool: 0,
            dmid_str: item.danmaku_id,
            attr: 0,
        })
        .collect();
    write_danmaku_ass(output_path, title, duration, elems).await?;
    let _ = remove_file_if_exists(&checked_path).await;
    info!(aweme_id = %metadata.id, count, path = %ass_path.display(), "抖音弹幕 JSON 和 ASS 生成完成");
    Ok(())
}

async fn fetch_media(downloader: &UnifiedDownloader, urls: &[&str], path: &Path) -> Result<()> {
    let cookie = cookie_header()?;
    downloader
        .fetch_with_fallback_with_referer_and_cookie(urls, path, "https://www.douyin.com/", &cookie)
        .await
}

async fn write_danmaku_ass(
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
        return Err(error).context("生成抖音 ASS 弹幕失败");
    }
    replace_file(&temporary, &ass_path).await
}

fn quality_height(quality: VideoQuality) -> i32 {
    match quality {
        VideoQuality::Quality360p => 360,
        VideoQuality::Quality480p => 480,
        VideoQuality::Quality720p => 720,
        VideoQuality::Quality1080p | VideoQuality::Quality1080pPLUS | VideoQuality::Quality1080p60 => 1080,
        VideoQuality::Quality4k | VideoQuality::QualityHdr | VideoQuality::QualityDolby => 2160,
        VideoQuality::Quality8k => 4320,
    }
}

fn json_i32(value: Option<&serde_json::Value>) -> Option<i32> {
    value
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

async fn replace_file(source: &Path, target: &Path) -> Result<()> {
    if tokio::fs::try_exists(target).await? {
        tokio::fs::remove_file(target).await?;
    }
    tokio::fs::rename(source, target)
        .await
        .with_context(|| format!("保存抖音下载文件失败: {}", target.display()))
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn process_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        stderr
    }
}

fn text(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_i64().map(|value| value.to_string()))
        })
        .filter(|value| !value.is_empty())
}

fn integer(value: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(serde_json::Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_netscape_douyin_cookie() {
        let contents = "# Netscape HTTP Cookie File\n.douyin.com\tTRUE\t/\tTRUE\t0\tttwid\tvalue\n";
        assert!(is_netscape_douyin_cookie_file(contents));
    }

    #[test]
    fn parses_douyin_post() {
        let post = parse_post(&serde_json::json!({
            "aweme_id": "123",
            "desc": "测试",
            "create_time": 1700000000,
            "duration": 61500,
            "author": {"nickname": "作者"},
            "video": {"cover": {"url_list": ["https://example.com/a.jpg"]}}
        }))
        .unwrap();
        assert_eq!(post.id, "123");
        assert_eq!(post.duration_seconds, Some(62));
        assert_eq!(post.uploader, "作者");
    }

    #[test]
    fn finds_complete_aweme_in_source_list() {
        let response = serde_json::json!({
            "aweme_list": [
                {"aweme_id": "100", "desc": "第一集"},
                {"aweme_id": "200", "desc": "目标剧集", "video": {"bit_rate": []}}
            ]
        });
        let detail = find_aweme_detail(&response, "aweme_list", "200").expect("应找到目标剧集");
        assert_eq!(text(&detail, &["desc"]).as_deref(), Some("目标剧集"));
        assert!(detail.get("video").is_some());
        assert!(find_aweme_detail(&response, "aweme_list", "300").is_none());
    }

    #[test]
    fn parses_nested_user_search_results() {
        let response = serde_json::json!({
            "data": [{
                "user_list": [{
                    "user_info": {
                        "sec_uid": "MS4w.test",
                        "nickname": "测试作者",
                        "unique_id": "author-id"
                    }
                }]
            }]
        });
        let mut users = Vec::new();
        collect_user_infos(&response, &mut users);
        assert!(users
            .iter()
            .any(|user| text(user, &["sec_uid"]).as_deref() == Some("MS4w.test")));
    }

    #[test]
    fn parses_douyin_danmaku() {
        let item: DouyinDanmaku = serde_json::from_value(serde_json::json!({
            "danmaku_id": "7570519311301133093",
            "user_id": "2502937746111147",
            "offset_time": 447,
            "text": "欢迎回来",
            "digg_count": 7
        }))
        .unwrap();
        assert_eq!(item.offset_time, 447);
        assert_eq!(item.text, "欢迎回来");
    }

    #[test]
    fn extracts_aweme_id_from_supported_links() {
        assert_eq!(aweme_id("https://www.douyin.com/video/123456?from=web"), Some("123456"));
        assert_eq!(aweme_id("https://www.douyin.com/note/987654/"), Some("987654"));
        assert_eq!(aweme_id("https://www.douyin.com/video/not-an-id"), None);
    }

    #[test]
    fn parses_native_douyin_media_format() {
        let format = native_format(&serde_json::json!({
            "gear_name": "normal_1080_0",
            "bit_rate": 2500000,
            "is_h265": 0,
            "play_addr": {
                "width": 1080,
                "height": 1920,
                "url_list": ["https://example.com/main.mp4", "https://example.com/fallback.mp4"]
            }
        }))
        .expect("应解析出抖音原生媒体格式");
        assert_eq!(format.height, Some(1920));
        assert_eq!(format.vcodec.as_deref(), Some("h264"));
        assert_eq!(format.url.as_deref(), Some("https://example.com/main.mp4"));
        assert_eq!(format.fallback_urls, vec!["https://example.com/fallback.mp4"]);
    }
}
