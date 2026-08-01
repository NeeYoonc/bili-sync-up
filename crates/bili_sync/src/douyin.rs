//! 抖音作者作品源。
//!
//! 作者枚举由抖音 Web API 完成；每条作品的媒体格式仍交给 yt-dlp
//! 解析，文件传输、并发、路径模板、质量筛选、NFO 与状态管理继续复用
//! `youtube.rs` 中已经接入项目统一下载器的外部媒体链路。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::{Json, Query};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::api::response::{SubmissionVideoInfo, SubmissionVideosResponse};
use crate::api::wrapper::{ApiError, ApiResponse};
use crate::config::CONFIG_DIR;
use crate::douyin_sign;
use crate::youtube::{YouTubeLoginResponse, YouTubeSearchResponse, YouTubeSearchResult};

const DOUYIN_POST_API: &str = "https://www.douyin.com/aweme/v1/web/aweme/post/";
const DOUYIN_DETAIL_API: &str = "https://www.douyin.com/aweme/v1/web/aweme/detail/";
const DOUYIN_SEARCH_API: &str = "https://www.douyin.com/aweme/v1/web/general/search/single/";
const DOUYIN_PROFILE_SELF_API: &str = "https://www.douyin.com/aweme/v1/web/user/profile/self/";
const DOUYIN_PROFILE_OTHER_API: &str = "https://www.douyin.com/aweme/v1/web/user/profile/other/";
const DOUYIN_FOLLOWING_API: &str = "https://www.douyin.com/aweme/v1/web/user/following/list/";
const DOUYIN_PUBLIC_SEARCH_API: &str = "https://www.so.com/s";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Deserialize)]
pub struct DouyinCookieImportRequest {
    pub cookies: String,
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
    pub page: Option<i32>,
    pub page_size: Option<i32>,
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
}

#[derive(Debug, Clone)]
pub struct DouyinProfile {
    pub avatar_url: Option<String>,
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
    Ok(ApiResponse::ok(YouTubeLoginResponse {
        logged_in: true,
        message: "已导入抖音 cookies.txt；作者作品扫描和媒体解析将使用此状态".to_string(),
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
    let mut pairs = common_query_pairs();
    pairs.extend([
        ("search_channel", "aweme_user_web".to_string()),
        ("keyword", keyword.to_string()),
        ("enable_history", "1".to_string()),
        ("search_source", "tab_search".to_string()),
        ("query_correct_type", "1".to_string()),
        ("is_filter_search", "0".to_string()),
        ("from_group_id", String::new()),
        ("offset", "0".to_string()),
        ("count", "15".to_string()),
        ("need_filter_settings", "1".to_string()),
        ("list_type", "multi".to_string()),
        ("search_id", String::new()),
    ]);
    let response = signed_get(DOUYIN_SEARCH_API, pairs).await?;
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
    let url = reqwest::Url::parse_with_params(DOUYIN_PUBLIC_SEARCH_API, &[("q", query)])?;
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
    let regex = Regex::new(r#"https?://www\.douyin\.com/user/(MS4w[A-Za-z0-9_-]+)"#)?;
    let mut sec_uids = Vec::new();
    let mut seen = HashSet::new();
    for captures in regex.captures_iter(&body) {
        let Some(sec_uid) = captures.get(1).map(|value| value.as_str().to_string()) else {
            continue;
        };
        if seen.insert(sec_uid.clone()) {
            sec_uids.push(sec_uid);
        }
        if sec_uids.len() >= 20 {
            break;
        }
    }

    let mut results = Vec::new();
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
        if search_result_matches(&result, keyword) {
            results.push(result);
        }
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
    let sec_uid = resolve_sec_user_id(&request.url).await?;
    let target_end = page.saturating_mul(page_size) as usize;
    let posts = fetch_posts_until(&sec_uid, target_end.saturating_add(1)).await?;
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
            cover: post.thumbnail.unwrap_or_default(),
            pubtime: post.published_at.unwrap_or_default(),
            duration: post.duration_seconds.unwrap_or_default(),
            view: 0,
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
    let page = fetch_post_page(&sec_uid, 0, 1).await?;
    page.profile
        .ok_or_else(|| anyhow!("抖音作者没有公开作品，无法取得头像资料"))
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
    // 图文笔记会包含多张 images，但没有能够进入项目现有
    // mp4/m4a 下载和媒体库链路的单一视频流；视频源只枚举视频作品。
    if item
        .get("images")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|images| !images.is_empty())
    {
        return None;
    }
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
        thumbnail: item
            .get("video")
            .and_then(|video| image_url(video.get("cover").or_else(|| video.get("origin_cover")))),
        published_at: timestamp
            .and_then(|value| chrono::DateTime::from_timestamp(value, 0).map(|date| date.format("%Y%m%d").to_string())),
        timestamp,
        duration_seconds: item
            .get("duration")
            .and_then(serde_json::Value::as_i64)
            .and_then(|duration| i32::try_from((duration + 500) / 1000).ok()),
    })
}

fn common_query_pairs() -> Vec<(&'static str, String)> {
    let mut pairs = vec![
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
    if let Some(ms_token) = cookie_values().get("msToken").cloned() {
        pairs.push(("msToken", ms_token));
    }
    pairs
}

async fn signed_get(base_url: &str, pairs: Vec<(&str, String)>) -> Result<serde_json::Value> {
    let params = serde_urlencoded::to_string(&pairs)?;
    let signature = douyin_sign::generate(&params);
    let mut url = reqwest::Url::parse_with_params(base_url, &pairs)?;
    url.query_pairs_mut().append_pair("a_bogus", &signature);
    let response = client()?
        .get(url)
        .header(reqwest::header::REFERER, "https://www.douyin.com/")
        .send()
        .await
        .context("请求抖音 Web API 失败")?;
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

fn cookie_header() -> Result<String> {
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

fn cookie_values() -> HashMap<String, String> {
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

fn image_url(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(|value| value.get("url_list").and_then(serde_json::Value::as_array))
        .and_then(|items| items.first())
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| value.and_then(serde_json::Value::as_str).map(str::to_string))
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
}
